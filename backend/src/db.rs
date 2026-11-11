//! SQLite 存储层。
//!
//! 表结构覆盖账户、服务器文件夹、邮件头、正文、附件、草稿、发件箱、
//! 联系人与键值设置。所有写操作都在调用方持有的单连接互斥锁内完成，
//! 本地单用户场景足够，也避免多连接写冲突。

use std::path::Path;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

pub use crate::model::Mailbox;
use crate::model::{
    Account, Attachment, Draft, MessageDetail, MessageSummary, OutboxItem, Participant, Settings,
    ThreadItem,
};

/// 打开数据库并完成迁移
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&conn)?;
    Ok(conn)
}

/// 建表（幂等）。schema 变更时递增 user_version 并补 ALTER。
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    email         TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL DEFAULT '',
    username      TEXT NOT NULL,
    password_ref  TEXT NOT NULL DEFAULT '',
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL DEFAULT 993,
    imap_security TEXT NOT NULL DEFAULT 'tls',
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL DEFAULT 465,
    smtp_security TEXT NOT NULL DEFAULT 'tls',
    signature     TEXT NOT NULL DEFAULT '',
    color         TEXT NOT NULL DEFAULT '#e8660c',
    sync_enabled  INTEGER NOT NULL DEFAULT 1,
    sync_interval INTEGER NOT NULL DEFAULT 5,
    last_sync_at  INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT NOT NULL DEFAULT '',
    sort_order    INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS mailboxes (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    delimiter      TEXT NOT NULL DEFAULT '/',
    role           TEXT NOT NULL DEFAULT 'custom',
    unread         INTEGER NOT NULL DEFAULT 0,
    total          INTEGER NOT NULL DEFAULT 0,
    uid_validity   INTEGER NOT NULL DEFAULT 0,
    last_uid       INTEGER NOT NULL DEFAULT 0,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    selectable     INTEGER NOT NULL DEFAULT 1,
    UNIQUE(account_id, name)
);
CREATE INDEX IF NOT EXISTS mailboxes_by_account ON mailboxes(account_id, sort_order);

CREATE TABLE IF NOT EXISTS messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    mailbox_id      INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid             INTEGER,
    message_id      TEXT NOT NULL DEFAULT '',
    thread_id       TEXT NOT NULL DEFAULT '',
    subject         TEXT NOT NULL DEFAULT '',
    from_name       TEXT NOT NULL DEFAULT '',
    from_addr       TEXT NOT NULL DEFAULT '',
    to_addrs        TEXT NOT NULL DEFAULT '[]',
    cc_addrs        TEXT NOT NULL DEFAULT '[]',
    bcc_addrs       TEXT NOT NULL DEFAULT '[]',
    reply_to        TEXT NOT NULL DEFAULT '',
    in_reply_to     TEXT NOT NULL DEFAULT '',
    refs            TEXT NOT NULL DEFAULT '[]',
    date            INTEGER NOT NULL DEFAULT 0,
    internal_date   INTEGER NOT NULL DEFAULT 0,
    size            INTEGER NOT NULL DEFAULT 0,
    snippet         TEXT NOT NULL DEFAULT '',
    seen            INTEGER NOT NULL DEFAULT 0,
    flagged         INTEGER NOT NULL DEFAULT 0,
    answered        INTEGER NOT NULL DEFAULT 0,
    draft           INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    raw_path        TEXT NOT NULL DEFAULT '',
    body_loaded     INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    UNIQUE(account_id, mailbox_id, uid)
);
CREATE INDEX IF NOT EXISTS messages_by_mailbox ON messages(mailbox_id, date DESC);
CREATE INDEX IF NOT EXISTS messages_by_thread ON messages(account_id, thread_id, date);
CREATE INDEX IF NOT EXISTS messages_by_date ON messages(date DESC);
CREATE INDEX IF NOT EXISTS messages_unread ON messages(account_id, seen);

CREATE TABLE IF NOT EXISTS bodies (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    text_body  TEXT NOT NULL DEFAULT '',
    html_body  TEXT NOT NULL DEFAULT '',
    calendar   TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS attachments (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part       INTEGER NOT NULL,
    filename   TEXT NOT NULL DEFAULT '',
    mime       TEXT NOT NULL DEFAULT 'application/octet-stream',
    size       INTEGER NOT NULL DEFAULT 0,
    content_id TEXT NOT NULL DEFAULT '',
    inline     INTEGER NOT NULL DEFAULT 0,
    saved_path TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS attachments_by_message ON attachments(message_id, part);

CREATE TABLE IF NOT EXISTS drafts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    to_addrs    TEXT NOT NULL DEFAULT '[]',
    cc_addrs    TEXT NOT NULL DEFAULT '[]',
    bcc_addrs   TEXT NOT NULL DEFAULT '[]',
    subject     TEXT NOT NULL DEFAULT '',
    body_html   TEXT NOT NULL DEFAULT '',
    attachments TEXT NOT NULL DEFAULT '[]',
    in_reply_to TEXT NOT NULL DEFAULT '',
    refs        TEXT NOT NULL DEFAULT '[]',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    to_addrs    TEXT NOT NULL DEFAULT '[]',
    cc_addrs    TEXT NOT NULL DEFAULT '[]',
    bcc_addrs   TEXT NOT NULL DEFAULT '[]',
    subject     TEXT NOT NULL DEFAULT '',
    body_html   TEXT NOT NULL DEFAULT '',
    attachments TEXT NOT NULL DEFAULT '[]',
    in_reply_to TEXT NOT NULL DEFAULT '',
    refs        TEXT NOT NULL DEFAULT '[]',
    scheduled_at INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'queued',
    error       TEXT NOT NULL DEFAULT '',
    attempts    INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    sent_at     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(status, scheduled_at);

CREATE TABLE IF NOT EXISTS contacts (
    email      TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT '',
    seen_count INTEGER NOT NULL DEFAULT 1,
    last_seen  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    at         INTEGER NOT NULL,
    ok         INTEGER NOT NULL,
    message    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_flags (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    seen       INTEGER,
    flagged    INTEGER,
    answered   INTEGER,
    created_at INTEGER NOT NULL
);
"#,
    )?;
    // v2：Microsoft 365 / Outlook.com（Graph API）支持。
    // 账户增加 provider/auth_type/tenant_id/client_id；邮件按 Graph remote_id
    // 去重；文件夹持久化 Graph folder id 与 delta 增量游标。
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if version < 2 {
        conn.execute_batch(
            r#"
ALTER TABLE accounts ADD COLUMN provider  TEXT NOT NULL DEFAULT 'imap';
ALTER TABLE accounts ADD COLUMN auth_type TEXT NOT NULL DEFAULT 'password';
ALTER TABLE accounts ADD COLUMN tenant_id TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN client_id TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN remote_id TEXT NOT NULL DEFAULT '';
ALTER TABLE mailboxes ADD COLUMN remote_id  TEXT NOT NULL DEFAULT '';
ALTER TABLE mailboxes ADD COLUMN sync_state TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS messages_by_remote
    ON messages(account_id, mailbox_id, remote_id);
"#,
        )?;
    }
    // v3：DBX 宿主连接管理（connection-provider）。账户行带 host 标记：
    // 连接期凭据只在 sidecar 内存会话里（不进本地密钥库），断开只复位标记、
    // 保留本地缓存。旧行默认 host_bound=0，行为与 v2 完全一致。
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if version < 3 {
        conn.execute_batch(
            r#"
ALTER TABLE accounts ADD COLUMN host_bound         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN host_connection_id TEXT    NOT NULL DEFAULT '';
"#,
        )?;
    }
    // v4：M365 纯后台接入与机密客户端。user_id 为 client_credentials 模式的
    // 目标邮箱（Graph /users/{id}/... 路径）；client_secret 只存 SecretStore，
    // 这里只放引用，绝不落明文。
    if version < 4 {
        conn.execute_batch(
            r#"
ALTER TABLE accounts ADD COLUMN user_id            TEXT    NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN client_secret_ref  TEXT    NOT NULL DEFAULT '';
"#,
        )?;
    }
    conn.pragma_update(None, "user_version", 4)?;
    Ok(())
}

/* ---------------- 通用工具 ---------------- */

/// JSON 数组字符串 → Vec<String>；解析失败返回空集合
pub fn parse_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Vec<String> → JSON 数组字符串
pub fn json_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

fn int_bool(value: i64) -> bool {
    value != 0
}

/* ---------------- 设置 ---------------- */

pub fn load_settings(conn: &Connection) -> rusqlite::Result<Settings> {
    let mut settings = Settings::default();
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    for row in rows {
        let (key, value) = row?;
        match key.as_str() {
            "theme" => settings.theme = value,
            "locale" => settings.locale = value,
            "density" => settings.density = value,
            "remoteImages" => settings.remote_images = value == "1",
            "markReadOnOpen" => settings.mark_read_on_open = value == "1",
            "undoDelay" => settings.undo_delay = value.parse().unwrap_or(settings.undo_delay),
            "syncInterval" => settings.sync_interval = value.parse().unwrap_or(settings.sync_interval),
            "syncScope" => settings.sync_scope = value,
            "initialLimit" => settings.initial_limit = value.parse().unwrap_or(settings.initial_limit),
            "layout" => settings.layout = value,
            "conversations" => settings.conversations = value == "1",
            "pageSize" => settings.page_size = value.parse().unwrap_or(settings.page_size),
            _ => {}
        }
    }
    Ok(settings)
}

pub fn save_settings(conn: &Connection, settings: &Settings) -> rusqlite::Result<()> {
    let entries = [
        ("theme", settings.theme.clone()),
        ("locale", settings.locale.clone()),
        ("density", settings.density.clone()),
        ("remoteImages", if settings.remote_images { "1" } else { "0" }.into()),
        ("markReadOnOpen", if settings.mark_read_on_open { "1" } else { "0" }.into()),
        ("undoDelay", settings.undo_delay.to_string()),
        ("syncInterval", settings.sync_interval.to_string()),
        ("syncScope", settings.sync_scope.clone()),
        ("initialLimit", settings.initial_limit.to_string()),
        ("layout", settings.layout.clone()),
        ("conversations", if settings.conversations { "1" } else { "0" }.into()),
        ("pageSize", settings.page_size.to_string()),
    ];
    for (key, value) in entries {
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
    }
    Ok(())
}

/* ---------------- 账户 ---------------- */

const ACCOUNT_COLUMNS: &str = "id, email, display_name, username, provider, auth_type, tenant_id, client_id, \
     imap_host, imap_port, imap_security, \
     smtp_host, smtp_port, smtp_security, signature, color, sync_enabled, sync_interval, \
     last_sync_at, last_error, host_bound, host_connection_id, user_id";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get("id")?,
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        username: row.get("username")?,
        provider: row.get("provider")?,
        auth_type: row.get("auth_type")?,
        tenant_id: row.get("tenant_id")?,
        client_id: row.get("client_id")?,
        imap_host: row.get("imap_host")?,
        imap_port: row.get::<_, i64>("imap_port")? as u16,
        imap_security: row.get("imap_security")?,
        smtp_host: row.get("smtp_host")?,
        smtp_port: row.get::<_, i64>("smtp_port")? as u16,
        smtp_security: row.get("smtp_security")?,
        signature: row.get("signature")?,
        color: row.get("color")?,
        sync_enabled: int_bool(row.get("sync_enabled")?),
        sync_interval: row.get("sync_interval")?,
        last_sync_at: row.get("last_sync_at")?,
        last_error: row.get("last_error")?,
        host_bound: int_bool(row.get("host_bound")?),
        host_connection_id: row.get("host_connection_id")?,
        user_id: row.get("user_id")?,
    })
}

pub fn account_list(conn: &Connection) -> rusqlite::Result<Vec<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY sort_order, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_account)?;
    rows.collect()
}

pub fn account_get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1");
    conn.query_row(&sql, params![id], row_to_account).optional()
}

pub fn account_by_email(conn: &Connection, email: &str) -> rusqlite::Result<Option<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE email = ?1");
    conn.query_row(&sql, params![email], row_to_account).optional()
}

/// 按 DBX 宿主连接 id 找绑定的账户行
pub fn account_by_host(conn: &Connection, connection_id: &str) -> rusqlite::Result<Option<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE host_connection_id = ?1");
    conn.query_row(&sql, params![connection_id], row_to_account).optional()
}

/// 把账户行绑定为「宿主连接管理」来源，并刷新连接相关列。
///
/// 只动身份列、服务器端点列与 host 标记列；签名、颜色、同步开关等用户偏好
/// 保持不变（复用向导账户时也不覆盖用户设置）。凭据不在这里写：host 账户的
/// 密码只在 sidecar 内存会话里，`password_ref` 维持原值（新建行则为空）。
pub fn account_apply_host(
    conn: &Connection,
    id: i64,
    account: &Account,
    connection_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET email = ?1, display_name = ?2, username = ?3, \
             imap_host = ?4, imap_port = ?5, imap_security = ?6, \
             smtp_host = ?7, smtp_port = ?8, smtp_security = ?9, \
             provider = ?10, auth_type = ?11, last_error = '', \
             host_bound = 1, host_connection_id = ?12, user_id = ?13, \
             tenant_id = ?14, client_id = ?15 \
         WHERE id = ?16",
        params![
            account.email,
            account.display_name,
            account.username,
            account.imap_host,
            account.imap_port,
            account.imap_security,
            account.smtp_host,
            account.smtp_port,
            account.smtp_security,
            account.provider,
            account.auth_type,
            connection_id,
            account.user_id,
            account.tenant_id,
            account.client_id,
            id,
        ],
    )?;
    Ok(())
}

/// 只绑定连接标记（不动身份/端点/凭据）：浏览器授权壳复用向导账户时用，
/// 保证 account/byConnection 能按 host_connection_id 找到该账户
pub fn account_bind_host(conn: &Connection, id: i64, connection_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET host_bound = 1, host_connection_id = ?2, last_error = '' WHERE id = ?1",
        params![id, connection_id],
    )?;
    Ok(())
}

/// 断开宿主连接：只复位 host 标记，账户行与全部本地缓存保留
pub fn account_unbind_host(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET host_bound = 0, host_connection_id = '' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// 新增账户，返回自增 id
#[allow(clippy::too_many_arguments)]
pub fn account_insert(conn: &Connection, account: &Account, password_ref: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO accounts(email, display_name, username, password_ref, provider, auth_type,
             tenant_id, client_id, imap_host, imap_port, imap_security, smtp_host, smtp_port,
             smtp_security, signature, color, sync_enabled, sync_interval, last_sync_at,
             last_error, sort_order, created_at, user_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
             ?18, 0, '', 0, ?19, ?20)",
        params![
            account.email,
            account.display_name,
            account.username,
            password_ref,
            account.provider,
            account.auth_type,
            account.tenant_id,
            account.client_id,
            account.imap_host,
            account.imap_port,
            account.imap_security,
            account.smtp_host,
            account.smtp_port,
            account.smtp_security,
            account.signature,
            account.color,
            if account.sync_enabled { 1 } else { 0 },
            account.sync_interval,
            crate::now_ms(),
            account.user_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn account_update(conn: &Connection, account: &Account) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET display_name = ?2, username = ?3, imap_host = ?4, imap_port = ?5,
             imap_security = ?6, smtp_host = ?7, smtp_port = ?8, smtp_security = ?9,
             signature = ?10, color = ?11, sync_enabled = ?12, sync_interval = ?13,
             tenant_id = ?14, client_id = ?15, user_id = ?16
         WHERE id = ?1",
        params![
            account.id,
            account.display_name,
            account.username,
            account.imap_host,
            account.imap_port,
            account.imap_security,
            account.smtp_host,
            account.smtp_port,
            account.smtp_security,
            account.signature,
            account.color,
            if account.sync_enabled { 1 } else { 0 },
            account.sync_interval,
            account.tenant_id,
            account.client_id,
            account.user_id,
        ],
    )?;
    Ok(())
}

pub fn account_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn account_password_ref(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT password_ref FROM accounts WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

/// Azure client_secret 只存 SecretStore，这里只放引用；空串表示未配置
pub fn account_client_secret_ref(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT client_secret_ref FROM accounts WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

pub fn account_set_client_secret_ref(conn: &Connection, id: i64, reference: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET client_secret_ref = ?2 WHERE id = ?1",
        params![id, reference],
    )?;
    Ok(())
}

pub fn account_set_sync(conn: &Connection, id: i64, at: i64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET last_sync_at = ?2, last_error = ?3 WHERE id = ?1",
        params![id, at, error],
    )?;
    Ok(())
}

/* ---------------- 文件夹 ---------------- */

fn row_to_mailbox(row: &Row<'_>) -> rusqlite::Result<Mailbox> {
    Ok(Mailbox {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        name: row.get("name")?,
        delimiter: row.get("delimiter")?,
        role: row.get("role")?,
        unread: row.get("unread")?,
        total: row.get("total")?,
        uid_validity: row.get("uid_validity")?,
        last_uid: row.get("last_uid")?,
        selectable: int_bool(row.get("selectable")?),
        sort_order: row.get("sort_order")?,
    })
}

pub fn mailbox_list(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<Mailbox>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, name, delimiter, role, unread, total, uid_validity, last_uid,
                selectable, sort_order
         FROM mailboxes WHERE account_id = ?1 ORDER BY sort_order, id",
    )?;
    let rows = stmt.query_map(params![account_id], row_to_mailbox)?;
    rows.collect()
}

pub fn mailbox_get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Mailbox>> {
    conn.query_row(
        "SELECT id, account_id, name, delimiter, role, unread, total, uid_validity, last_uid,
                selectable, sort_order
         FROM mailboxes WHERE id = ?1",
        params![id],
        row_to_mailbox,
    )
    .optional()
}

/// 按服务器名称 upsert 文件夹，返回本地 id。角色只在服务器未声明时保留原值。
/// `remote_id` 是 Graph 的 folder id；IMAP 路径传空串（保留原值）。
pub fn mailbox_upsert(
    conn: &Connection,
    account_id: i64,
    name: &str,
    delimiter: &str,
    role: &str,
    selectable: bool,
    sort_order: i64,
    remote_id: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO mailboxes(account_id, name, delimiter, role, selectable, sort_order, remote_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id, name) DO UPDATE SET
             delimiter = excluded.delimiter,
             role = CASE WHEN excluded.role <> 'custom' THEN excluded.role ELSE mailboxes.role END,
             selectable = excluded.selectable,
             sort_order = excluded.sort_order,
             remote_id = CASE WHEN excluded.remote_id <> '' THEN excluded.remote_id ELSE mailboxes.remote_id END",
        params![account_id, name, delimiter, role, if selectable { 1 } else { 0 }, sort_order, remote_id],
    )?;
    conn.query_row(
        "SELECT id FROM mailboxes WHERE account_id = ?1 AND name = ?2",
        params![account_id, name],
        |row| row.get(0),
    )
}

pub fn mailbox_set_counts(conn: &Connection, id: i64, total: i64, unread: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mailboxes SET total = ?2, unread = ?3 WHERE id = ?1",
        params![id, total, unread],
    )?;
    Ok(())
}

pub fn mailbox_sync_state(
    conn: &Connection,
    id: i64,
    uid_validity: i64,
    last_uid: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mailboxes SET uid_validity = ?2, last_uid = ?3 WHERE id = ?1",
        params![id, uid_validity, last_uid],
    )?;
    Ok(())
}

/// UIDVALIDITY 变化时清空该文件夹的本地邮件（服务器重新编号了）
pub fn mailbox_reset(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM messages WHERE mailbox_id = ?1", params![id])?;
    conn.execute(
        "UPDATE mailboxes SET last_uid = 0, uid_validity = 0, total = 0, unread = 0 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/* ---------------- Graph 增量（Microsoft 365 / Outlook.com） ---------------- */

/// 读取 Graph 文件夹 id（IMAP 文件夹为空）
pub fn mailbox_remote_id(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT remote_id FROM mailboxes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

/// 读取 delta 增量游标
pub fn mailbox_delta_link(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT sync_state FROM mailboxes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

/// 保存 delta 增量游标
pub fn mailbox_set_delta_link(conn: &Connection, id: i64, link: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE mailboxes SET sync_state = ?2 WHERE id = ?1",
        params![id, link],
    )?;
    Ok(())
}

/* ---------------- 邮件 ---------------- */

const SUMMARY_SQL: &str = "m.id, m.account_id, m.mailbox_id, mb.role AS mailbox_role, m.uid, \
    m.message_id, m.thread_id, m.subject, m.from_name, m.from_addr, m.to_addrs, m.cc_addrs, \
    m.date, m.size, m.snippet, m.seen, m.flagged, m.answered, m.draft, m.has_attachments, m.body_loaded";

fn row_to_summary(row: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        mailbox_id: row.get("mailbox_id")?,
        mailbox_role: row.get("mailbox_role")?,
        uid: row.get("uid")?,
        message_id: row.get("message_id")?,
        thread_id: row.get("thread_id")?,
        subject: row.get("subject")?,
        from_name: row.get("from_name")?,
        from_addr: row.get("from_addr")?,
        to_addrs: parse_list(&row.get::<_, String>("to_addrs")?),
        cc_addrs: parse_list(&row.get::<_, String>("cc_addrs")?),
        date: row.get("date")?,
        size: row.get("size")?,
        snippet: row.get("snippet")?,
        seen: int_bool(row.get("seen")?),
        flagged: int_bool(row.get("flagged")?),
        answered: int_bool(row.get("answered")?),
        draft: int_bool(row.get("draft")?),
        has_attachments: int_bool(row.get("has_attachments")?),
        body_loaded: int_bool(row.get("body_loaded")?),
    })
}

/// 邮件头写入参数
pub struct HeaderRow {
    pub account_id: i64,
    pub mailbox_id: i64,
    pub uid: Option<i64>,
    pub message_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub reply_to: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub date: i64,
    pub internal_date: i64,
    pub size: i64,
    pub snippet: String,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub has_attachments: bool,
}

/// 写入或更新邮件头；返回本地 id。
///
/// 已存在的行只更新「服务器可变」字段：主题、发件人、日期、旗标、大小，
/// 不覆盖本地正文加载状态与 raw_path。
pub fn message_upsert_header(conn: &Connection, row: &HeaderRow) -> rusqlite::Result<i64> {
    conn.query_row(
        "INSERT INTO messages(account_id, mailbox_id, uid, message_id, thread_id, subject,
             from_name, from_addr, to_addrs, cc_addrs, reply_to, in_reply_to, refs, date,
             internal_date, size, snippet, seen, flagged, answered, draft, has_attachments,
             created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
             ?18, ?19, ?20, ?21, ?22, ?23)
         ON CONFLICT(account_id, mailbox_id, uid) DO UPDATE SET
             message_id = excluded.message_id,
             thread_id = excluded.thread_id,
             subject = excluded.subject,
             from_name = excluded.from_name,
             from_addr = excluded.from_addr,
             to_addrs = excluded.to_addrs,
             cc_addrs = excluded.cc_addrs,
             reply_to = excluded.reply_to,
             in_reply_to = excluded.in_reply_to,
             refs = excluded.refs,
             date = excluded.date,
             internal_date = excluded.internal_date,
             size = excluded.size,
             snippet = CASE WHEN excluded.snippet <> '' THEN excluded.snippet ELSE messages.snippet END,
             flagged = excluded.flagged,
             answered = excluded.answered,
             draft = excluded.draft,
             has_attachments = excluded.has_attachments
         RETURNING id",
        params![
            row.account_id,
            row.mailbox_id,
            row.uid,
            row.message_id,
            row.thread_id,
            row.subject,
            row.from_name,
            row.from_addr,
            json_list(&row.to_addrs),
            json_list(&row.cc_addrs),
            row.reply_to,
            row.in_reply_to,
            json_list(&row.references),
            row.date,
            row.internal_date,
            row.size,
            row.snippet,
            if row.seen { 1 } else { 0 },
            if row.flagged { 1 } else { 0 },
            if row.answered { 1 } else { 0 },
            if row.draft { 1 } else { 0 },
            if row.has_attachments { 1 } else { 0 },
            crate::now_ms(),
        ],
        |row| row.get(0),
    )
}

/// 本地邮件（.eml 导入、无服务器 UID）插入
pub fn message_insert_local(conn: &Connection, row: &HeaderRow, raw_path: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO messages(account_id, mailbox_id, uid, message_id, thread_id, subject,
             from_name, from_addr, to_addrs, cc_addrs, reply_to, in_reply_to, refs, date,
             internal_date, size, snippet, seen, flagged, answered, draft, has_attachments,
             raw_path, created_at)
         VALUES(?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
             ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        params![
            row.account_id,
            row.mailbox_id,
            row.message_id,
            row.thread_id,
            row.subject,
            row.from_name,
            row.from_addr,
            json_list(&row.to_addrs),
            json_list(&row.cc_addrs),
            row.reply_to,
            row.in_reply_to,
            json_list(&row.references),
            row.date,
            row.internal_date,
            row.size,
            row.snippet,
            if row.seen { 1 } else { 0 },
            if row.flagged { 1 } else { 0 },
            if row.answered { 1 } else { 0 },
            if row.draft { 1 } else { 0 },
            if row.has_attachments { 1 } else { 0 },
            raw_path,
            crate::now_ms(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Graph 邮件按 remote_id 写入或更新；返回本地 id。
///
/// uid 对 Graph 账户只是文件夹内的本地序号（MAX+1），真实去重键是
/// (account_id, mailbox_id, remote_id)。已存在时不覆盖 seen（与 IMAP 路径
/// 语义一致，本地已读状态由 pending_flags 机制双向同步）。
pub fn message_upsert_remote(
    conn: &Connection,
    row: &HeaderRow,
    remote_id: &str,
) -> rusqlite::Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM messages WHERE account_id = ?1 AND mailbox_id = ?2 AND remote_id = ?3",
            params![row.account_id, row.mailbox_id, remote_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        conn.execute(
            "UPDATE messages SET message_id = ?2, thread_id = ?3, subject = ?4, from_name = ?5,
                 from_addr = ?6, to_addrs = ?7, cc_addrs = ?8, reply_to = ?9, in_reply_to = ?10,
                 refs = ?11, date = ?12, internal_date = ?13, size = ?14,
                 snippet = CASE WHEN ?15 <> '' THEN ?15 ELSE snippet END,
                 flagged = ?16, answered = ?17, draft = ?18, has_attachments = ?19
             WHERE id = ?1",
            params![
                id,
                row.message_id,
                row.thread_id,
                row.subject,
                row.from_name,
                row.from_addr,
                json_list(&row.to_addrs),
                json_list(&row.cc_addrs),
                row.reply_to,
                row.in_reply_to,
                json_list(&row.references),
                row.date,
                row.internal_date,
                row.size,
                row.snippet,
                if row.flagged { 1 } else { 0 },
                if row.answered { 1 } else { 0 },
                if row.draft { 1 } else { 0 },
                if row.has_attachments { 1 } else { 0 },
            ],
        )?;
        return Ok(id);
    }
    let uid: i64 = conn.query_row(
        "SELECT COALESCE(MAX(uid), 0) + 1 FROM messages WHERE account_id = ?1 AND mailbox_id = ?2",
        params![row.account_id, row.mailbox_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO messages(account_id, mailbox_id, uid, remote_id, message_id, thread_id,
             subject, from_name, from_addr, to_addrs, cc_addrs, reply_to, in_reply_to, refs,
             date, internal_date, size, snippet, seen, flagged, answered, draft,
             has_attachments, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
             ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            row.account_id,
            row.mailbox_id,
            uid,
            remote_id,
            row.message_id,
            row.thread_id,
            row.subject,
            row.from_name,
            row.from_addr,
            json_list(&row.to_addrs),
            json_list(&row.cc_addrs),
            row.reply_to,
            row.in_reply_to,
            json_list(&row.references),
            row.date,
            row.internal_date,
            row.size,
            row.snippet,
            if row.seen { 1 } else { 0 },
            if row.flagged { 1 } else { 0 },
            if row.answered { 1 } else { 0 },
            if row.draft { 1 } else { 0 },
            if row.has_attachments { 1 } else { 0 },
            crate::now_ms(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 删除 Graph 远端已不存在的邮件（delta `@removed`）
pub fn message_delete_by_remote(
    conn: &Connection,
    account_id: i64,
    mailbox_id: i64,
    remote_id: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND mailbox_id = ?2 AND remote_id = ?3",
        params![account_id, mailbox_id, remote_id],
    )
}

/// 按本地邮件 id 取 Graph remote id（旗标推送用；IMAP 邮件为空）
pub fn message_remote_id(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT remote_id FROM messages WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

pub fn message_get(conn: &Connection, id: i64) -> rusqlite::Result<Option<MessageSummary>> {
    let sql = format!(
        "SELECT {SUMMARY_SQL} FROM messages m JOIN mailboxes mb ON mb.id = m.mailbox_id WHERE m.id = ?1"
    );
    conn.query_row(&sql, params![id], row_to_summary).optional()
}

pub fn message_detail(conn: &Connection, id: i64) -> rusqlite::Result<Option<MessageDetail>> {
    let sql = format!(
        "SELECT {SUMMARY_SQL}, m.in_reply_to, m.refs, b.text_body, b.html_body,
                a.email AS account_email, a.display_name AS account_name, a.signature
         FROM messages m
         JOIN mailboxes mb ON mb.id = m.mailbox_id
         JOIN accounts a ON a.id = m.account_id
         LEFT JOIN bodies b ON b.message_id = m.id
         WHERE m.id = ?1"
    );
    let detail = conn
        .query_row(&sql, params![id], |row| {
            let summary = row_to_summary(row)?;
            Ok(MessageDetail {
                summary,
                text_body: row.get::<_, Option<String>>("text_body")?.unwrap_or_default(),
                html_body: row.get::<_, Option<String>>("html_body")?.unwrap_or_default(),
                attachments: Vec::new(),
                in_reply_to: row.get::<_, String>("in_reply_to")?,
                references: parse_list(&row.get::<_, String>("refs")?),
                account_email: row.get("account_email")?,
                account_name: row.get::<_, Option<String>>("account_name")?.unwrap_or_default(),
                signature: row.get::<_, Option<String>>("signature")?.unwrap_or_default(),
            })
        })
        .optional()?;
    Ok(detail)
}

pub fn message_thread(conn: &Connection, account_id: i64, thread_id: &str) -> rusqlite::Result<Vec<MessageSummary>> {
    let sql = format!(
        "SELECT {SUMMARY_SQL} FROM messages m JOIN mailboxes mb ON mb.id = m.mailbox_id
         WHERE m.account_id = ?1 AND m.thread_id = ?2 ORDER BY m.date ASC, m.id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![account_id, thread_id], row_to_summary)?;
    rows.collect()
}

pub fn message_raw_path(conn: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT raw_path FROM messages WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or_default())
}

pub fn message_set_raw(conn: &Connection, id: i64, raw_path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages SET raw_path = ?2 WHERE id = ?1",
        params![id, raw_path],
    )?;
    Ok(())
}

pub fn message_mark_body_loaded(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE messages SET body_loaded = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

/// 懒加载补全线程头：只填空字段，不覆盖已有值（正文按需下载后由 ensure_body 回填）
pub fn message_backfill_header(
    conn: &Connection,
    id: i64,
    message_id: &str,
    in_reply_to: &str,
    references: &[String],
    reply_to: &str,
    size: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages SET
            message_id = CASE WHEN message_id = '' THEN ?2 ELSE message_id END,
            in_reply_to = CASE WHEN in_reply_to = '' THEN ?3 ELSE in_reply_to END,
            refs = CASE WHEN refs = '' OR refs = '[]' THEN ?4 ELSE refs END,
            reply_to = CASE WHEN reply_to = '' THEN ?5 ELSE reply_to END,
            size = CASE WHEN size = 0 THEN ?6 ELSE size END
         WHERE id = ?1",
        params![
            id,
            message_id,
            in_reply_to,
            json_list(references),
            reply_to,
            size
        ],
    )?;
    Ok(())
}

/// 正文入库；`html` 已内联 cid 图片，`text` 用于搜索与纯文本视图
pub fn body_save(conn: &Connection, id: i64, text: &str, html: &str, calendar: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bodies(message_id, text_body, html_body, calendar) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(message_id) DO UPDATE SET text_body = excluded.text_body,
             html_body = excluded.html_body, calendar = excluded.calendar",
        params![id, text, html, calendar],
    )?;
    Ok(())
}

pub fn attachments_replace(conn: &Connection, message_id: i64, items: &[Attachment]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM attachments WHERE message_id = ?1", params![message_id])?;
    for item in items {
        conn.execute(
            "INSERT INTO attachments(message_id, part, filename, mime, size, content_id, inline)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message_id,
                item.index,
                item.filename,
                item.mime,
                item.size,
                item.content_id,
                if item.inline { 1 } else { 0 }
            ],
        )?;
    }
    Ok(())
}

pub fn attachments_list(conn: &Connection, message_id: i64) -> rusqlite::Result<Vec<Attachment>> {
    let mut stmt = conn.prepare(
        "SELECT part AS idx, filename, mime, size, content_id, inline FROM attachments
         WHERE message_id = ?1 ORDER BY part",
    )?;
    let rows = stmt.query_map(params![message_id], |row| {
        Ok(Attachment {
            index: row.get("idx")?,
            filename: row.get("filename")?,
            mime: row.get("mime")?,
            size: row.get("size")?,
            content_id: row.get("content_id")?,
            inline: int_bool(row.get("inline")?),
        })
    })?;
    rows.collect()
}

pub fn message_update_flags(
    conn: &Connection,
    id: i64,
    seen: Option<bool>,
    flagged: Option<bool>,
    answered: Option<bool>,
    pending: bool,
) -> rusqlite::Result<()> {
    if let Some(value) = seen {
        conn.execute("UPDATE messages SET seen = ?2 WHERE id = ?1", params![id, value as i64])?;
    }
    if let Some(value) = flagged {
        conn.execute("UPDATE messages SET flagged = ?2 WHERE id = ?1", params![id, value as i64])?;
    }
    if let Some(value) = answered {
        conn.execute("UPDATE messages SET answered = ?2 WHERE id = ?1", params![id, value as i64])?;
    }
    if pending {
        conn.execute(
            "INSERT INTO pending_flags(message_id, seen, flagged, answered, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(message_id) DO UPDATE SET
                 seen = COALESCE(excluded.seen, pending_flags.seen),
                 flagged = COALESCE(excluded.flagged, pending_flags.flagged),
                 answered = COALESCE(excluded.answered, pending_flags.answered)",
            params![
                id,
                seen.map(|value| value as i64),
                flagged.map(|value| value as i64),
                answered.map(|value| value as i64),
                crate::now_ms()
            ],
        )?;
    }
    Ok(())
}

/// 服务器旗标刷新：跳过有本地待同步标记的邮件
pub fn message_apply_server_flags(conn: &Connection, id: i64, seen: bool, flagged: bool, answered: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages SET seen = ?2, flagged = ?3, answered = ?4
         WHERE id = ?1 AND id NOT IN (SELECT message_id FROM pending_flags)",
        params![id, seen as i64, flagged as i64, answered as i64],
    )?;
    Ok(())
}

pub fn pending_flags_list(conn: &Connection) -> rusqlite::Result<Vec<(i64, Option<bool>, Option<bool>, Option<bool>)>> {
    let mut stmt = conn.prepare("SELECT message_id, seen, flagged, answered FROM pending_flags")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?.map(int_bool),
            row.get::<_, Option<i64>>(2)?.map(int_bool),
            row.get::<_, Option<i64>>(3)?.map(int_bool),
        ))
    })?;
    rows.collect()
}

pub fn pending_flags_clear(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM pending_flags WHERE message_id = ?1", params![id])?;
    Ok(())
}

pub fn message_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
    Ok(())
}

/// 移动邮件后更新本地归属（服务器已 MOVE 成功）
pub fn message_set_mailbox(conn: &Connection, id: i64, mailbox_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages SET mailbox_id = ?2 WHERE id = ?1",
        params![id, mailbox_id],
    )?;
    Ok(())
}

/// Graph 移动后更新远端 ID：默认 ID 机制下 move 响应返回替换后的新 ID，
/// 不更新会导致后续正文/旗标寻址旧 ID 失败、delta 回流产生重复
pub fn message_set_remote(conn: &Connection, id: i64, remote_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages SET remote_id = ?2 WHERE id = ?1",
        params![id, remote_id],
    )?;
    Ok(())
}

/* ---------------- 会话列表 ---------------- */

/// 会话列表筛选条件
#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    pub mailbox_id: Option<i64>,
    pub role: Option<String>,
    pub account_id: Option<i64>,
    pub query: String,
    /// all / unread / flagged / files
    pub kind: String,
    pub limit: i64,
    pub offset: i64,
}

pub fn thread_list(conn: &Connection, filter: &ThreadFilter) -> rusqlite::Result<Vec<ThreadItem>> {
    let mut conditions: Vec<String> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(mailbox_id) = filter.mailbox_id {
        conditions.push("m.mailbox_id = ?".into());
        args.push(Box::new(mailbox_id));
    }
    if let Some(role) = filter.role.as_deref().filter(|value| !value.is_empty()) {
        match role {
            "starred" => conditions.push("m.flagged = 1 AND mb.role NOT IN ('trash','junk')".into()),
            "all" => conditions.push("mb.role NOT IN ('trash','junk')".into()),
            "inbox" => conditions.push("mb.role = 'inbox'".into()),
            other => {
                conditions.push("mb.role = ?".into());
                args.push(Box::new(other.to_string()));
            }
        }
    }
    if let Some(account_id) = filter.account_id {
        conditions.push("m.account_id = ?".into());
        args.push(Box::new(account_id));
    }
    let query = filter.query.trim();
    if !query.is_empty() {
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        conditions.push(
            "(m.subject LIKE ? ESCAPE '\\' OR m.snippet LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\' \
             OR m.from_name LIKE ? ESCAPE '\\' OR m.to_addrs LIKE ? ESCAPE '\\' \
             OR EXISTS(SELECT 1 FROM bodies b WHERE b.message_id = m.id AND \
                (b.text_body LIKE ? ESCAPE '\\' OR b.html_body LIKE ? ESCAPE '\\')))"
                .into(),
        );
        for _ in 0..6 {
            args.push(Box::new(like.clone()));
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };
    // 扫描上限：本地缓存通常只有几千封，800 条足够覆盖当前视图
    let scan_limit = (filter.limit + filter.offset).max(200) * 4;
    let sql = format!(
        "SELECT {SUMMARY_SQL} FROM messages m JOIN mailboxes mb ON mb.id = m.mailbox_id
         {where_sql} ORDER BY m.date DESC, m.id DESC LIMIT {scan_limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), row_to_summary)?;

    let mut threads: Vec<ThreadItem> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in rows {
        let message = row?;
        let key = if message.thread_id.is_empty() {
            format!("m{}", message.id)
        } else {
            message.thread_id.clone()
        };
        if let Some(position) = index.get(&key).copied() {
            let thread = &mut threads[position];
            thread.count += 1;
            if !message.seen {
                thread.unread += 1;
            }
            thread.flagged |= message.flagged;
            thread.has_attachments |= message.has_attachments;
            thread.answered |= message.answered;
            if !thread.participants.iter().any(|p| p.email == message.from_addr) {
                thread.participants.push(Participant {
                    name: message.from_name.clone(),
                    email: message.from_addr.clone(),
                });
            }
            continue;
        }
        index.insert(key.clone(), threads.len());
        threads.push(ThreadItem {
            thread_id: key,
            subject: clean_subject(&message.subject),
            snippet: message.snippet.clone(),
            date: message.date,
            from_name: message.from_name.clone(),
            from_addr: message.from_addr.clone(),
            to_addrs: message.to_addrs.clone(),
            account_id: message.account_id,
            account_email: String::new(),
            account_color: String::new(),
            mailbox_id: message.mailbox_id,
            mailbox_role: message.mailbox_role.clone(),
            latest_id: message.id,
            count: 1,
            unread: if message.seen { 0 } else { 1 },
            flagged: message.flagged,
            has_attachments: message.has_attachments,
            answered: message.answered,
            participants: vec![Participant {
                name: message.from_name.clone(),
                email: message.from_addr.clone(),
            }],
        });
    }

    // 过滤（会话级）与账户装饰
    let mut accounts: std::collections::HashMap<i64, (String, String)> = std::collections::HashMap::new();
    for account in account_list(conn)? {
        accounts.insert(account.id, (account.email, account.color));
    }
    threads.retain(|thread| match filter.kind.as_str() {
        "unread" => thread.unread > 0,
        "flagged" => thread.flagged,
        "files" => thread.has_attachments,
        _ => true,
    });
    for thread in &mut threads {
        if let Some((email, color)) = accounts.get(&thread.account_id) {
            thread.account_email = email.clone();
            thread.account_color = color.clone();
        }
    }

    let start = filter.offset.max(0) as usize;
    let limit = filter.limit.max(1) as usize;
    Ok(threads.into_iter().skip(start).take(limit).collect())
}

/// 去掉 Re:/Fwd: 等前缀，用于会话标题
pub fn clean_subject(subject: &str) -> String {
    let mut value = subject.trim().to_string();
    loop {
        let lower = value.to_lowercase();
        let trimmed = ["re:", "re：", "fw:", "fwd:", "答复:", "转发:"]
            .iter()
            .find_map(|prefix| lower.strip_prefix(prefix).map(|_| value[prefix.len()..].trim().to_string()));
        match trimmed {
            Some(next) if !next.is_empty() => value = next,
            _ => break,
        }
    }
    value
}

/* ---------------- 草稿 ---------------- */

const DRAFT_SQL: &str = "id, account_id, to_addrs, cc_addrs, bcc_addrs, subject, body_html, \
    attachments, in_reply_to, refs, created_at, updated_at";

fn row_to_draft(row: &Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        to_addrs: parse_list(&row.get::<_, String>("to_addrs")?),
        cc_addrs: parse_list(&row.get::<_, String>("cc_addrs")?),
        bcc_addrs: parse_list(&row.get::<_, String>("bcc_addrs")?),
        subject: row.get("subject")?,
        body_html: row.get("body_html")?,
        attachments: serde_json::from_str(&row.get::<_, String>("attachments")?).unwrap_or_default(),
        in_reply_to: row.get("in_reply_to")?,
        references: parse_list(&row.get::<_, String>("refs")?),
        updated_at: row.get("updated_at")?,
        created_at: row.get("created_at")?,
    })
}

pub fn draft_list(conn: &Connection) -> rusqlite::Result<Vec<Draft>> {
    let sql = format!("SELECT {DRAFT_SQL} FROM drafts ORDER BY updated_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_draft)?;
    rows.collect()
}

pub fn draft_save(conn: &Connection, draft: &Draft) -> rusqlite::Result<i64> {
    let attachments = serde_json::to_string(&draft.attachments).unwrap_or_else(|_| "[]".into());
    if draft.id > 0 {
        conn.execute(
            "UPDATE drafts SET account_id = ?2, to_addrs = ?3, cc_addrs = ?4, bcc_addrs = ?5,
                 subject = ?6, body_html = ?7, attachments = ?8, in_reply_to = ?9, refs = ?10,
                 updated_at = ?11 WHERE id = ?1",
            params![
                draft.id,
                draft.account_id,
                json_list(&draft.to_addrs),
                json_list(&draft.cc_addrs),
                json_list(&draft.bcc_addrs),
                draft.subject,
                draft.body_html,
                attachments,
                draft.in_reply_to,
                json_list(&draft.references),
                crate::now_ms(),
            ],
        )?;
        Ok(draft.id)
    } else {
        conn.execute(
            "INSERT INTO drafts(account_id, to_addrs, cc_addrs, bcc_addrs, subject, body_html,
                 attachments, in_reply_to, refs, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                draft.account_id,
                json_list(&draft.to_addrs),
                json_list(&draft.cc_addrs),
                json_list(&draft.bcc_addrs),
                draft.subject,
                draft.body_html,
                attachments,
                draft.in_reply_to,
                json_list(&draft.references),
                crate::now_ms(),
                crate::now_ms(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

pub fn draft_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
    Ok(())
}

/* ---------------- 发件箱 ---------------- */

const OUTBOX_SQL: &str = "o.id, o.account_id, o.to_addrs, o.cc_addrs, o.bcc_addrs, o.subject, \
    o.body_html, o.attachments, o.scheduled_at, o.status, o.error, o.attempts, o.created_at, \
    o.sent_at, o.in_reply_to, o.refs, a.email AS account_email";

fn row_to_outbox(row: &Row<'_>) -> rusqlite::Result<OutboxItem> {
    Ok(OutboxItem {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        account_email: row.get("account_email")?,
        to_addrs: parse_list(&row.get::<_, String>("to_addrs")?),
        cc_addrs: parse_list(&row.get::<_, String>("cc_addrs")?),
        bcc_addrs: parse_list(&row.get::<_, String>("bcc_addrs")?),
        subject: row.get("subject")?,
        body_html: row.get("body_html")?,
        attachments: serde_json::from_str(&row.get::<_, String>("attachments")?).unwrap_or_default(),
        scheduled_at: row.get("scheduled_at")?,
        status: row.get("status")?,
        error: row.get("error")?,
        attempts: row.get("attempts")?,
        created_at: row.get("created_at")?,
        sent_at: row.get("sent_at")?,
        in_reply_to: row.get("in_reply_to")?,
        references: parse_list(&row.get::<_, String>("refs")?),
    })
}

pub fn outbox_insert(conn: &Connection, item: &OutboxItem) -> rusqlite::Result<i64> {
    let attachments = serde_json::to_string(&item.attachments).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO outbox(account_id, to_addrs, cc_addrs, bcc_addrs, subject, body_html,
             attachments, in_reply_to, refs, scheduled_at, status, error, attempts, created_at, sent_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', '', 0, ?11, 0)",
        params![
            item.account_id,
            json_list(&item.to_addrs),
            json_list(&item.cc_addrs),
            json_list(&item.bcc_addrs),
            item.subject,
            item.body_html,
            attachments,
            item.in_reply_to,
            json_list(&item.references),
            item.scheduled_at,
            crate::now_ms(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn outbox_list(conn: &Connection) -> rusqlite::Result<Vec<OutboxItem>> {
    let sql = format!(
        "SELECT {OUTBOX_SQL} FROM outbox o JOIN accounts a ON a.id = o.account_id
         WHERE o.status IN ('queued','sending','failed') OR o.sent_at > ?1
         ORDER BY o.scheduled_at DESC LIMIT 100"
    );
    let cutoff = crate::now_ms() - 30 * 24 * 3600 * 1000;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![cutoff], row_to_outbox)?;
    rows.collect()
}

pub fn outbox_due(conn: &Connection, now: i64) -> rusqlite::Result<Vec<OutboxItem>> {
    let sql = format!(
        "SELECT {OUTBOX_SQL} FROM outbox o JOIN accounts a ON a.id = o.account_id
         WHERE o.status = 'queued' AND o.scheduled_at <= ?1 ORDER BY o.scheduled_at LIMIT 10"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![now], row_to_outbox)?;
    rows.collect()
}

pub fn outbox_get(conn: &Connection, id: i64) -> rusqlite::Result<Option<OutboxItem>> {
    let sql = format!(
        "SELECT {OUTBOX_SQL} FROM outbox o JOIN accounts a ON a.id = o.account_id WHERE o.id = ?1"
    );
    conn.query_row(&sql, params![id], row_to_outbox).optional()
}

pub fn outbox_mark(conn: &Connection, id: i64, status: &str, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE outbox SET status = ?2, error = ?3 WHERE id = ?1",
        params![id, status, error],
    )?;
    Ok(())
}

pub fn outbox_mark_sent(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE outbox SET status = 'sent', sent_at = ?2, error = '' WHERE id = ?1",
        params![id, crate::now_ms()],
    )?;
    Ok(())
}

pub fn outbox_bump(conn: &Connection, id: i64) -> rusqlite::Result<i64> {
    conn.execute("UPDATE outbox SET attempts = attempts + 1 WHERE id = ?1", params![id])?;
    Ok(conn.query_row("SELECT attempts FROM outbox WHERE id = ?1", params![id], |row| row.get(0))?)
}

pub fn outbox_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn outbox_requeue(conn: &Connection, id: i64, at: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE outbox SET status = 'queued', error = '', scheduled_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/* ---------------- 联系人 ---------------- */

pub fn contacts_touch(conn: &Connection, email: &str, name: &str, at: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO contacts(email, name, seen_count, last_seen) VALUES(?1, ?2, 1, ?3)
         ON CONFLICT(email) DO UPDATE SET
             name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE contacts.name END,
             seen_count = contacts.seen_count + 1, last_seen = excluded.last_seen",
        params![email, name, at],
    )?;
    Ok(())
}

pub fn contacts_list(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<Participant>> {
    let mut stmt = conn.prepare(
        "SELECT email, name FROM contacts ORDER BY seen_count DESC, last_seen DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(Participant {
            email: row.get("email")?,
            name: row.get("name")?,
        })
    })?;
    rows.collect()
}

/* ---------------- 同步日志 ---------------- */

pub fn sync_log_append(conn: &Connection, account_id: i64, ok: bool, message: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_log(account_id, at, ok, message) VALUES(?1, ?2, ?3, ?4)",
        params![account_id, crate::now_ms(), if ok { 1 } else { 0 }, message],
    )?;
    conn.execute(
        "DELETE FROM sync_log WHERE id < (SELECT MAX(id) - 500 FROM sync_log)",
        [],
    )?;
    Ok(())
}

pub fn sync_log_list(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT account_id, at, ok, message FROM sync_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(serde_json::json!({
            "accountId": row.get::<_, i64>(0)?,
            "at": row.get::<_, i64>(1)?,
            "ok": row.get::<_, i64>(2)? != 0,
            "message": row.get::<_, String>(3)?,
        }))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Account;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_db() -> Connection {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dbx-mail-dbtest-{}-{}-{seq}.sqlite",
            std::process::id(),
            crate::now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        open(&path).expect("打开测试库失败")
    }

    /// 按迁移前的 v1 形状建最小 accounts/messages/mailboxes 表，模拟老库
    fn legacy_v1_db() -> Connection {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dbx-mail-legacy-{}-{}-{seq}.sqlite",
            std::process::id(),
            crate::now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL DEFAULT '',
    username TEXT NOT NULL,
    password_ref TEXT NOT NULL DEFAULT '',
    imap_host TEXT NOT NULL,
    imap_port INTEGER NOT NULL DEFAULT 993,
    imap_security TEXT NOT NULL DEFAULT 'tls',
    smtp_host TEXT NOT NULL,
    smtp_port INTEGER NOT NULL DEFAULT 465,
    smtp_security TEXT NOT NULL DEFAULT 'tls',
    signature TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL DEFAULT '#e8660c',
    sync_enabled INTEGER NOT NULL DEFAULT 1,
    sync_interval INTEGER NOT NULL DEFAULT 5,
    last_sync_at INTEGER NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE mailboxes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    delimiter TEXT NOT NULL DEFAULT '/',
    role TEXT NOT NULL DEFAULT 'custom',
    unread INTEGER NOT NULL DEFAULT 0,
    total INTEGER NOT NULL DEFAULT 0,
    uid_validity INTEGER NOT NULL DEFAULT 0,
    last_uid INTEGER NOT NULL DEFAULT 0,
    sort_order INTEGER NOT NULL DEFAULT 0,
    selectable INTEGER NOT NULL DEFAULT 1,
    UNIQUE(account_id, name)
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid INTEGER,
    message_id TEXT NOT NULL DEFAULT '',
    thread_id TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL DEFAULT '',
    from_name TEXT NOT NULL DEFAULT '',
    from_addr TEXT NOT NULL DEFAULT '',
    to_addrs TEXT NOT NULL DEFAULT '[]',
    cc_addrs TEXT NOT NULL DEFAULT '[]',
    bcc_addrs TEXT NOT NULL DEFAULT '[]',
    reply_to TEXT NOT NULL DEFAULT '',
    in_reply_to TEXT NOT NULL DEFAULT '',
    refs TEXT NOT NULL DEFAULT '[]',
    date INTEGER NOT NULL DEFAULT 0,
    internal_date INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    snippet TEXT NOT NULL DEFAULT '',
    seen INTEGER NOT NULL DEFAULT 0,
    flagged INTEGER NOT NULL DEFAULT 0,
    answered INTEGER NOT NULL DEFAULT 0,
    draft INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    raw_path TEXT NOT NULL DEFAULT '',
    body_loaded INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    UNIQUE(account_id, mailbox_id, uid)
);
PRAGMA user_version = 1;
"#,
        )
        .unwrap();
        conn
    }

    fn sample_account(provider: &str) -> Account {
        Account {
            id: 0,
            email: "user@example.com".into(),
            display_name: "示例".into(),
            username: "user@example.com".into(),
            provider: provider.into(),
            auth_type: if provider == "graph" { "oauth2".into() } else { "password".into() },
            tenant_id: if provider == "graph" { "common".into() } else { String::new() },
            client_id: if provider == "graph" { "client-1".into() } else { String::new() },
            user_id: String::new(),
            imap_host: if provider == "graph" { "graph.microsoft.com".into() } else { "imap.example.com".into() },
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            signature: String::new(),
            color: "#e8660c".into(),
            sync_enabled: true,
            sync_interval: 5,
            last_sync_at: 0,
            last_error: String::new(),
            host_bound: false,
            host_connection_id: String::new(),
        }
    }

    fn sample_header(account_id: i64, mailbox_id: i64, subject: &str) -> HeaderRow {
        HeaderRow {
            account_id,
            mailbox_id,
            uid: Some(0),
            message_id: format!("<{subject}@example.com>"),
            thread_id: String::new(),
            subject: subject.into(),
            from_name: "发件人".into(),
            from_addr: "from@example.com".into(),
            to_addrs: vec!["user@example.com".into()],
            cc_addrs: vec![],
            reply_to: String::new(),
            in_reply_to: String::new(),
            references: vec![],
            date: 1_700_000_000_000,
            internal_date: 1_700_000_000_000,
            size: 1024,
            snippet: "摘要".into(),
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
        }
    }

    fn insert_account(conn: &Connection, provider: &str) -> i64 {
        account_insert(conn, &sample_account(provider), "ref-1").unwrap()
    }

    #[test]
    fn migrate_upgrades_legacy_v1_and_is_idempotent() {
        let conn = legacy_v1_db();
        // 幂等：迁移跑两次都不报错，且老库升级后新列可用
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        let account_id = insert_account(&conn, "graph");
        let account = account_get(&conn, account_id).unwrap().unwrap();
        assert_eq!(account.provider, "graph");
        assert_eq!(account.auth_type, "oauth2");
        assert_eq!(account.tenant_id, "common");
        // 全新库（建表后直接迁移）同样拿到 v4 形状
        let fresh = test_db();
        let version: i64 = fresh
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
    }

    #[test]
    fn message_upsert_remote_dedupes_and_assigns_uid() {
        let conn = test_db();
        let account_id = insert_account(&conn, "graph");
        let mailbox_id = mailbox_upsert(&conn, account_id, "收件箱", "/", "inbox", true, 0, "folder-1").unwrap();
        let header = sample_header(account_id, mailbox_id, "第一封");

        let first = message_upsert_remote(&conn, &header, "msg-1").unwrap();
        let second = message_upsert_remote(&conn, &header, "msg-2").unwrap();
        assert_ne!(first, second);

        // 同 remote_id 重复写入 → 去重为同一行，uid 不变
        let again = message_upsert_remote(&conn, &header, "msg-1").unwrap();
        assert_eq!(again, first);
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 2);

        // uid 为文件夹内自增序号
        let uid: i64 = conn
            .query_row("SELECT uid FROM messages WHERE id = ?1", params![second], |row| row.get(0))
            .unwrap();
        assert_eq!(uid, 2);

        // 更新不覆盖 seen（本地已读状态由 pending_flags 机制管理）
        conn.execute("UPDATE messages SET seen = 1 WHERE id = ?1", params![first]).unwrap();
        message_upsert_remote(&conn, &header, "msg-1").unwrap();
        let seen: i64 = conn
            .query_row("SELECT seen FROM messages WHERE id = ?1", params![first], |row| row.get(0))
            .unwrap();
        assert_eq!(seen, 1);
    }

    #[test]
    fn remote_mailbox_and_delta_link_roundtrip() {
        let conn = test_db();
        let account_id = insert_account(&conn, "graph");
        let mailbox_id = mailbox_upsert(&conn, account_id, "收件箱", "/", "inbox", true, 0, "folder-1").unwrap();
        assert_eq!(mailbox_remote_id(&conn, mailbox_id).unwrap(), "folder-1");

        // IMAP 路径传空 remote_id 时不覆盖已存值
        mailbox_upsert(&conn, account_id, "收件箱", "/", "inbox", true, 0, "").unwrap();
        assert_eq!(mailbox_remote_id(&conn, mailbox_id).unwrap(), "folder-1");

        mailbox_set_delta_link(&conn, mailbox_id, "delta://link-1").unwrap();
        assert_eq!(mailbox_delta_link(&conn, mailbox_id).unwrap(), "delta://link-1");
    }

    #[test]
    fn message_delete_by_remote_removes_only_target() {
        let conn = test_db();
        let account_id = insert_account(&conn, "graph");
        let mailbox_id = mailbox_upsert(&conn, account_id, "收件箱", "/", "inbox", true, 0, "folder-1").unwrap();
        message_upsert_remote(&conn, &sample_header(account_id, mailbox_id, "a"), "msg-a").unwrap();
        let keep = message_upsert_remote(&conn, &sample_header(account_id, mailbox_id, "b"), "msg-b").unwrap();

        let removed = message_delete_by_remote(&conn, account_id, mailbox_id, "msg-a").unwrap();
        assert_eq!(removed, 1);
        assert!(message_get(&conn, keep).unwrap().is_some());
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn message_backfill_header_fills_only_empty_fields() {
        let conn = test_db();
        let account_id = insert_account(&conn, "graph");
        let mailbox_id = mailbox_upsert(&conn, account_id, "收件箱", "/", "inbox", true, 0, "folder-1").unwrap();
        // 懒加载入库：线程头字段全空、size 未知
        let mut header = sample_header(account_id, mailbox_id, "a");
        header.message_id = String::new();
        header.in_reply_to = String::new();
        header.reply_to = String::new();
        header.references = Vec::new();
        header.size = 0;
        let id = message_upsert_remote(&conn, &header, "msg-a").unwrap();

        message_backfill_header(
            &conn,
            id,
            "<m1@example.com>",
            "<parent@example.com>",
            &["<root@example.com>".to_string()],
            "reply@example.com",
            4096,
        )
        .unwrap();
        let row = |conn: &Connection| -> (String, String, String, String, i64) {
            conn.query_row(
                "SELECT message_id, in_reply_to, refs, reply_to, size FROM messages WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap()
        };
        let (message_id, in_reply_to, refs, reply_to, size) = row(&conn);
        assert_eq!(message_id, "<m1@example.com>");
        assert_eq!(in_reply_to, "<parent@example.com>");
        assert_eq!(reply_to, "reply@example.com");
        assert_eq!(size, 4096);
        assert!(refs.contains("<root@example.com>"));

        // 已有值不被覆盖
        message_backfill_header(
            &conn,
            id,
            "<other@example.com>",
            "<other-parent@example.com>",
            &["<other-root@example.com>".to_string()],
            "other@example.com",
            8192,
        )
        .unwrap();
        let (message_id, in_reply_to, refs, reply_to, size) = row(&conn);
        assert_eq!(message_id, "<m1@example.com>");
        assert_eq!(in_reply_to, "<parent@example.com>");
        assert_eq!(reply_to, "reply@example.com");
        assert_eq!(size, 4096);
        assert!(refs.contains("<root@example.com>"));
        assert!(!refs.contains("<other-root@example.com>"));
    }
}

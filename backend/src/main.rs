//! DBX 邮件 —— 原生 Sidecar
//!
//! 职责：
//! 1. IMAP 同步与 SMTP 投递（网络只在这里发生，插件页面处于 `connect-src 'none'` 沙箱）；
//! 2. SQLite 本地缓存：账户、文件夹、邮件头、正文、附件、草稿、发件箱；
//! 3. 文件系统交互：附件落盘、.eml 导入导出、拖入附件暂存。
//!
//! 协议：DBX Plugin Protocol v1，stdin/stdout 上的 JSON-RPC 2.0 逐行消息。
//! `Metadata.version` 必须与 manifest.json 完全一致，否则宿主会拒绝启动。

mod accounts;
mod compose;
mod db;
mod files;
mod graph;
mod hostconn;
mod imap;
mod mcp;
mod mime;
mod model;
mod net_guard;
mod oauth;
mod secrets;
mod smtp;
mod stream;
mod sync;
mod tls;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use crate::mime::ParseContext;
use crate::model::Settings;

/// 插件标识与版本，必须与 manifest.json 保持一致
const PLUGIN_ID: &str = "com.mjj3610.mail";
/// 版本号直接取 Cargo 包版本，避免与 package.json / manifest.json 漂移
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 协议版本
const PROTOCOL_VERSION: i64 = 1;

/// 运行期共享状态
pub struct App {
    /// 宿主分配的插件数据目录（DBX_PLUGIN_DATA_DIR）
    pub data_dir: PathBuf,
    /// SQLite 连接（单连接 + 互斥锁）
    pub db: Mutex<Connection>,
    /// 密码保管
    pub secrets: secrets::SecretStore,
    /// 网络任务运行时
    pub rt: tokio::runtime::Runtime,
    /// 正在同步的账户与进度，避免重入
    pub syncing: SyncRegistry,
    /// Graph 账户的 access_token 缓存（不落盘）
    pub token_cache: oauth::TokenCache,
    /// 进行中的 OAuth 授权（state → 上下文）
    pub oauth_pending: oauth::PendingMap,
    /// DBX 宿主连接（connection-provider）的活跃会话；密码只在内存，不落盘
    pub host_sessions: Mutex<HashMap<String, hostconn::HostSession>>,
}

impl App {
    /// 取数据库连接；锁中毒时继续使用（本地单用户，数据不会因此损坏）
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// 读取账户密码
    pub fn load_password(&self, account_id: i64) -> Result<String, String> {
        // 宿主连接（connection-provider）的密码只在内存会话里，优先于本地密钥库
        if let Some(password) = hostconn::session_password(self, account_id) {
            return Ok(password);
        }
        let reference = {
            let conn = self.db();
            db::account_password_ref(&conn, account_id)
                .map_err(|error| format!("读取账户失败：{error}"))?
        };
        if reference.is_empty() {
            return Err("这个账户没有保存密码，请在账户设置里重新填写".into());
        }
        self.secrets
            .load(&reference)
            .map_err(|error| format!("读取密码失败：{error}"))
    }

    /// 尝试占用同步槽位
    fn begin_sync(&self, account_id: i64) -> bool {
        self.syncing.begin(account_id)
    }

    fn end_sync(&self, account_id: i64) {
        self.syncing.finish(account_id);
    }

    /// 汇报同步进度（当前文件夹 + 已处理/总数）
    pub fn sync_progress(&self, account_id: i64, mailbox: &str, done: i64, total: i64) {
        self.syncing.update(account_id, mailbox, done, total);
    }
}

/// 正在同步的账户与进度（begin 返回 false 表示该账户已在同步中）
pub struct SyncRegistry {
    inner: Mutex<HashMap<i64, model::SyncProgress>>,
}

impl SyncRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn begin(&self, account_id: i64) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        map.insert(
            account_id,
            model::SyncProgress {
                account_id,
                ..Default::default()
            },
        )
        .is_none()
    }

    pub fn finish(&self, account_id: i64) {
        let mut map = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        map.remove(&account_id);
    }

    pub fn update(&self, account_id: i64, mailbox: &str, done: i64, total: i64) {
        let mut map = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(progress) = map.get_mut(&account_id) {
            progress.mailbox = mailbox.to_string();
            progress.done = done;
            progress.total = total;
        }
    }

    /// 进度快照（按账户 id 排序）
    pub fn snapshot(&self) -> Vec<model::SyncProgress> {
        let map = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut list: Vec<_> = map.values().cloned().collect();
        list.sort_by_key(|progress| progress.account_id);
        list
    }
}

/// JSON-RPC 错误
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 参数非法
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    /// 业务/系统异常统一 -32000，前端按 message 展示
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(-32000, message)
    }
}

impl From<rusqlite::Error> for RpcError {
    fn from(value: rusqlite::Error) -> Self {
        RpcError::internal(format!("数据库错误：{value}"))
    }
}

impl From<std::io::Error> for RpcError {
    fn from(value: std::io::Error) -> Self {
        RpcError::internal(format!("文件操作失败：{value}"))
    }
}

impl From<String> for RpcError {
    fn from(value: String) -> Self {
        RpcError::internal(value)
    }
}

pub type RpcResult = Result<Value, RpcError>;

fn main() {
    let data_dir = resolve_data_dir();
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("[dbx-mail] 创建数据目录失败：{error}");
        std::process::exit(1);
    }

    let conn = match db::open(&data_dir.join("dbx-mail.db")) {
        Ok(conn) => conn,
        Err(error) => {
            eprintln!("[dbx-mail] 打开数据库失败：{error}");
            std::process::exit(1);
        }
    };
    // 上次异常退出可能留下「发送中」状态，重新排队
    let _ = conn.execute(
        "UPDATE outbox SET status = 'queued' WHERE status = 'sending'",
        [],
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[dbx-mail] 初始化运行时失败：{error}");
            std::process::exit(1);
        }
    };

    let secrets = secrets::SecretStore::new(&data_dir);
    let app = Arc::new(App {
        data_dir,
        db: Mutex::new(conn),
        secrets,
        rt: runtime,
        syncing: SyncRegistry::new(),
        token_cache: Mutex::new(HashMap::new()),
        oauth_pending: Mutex::new(HashMap::new()),
        host_sessions: Mutex::new(HashMap::new()),
    });

    start_workers(app.clone());
    serve(app);
}

/// 数据目录：优先使用宿主下发的 DBX_PLUGIN_DATA_DIR
fn resolve_data_dir() -> PathBuf {
    if let Ok(value) = std::env::var("DBX_PLUGIN_DATA_DIR") {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(PathBuf::from))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("dbx-mail")
}

/* ---------------- 后台任务 ---------------- */

fn start_workers(app: Arc<App>) {
    // 发件箱投递
    {
        let app = Arc::clone(&app);
        std::thread::spawn(move || loop {
            compose::process_due(Arc::clone(&app));
            std::thread::sleep(Duration::from_secs(2));
        });
    }

    // 定时同步：启动先立即跑一轮（打开即同步），之后每 45s 检查、按间隔触发；
    // 平时只看收件箱与已发送，完整同步由用户手动触发
    {
        let app = Arc::clone(&app);
        std::thread::spawn(move || {
            let mut rounds = 0u64;
            loop {
                let startup_round = rounds == 0;
                if !startup_round {
                    std::thread::sleep(Duration::from_secs(45));
                }
                rounds += 1;
                let (accounts, interval) = {
                    let conn = app.db();
                    let settings = db::load_settings(&conn).unwrap_or_default();
                    (db::account_list(&conn).unwrap_or_default(), settings.sync_interval)
                };
                if interval <= 0 {
                    continue;
                }
                let now = now_ms();
                for account in accounts {
                    if !account.sync_enabled || account.sync_interval <= 0 {
                        continue;
                    }
                    let period = account.sync_interval.max(1) * 60_000;
                    // 启动首轮豁免到期检查，其余轮次按间隔
                    if !startup_round && account.last_sync_at > 0 && now - account.last_sync_at < period {
                        continue;
                    }
                    if !app.begin_sync(account.id) {
                        continue;
                    }
                    let mailbox_ids = {
                        let conn = app.db();
                        accounts::mailbox_ids_for(&conn, account.id, &["inbox", "sent"])
                    };
                    let report = app.rt.block_on(sync::sync_account(
                        Arc::clone(&app),
                        account.id,
                        sync::SyncOptions {
                            mailbox_ids,
                            limit: 30,
                        },
                    ));
                    app.end_sync(account.id);
                    if let Err(error) = report {
                        eprintln!("[dbx-mail] 后台同步失败（账户 {}）：{error}", account.id);
                    }
                }
            }
        });
    }
}

/* ---------------- 协议主循环 ---------------- */

fn serve(app: Arc<App>) {
    let stdin = std::io::stdin();
    let mut workers = Vec::new();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("[dbx-mail] 读取 stdin 失败：{error}");
                break;
            }
        };
        let payload = line.trim();
        if payload.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[dbx-mail] 无法解析请求：{error}");
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        if id.is_null() {
            eprintln!("[dbx-mail] 忽略通知：{method}");
            continue;
        }
        if method == "plugin/initialize" {
            let result = initialize(&params);
            write_message(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            continue;
        }

        let app = Arc::clone(&app);
        workers.push(std::thread::spawn(move || match dispatch(&app, &method, &params) {
            Ok(result) => write_message(&json!({ "jsonrpc": "2.0", "id": id, "result": result })),
            Err(error) => write_message(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": error.code, "message": error.message }
            })),
        }));
    }

    for worker in workers {
        let _ = worker.join();
    }
}

/// 处理握手
fn initialize(params: &Value) -> Value {
    let versions = params
        .get("host")
        .and_then(|host| host.get("protocolVersions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let supports = versions.is_empty()
        || versions
            .iter()
            .any(|version| version.as_i64() == Some(PROTOCOL_VERSION));
    if !supports {
        return json!({ "code": -32001, "message": "DBX 与插件没有共同的协议版本" });
    }
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": Vec::<String>::new(),
        "plugin": { "id": PLUGIN_ID, "version": PLUGIN_VERSION }
    })
}

/// 写一行协议消息（stdout 串行化）
fn write_message(value: &Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if serde_json::to_writer(&mut lock, value).is_ok() {
        let _ = lock.write_all(b"\n");
        let _ = lock.flush();
    }
}

/* ---------------- 方法分发 ---------------- */

fn dispatch(app: &Arc<App>, method: &str, params: &Value) -> RpcResult {
    match method {
        "ping" => Ok(json!({ "ok": true, "version": PLUGIN_VERSION })),

        // 初始化数据
        "app/bootstrap" => bootstrap(app, params),
        "app/info" => Ok(json!({
            "version": PLUGIN_VERSION,
            "dataDir": app.data_dir.to_string_lossy(),
            "vault": app.secrets.backend(),
            "platform": std::env::consts::OS,
        })),

        // 账户
        "account/presets" => Ok(json!({ "items": accounts::presets() })),
        "account/list" => {
            let conn = app.db();
            Ok(json!({ "items": db::account_list(&conn)? }))
        }
        "account/add" => Ok(accounts::add(Arc::clone(app), params)?),
        "account/update" => Ok(accounts::update(Arc::clone(app), params)?),
        "account/remove" => Ok(accounts::remove(Arc::clone(app), params)?),
        "account/test" => Ok(accounts::test(Arc::clone(app), params)?),
        "account/discover" => {
            let id = param_int(params, "id", 0);
            Ok(accounts::discover(Arc::clone(app), id)?)
        }
        // 宿主连接作用域：connectionId → 绑定的账户（宿主工作台按连接独立邮箱）
        "account/byConnection" => {
            let connection_id = param_str(params, "connectionId");
            if connection_id.trim().is_empty() {
                return Err(RpcError::invalid("缺少 connectionId"));
            }
            let conn = app.db();
            let account = db::account_by_host(&conn, connection_id.trim())
                .map_err(|error| RpcError::invalid(&format!("读取账户失败：{error}")))?;
            Ok(json!({ "account": account }))
        }

        // OAuth 授权（Microsoft 365 / Outlook.com）
        "account/oauthBegin" => {
            let tenant = param_str(params, "tenantId");
            let client = param_str(params, "clientId");
            // 机密客户端：回调时的 code 交换需要 client_secret（仅驻内存）
            let mut secret = param_str(params, "clientSecret");
            if secret.trim().is_empty() {
                // 重新授权场景：前端拿不到（也不该拿到）存量 secret，按账户从
                // SecretStore 取回注入 pending，否则机密客户端的 code 交换必被拒
                let account_id = param_int(params, "accountId", 0);
                if account_id > 0 {
                    let conn = app.db();
                    if let Ok(Some(row)) = db::account_get(&conn, account_id) {
                        if row.provider == "graph" {
                            if let Ok(stored) = app.secrets.load(&format!("graphsecret:{}", row.email)) {
                                secret = stored;
                            }
                        }
                    }
                }
            }
            if client.trim().is_empty() {
                return Err(RpcError::invalid("请填写 Azure 应用的 Client ID"));
            }
            let app = Arc::clone(app);
            let (url, state) = app
                .rt
                .block_on(oauth::begin(&app, &tenant, &client, &secret))
                .map_err(RpcError::internal)?;
            Ok(json!({ "authorizeUrl": url, "state": state, "expiresIn": 300 }))
        }
        "account/oauthStatus" => {
            let state = param_str(params, "state");
            oauth::status(app, &state).map_err(RpcError::internal)
        }
        "account/oauthCancel" => {
            let state = param_str(params, "state");
            oauth::cancel(app, &state);
            Ok(json!({ "ok": true }))
        }

        // 文件夹
        "mailbox/list" => {
            let conn = app.db();
            let accounts = db::account_list(&conn)?;
            let mut items = Vec::new();
            for account in accounts {
                for mailbox in db::mailbox_list(&conn, account.id)? {
                    items.push(mailbox);
                }
            }
            Ok(json!({ "items": items }))
        }

        // 同步
        "sync/now" => sync_now(app, params),
        "sync/status" => {
            let progress = app.syncing.snapshot();
            let running: Vec<i64> = progress.iter().map(|item| item.account_id).collect();
            let conn = app.db();
            Ok(json!({
                "running": running,
                "progress": progress,
                "log": db::sync_log_list(&conn, 40)?,
            }))
        }
        "mailbox/sync" => sync_now(app, params),

        // 邮件列表与阅读
        "mail/list" => mail_list(app, params),
        "mail/thread" => mail_thread(app, params),
        "mail/open" => {
            let id = param_int(params, "id", 0);
            let network = param_bool(params, "network", true);
            let detail = app
                .rt
                .block_on(sync::ensure_body(Arc::clone(app), id, network))?;
            Ok(serde_json::to_value(detail).unwrap_or(Value::Null))
        }
        "mail/flags" => {
            let ids = param_i64_array(params, "ids");
            let seen = params.get("seen").and_then(Value::as_bool);
            let flagged = params.get("flagged").and_then(Value::as_bool);
            let answered = params.get("answered").and_then(Value::as_bool);
            app.rt
                .block_on(sync::set_flags(Arc::clone(app), &ids, seen, flagged, answered))?;
            Ok(json!({ "ok": true }))
        }
        "mail/move" => {
            let ids = param_i64_array(params, "ids");
            let destination = params.get("destination").cloned().unwrap_or(Value::Null);
            let kind = destination
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("archive");
            let target = match kind {
                "trash" => sync::Destination::Trash,
                "junk" => sync::Destination::Junk,
                "mailbox" => sync::Destination::Mailbox(
                    destination.get("mailboxId").and_then(Value::as_i64).unwrap_or(0),
                ),
                _ => sync::Destination::Archive,
            };
            app.rt.block_on(sync::move_messages(Arc::clone(app), &ids, target))?;
            Ok(json!({ "ok": true }))
        }
        "mail/part" => mail_part(app, params),
        "mail/export" => mail_export(app, params),
        "mail/import" => mail_import(app, params),
        "mail/fetchImage" => mail_fetch_image(app, params),

        // 撰写
        "compose/send" => Ok(compose::send(Arc::clone(app), params)?),
        "draft/list" => Ok(compose::draft_list(app)?),
        "draft/save" => Ok(compose::draft_save(app, params)?),
        "draft/delete" => Ok(compose::draft_delete(app, params)?),
        "outbox/list" => Ok(compose::list(app)?),
        "outbox/cancel" => Ok(compose::cancel(app, params)?),
        "outbox/retry" => Ok(compose::retry(app, params)?),
        "outbox/delete" => Ok(compose::delete(app, params)?),

        // 联系人
        "contacts/list" => {
            let conn = app.db();
            Ok(json!({ "items": db::contacts_list(&conn, 400)? }))
        }

        // 设置
        "settings/get" => {
            let conn = app.db();
            Ok(json!({ "settings": db::load_settings(&conn)? }))
        }
        "settings/set" => settings_set(app, params),

        // 文件系统
        "fs/dirs" => {
            let path = param_str(params, "path");
            Ok(serde_json::to_value(files::list_dirs(&path)?).unwrap_or(Value::Null))
        }
        "fs/files" => {
            let path = param_str(params, "path");
            Ok(json!({ "items": files::list_files(&path)? }))
        }
        "fs/readFile" => {
            let path = param_str(params, "path");
            let file = files::read_local_file(&path)?;
            Ok(json!({
                "name": file.name,
                "path": file.path,
                "mime": file.mime,
                "size": file.size,
                "dataBase64": file.data_base64,
            }))
        }
        "fs/saveUpload" => {
            let name = param_str(params, "name");
            let data = param_str(params, "dataBase64");
            let file = files::save_upload(&app.data_dir, &name, &data)?;
            Ok(json!({
                "name": file.name,
                "path": file.path,
                "mime": file.mime,
                "size": file.size,
            }))
        }
        "fs/defaultDir" => Ok(json!({ "path": files::default_output_dir(&app.data_dir) })),
        "fs/saveAttachment" => {
            let message_id = param_int(params, "messageId", 0);
            let index = param_int(params, "index", 0);
            let dir = param_str(params, "dir");
            let fallback = param_str(params, "filename");
            let detail = app
                .rt
                .block_on(sync::ensure_body(Arc::clone(app), message_id, true))?;
            let raw_path = {
                let conn = app.db();
                db::message_raw_path(&conn, message_id)?.unwrap_or_default()
            };
            if raw_path.is_empty() {
                return Err(RpcError::internal("本机没有这封邮件的副本"));
            }
            let raw = std::fs::read(&raw_path)?;
            let path = files::save_attachment(&raw, index, &dir, &fallback)?;
            let _ = detail;
            Ok(json!({ "path": path }))
        }

        // MCP 工具面（内置 AI / 外部 dbx MCP；sidecar-protocol §9）
        "mcp/tools" => Ok(mcp::tool_definitions()),
        "mcp/call" => mcp::call(app, params),

        // DBX 宿主连接管理（connection-provider；生命周期 Secret 不落盘）
        "connection/test" => Ok(hostconn::test(Arc::clone(app), params)),
        "connection/connect" => Ok(hostconn::connect(Arc::clone(app), params)?),
        "connection/disconnect" => Ok(hostconn::disconnect(Arc::clone(app), params)?),

        other => Err(RpcError::new(-32601, format!("未实现的方法：{other}"))),
    }
}

/* ---------------- 具体处理 ---------------- */

/// 首屏数据：账户、文件夹、设置、计数
fn bootstrap(app: &Arc<App>, params: &Value) -> RpcResult {
    let conn = app.db();
    // 宿主连接作用域：accountId > 0 时只返回该账户的元数据与计数（独立邮箱 tab）
    let scope_account = param_int(params, "accountId", 0);
    let accounts = if scope_account > 0 {
        db::account_get(&conn, scope_account)?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        db::account_list(&conn)?
    };
    let settings = db::load_settings(&conn)?;
    let mut mailboxes = Vec::new();
    for account in &accounts {
        mailboxes.extend(db::mailbox_list(&conn, account.id)?);
    }
    // 作用域计数：纯后台 scoped 时按账户过滤；多表查询用表别名限定列
    let count = |sql_all: &'static str, sql_scoped: &'static str| -> i64 {
        if scope_account > 0 {
            conn.query_row(sql_scoped, rusqlite::params![scope_account], |row| row.get(0))
                .unwrap_or(0)
        } else {
            conn.query_row(sql_all, [], |row| row.get(0)).unwrap_or(0)
        }
    };
    let inbox_unread: i64 = count(
        "SELECT COALESCE(SUM(unread), 0) FROM mailboxes WHERE role = 'inbox'",
        "SELECT COALESCE(SUM(unread), 0) FROM mailboxes WHERE role = 'inbox' AND mailboxes.account_id = ?1",
    );
    let starred: i64 = count(
        "SELECT COUNT(*) FROM messages m JOIN mailboxes mb ON mb.id = m.mailbox_id
             WHERE m.flagged = 1 AND mb.role NOT IN ('trash','junk')",
        "SELECT COUNT(*) FROM messages m JOIN mailboxes mb ON mb.id = m.mailbox_id
             WHERE m.flagged = 1 AND mb.role NOT IN ('trash','junk') AND m.account_id = ?1",
    );
    let drafts: i64 = count(
        "SELECT COUNT(*) FROM drafts",
        "SELECT COUNT(*) FROM drafts WHERE drafts.account_id = ?1",
    );
    let outbox: i64 = count(
        "SELECT COUNT(*) FROM outbox WHERE status IN ('queued','sending','failed')",
        "SELECT COUNT(*) FROM outbox WHERE status IN ('queued','sending','failed') AND outbox.account_id = ?1",
    );

    Ok(json!({
        "version": PLUGIN_VERSION,
        "dataDir": app.data_dir.to_string_lossy(),
        "vault": app.secrets.backend(),
        "platform": std::env::consts::OS,
        "accounts": accounts,
        "mailboxes": mailboxes,
        "settings": settings,
        "counts": {
            "inboxUnread": inbox_unread,
            "starred": starred,
            "drafts": drafts,
            "outbox": outbox,
        }
    }))
}

fn mail_list(app: &Arc<App>, params: &Value) -> RpcResult {
    let settings = {
        let conn = app.db();
        db::load_settings(&conn).unwrap_or_default()
    };
    let filter = db::ThreadFilter {
        mailbox_id: params.get("mailboxId").and_then(Value::as_i64),
        role: params
            .get("role")
            .and_then(Value::as_str)
            .map(|value| value.to_string()),
        account_id: params.get("accountId").and_then(Value::as_i64),
        query: param_str(params, "query"),
        kind: {
            let value = param_str(params, "kind");
            if value.is_empty() {
                "all".into()
            } else {
                value
            }
        },
        limit: param_int(params, "limit", settings.page_size).clamp(1, 300),
        offset: param_int(params, "offset", 0).max(0),
    };
    let conn = app.db();
    let items = db::thread_list(&conn, &filter)?;
    Ok(json!({
        "items": items,
        "offset": filter.offset,
        "limit": filter.limit,
    }))
}

fn mail_thread(app: &Arc<App>, params: &Value) -> RpcResult {
    let account_id = param_int(params, "accountId", 0);
    let thread_id = param_str(params, "threadId");
    if thread_id.is_empty() {
        return Err(RpcError::invalid("缺少会话 ID"));
    }
    let conn = app.db();
    let items = db::message_thread(&conn, account_id, &thread_id)?;
    Ok(json!({ "items": items }))
}

fn mail_part(app: &Arc<App>, params: &Value) -> RpcResult {
    let message_id = param_int(params, "id", 0);
    let index = param_int(params, "index", 0);
    let raw_path = {
        let conn = app.db();
        db::message_raw_path(&conn, message_id)?.unwrap_or_default()
    };
    if raw_path.is_empty() || !std::path::Path::new(&raw_path).is_file() {
        app.rt
            .block_on(sync::ensure_body(Arc::clone(app), message_id, true))?;
    }
    let raw_path = {
        let conn = app.db();
        db::message_raw_path(&conn, message_id)?.unwrap_or_default()
    };
    if raw_path.is_empty() {
        return Err(RpcError::internal("本机没有这封邮件的副本"));
    }
    let raw = std::fs::read(&raw_path)?;
    let (name, bytes, mime) =
        mime::part_bytes(&raw, index).ok_or_else(|| RpcError::internal("找不到该附件"))?;
    if bytes.len() > 30 * 1024 * 1024 {
        return Err(RpcError::internal("附件超过 30 MB，请直接保存到磁盘"));
    }
    Ok(json!({
        "name": name,
        "mime": mime,
        "size": bytes.len(),
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

fn mail_export(app: &Arc<App>, params: &Value) -> RpcResult {
    let ids = param_i64_array(params, "ids");
    let dir = {
        let value = param_str(params, "dir");
        if value.is_empty() {
            files::default_output_dir(&app.data_dir)
        } else {
            value
        }
    };
    let mut paths = Vec::new();
    for id in ids {
        app.rt
            .block_on(sync::ensure_body(Arc::clone(app), id, true))?;
        let (raw_path, summary) = {
            let conn = app.db();
            let raw = db::message_raw_path(&conn, id)?.unwrap_or_default();
            let summary = db::message_get(&conn, id)?;
            (raw, summary)
        };
        let Some(summary) = summary else { continue };
        if raw_path.is_empty() {
            continue;
        }
        let raw = std::fs::read(&raw_path)?;
        let suggested = format!(
            "{}-{}.eml",
            summary.subject.chars().take(40).collect::<String>(),
            summary.date
        );
        paths.push(files::export_eml(&raw, &dir, &suggested)?);
    }
    Ok(json!({ "paths": paths, "dir": dir }))
}

fn mail_import(app: &Arc<App>, params: &Value) -> RpcResult {
    let account_id = param_int(params, "accountId", 0);
    let mailbox_id = param_int(params, "mailboxId", 0);
    let path = param_str(params, "path");
    if mailbox_id <= 0 {
        return Err(RpcError::invalid("请选择要导入的文件夹"));
    }
    let bytes = files::read_eml(&path)?;
    let now = now_ms();
    let ctx = ParseContext {
        account_id,
        mailbox_id,
        uid: None,
        internal_date: now,
        seen: true,
        flagged: false,
        answered: false,
        draft: false,
        size: bytes.len() as i64,
    };
    let parsed = mime::parse(&bytes, &ctx).ok_or_else(|| RpcError::internal("无法解析这封 .eml"))?;
    let key = format!("local-{}", uuid::Uuid::new_v4());
    let raw_path = files::raw_path(&app.data_dir, account_id, mailbox_id, &key);
    files::write_raw(&raw_path, &bytes)?;
    let conn = app.db();
    let id = db::message_insert_local(&conn, &parsed.header, &raw_path.to_string_lossy())?;
    db::body_save(&conn, id, &parsed.text, &parsed.html, &parsed.calendar)?;
    db::attachments_replace(&conn, id, &parsed.attachments)?;
    db::message_mark_body_loaded(&conn, id)?;
    Ok(json!({ "id": id }))
}

fn sync_now(app: &Arc<App>, params: &Value) -> RpcResult {
    let mailbox_id = params.get("mailboxId").and_then(Value::as_i64);
    let mut account_id = params.get("accountId").and_then(Value::as_i64);
    // 指定了文件夹就只同步它所属的账户：否则每个账户都会跑一遍 list + status，白等一轮
    if account_id.is_none() {
        if let Some(id) = mailbox_id {
            let conn = app.db();
            if let Some(mailbox) = db::mailbox_get(&conn, id)? {
                account_id = Some(mailbox.account_id);
            }
        }
    }
    let accounts = {
        let conn = app.db();
        match account_id {
            Some(id) => db::account_get(&conn, id)?.into_iter().collect::<Vec<_>>(),
            None => db::account_list(&conn)?,
        }
    };
    if accounts.is_empty() {
        return Err(RpcError::internal("还没有添加账户"));
    }
    let mut reports = Vec::new();
    let mut errors = Vec::new();
    for account in accounts {
        if !app.begin_sync(account.id) {
            errors.push(format!("{} 正在同步中", account.email));
            continue;
        }
        let options = sync::SyncOptions {
            mailbox_ids: mailbox_id.into_iter().collect(),
            limit: param_int(params, "limit", 0).max(0) as usize,
        };
        let result = app
            .rt
            .block_on(sync::sync_account(Arc::clone(app), account.id, options));
        app.end_sync(account.id);
        match result {
            Ok(report) => reports.push(serde_json::to_value(report).unwrap_or(Value::Null)),
            Err(error) => errors.push(format!("{}：{error}", account.email)),
        }
    }
    if reports.is_empty() && !errors.is_empty() {
        return Err(RpcError::internal(errors.join("；")));
    }
    Ok(json!({ "reports": reports, "errors": errors }))
}

fn settings_set(app: &Arc<App>, params: &Value) -> RpcResult {
    let conn = app.db();
    let mut settings: Settings = db::load_settings(&conn)?;
    let patch = params.get("settings").cloned().unwrap_or_else(|| params.clone());
    if let Some(value) = patch.get("theme").and_then(Value::as_str) {
        settings.theme = value.to_string();
    }
    if let Some(value) = patch.get("locale").and_then(Value::as_str) {
        settings.locale = value.to_string();
    }
    if let Some(value) = patch.get("density").and_then(Value::as_str) {
        settings.density = value.to_string();
    }
    if let Some(value) = patch.get("remoteImages").and_then(Value::as_bool) {
        settings.remote_images = value;
    }
    if let Some(value) = patch.get("markReadOnOpen").and_then(Value::as_bool) {
        settings.mark_read_on_open = value;
    }
    if let Some(value) = patch.get("undoDelay").and_then(Value::as_i64) {
        settings.undo_delay = value.clamp(0, 120);
    }
    if let Some(value) = patch.get("syncInterval").and_then(Value::as_i64) {
        settings.sync_interval = value.clamp(0, 240);
    }
    if let Some(value) = patch.get("syncScope").and_then(Value::as_str) {
        settings.sync_scope = value.to_string();
    }
    if let Some(value) = patch.get("initialLimit").and_then(Value::as_i64) {
        settings.initial_limit = value.clamp(20, 2000);
    }
    if let Some(value) = patch.get("layout").and_then(Value::as_str) {
        settings.layout = value.to_string();
    }
    if let Some(value) = patch.get("conversations").and_then(Value::as_bool) {
        settings.conversations = value;
    }
    if let Some(value) = patch.get("pageSize").and_then(Value::as_i64) {
        settings.page_size = value.clamp(10, 300);
    }
    db::save_settings(&conn, &settings)?;
    Ok(json!({ "settings": settings }))
}

/// 远程图片代理：沙箱里无法直接请求，且要防止打内网
fn mail_fetch_image(app: &Arc<App>, params: &Value) -> RpcResult {
    let url = param_str(params, "url");
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(RpcError::invalid("图片地址非法"));
    }
    let parsed = reqwest::Url::parse(&url).map_err(|_| RpcError::invalid("图片地址非法"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| RpcError::invalid("图片地址缺少主机名"))?;
    // 首个 URL 与每一跳重定向都要过白名单，否则 302 一跳就能进内网
    net_guard::check_host(host).map_err(|reason| RpcError::invalid(reason.message()))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(net_guard::redirect_policy())
        .build()
        .map_err(|error| RpcError::internal(format!("创建请求失败：{error}")))?;
    let result = app.rt.block_on(async {
        let response = client
            .get(parsed)
            .header("User-Agent", "DBX-Mail/0.1")
            .header("Accept", "image/*")
            .send()
            .await
            .map_err(|error| format!("下载图片失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("服务器返回 {}", response.status()));
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .to_string();
        if !mime.starts_with("image/") {
            return Err("目标不是图片".into());
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("读取图片失败：{error}"))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("图片超过 8 MB".into());
        }
        Ok::<_, String>((
            mime,
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))
    })?;
    Ok(json!({
        "mime": result.0,
        "dataUrl": format!("data:{};base64,{}", result.0, result.1),
    }))
}

/* ---------------- 参数工具 ---------------- */

/// 取字符串参数
pub fn param_str(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 取整型参数
pub fn param_int(params: &Value, key: &str, fallback: i64) -> i64 {
    params.get(key).and_then(Value::as_i64).unwrap_or(fallback)
}

/// 取布尔参数
pub fn param_bool(params: &Value, key: &str, fallback: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

/// 取字符串数组（兼容单个字符串，按逗号/分号拆分）
pub fn param_str_array(params: &Value, key: &str) -> Vec<String> {
    match params.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        Some(Value::String(value)) => value
            .split([',', ';'])
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// 取 i64 数组
fn param_i64_array(params: &Value, key: &str) -> Vec<i64> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// 当前毫秒时间戳
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod sync_registry_tests {
    use super::*;

    #[test]
    fn registry_begin_guards_and_finish_frees_slot() {
        let registry = SyncRegistry::new();
        assert!(registry.begin(1));
        assert!(!registry.begin(1));
        registry.update(1, "收件箱", 10, 100);
        registry.finish(1);
        assert!(registry.begin(1));
    }

    #[test]
    fn registry_snapshot_reports_progress() {
        let registry = SyncRegistry::new();
        registry.begin(7);
        registry.update(7, "Inbox", 3, 50);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].account_id, 7);
        assert_eq!(snapshot[0].mailbox, "Inbox");
        assert_eq!(snapshot[0].done, 3);
        assert_eq!(snapshot[0].total, 50);
        registry.finish(7);
        assert!(registry.snapshot().is_empty());
    }
}

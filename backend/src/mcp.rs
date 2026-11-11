//! MCP 工具面：把邮件能力暴露给 DBX 内置 AI Agent 与外部 `dbx` MCP 服务。
//!
//! 契约（sidecar-protocol §9 + manifest `mcp` 贡献点）：
//! - `mcp/tools`：纯发现。只返回静态描述，不联网、不写库、不开监听；
//!   返回 `{ "tools": [{ name, description, inputSchema, annotations? }] }`。
//! - `mcp/call`：严格执行。入参 `{ tool, arguments, lifecycle? }`，返回 MCP
//!   `CallToolResult`（`{ content: [{type:"text",text}], isError }`）。
//!   逐字段严格校验（未知工具 / 未知参数键 / 类型不符都报 -32602）；
//!   凭据与密码绝不进参数与输出；写操作显式；`mail_send` 走两阶段语义
//!   （`dryRun:true` 只预览，不写发件箱、不联网）。
//!
//! 失败语义：本地写成功、远端推送失败时保持插件既有「待同步队列」行为，
//! 返回如实标注 deferred，不宣称已同步、不自动重试、不谎称回滚。
//! 工具作用域是插件自有账户（accountId 指向本地 SQLite），不依赖宿主连接。

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::db;
use crate::sync;
use crate::{compose, App, RpcError, RpcResult};

/// 发现：工具定义。纯静态数据，可安全地在任何时机调用。
pub fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "folder_list",
                "description": "List mail accounts and their folders (name, role, unread and total counts). Read-only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "accountId": { "type": "integer", "description": "Only list folders of this plugin account id; omit for all accounts" },
                    },
                    "required": [],
                },
                "annotations": { "readOnlyHint": true },
            },
            {
                "name": "mail_list",
                "description": "List cached conversations, newest first. Returns threadId, subject, snippet, sender, date, unread/flag state and the latest messageId per conversation. Read-only; searches the local cache (including cached bodies), not the server.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "accountId": { "type": "integer", "description": "Plugin account id; omit for all accounts" },
                        "folder": { "type": "string", "description": "Folder role: inbox, sent, drafts, trash, junk, archive, starred, all (default all)" },
                        "folderId": { "type": "integer", "description": "Exact folder id from folder_list; wins over folder" },
                        "query": { "type": "string", "description": "Search subject, snippet, sender, recipients and cached bodies" },
                        "cursor": { "type": "string", "description": "Opaque page cursor from a previous nextCursor; omit for the first page" },
                        "limit": { "type": "integer", "description": "Page size, 1-50 (default 20)" },
                    },
                    "required": [],
                },
                "annotations": { "readOnlyHint": true },
            },
            {
                "name": "mail_read",
                "description": "Read one conversation (or a single message) as plain text: one section per message with sender, date, recipients and body. Does NOT mark anything as read. Bodies not cached locally are fetched on demand (set network:false to stay offline).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "threadId": { "type": "string", "description": "Conversation id from mail_list; reads every message in it" },
                        "messageId": { "type": "integer", "description": "Single message id instead of threadId" },
                        "accountId": { "type": "integer", "description": "Plugin account id; required only when the same threadId exists in several accounts" },
                        "network": { "type": "boolean", "description": "Fetch uncached bodies from the server (default true)" },
                    },
                    "required": [],
                },
                "annotations": { "readOnlyHint": true },
            },
            {
                "name": "mail_flag",
                "description": "Set flags on one message or a whole conversation: seen, flagged (star) and/or answered. Give at least one flag and one target. Local change is applied immediately, the server push is best-effort and retries from the pending queue on the next sync.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "messageId": { "type": "integer", "description": "Message id; wins over threadId when both are given" },
                        "threadId": { "type": "string", "description": "Flag every message in the conversation" },
                        "accountId": { "type": "integer", "description": "Plugin account id; required only when the same threadId exists in several accounts" },
                        "seen": { "type": "boolean", "description": "Read / unread" },
                        "flagged": { "type": "boolean", "description": "Star on / off" },
                        "answered": { "type": "boolean", "description": "Answered marker" },
                    },
                    "required": [],
                },
            },
            {
                "name": "mail_move",
                "description": "Move one message or a whole conversation to archive, trash, junk or an explicit folder. Local change is applied immediately, the server push is best-effort and retries from the pending queue on the next sync.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "messageId": { "type": "integer", "description": "Message id; wins over threadId when both are given" },
                        "threadId": { "type": "string", "description": "Move every message in the conversation" },
                        "accountId": { "type": "integer", "description": "Plugin account id; required only when the same threadId exists in several accounts" },
                        "destination": { "type": "string", "enum": ["archive", "trash", "junk"], "description": "Well-known destination (default archive when neither destination nor folderId is given)" },
                        "folderId": { "type": "integer", "description": "Target folder id from folder_list; wins over destination" },
                    },
                    "required": [],
                },
            },
            {
                "name": "mail_send",
                "description": "Send an email from one of the plugin accounts. Two-phase: call with dryRun:true first to preview recipients, subject and body without queueing anything; then call again without dryRun to actually queue it. Delivery goes through the plugin outbox with an undo window (undoSeconds) and reports queued, not sent. Attachments are not supported over MCP. Never include account log-in details in parameters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "accountId": { "type": "integer", "description": "Plugin account id to send from" },
                        "to": { "type": "array", "items": { "type": "string" }, "description": "Recipient addresses (at least one across to/cc/bcc)" },
                        "cc": { "type": "array", "items": { "type": "string" }, "description": "Cc addresses" },
                        "bcc": { "type": "array", "items": { "type": "string" }, "description": "Bcc addresses" },
                        "subject": { "type": "string", "description": "Subject line" },
                        "body": { "type": "string", "description": "Body; HTML is used as-is, plain text is wrapped into paragraphs" },
                        "dryRun": { "type": "boolean", "description": "true = validate and preview only, nothing is queued" },
                    },
                    "required": ["accountId", "to"],
                },
            },
        ],
    })
}

/* ---------------- 调用入口 ---------------- */

/// 执行：`mcp/call` 的 `{ tool, arguments, lifecycle? }`。
/// 返回 MCP `CallToolResult`；错误以 RpcError 冒泡（-32602 参数类 / -32000 业务类）。
pub fn call(app: &Arc<App>, params: &Value) -> RpcResult {
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::invalid("缺少 tool 参数"))?;
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let map = arguments
        .as_object()
        .ok_or_else(|| RpcError::invalid("arguments 必须是 JSON 对象"))?;
    // lifecycle：内置 Agent / 外部 MCP 的连接绑定信息，由宿主注入；本插件工具
    // 语义不依赖它，但存在时必须是对象（尽早失败，避免上游误用）。
    if let Some(lifecycle) = params.get("lifecycle") {
        if !lifecycle.is_object() {
            return Err(RpcError::invalid("lifecycle 必须是 JSON 对象"));
        }
    }

    let payload = match tool {
        "folder_list" => folder_list(app, map)?,
        "mail_list" => mail_list(app, map)?,
        "mail_read" => mail_read(app, map)?,
        "mail_flag" => mail_flag(app, map)?,
        "mail_move" => mail_move(app, map)?,
        "mail_send" => mail_send(app, map)?,
        other => return Err(RpcError::invalid(format!("未知工具：{other}"))),
    };

    // mail_read 的 payload 已经是 { text: 会话纯文本 }，模型直接读正文；
    // 其余工具把结构化结果序列化进 content[0].text。
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .filter(|_value| payload.get("format").and_then(Value::as_str) == Some("plain"))
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string_pretty(&payload).unwrap_or_default());
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    }))
}

/* ---------------- 严格参数工具 ---------------- */

/// 拒绝未声明的参数键。宿主注入的 `connectionId` 只接受、不透传。
fn check_keys(map: &Map<String, Value>, allowed: &[&str]) -> Result<(), RpcError> {
    for key in map.keys() {
        if key == "connectionId" {
            continue;
        }
        if !allowed.contains(&key.as_str()) {
            return Err(RpcError::invalid(format!("未知参数：{key}")));
        }
    }
    Ok(())
}

fn opt_str(map: &Map<String, Value>, key: &str) -> Result<Option<String>, RpcError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim().to_string())),
        Some(_) => Err(RpcError::invalid(format!("参数 {key} 必须是字符串"))),
    }
}

fn opt_i64(map: &Map<String, Value>, key: &str) -> Result<Option<i64>, RpcError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            RpcError::invalid(format!("参数 {key} 必须是整数"))
        }),
    }
}

fn opt_bool(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, RpcError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(|| {
            RpcError::invalid(format!("参数 {key} 必须是布尔值"))
        }),
    }
}

/// 字符串数组：元素须为字符串；trim 后为空的元素直接丢弃，全部为空时报错。
fn opt_str_array(map: &Map<String, Value>, key: &str) -> Result<Vec<String>, RpcError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if let Some(value) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                    out.push(value.to_string());
                }
            }
            if out.is_empty() {
                return Err(RpcError::invalid(format!("参数 {key} 必须是非空字符串数组")));
            }
            Ok(out)
        }
        Some(_) => Err(RpcError::invalid(format!("参数 {key} 必须是字符串数组"))),
    }
}

/// mail_list 的不透明游标：`o<offset>`；非法值一律参数错误。
fn parse_cursor(map: &Map<String, Value>) -> Result<i64, RpcError> {
    let raw = match opt_str(map, "cursor")? {
        None => return Ok(0),
        Some(value) => value,
    };
    let offset = raw
        .strip_prefix('o')
        .and_then(|rest| rest.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .ok_or_else(|| RpcError::invalid("cursor 非法：请使用上次返回的 nextCursor"))?;
    Ok(offset)
}

/* ---------------- 只读工具 ---------------- */

fn folder_list(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["accountId"])?;
    let wanted = opt_i64(map, "accountId")?.unwrap_or(0);
    let conn = app.db();
    let mut items = Vec::new();
    for account in db::account_list(&conn)? {
        if wanted > 0 && account.id != wanted {
            continue;
        }
        for mailbox in db::mailbox_list(&conn, account.id)? {
            items.push(json!({
                "accountId": account.id,
                "accountEmail": account.email,
                "folderId": mailbox.id,
                "name": mailbox.name,
                "role": mailbox.role,
                "unread": mailbox.unread,
                "total": mailbox.total,
                "selectable": mailbox.selectable,
            }));
        }
    }
    Ok(json!({ "items": items, "count": items.len() }))
}

fn mail_list(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["accountId", "folder", "folderId", "query", "cursor", "limit"])?;
    let offset = parse_cursor(map)?;
    let limit = opt_i64(map, "limit")?.unwrap_or(20).clamp(1, 50);
    // folder 兼容两种写法：role 名，或直接给数字文件夹 id
    let folder_raw = opt_str(map, "folder")?;
    let folder_id_param = opt_i64(map, "folderId")?;
    let parsed_folder_id = folder_id_param.or_else(|| {
        folder_raw.as_deref().and_then(|name| {
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
                name.parse::<i64>().ok()
            } else {
                None
            }
        })
    });
    let mailbox_id = parsed_folder_id;
    let role = if mailbox_id.is_some() { None } else { folder_raw };
    let filter = db::ThreadFilter {
        mailbox_id,
        role,
        account_id: opt_i64(map, "accountId")?.filter(|value| *value > 0),
        query: opt_str(map, "query")?.unwrap_or_default(),
        kind: "all".into(),
        limit,
        offset,
    };
    let conn = app.db();
    let items = db::thread_list(&conn, &filter)?;
    let mut payload = json!({
        "items": items,
        "count": items.len(),
        "offset": offset,
        "limit": limit,
    });
    if items.len() as i64 == limit {
        payload["nextCursor"] = json!(format!("o{}", offset + limit));
    }
    Ok(payload)
}

fn mail_read(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["threadId", "messageId", "accountId", "network"])?;
    let thread_id = opt_str(map, "threadId")?;
    let message_id = opt_i64(map, "messageId")?;
    if thread_id.is_none() && message_id.is_none() {
        return Err(RpcError::invalid("需要 threadId 或 messageId 之一"));
    }
    if thread_id.is_some() && message_id.is_some() {
        return Err(RpcError::invalid("threadId 与 messageId 只能给一个"));
    }
    let network = opt_bool(map, "network")?.unwrap_or(true);

    // 定位消息集合与所属账户
    let (account_id, subject, summaries) = {
        let conn = app.db();
        match message_id {
            Some(id) => {
                let summary = db::message_get(&conn, id)?
                    .ok_or_else(|| RpcError::invalid(format!("邮件不存在：{id}")))?;
                let subject = summary.subject.clone();
                (summary.account_id, subject, vec![summary])
            }
            None => {
                let thread = thread_id.clone().unwrap_or_default();
                let account_id =
                    resolve_thread_account(&conn, &thread, opt_i64(map, "accountId")?)?;
                let summaries = db::message_thread(&conn, account_id, &thread)?;
                if summaries.is_empty() {
                    return Err(RpcError::invalid(format!("会话不存在：{thread}")));
                }
                let subject = summaries[0].subject.clone();
                (account_id, subject, summaries)
            }
        }
    };

    // 逐封取正文并拼纯文本；单封取正文失败不打断整封会话
    let account = {
        let conn = app.db();
        db::account_get(&conn, account_id)?.ok_or_else(|| RpcError::invalid("账户不存在"))?
    };
    let total = summaries.len();
    let mut sections = Vec::with_capacity(total);
    let mut message_ids = Vec::with_capacity(total);
    let mut pending_bodies = 0usize;
    for (index, summary) in summaries.iter().enumerate() {
        message_ids.push(summary.id);
        let header = format!(
            "[{}/{}] id={} {}",
            index + 1,
            total,
            summary.id,
            format_time(summary.date),
        );
        let mut section = format!(
            "{header}\n发件人：{} <{}>\n收件人：{}",
            summary.from_name, summary.from_addr, summary.to_addrs.join(", "),
        );
        if !summary.cc_addrs.is_empty() {
            section.push_str(&format!("\n抄送：{}", summary.cc_addrs.join(", ")));
        }
        if summary.has_attachments {
            section.push_str("\n（有附件，MCP 不展示附件内容）");
        }
        let body = app.rt.block_on(sync::ensure_body(Arc::clone(app), summary.id, network));
        match body {
            Ok(detail) => {
                let text = if detail.text_body.trim().is_empty() {
                    html_to_text(&detail.html_body)
                } else {
                    detail.text_body.clone()
                };
                if text.trim().is_empty() {
                    section.push_str(&format!("\n正文：（空）\n摘要：{}", summary.snippet));
                } else {
                    section.push_str(&format!("\n正文：\n{}", text.trim()));
                }
            }
            Err(_) => {
                pending_bodies += 1;
                section.push_str(&format!(
                    "\n正文：（未缓存，本次未能获取）\n摘要：{}",
                    summary.snippet
                ));
            }
        }
        sections.push(section);
    }

    let mut text = format!(
        "会话「{}」（{} 封，账户 {}）\n{}",
        subject,
        total,
        account.email,
        sections.join("\n\n----\n\n"),
    );
    if pending_bodies > 0 {
        text.push_str(&format!(
            "\n\n注意：{pending_bodies} 封正文未取到（离线或服务器不可达）。",
        ));
    }
    Ok(json!({
        "format": "plain",
        "text": text,
        "threadId": thread_id,
        "accountId": account_id,
        "subject": subject,
        "messageIds": message_ids,
    }))
}

/* ---------------- 写工具 ---------------- */

fn mail_flag(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["messageId", "threadId", "accountId", "seen", "flagged", "answered"])?;
    let seen = opt_bool(map, "seen")?;
    let flagged = opt_bool(map, "flagged")?;
    let answered = opt_bool(map, "answered")?;
    if seen.is_none() && flagged.is_none() && answered.is_none() {
        return Err(RpcError::invalid("至少要给 seen / flagged / answered 之一"));
    }
    let ids = resolve_targets(app, map)?;
    let count = ids.len();
    app.rt.block_on(sync::set_flags(
        Arc::clone(app),
        &ids,
        seen,
        flagged,
        answered,
    ))
    .map_err(RpcError::internal)?;
    Ok(json!({
        "ok": true,
        "updated": count,
        "sync": "deferred",
        "note": "本地已生效；服务器推送为尽力而为，失败会留在待同步队列随下次同步重试",
    }))
}

fn mail_move(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["messageId", "threadId", "accountId", "destination", "folderId"])?;
    let destination = opt_str(map, "destination")?;
    let folder_id = opt_i64(map, "folderId")?;
    if destination.is_some() && folder_id.is_some() {
        return Err(RpcError::invalid("destination 与 folderId 只能给一个"));
    }
    let target = match (folder_id, destination.as_deref()) {
        (Some(id), _) => {
            // 目标文件夹必须存在（账户归属由 move_messages 校验）
            let conn = app.db();
            db::mailbox_get(&conn, id)?
                .ok_or_else(|| RpcError::invalid(format!("目标文件夹不存在：{id}")))?;
            sync::Destination::Mailbox(id)
        }
        (None, Some("trash")) => sync::Destination::Trash,
        (None, Some("junk")) => sync::Destination::Junk,
        (None, Some("archive")) => sync::Destination::Archive,
        (None, None) => sync::Destination::Archive,
        (None, Some(other)) => {
            return Err(RpcError::invalid(format!(
                "destination 只支持 archive / trash / junk，收到：{other}"
            )))
        }
    };
    let ids = resolve_targets(app, map)?;
    let count = ids.len();
    app.rt
        .block_on(sync::move_messages(Arc::clone(app), &ids, target))
        .map_err(RpcError::internal)?;
    Ok(json!({
        "ok": true,
        "moved": count,
        "destination": folder_id.map(|id| json!(format!("folder:{id}"))).unwrap_or_else(|| json!(destination.unwrap_or_else(|| "archive".into()))),
        "sync": "deferred",
    }))
}

fn mail_send(app: &Arc<App>, map: &Map<String, Value>) -> RpcResult {
    check_keys(map, &["accountId", "to", "cc", "bcc", "subject", "body", "dryRun"])?;
    let account_id = opt_i64(map, "accountId")?
        .ok_or_else(|| RpcError::invalid("缺少参数 accountId"))?;
    if account_id <= 0 {
        return Err(RpcError::invalid("accountId 必须是插件账户 id（正整数）"));
    }
    let to = opt_str_array(map, "to")?;
    let cc = opt_str_array(map, "cc")?;
    let bcc = opt_str_array(map, "bcc")?;
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err(RpcError::invalid("请至少填写一个收件人（to / cc / bcc）"));
    }
    let subject = opt_str(map, "subject")?.unwrap_or_default();
    let body = opt_str(map, "body")?.unwrap_or_default();
    let dry_run = opt_bool(map, "dryRun")?.unwrap_or(false);

    // 账户必须存在；dry_run 也走同样的校验，保证预览与真实发送口径一致
    let account = {
        let conn = app.db();
        db::account_get(&conn, account_id)?
            .ok_or_else(|| RpcError::invalid(format!("账户不存在：{account_id}")))?
    };

    let settings = {
        let conn = app.db();
        db::load_settings(&conn).unwrap_or_default()
    };
    if dry_run {
        let preview = json!({
            "dryRun": true,
            "from": format!("{} <{}>", account.display_name, account.email),
            "to": to,
            "cc": cc,
            "bcc": bcc,
            "subject": if subject.trim().is_empty() { "(无主题)" } else { subject.as_str() },
            "bodyPreview": preview_text(&body),
            "undoSeconds": settings.undo_delay,
            "note": "预览模式：未写发件箱、未联网。去掉 dryRun 或设为 false 再次调用才会真正排队发送",
        });
        return Ok(preview);
    }

    // 真实发送：复用 compose::send（发件箱队列 + 撤销窗口），默认调度
    let send_params = json!({
        "accountId": account_id,
        "to": to,
        "cc": cc,
        "bcc": bcc,
        "subject": subject,
        "html": body_to_html(&body),
    });
    let result = compose::send(Arc::clone(app), &send_params).map_err(RpcError::internal)?;
    Ok(json!({
        "queued": true,
        "outboxId": result.get("outboxId"),
        "scheduledAt": result.get("scheduledAt"),
        "undoSeconds": result.get("undoSeconds"),
        "note": "已进入发件箱（撤销窗口内可在界面撤销）；投递是异步的，本响应不代表服务器已接收",
    }))
}

/* ---------------- 复用辅助 ---------------- */

/// threadId 的账户归属：优先显式 accountId；否则查消息表，
/// 命中多个账户时要求调用方指定（thread_id 不含账户维度）。
fn resolve_thread_account(
    conn: &Connection,
    thread_id: &str,
    requested: Option<i64>,
) -> Result<i64, RpcError> {
    if let Some(id) = requested.filter(|value| *value > 0) {
        return Ok(id);
    }
    let ids: Vec<i64> = conn
        .prepare("SELECT DISTINCT account_id FROM messages WHERE thread_id = ?1 ORDER BY account_id")?
        .query_map(rusqlite::params![thread_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    match ids.len() {
        1 => Ok(ids[0]),
        0 => Err(RpcError::invalid(format!("会话不存在：{thread_id}"))),
        _ => Err(RpcError::invalid(format!(
            "会话 {} 在多个账户下都存在，请指定 accountId",
            thread_id
        ))),
    }
}

/// 写工具的目标消息：messageId 优先，其次整条 threadId 会话。
fn resolve_targets(app: &Arc<App>, map: &Map<String, Value>) -> Result<Vec<i64>, RpcError> {
    let conn = app.db();
    if let Some(id) = opt_i64(map, "messageId")? {
        db::message_get(&conn, id)?
            .ok_or_else(|| RpcError::invalid(format!("邮件不存在：{id}")))?;
        return Ok(vec![id]);
    }
    let thread_id = opt_str(map, "threadId")?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::invalid("需要 messageId 或 threadId 之一"))?;
    let account_id = resolve_thread_account(&conn, &thread_id, opt_i64(map, "accountId")?)?;
    let summaries = db::message_thread(&conn, account_id, &thread_id)?;
    if summaries.is_empty() {
        return Err(RpcError::invalid(format!("会话不存在：{thread_id}")));
    }
    Ok(summaries.iter().map(|summary| summary.id).collect())
}

/// 毫秒时间戳 → 本地时间文本
fn format_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// 极简 HTML → 纯文本（正文只有 HTML 时的降级展示）
fn html_to_text(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let mut text = html.to_string();
    for tag in ["style", "script", "head"] {
        // 成对块整体删除（不区分大小写的简单实现）
        let open = format!("<{tag}");
        while let Some(start) = text.to_lowercase().find(&open) {
            let marker = format!("</{tag}>");
            let end = text.to_lowercase()[start..]
                .find(&marker)
                .map(|offset| start + offset + marker.len());
            let Some(end) = end else { break };
            text.replace_range(start..end, "");
        }
    }
    for marker in ["<br", "</p>", "</div>", "</tr>", "</li>", "</h"] {
        while let Some(position) = text.to_lowercase().find(marker) {
            let cut = position + marker.len();
            // <br...> 可能带属性，吞到 '>'
            let cut = text[cut..].find('>').map(|offset| cut + offset + 1).unwrap_or(cut);
            text.replace_range(position..cut, "\n");
        }
    }
    // 去掉剩余标签
    let mut out = String::with_capacity(text.len());
    let mut depth = false;
    for ch in text.chars() {
        match ch {
            '<' => depth = true,
            '>' => depth = false,
            c if !depth => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // 折叠多余空行
    let mut collapsed = String::with_capacity(out.len());
    let mut blanks = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 2 {
                continue;
            }
        } else {
            blanks = 0;
        }
        collapsed.push_str(line);
        collapsed.push('\n');
    }
    collapsed.trim().to_string()
}

/// 纯文本正文 → 简单 HTML（无任何标签时）；已有标签则原样交由 MIME 组装
fn body_to_html(body: &str) -> String {
    if body.contains('<') {
        return body.to_string();
    }
    let escaped = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let paragraphs: Vec<String> = escaped
        .split("\n\n")
        .map(|block| format!("<p>{}</p>", block.replace('\n', "<br>")))
        .collect();
    paragraphs.join("")
}

/// dry_run 预览用的正文摘录（首 600 字符）
fn preview_text(body: &str) -> String {
    let plain = if body.contains('<') {
        html_to_text(body)
    } else {
        body.to_string()
    };
    let trimmed = plain.trim();
    const MAX: usize = 600;
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let mut cut = MAX;
    while !trimmed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…（截断）", &trimmed[..cut])
}

/* ---------------- 单测 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Account;
    use crate::SyncRegistry;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// 临时目录 + 空库 + 独立运行时的 App（不起后台 worker，不联网）
    fn test_app(tag: &str) -> (Arc<App>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dbx-mail-mcp-test-{}-{}-{}",
            tag,
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("test.db")).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let app = Arc::new(App {
            data_dir: dir.clone(),
            db: Mutex::new(conn),
            secrets: crate::secrets::SecretStore::new(&dir),
            rt: runtime,
            syncing: SyncRegistry::new(),
            token_cache: Mutex::new(HashMap::new()),
            oauth_pending: Mutex::new(HashMap::new()),
            host_sessions: Mutex::new(HashMap::new()),
        });
        (app, dir)
    }

    fn insert_account(conn: &Connection) -> i64 {
        let account = Account {
            id: 0,
            email: "alice@example.com".into(),
            display_name: "Alice".into(),
            username: "alice".into(),
            provider: "imap".into(),
            auth_type: "password".into(),
            tenant_id: String::new(),
            client_id: String::new(),
            user_id: String::new(),
            imap_host: "imap.example.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            signature: String::new(),
            color: "#e8660c".into(),
            sync_enabled: false,
            sync_interval: 5,
            last_sync_at: 0,
            last_error: String::new(),
            host_bound: false,
            host_connection_id: String::new(),
        };
        db::account_insert(conn, &account, "test-ref").unwrap()
    }

    fn call_args(app: &Arc<App>, tool: &str, arguments: Value) -> Result<Value, RpcError> {
        call(app, &json!({ "tool": tool, "arguments": arguments }))
    }

    #[test]
    fn tools_have_expected_shape() {
        let defs = tool_definitions();
        let tools = defs.get("tools").and_then(Value::as_array).unwrap();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            ["folder_list", "mail_list", "mail_read", "mail_flag", "mail_move", "mail_send"]
        );
        for tool in tools {
            // 形状：name / description / inputSchema(object)
            assert!(!tool.get("description").and_then(Value::as_str).unwrap_or("").is_empty());
            assert_eq!(tool.get("inputSchema").and_then(|schema| schema.get("type")).unwrap(), "object");
            // 必填字段只能出现在 properties 里
            for required in tool
                .get("inputSchema")
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                let key = required.as_str().unwrap();
                assert!(tool["inputSchema"]["properties"].get(key).is_some());
            }
        }
        // 只读标注：三个读工具为 readOnlyHint，三个写工具没有该标注
        let read_only = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()["annotations"]["readOnlyHint"]
                .as_bool()
                .unwrap_or(false)
        };
        assert!(read_only("folder_list"));
        assert!(read_only("mail_list"));
        assert!(read_only("mail_read"));
        assert!(!read_only("mail_flag"));
        assert!(!read_only("mail_move"));
        assert!(!read_only("mail_send"));
        // 凭据红线：定义里不得出现密码/token/secret 字样
        let dump = defs.to_string().to_lowercase();
        for word in ["password", "token", "secret", "credential"] {
            assert!(!dump.contains(word), "工具定义不应包含 {word}");
        }
    }

    #[test]
    fn rejects_bad_tool_and_arguments() {
        let (app, dir) = test_app("validation");
        {
            let conn = app.db();
            insert_account(&conn);
        }

        // 未知工具
        let error = call_args(&app, "mail_delete", json!({})).unwrap_err();
        assert!(error.message.contains("未知工具"), "{}", error.message);
        assert_eq!(error.code, -32602);

        // 缺 tool / arguments 非对象 / lifecycle 非对象
        let error = call(&app, &json!({ "arguments": {} })).unwrap_err();
        assert!(error.message.contains("tool"));
        let error = call(&app, &json!({ "tool": "folder_list", "arguments": [1] })).unwrap_err();
        assert!(error.message.contains("arguments"));
        let error = call(
            &app,
            &json!({ "tool": "folder_list", "arguments": {}, "lifecycle": "x" }),
        )
        .unwrap_err();
        assert!(error.message.contains("lifecycle"));

        // 未知参数键
        let error = call_args(&app, "folder_list", json!({ "evil": true })).unwrap_err();
        assert!(error.message.contains("未知参数"));

        // mail_flag：没有旗标 / 没有目标
        let error = call_args(&app, "mail_flag", json!({ "messageId": 1 })).unwrap_err();
        assert!(error.message.contains("seen / flagged / answered"));
        let error = call_args(&app, "mail_flag", json!({ "seen": true })).unwrap_err();
        assert!(error.message.contains("messageId 或 threadId"));
        let error = call_args(&app, "mail_flag", json!({ "threadId": "nope", "seen": true })).unwrap_err();
        assert!(error.message.contains("会话不存在"));

        // mail_move：destination 非法 / 两者同给 / 目标文件夹不存在
        let error = call_args(&app, "mail_move", json!({ "messageId": 1, "destination": "desk" })).unwrap_err();
        assert!(error.message.contains("destination"));
        let error = call_args(
            &app,
            "mail_move",
            json!({ "messageId": 1, "destination": "trash", "folderId": 2 }),
        )
        .unwrap_err();
        assert!(error.message.contains("只能给一个"));
        let error = call_args(&app, "mail_move", json!({ "messageId": 1, "folderId": 999 })).unwrap_err();
        assert!(error.message.contains("目标文件夹不存在"));

        // mail_send：缺 accountId / accountId 非法 / 缺收件人
        let error = call_args(&app, "mail_send", json!({ "to": ["b@x.com"] })).unwrap_err();
        assert!(error.message.contains("accountId"));
        let error = call_args(&app, "mail_send", json!({ "accountId": 0, "to": ["b@x.com"] })).unwrap_err();
        assert!(error.message.contains("正整数"));
        let error = call_args(&app, "mail_send", json!({ "accountId": 1 })).unwrap_err();
        assert!(error.message.contains("收件人"));
        let error = call_args(&app, "mail_send", json!({ "accountId": 404, "to": ["b@x.com"] })).unwrap_err();
        assert!(error.message.contains("账户不存在"));

        // mail_read：缺目标 / 两个都给
        let error = call_args(&app, "mail_read", json!({})).unwrap_err();
        assert!(error.message.contains("threadId 或 messageId"));
        let error = call_args(
            &app,
            "mail_read",
            json!({ "threadId": "t", "messageId": 1 }),
        )
        .unwrap_err();
        assert!(error.message.contains("只能给一个"));

        // mail_list：坏 cursor；好 cursor 走通空列表
        let error = call_args(&app, "mail_list", json!({ "cursor": "page2" })).unwrap_err();
        assert!(error.message.contains("cursor 非法"));
        let error = call_args(&app, "mail_list", json!({ "limit": "ten" })).unwrap_err();
        assert!(error.message.contains("limit 必须是整数"));
        let ok = call_args(&app, "mail_list", json!({ "cursor": "o20", "limit": 5 })).unwrap();
        let payload: Value =
            serde_json::from_str(ok["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["count"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_dry_run_never_enqueues() {
        let (app, dir) = test_app("dryrun");
        let account_id = {
            let conn = app.db();
            insert_account(&conn)
        };

        let outbox_count = |app: &App| -> i64 {
            let conn = app.db();
            conn.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(outbox_count(&app), 0);

        // dry_run：返回预览，发件箱仍为空
        let result = call_args(
            &app,
            "mail_send",
            json!({
                "accountId": account_id,
                "to": ["bob@example.com", "  "],
                "cc": ["carol@example.com"],
                "subject": "预览测试",
                "body": "第一段。\n\n第二段 <b>加粗</b>。",
                "dryRun": true,
            }),
        )
        .unwrap();
        assert_eq!(result["isError"], false);
        let preview: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(preview["dryRun"], true);
        assert_eq!(preview["subject"], "预览测试");
        assert_eq!(preview["to"], json!(["bob@example.com"]));
        assert_eq!(preview["cc"], json!(["carol@example.com"]));
        assert!(preview["from"].as_str().unwrap().contains("alice@example.com"));
        assert!(preview["bodyPreview"].as_str().unwrap().contains("加粗"));
        assert_eq!(outbox_count(&app), 0, "dry_run 不得写发件箱");

        // 对照：不带 dryRun 的真实调用会排队（测试 App 无投递线程，不会联网）
        let result = call_args(
            &app,
            "mail_send",
            json!({
                "accountId": account_id,
                "to": ["bob@example.com"],
                "subject": "真实排队",
                "body": "hello",
            }),
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["queued"], true);
        assert!(payload["outboxId"].as_i64().unwrap() > 0);
        assert_eq!(outbox_count(&app), 1, "真实发送应恰好排队一封");

        // 空会话/邮件的目标解析
        let error = call_args(&app, "mail_flag", json!({ "messageId": 12345, "seen": true })).unwrap_err();
        assert!(error.message.contains("邮件不存在"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn html_and_preview_helpers() {
        let text = html_to_text("<style>p{}</style><p>你好</p><p>第二段</p>");
        assert!(text.contains("你好"));
        assert!(text.contains("第二段"));
        assert!(!text.contains('<'));
        assert_eq!(body_to_html("a\nb\n\nc"), "<p>a<br>b</p><p>c</p>");
        assert_eq!(body_to_html("<p>raw</p>"), "<p>raw</p>");
        assert!(preview_text("短文本").contains("短文本"));
        let long = "x".repeat(1000);
        assert!(preview_text(&long).chars().count() <= 605);
    }
}

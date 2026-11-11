//! Microsoft Graph REST 客户端（Microsoft 365 / Outlook.com）。
//!
//! 与 IMAP 路径同构的三类能力：
//! 1. 文件夹与增量同步（`/me/mailFolders` + `/messages/delta`）；
//! 2. 邮件体按需取回（`/messages/{id}/$value`，完整 MIME，复用 mime.rs 解析）；
//! 3. 旗标 / 移动 / 删除 / 发送（MIME 建草稿保住线程头，大附件走上传会话）。
//!
//! 所有请求都带 Bearer token（oauth.rs 提供），429/503 按 `Retry-After` 退避。

use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sha2::Digest;

use crate::oauth;

/// 单个附件与 MIME 建草稿的大小红线（超过走 JSON + 上传会话回退）
const MIME_CREATE_LIMIT: i64 = 4 * 1024 * 1024;
/// 上传会话分片大小：Graph 要求非末片必须是 320KB 的倍数（4 MiB 不对齐会被拒收）
const UPLOAD_CHUNK: usize = 320 * 1024 * 15;
/// 429/503 退避重试上限
const MAX_RETRIES: usize = 3;

/// Graph 邮件（delta 列表项）
#[derive(Debug, Clone, Default)]
pub struct GraphMessage {
    pub id: String,
    /// delta 里的 `@removed`（远端已删除）
    pub removed: bool,
    /// 会话标识：懒加载入库时用作线程聚合键
    pub conversation_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub received_ms: i64,
    pub seen: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    pub preview: String,
}

/// Graph 文件夹
#[derive(Debug, Clone)]
pub struct GraphFolder {
    pub id: String,
    pub display_name: String,
    /// 本地唯一键：完整路径（顶层=显示名；子文件夹=父路径/显示名，"/"分隔）。
    /// Outlook 允许不同父级下同名文件夹，裸显示名会撞 (account_id, name) 唯一键
    pub path: String,
    /// wellKnownFolderName（inbox / sentitems / drafts / deleteditems / junkemail / archive…）
    pub well_known: String,
    pub total: i64,
    pub unread: i64,
}

/// delta 翻页结果
#[derive(Debug, Default)]
pub struct DeltaPage {
    pub messages: Vec<GraphMessage>,
    pub next_link: Option<String>,
    pub delta_link: Option<String>,
}

/// Graph REST 客户端；reqwest::Client 内部是连接池，Clone 供并发同步共用
#[derive(Clone)]
pub struct GraphClient {
    http: reqwest::Client,
    /// 含路径段：{graph_base}/v1.0/me 或 {graph_base}/v1.0/users/{user_id}
    base: String,
    token: String,
}

impl GraphClient {
    /// user_id 非空 → 纯后台接入（client_credentials），访问该用户邮箱；
    /// 为空 → 浏览器授权账户，走 /me。
    pub fn new(access_token: String, user_id: &str) -> Self {
        let segment = if user_id.trim().is_empty() {
            "me".to_string()
        } else {
            format!("users/{}", user_id.trim())
        };
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
            base: format!("{}/v1.0/{}", oauth::graph_base(), segment),
            token: access_token,
        }
    }

    /* ---------------- 请求基础设施 ---------------- */

    /// 统一请求入口：429/503 退避重试，非 2xx 解析 Graph 错误体
    async fn request(&self, method: reqwest::Method, url: &str, body: Option<Value>, mime_body: Option<&[u8]>, content_type: &str) -> Result<reqwest::Response, String> {
        let mut attempts = 0;
        loop {
            let mut request = self.http.request(method.clone(), url).bearer_auth(&self.token);
            if let Some(value) = &body {
                request = request.json(value);
            }
            if let Some(bytes) = mime_body {
                request = request.body(bytes.to_vec()).header("Content-Type", content_type);
            }
            let response = request.send().await.map_err(|error| format!("连接 Graph 服务器失败：{error}"))?;
            let status = response.status();
            if (status.as_u16() == 429 || status.as_u16() == 503) && attempts < MAX_RETRIES {
                attempts += 1;
                let seconds = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(2u64.pow(attempts as u32));
                tokio::time::sleep(Duration::from_secs(seconds.min(30))).await;
                continue;
            }
            if status.is_success() {
                return Ok(response);
            }
            let text = response.text().await.unwrap_or_default();
            let value: Value = serde_json::from_str(&text).unwrap_or_default();
            return Err(graph_error_message(&value, status.as_u16()));
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let response = self.request(reqwest::Method::GET, url, None, None, "").await?;
        response.json().await.map_err(|error| format!("解析 Graph 响应失败：{error}"))
    }

    async fn send_json(&self, method: reqwest::Method, url: &str, body: Value) -> Result<Value, String> {
        let response = self.request(method, url, Some(body), None, "").await?;
        // send/move 等动作可能返回空体（202 Accepted）
        let text = response.text().await.map_err(|error| format!("读取 Graph 响应失败：{error}"))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|error| format!("解析 Graph 响应失败：{error}"))
    }

    /* ---------------- 账户与文件夹 ---------------- */

    /// 授权/连通性验证
    pub async fn me(&self) -> Result<Value, String> {
        self.get_json(&format!("{}", self.base)).await
    }

    /// 单个文件夹（同步后刷新计数用）
    pub async fn folder(&self, folder_id: &str) -> Result<GraphFolder, String> {
        let value = self.get_json(&format!("{}/mailFolders/{folder_id}", self.base)).await?;
        let path = value.get("displayName").and_then(Value::as_str).unwrap_or("").to_string();
        folder_from(&value, path).ok_or_else(|| "文件夹响应缺少 id".to_string())
    }

    /// 全部可见文件夹（含子文件夹，深度 ≤3）
    pub async fn folders(&self) -> Result<Vec<GraphFolder>, String> {
        let mut result = Vec::new();
        let mut url = format!("{}/mailFolders?$top=99", self.base);
        loop {
            let page = self.get_json(&url).await?;
            let items = page.get("value").and_then(Value::as_array).cloned().unwrap_or_default();
            for item in items {
                if item.get("isHidden").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                if let Some(folder) = folder_from(&item, item.get("displayName").and_then(Value::as_str).unwrap_or("").to_string()) {
                    result.push(folder.clone());
                    let child_path = folder.path.clone();
                    self.collect_children(&folder.id, &child_path, 1, &mut result).await?;
                }
            }
            match page.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => url = next.to_string(),
                None => break,
            }
        }
        Ok(result)
    }

    async fn collect_children(&self, parent_id: &str, parent_path: &str, depth: usize, result: &mut Vec<GraphFolder>) -> Result<(), String> {
        if depth > 3 {
            return Ok(());
        }
        let mut url = format!("{}/mailFolders/{parent_id}/childFolders?$top=99", self.base);
        loop {
            let page = self.get_json(&url).await?;
            let items = page.get("value").and_then(Value::as_array).cloned().unwrap_or_default();
            for item in items {
                if item.get("isHidden").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                let path = format!(
                    "{parent_path}/{}",
                    item.get("displayName").and_then(Value::as_str).unwrap_or("")
                );
                if let Some(folder) = folder_from(&item, path) {
                    result.push(folder.clone());
                    let child_path = folder.path.clone();
                    Box::pin(self.collect_children(&folder.id, &child_path, depth + 1, result)).await?;
                }
            }
            match page.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => url = next.to_string(),
                None => break,
            }
        }
        Ok(())
    }

    /* ---------------- 增量同步 ---------------- */

    /// 文件夹 delta 第一页：有 deltaLink 用 deltaLink，否则全量拉取
    pub async fn delta_first(&self, folder_id: &str, delta_link: Option<&str>) -> Result<DeltaPage, String> {
        let url = match delta_link.filter(|link| !link.is_empty()) {
            Some(link) => link.to_string(),
            None => format!(
                "{}/mailFolders/{folder_id}/messages/delta?$top=50&$select=id,subject,conversationId,from,toRecipients,ccRecipients,receivedDateTime,isRead,flag,hasAttachments,bodyPreview",
                self.base
            ),
        };
        self.delta_page(&url).await
    }

    /// 最新 N 封（首次同步用；delta 接口不支持按时间排序）
    pub async fn recent_messages(&self, folder_id: &str, limit: usize) -> Result<Vec<GraphMessage>, String> {
        let mut result = Vec::new();
        let mut url = format!(
            "{}/mailFolders/{folder_id}/messages?$top={}&$orderBy=receivedDateTime desc&$select=id,subject,conversationId,from,toRecipients,ccRecipients,receivedDateTime,isRead,flag,hasAttachments,bodyPreview",
            self.base,
            limit.clamp(1, 1000)
        );
        loop {
            let page = self.get_json(&url).await?;
            for item in page.get("value").and_then(Value::as_array).into_iter().flatten() {
                result.push(parse_delta_message(item));
            }
            // 首次同步限量：收满即止（$top 只是页大小，nextLink 会翻完整邮箱）
            if result.len() >= limit {
                break;
            }
            match page.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => url = next.to_string(),
                None => break,
            }
        }
        result.truncate(limit);
        Ok(result)
    }

    pub async fn delta_page(&self, url: &str) -> Result<DeltaPage, String> {
        let value = self.get_json(url).await?;
        Ok(parse_delta(&value))
    }

    /// 完整 MIME（与 IMAP 的 raw 等价，交给 mime.rs 统一解析）
    pub async fn raw_message(&self, message_id: &str) -> Result<Vec<u8>, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("{}/messages/{message_id}/$value", self.base),
                None,
                None,
                "",
            )
            .await?;
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| format!("读取邮件原文失败：{error}"))
    }

    /* ---------------- 旗标与整理 ---------------- */

    pub async fn set_read(&self, message_id: &str, read: bool) -> Result<(), String> {
        self.send_json(reqwest::Method::PATCH, &self.message_url(message_id), json!({ "isRead": read }))
            .await
            .map(|_| ())
    }

    pub async fn set_flag(&self, message_id: &str, flagged: bool) -> Result<(), String> {
        let status = if flagged { "flagged" } else { "notFlagged" };
        self.send_json(
            reqwest::Method::PATCH,
            &self.message_url(message_id),
            json!({ "flag": { "flagStatus": status } }),
        )
        .await
        .map(|_| ())
    }

    /// 移动消息；Graph 默认 ID 机制下移动会换新 ID，返回响应里的新 remote_id
    pub async fn move_message(&self, message_id: &str, folder_id: &str) -> Result<String, String> {
        let moved = self
            .send_json(
                reqwest::Method::POST,
                &format!("{}/move", self.message_url(message_id)),
                json!({ "destinationId": folder_id }),
            )
            .await?;
        moved
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "移动响应缺少新消息 ID".to_string())
    }

    pub async fn delete_message(&self, message_id: &str) -> Result<(), String> {
        let response = self
            .request(reqwest::Method::DELETE, &self.message_url(message_id), None, None, "")
            .await?;
        drop(response);
        Ok(())
    }

    fn message_url(&self, message_id: &str) -> String {
        format!("{}/messages/{message_id}", self.base)
    }

    /* ---------------- 发送 ---------------- */

    /// 发送原始 MIME。
    ///
    /// 主路径：建空草稿 → PUT 完整 MIME（保留 Message-ID/References 头）→ send。
    /// MIME 超 4MB（含附件）时回退 JSON 草稿 + 上传会话分片（会丢失线程头，
    /// 属可接受的罕见场景）。
    pub async fn send_mime(&self, mime: &[u8]) -> Result<(), String> {
        if mime.len() as i64 > MIME_CREATE_LIMIT {
            return self.send_mime_via_session(mime).await;
        }
        let draft = self
            .send_json(reqwest::Method::POST, &format!("{}/messages", self.base), json!({}))
            .await?;
        let draft_id = draft
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "创建发件草稿失败".to_string())?
            .to_string();
        let put = self
            .request(
                reqwest::Method::PUT,
                &format!("{}/$value", self.message_url(&draft_id)),
                None,
                Some(mime),
                "text/plain; charset=UTF-8",
            )
            .await;
        if let Err(_error) = put {
            // MIME 路径被拒（如附件策略）时回退上传会话路径
            let _ = self.delete_message(&draft_id).await;
            return self.send_mime_via_session(mime).await;
        }
        self.send_json(
            reqwest::Method::POST,
            &format!("{}/send", self.message_url(&draft_id)),
            json!({}),
        )
        .await
        .map(|_| ())
    }

    /// 回退路径：JSON 草稿 + 附件上传会话 + send
    async fn send_mime_via_session(&self, mime: &[u8]) -> Result<(), String> {
        // 从 MIME 里解出主题/正文/收件人/附件
        let ctx = crate::mime::ParseContext {
            account_id: 0,
            mailbox_id: 0,
            uid: None,
            internal_date: 0,
            seen: false,
            flagged: false,
            answered: false,
            draft: false,
            size: mime.len() as i64,
        };
        let parsed = crate::mime::parse(mime, &ctx).ok_or_else(|| "解析发件内容失败".to_string())?;
        let bcc_addrs = crate::mime::parse_bcc(mime);
        let html = parsed.html.clone();
        let text = parsed.text.clone();
        let content_type = if html.is_empty() { "Text" } else { "HTML" };
        let body_content = if html.is_empty() { text } else { html };

        let message = json!({
            "subject": parsed.header.subject,
            "body": { "contentType": content_type, "content": body_content },
            "toRecipients": recipients(&parsed.header.to_addrs),
            "ccRecipients": recipients(&parsed.header.cc_addrs),
            "bccRecipients": recipients(&bcc_addrs),
        });
        let draft = self
            .send_json(reqwest::Method::POST, &format!("{}/messages", self.base), message)
            .await?;
        let draft_id = draft
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "创建发件草稿失败".to_string())?
            .to_string();

        // 附件逐个走上传会话
        for attachment in &parsed.attachments {
            if attachment.inline {
                continue;
            }
            let (name, bytes, mime_type) =
                crate::mime::part_bytes(mime, attachment.index).ok_or_else(|| format!("读取附件「{}」失败", attachment.filename))?;
            self.upload_attachment(&draft_id, &name, &mime_type, &bytes).await?;
        }
        self.send_json(
            reqwest::Method::POST,
            &format!("{}/send", self.message_url(&draft_id)),
            json!({}),
        )
        .await
        .map(|_| ())
    }

    async fn upload_attachment(&self, draft_id: &str, name: &str, mime_type: &str, bytes: &[u8]) -> Result<(), String> {
        let session = self
            .send_json(
                reqwest::Method::POST,
                &format!("{}/attachments/createUploadSession", self.message_url(draft_id)),
                json!({
                    "AttachmentItem": {
                        "attachmentType": "file",
                        "name": name,
                        "contentType": if mime_type.is_empty() { "application/octet-stream" } else { mime_type },
                        "size": bytes.len(),
                    }
                }),
            )
            .await?;
        let upload_url = session
            .get("uploadUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| "创建附件上传会话失败".to_string())?
            .to_string();

        let total = bytes.len();
        let mut start = 0;
        while start < total {
            let end = (start + UPLOAD_CHUNK).min(total);
            let chunk = &bytes[start..end];
            let response = self
                .http
                .put(&upload_url)
                .header("Content-Range", format!("bytes {}-{}/{}", start, end - 1, total))
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(|error| format!("上传附件失败：{error}"))?;
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                return Err(format!("上传附件失败（{status}）：{text}"));
            }
            start = end;
        }
        Ok(())
    }
}

/* ---------------- 纯函数与解析 ---------------- */

fn folder_from(item: &Value, path: String) -> Option<GraphFolder> {
    let id = item.get("id")?.as_str()?.to_string();
    Some(GraphFolder {
        id,
        display_name: item.get("displayName").and_then(Value::as_str).unwrap_or("").to_string(),
        path,
        well_known: item.get("wellKnownFolderName").and_then(Value::as_str).unwrap_or("").to_string(),
        total: item.get("totalItemCount").and_then(Value::as_i64).unwrap_or(0),
        unread: item.get("unreadItemCount").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// raw 文件键：Graph id 很长且含路径敏感字符，取 SHA-256 前缀保证可安全作文件名
pub fn raw_key(remote_id: &str) -> String {
    let digest = sha2::Sha256::digest(remote_id.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(&digest[..12])
        .replace('-', "a")
        .replace('_', "b")
}

/// Graph wellKnownFolderName / 显示名 → 本地 role
pub fn role_of(well_known: &str, display_name: &str) -> String {
    match well_known {
        "inbox" => "inbox",
        "sentitems" => "sent",
        "drafts" => "drafts",
        "deleteditems" => "trash",
        "junkemail" => "junk",
        "archive" => "archive",
        _ => {
            let lower = display_name.to_lowercase();
            match lower.as_str() {
                "inbox" | "收件箱" => "inbox",
                "sent items" | "已发送" | "已发送邮件" => "sent",
                "drafts" | "草稿" | "草稿箱" => "drafts",
                "deleted items" | "已删除" | "已删除邮件" => "trash",
                "junk email" | "垃圾邮件" => "junk",
                "archive" | "存档" => "archive",
                _ => "custom",
            }
        }
    }
    .to_string()
}

fn parse_delta(value: &Value) -> DeltaPage {
    let mut page = DeltaPage::default();
    for item in value.get("value").and_then(Value::as_array).into_iter().flatten() {
        page.messages.push(parse_delta_message(item));
    }
    page.next_link = value.get("@odata.nextLink").and_then(Value::as_str).map(str::to_string);
    page.delta_link = value.get("@odata.deltaLink").and_then(Value::as_str).map(str::to_string);
    page
}

fn parse_delta_message(item: &Value) -> GraphMessage {
    let address_list = |key: &str| -> Vec<String> {
        item.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|entry| entry.pointer("/emailAddress/address").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    GraphMessage {
        id: item.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        removed: item.get("@removed").is_some(),
        conversation_id: item.get("conversationId").and_then(Value::as_str).unwrap_or("").to_string(),
        subject: item.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
        from_name: item.pointer("/from/emailAddress/name").and_then(Value::as_str).unwrap_or("").to_string(),
        from_addr: item.pointer("/from/emailAddress/address").and_then(Value::as_str).unwrap_or("").to_string(),
        to_addrs: address_list("toRecipients"),
        cc_addrs: address_list("ccRecipients"),
        received_ms: item
            .get("receivedDateTime")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms)
            .unwrap_or(0),
        seen: item.get("isRead").and_then(Value::as_bool).unwrap_or(false),
        flagged: item.pointer("/flag/flagStatus").and_then(Value::as_str) == Some("flagged"),
        has_attachments: item.get("hasAttachments").and_then(Value::as_bool).unwrap_or(false),
        preview: item.get("bodyPreview").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

/// RFC3339（如 2024-01-01T10:00:00Z）→ 毫秒时间戳
pub fn parse_rfc3339_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn graph_error_message(value: &Value, status: u16) -> String {
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Graph 请求失败");
    format!("Graph 请求失败（{status}）：{message}")
}

fn recipients(addresses: &[String]) -> Value {
    Value::Array(
        addresses
            .iter()
            .map(|address| json!({ "emailAddress": { "address": address } }))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_of_maps_well_known_and_localized_names() {
        assert_eq!(role_of("inbox", ""), "inbox");
        assert_eq!(role_of("sentitems", ""), "sent");
        assert_eq!(role_of("drafts", ""), "drafts");
        assert_eq!(role_of("deleteditems", ""), "trash");
        assert_eq!(role_of("junkemail", ""), "junk");
        assert_eq!(role_of("archive", ""), "archive");
        assert_eq!(role_of("", "Custom Folder"), "custom");
        // wellKnown 缺失时按显示名兜底（中英文）
        assert_eq!(role_of("", "收件箱"), "inbox");
        assert_eq!(role_of("", "Deleted Items"), "trash");
    }

    #[test]
    fn parse_delta_extracts_messages_links_and_removals() {
        let value: Value = serde_json::from_str(
            r#"{
                "value": [
                    {"id":"m1","subject":"你好","from":{"emailAddress":{"name":"张三","address":"z@x.com"}},
                     "toRecipients":[{"emailAddress":{"address":"me@x.com"}}],
                     "ccRecipients":[{"emailAddress":{"address":"c@x.com"}}],
                     "receivedDateTime":"2024-01-01T10:00:00Z","isRead":true,
                     "flag":{"flagStatus":"flagged"},"hasAttachments":true,"bodyPreview":"预览"},
                    {"id":"m2","@removed":{"reason":"deleted"}}
                ],
                "@odata.nextLink":"https://g/next",
                "@odata.deltaLink":"https://g/delta"
            }"#,
        )
        .unwrap();
        let page = parse_delta(&value);
        assert_eq!(page.messages.len(), 2);
        let first = &page.messages[0];
        assert_eq!(first.id, "m1");
        assert_eq!(first.from_addr, "z@x.com");
        assert_eq!(first.to_addrs, vec!["me@x.com".to_string()]);
        assert!(first.seen && first.flagged && first.has_attachments);
        assert_eq!(first.received_ms, parse_rfc3339_ms("2024-01-01T10:00:00Z").unwrap());
        assert!(page.messages[1].removed);
        assert_eq!(page.next_link.as_deref(), Some("https://g/next"));
        assert_eq!(page.delta_link.as_deref(), Some("https://g/delta"));
    }

    #[test]
    fn parse_delta_handles_plain_page_without_links() {
        let value: Value = serde_json::from_str(r#"{"value": []}"#).unwrap();
        let page = parse_delta(&value);
        assert!(page.messages.is_empty());
        assert!(page.next_link.is_none());
        assert!(page.delta_link.is_none());
    }

    #[test]
    fn rfc3339_parses_and_rejects_garbage() {
        assert_eq!(parse_rfc3339_ms("2024-01-01T10:00:00Z").unwrap(), 1_704_103_200_000);
        assert_eq!(parse_rfc3339_ms("2024-01-01T10:00:00.123Z").unwrap(), 1_704_103_200_123);
        assert!(parse_rfc3339_ms("not-a-date").is_none());
    }

    #[test]
    fn graph_error_message_prefers_server_text() {
        let value: Value =
            serde_json::from_str(r#"{"error":{"code":"ErrorAccessDenied","message":"拒绝访问"}}"#).unwrap();
        let message = graph_error_message(&value, 403);
        assert!(message.contains("拒绝访问"));
        assert!(message.contains("403"));
        let message = graph_error_message(&serde_json::Value::Null, 500);
        assert!(message.contains("Graph 请求失败"));
    }

    #[test]
    fn recipients_shape_matches_graph_schema() {
        let value = recipients(&vec!["a@x.com".to_string(), "b@x.com".to_string()]);
        assert_eq!(value[0]["emailAddress"]["address"], "a@x.com");
        assert_eq!(value.as_array().unwrap().len(), 2);
    }

    #[test]
    fn parse_delta_extracts_conversation_id() {
        let value: Value = serde_json::from_str(
            r#"{"value":[
                {"id":"m1","conversationId":"AAkALgA=","subject":"你好"},
                {"id":"m2","subject":"无会话"}
            ]}"#,
        )
        .unwrap();
        let page = parse_delta(&value);
        assert_eq!(page.messages[0].conversation_id, "AAkALgA=");
        assert_eq!(page.messages[1].conversation_id, "");
    }
}

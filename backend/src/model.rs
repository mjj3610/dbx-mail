//! 数据模型：与前端 `src/lib/rpc.ts` 的类型一一对应。
//!
//! 所有时间字段都是毫秒时间戳（i64），前端用 `new Date(ms)` 直接展示。

use serde::{Deserialize, Serialize};

/// 纯后台接入（client_credentials）的 auth_type：无 refresh_token，过期即重取应用令牌
pub const AUTH_TYPE_APP_ONLY: &str = "oauth2_app";

/// 账户（不含密码；密码单独存放在系统凭据库或本地密钥加密文件）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub username: String,
    /// imap / graph（graph = Microsoft 365 / Outlook.com，走 Graph API）
    pub provider: String,
    /// password / oauth2 / AUTH_TYPE_APP_ONLY
    pub auth_type: String,
    /// OAuth2 租户（graph 账户）：common / organizations / consumers / GUID
    pub tenant_id: String,
    pub client_id: String,
    pub imap_host: String,
    pub imap_port: u16,
    /// tls / starttls / none
    pub imap_security: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: String,
    pub signature: String,
    pub color: String,
    pub sync_enabled: bool,
    pub sync_interval: i64,
    pub last_sync_at: i64,
    pub last_error: String,
    /// 来自 DBX 宿主连接管理（connection-provider）的账户：
    /// 连接期凭据只在内存会话里，不落本地密钥库
    #[serde(default)]
    pub host_bound: bool,
    /// 宿主连接 id（connection.id），断开后清空
    #[serde(default)]
    pub host_connection_id: String,
    /// 纯后台接入（client_credentials）的目标邮箱：Graph 请求走
    /// /users/{user_id}/...；浏览器授权账户为空（走 /me）
    #[serde(default)]
    pub user_id: String,
}

/// 邮箱（服务器文件夹）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub delimiter: String,
    /// inbox / sent / drafts / trash / junk / archive / flagged / all / custom
    pub role: String,
    pub unread: i64,
    pub total: i64,
    pub uid_validity: i64,
    pub last_uid: i64,
    pub selectable: bool,
    pub sort_order: i64,
}

/// 会话列表项（同一 thread 只出现一次）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadItem {
    pub thread_id: String,
    pub subject: String,
    pub snippet: String,
    pub date: i64,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: Vec<String>,
    pub account_id: i64,
    pub account_email: String,
    pub account_color: String,
    pub mailbox_id: i64,
    pub mailbox_role: String,
    pub latest_id: i64,
    pub count: i64,
    pub unread: i64,
    pub flagged: bool,
    pub has_attachments: bool,
    pub answered: bool,
    pub participants: Vec<Participant>,
}

/// 会话参与者（头像用）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub name: String,
    pub email: String,
}

/// 邮件摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub id: i64,
    pub account_id: i64,
    pub mailbox_id: i64,
    pub mailbox_role: String,
    pub uid: Option<i64>,
    pub message_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub date: i64,
    pub size: i64,
    pub snippet: String,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub has_attachments: bool,
    pub body_loaded: bool,
}

/// 附件/内嵌部件元信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub index: i64,
    pub filename: String,
    pub mime: String,
    pub size: i64,
    pub content_id: String,
    pub inline: bool,
}

/// 邮件详情（正文 + 附件）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub text_body: String,
    pub html_body: String,
    pub attachments: Vec<Attachment>,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub account_email: String,
    pub account_name: String,
    pub signature: String,
}

/// 撰写附件（本地文件路径）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAttachment {
    pub name: String,
    pub path: String,
    pub mime: String,
    pub size: i64,
}

/// 草稿
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: i64,
    pub account_id: i64,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub bcc_addrs: Vec<String>,
    pub subject: String,
    pub body_html: String,
    pub attachments: Vec<DraftAttachment>,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub updated_at: i64,
    pub created_at: i64,
}

/// 发件箱条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: i64,
    pub account_id: i64,
    pub account_email: String,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub bcc_addrs: Vec<String>,
    pub subject: String,
    pub body_html: String,
    pub attachments: Vec<DraftAttachment>,
    pub scheduled_at: i64,
    pub status: String,
    pub error: String,
    pub attempts: i64,
    pub created_at: i64,
    pub sent_at: i64,
    pub in_reply_to: String,
    pub references: Vec<String>,
}

/// 设置项（键值）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// system / light / dark
    pub theme: String,
    pub locale: String,
    /// comfortable / compact
    pub density: String,
    pub remote_images: bool,
    pub mark_read_on_open: bool,
    /// 撤销发送窗口（秒）
    pub undo_delay: i64,
    /// 自动同步间隔（分钟，0 表示关闭）
    pub sync_interval: i64,
    /// primary / all
    pub sync_scope: String,
    /// 首次同步每个文件夹的邮件数
    pub initial_limit: i64,
    /// split / list
    pub layout: String,
    /// 会话聚合
    pub conversations: bool,
    /// 列表每页条数
    pub page_size: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            locale: "zh-CN".into(),
            density: "comfortable".into(),
            remote_images: false,
            mark_read_on_open: true,
            undo_delay: 10,
            sync_interval: 5,
            sync_scope: "primary".into(),
            initial_limit: 200,
            layout: "split".into(),
            conversations: true,
            page_size: 60,
        }
    }
}

/// 账户预设（新增账户向导）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub key: String,
    pub label: String,
    pub domains: Vec<String>,
    /// imap / graph；graph 预设不含服务器字段，走 OAuth 授权
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub imap_host: String,
    #[serde(default)]
    pub imap_port: u16,
    #[serde(default)]
    pub imap_security: String,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default)]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_security: String,
    pub hint: String,
}

/// 同步结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub account_id: i64,
    pub mailboxes: i64,
    pub new_messages: i64,
    pub updated: i64,
    pub errors: Vec<String>,
    pub synced_at: i64,
}

/// 同步进度（sync/status 展示「同步中 N/M」）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub account_id: i64,
    /// 当前正在同步的文件夹名
    pub mailbox: String,
    pub done: i64,
    pub total: i64,
}

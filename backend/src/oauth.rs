//! OAuth2 授权（Microsoft 365 / Outlook.com）。
//!
//! 授权码 + PKCE（S256）+ 本地回环回调：sidecar 在 `127.0.0.1` 随机端口上
//! 起临时监听，用户在系统浏览器完成登录后，微软把 `code` 回调回来，
//! 这里换取 token 集。refresh_token 交给 `SecretStore`（复用密码保管通道，
//! 引用格式 `oauth2:{email}`），access_token 只驻内存、到期前自动刷新。
//!
//! 出网只打微软固定端点；仅 debug 构建且显式设置 `DBX_MAIL_TEST_ENDPOINTS=1`
//! 时才允许用环境变量覆盖端点（mock 冒烟测试用），release 构建永远不生效。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::model::Account;
use crate::App;

/// Graph API 请求需要的委托权限
pub const SCOPES: &str = "offline_access https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send https://graph.microsoft.com/User.Read";
/// authorize/redirect/refresh 的回调等待时长
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// pending 授权记录保留时长（超时未完成的清除）
const PENDING_TTL_MS: i64 = 10 * 60 * 1000;
/// access_token 提前刷新余量（毫秒）
const TOKEN_REFRESH_MARGIN_MS: i64 = 120 * 1000;

/// 令牌端点基址（含 oauth2/v2.0 之前的部分）
pub fn authority_base() -> String {
    #[cfg(debug_assertions)]
    {
        if test_endpoints_enabled() {
            if let Ok(value) = std::env::var("DBX_MAIL_AUTHORIZE_BASE") {
                return value.trim_end_matches('/').to_string();
            }
        }
    }
    "https://login.microsoftonline.com".to_string()
}

/// Graph API 基址（graph.rs 使用）
pub fn graph_base() -> String {
    #[cfg(debug_assertions)]
    {
        if test_endpoints_enabled() {
            if let Ok(value) = std::env::var("DBX_MAIL_GRAPH_BASE") {
                return value.trim_end_matches('/').to_string();
            }
        }
    }
    "https://graph.microsoft.com".to_string()
}

#[cfg(debug_assertions)]
fn test_endpoints_enabled() -> bool {
    std::env::var("DBX_MAIL_TEST_ENDPOINTS").ok().as_deref() == Some("1")
}

/* ---------------- PKCE ---------------- */

/// base64url（无填充）
fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// 由 verifier 计算 code_challenge（S256）
pub fn challenge_for(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// 生成 (verifier, challenge)；verifier 取 64 字节随机数的 base64url（86 字符）
pub fn pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 64];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let verifier = b64url(&bytes);
    (verifier.clone(), challenge_for(&verifier))
}

/* ---------------- URL 与请求 ---------------- */

/// 构造授权页 URL
pub fn authorize_url(authority: &str, tenant: &str, client_id: &str, redirect_uri: &str, verifier: &str, state: &str) -> String {
    format!(
        "{authority}/{tenant}/oauth2/v2.0/authorize?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        urlencoding::encode(client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(SCOPES),
        urlencoding::encode(state),
        urlencoding::encode(&challenge_for(verifier)),
    )
}

/// token 集（交换与刷新共用）
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// 过期时刻（毫秒时间戳，已扣安全余量）
    pub expires_at: i64,
}

impl TokenSet {
    fn from_response(value: &serde_json::Value, fallback_refresh: &str) -> Result<Self, String> {
        let access_token = value
            .get("access_token")
            .and_then(|item| item.as_str())
            .ok_or_else(|| "授权响应缺少 access_token".to_string())?
            .to_string();
        let refresh_token = value
            .get("refresh_token")
            .and_then(|item| item.as_str())
            .map(str::to_string)
            .filter(|item| !item.is_empty())
            .unwrap_or_else(|| fallback_refresh.to_string());
        let expires_in = value.get("expires_in").and_then(|item| item.as_i64()).unwrap_or(3600);
        Ok(Self {
            access_token,
            refresh_token,
            expires_at: crate::now_ms() + (expires_in * 1000).saturating_sub(TOKEN_REFRESH_MARGIN_MS),
        })
    }
}

fn token_error(value: &serde_json::Value) -> String {
    let error = value
        .pointer("/error_description")
        .or_else(|| value.pointer("/error"))
        .and_then(|item| item.as_str())
        .unwrap_or("未知授权错误");
    error.to_string()
}

/// token 端点请求（交换与刷新共用表单提交）
async fn token_request(authority: &str, tenant: &str, form: &[(&str, &str)]) -> Result<TokenSet, String> {
    let url = format!("{authority}/{tenant}/oauth2/v2.0/token");
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .form(form)
        .send()
        .await
        .map_err(|error| format!("连接授权服务器失败：{error}"))?;
    let status = response.status();
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("解析授权响应失败：{error}"))?;
    if !status.is_success() {
        return Err(token_error(&value));
    }
    TokenSet::from_response(&value, "")
}

/// 授权码换 token（机密客户端可选 client_secret）
pub async fn exchange_code(authority: &str, tenant: &str, client_id: &str, redirect_uri: &str, code: &str, verifier: &str, client_secret: Option<&str>) -> Result<TokenSet, String> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
        ("scope", SCOPES),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    token_request(authority, tenant, &form).await
}

/// 刷新 access_token；微软会轮换 refresh_token，调用方必须回存新值。
/// 机密客户端需带 client_secret。
pub async fn refresh_token(authority: &str, tenant: &str, client_id: &str, refresh: &str, client_secret: Option<&str>) -> Result<TokenSet, String> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh),
        ("scope", SCOPES),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    token_request(authority, tenant, &form).await
}

/// client_credentials 应用令牌（纯后台接入）：无 refresh_token，过期即重取
pub async fn client_credentials_token(authority: &str, tenant: &str, client_id: &str, client_secret: &str) -> Result<TokenSet, String> {
    token_request(
        authority,
        tenant,
        &[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("scope", "https://graph.microsoft.com/.default"),
        ],
    )
    .await
}

/* ---------------- 账户的 access_token 缓存 ---------------- */

/// 内存中的 access_token 缓存条目
#[derive(Debug, Clone)]
pub struct CachedToken {
    pub access_token: String,
    pub expires_at: i64,
}

/// 内存中的 access_token 缓存（App 持有）
pub type TokenCache = Mutex<HashMap<i64, CachedToken>>;

/// 取账户可用的 access_token；必要时用 refresh_token 换新并回存轮换值。
/// 纯后台账户（auth_type = "oauth2_app"）走 client_credentials，无 refresh_token。
pub async fn access_token(app: &Arc<App>, account: &Account) -> Result<String, String> {
    if let Some(cached) = app
        .token_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&account.id)
        .cloned()
    {
        if cached.expires_at > crate::now_ms() {
            return Ok(cached.access_token);
        }
    }

    let set = if account.auth_type == crate::model::AUTH_TYPE_APP_ONLY {
        app_token_set(app, account).await?
    } else {
        refresh_set(app, account).await?
    };
    app.token_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(account.id, CachedToken { access_token: set.access_token.clone(), expires_at: set.expires_at });
    Ok(set.access_token)
}

/// refresh_token 换新（机密客户端带 client_secret），轮换值立即回存
async fn refresh_set(app: &Arc<App>, account: &Account) -> Result<TokenSet, String> {
    let reference = {
        let conn = app.db();
        crate::db::account_password_ref(&conn, account.id).map_err(|error| format!("读取账户失败：{error}"))?
    };
    if reference.is_empty() {
        return Err("该账户尚未授权，请在账户设置里重新授权".into());
    }
    let refresh = app
        .secrets
        .load(&reference)
        .map_err(|error| format!("读取授权凭据失败：{error}"))?;
    // 机密客户端：token 交换与刷新都要带 client_secret
    let client_secret = load_client_secret(app, account.id)?;
    let authority = authority_base();
    let set = refresh_token(
        &authority,
        &account.tenant_id,
        &account.client_id,
        &refresh,
        client_secret.as_deref(),
    )
    .await?;
    // refresh_token 轮换：新值立即回存
    if set.refresh_token != refresh {
        app.secrets
            .save(&reference, &set.refresh_token)
            .map_err(|error| format!("保存授权凭据失败：{error}"))?;
    }
    Ok(set)
}

/// client_credentials 换应用令牌（无 refresh_token，过期重取）
async fn app_token_set(app: &Arc<App>, account: &Account) -> Result<TokenSet, String> {
    let secret = load_client_secret(app, account.id)?
        .ok_or_else(|| "纯后台账户缺少 Client Secret，请更新账户配置".to_string())?;
    let authority = authority_base();
    client_credentials_token(&authority, &account.tenant_id, &account.client_id, &secret).await
}

/// 读取账户的 client_secret（机密客户端/纯后台），未配置时返回 None。
/// 宿主连接会话优先：纯后台账户的 secret 只在内存里，账户行的引用列为空。
fn load_client_secret(app: &Arc<App>, account_id: i64) -> Result<Option<String>, String> {
    if let Some(secret) = crate::hostconn::session_client_secret(app, account_id) {
        return Ok(Some(secret));
    }
    let reference = {
        let conn = app.db();
        crate::db::account_client_secret_ref(&conn, account_id).map_err(|error| format!("读取账户失败：{error}"))?
    };
    if reference.is_empty() {
        return Ok(None);
    }
    let secret = app
        .secrets
        .load(&reference)
        .map_err(|error| format!("读取应用凭据失败：{error}"))?;
    Ok(Some(secret))
}

/* ---------------- 待完成的授权（向导流程） ---------------- */

/// 一次进行中的授权
#[derive(Debug)]
pub struct PendingAuth {
    pub tenant_id: String,
    pub client_id: String,
    /// 机密客户端的 client_secret（仅驻内存，用于回调时的 code 交换）
    pub client_secret: String,
    pub verifier: String,
    pub redirect_uri: String,
    pub created_at: i64,
    /// 回调完成后写入；Some(Err) 表示用户取消或换取失败
    pub result: Option<Result<String, String>>,
}

pub type PendingMap = Mutex<HashMap<String, PendingAuth>>;

/// 发起授权：起回环监听、返回授权 URL（在 tokio 运行时里调用）
pub async fn begin(app: &Arc<App>, tenant_id: &str, client_id: &str, client_secret: &str) -> Result<(String, String), String> {
    let tenant = if tenant_id.trim().is_empty() { "common" } else { tenant_id.trim() };
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("授权监听启动失败：{error}"))?;
    let port = listener.local_addr().map_err(|error| format!("授权监听启动失败：{error}"))?.port();
    let redirect_uri = format!("http://localhost:{port}/");
    let (verifier, _challenge) = pkce_pair();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let url = authorize_url(&authority_base(), tenant, client_id, &redirect_uri, &verifier, &state);

    app.oauth_pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            state.clone(),
            PendingAuth {
                tenant_id: tenant.to_string(),
                client_id: client_id.to_string(),
                client_secret: client_secret.trim().to_string(),
                verifier,
                redirect_uri,
                created_at: crate::now_ms(),
                result: None,
            },
        );

    let app = Arc::clone(app);
    let state_task = state.clone();
    let task_app = Arc::clone(&app);
    app.rt.spawn(async move {
        // 等回调 → 按 pending 里的上下文换 token → 把结果写回 pending
        let outcome = match listen_for_code(listener, &state_task).await {
            Ok(code) => {
                let context = {
                    let map = task_app.oauth_pending.lock().unwrap_or_else(|error| error.into_inner());
                    map.get(&state_task).map(|pending| {
                        (pending.tenant_id.clone(), pending.client_id.clone(), pending.redirect_uri.clone(), pending.verifier.clone(), pending.client_secret.clone())
                    })
                };
                match context {
                    Some((tenant, client_id, redirect_uri, verifier, client_secret)) => {
                        let authority = authority_base();
                        let secret = if client_secret.is_empty() { None } else { Some(client_secret.as_str()) };
                        exchange_code(&authority, &tenant, &client_id, &redirect_uri, &code, &verifier, secret)
                            .await
                            .map(|set| set.refresh_token)
                    }
                    None => Err("授权会话已失效".to_string()),
                }
            }
            Err(error) => Err(error),
        };
        let mut map = task_app.oauth_pending.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(pending) = map.get_mut(&state_task) {
            pending.result = Some(outcome);
        }
    });

    Ok((url, state))
}

/// 查询授权进度（只读，不消费结果；认领走 `claim`）
pub fn status(app: &Arc<App>, state: &str) -> Result<serde_json::Value, String> {
    let mut map = app.oauth_pending.lock().unwrap_or_else(|error| error.into_inner());
    let now = crate::now_ms();
    map.retain(|_, pending| now - pending.created_at < PENDING_TTL_MS);
    let Some(pending) = map.get(state) else {
        return Ok(serde_json::json!({ "status": "expired" }));
    };
    match &pending.result {
        None => Ok(serde_json::json!({ "status": "pending" })),
        Some(Err(error)) => Ok(serde_json::json!({ "status": "failed", "error": error })),
        Some(Ok(refresh_token)) => Ok(serde_json::json!({ "status": "done", "refreshToken": refresh_token })),
    }
}

/// 认领授权结果：取走 refresh_token 并移除会话（account/add、account/update 用）
pub fn claim(app: &Arc<App>, state: &str) -> Result<String, String> {
    let mut map = app.oauth_pending.lock().unwrap_or_else(|error| error.into_inner());
    let now = crate::now_ms();
    map.retain(|_, pending| now - pending.created_at < PENDING_TTL_MS);
    match map.remove(state) {
        Some(pending) => match pending.result {
            Some(Ok(refresh_token)) => Ok(refresh_token),
            Some(Err(error)) => Err(format!("授权失败：{error}")),
            None => Err("授权尚未完成，请等待浏览器授权结束".into()),
        },
        None => Err("授权会话已过期，请重新开始授权".into()),
    }
}

/// 用户取消授权
pub fn cancel(app: &Arc<App>, state: &str) {
    app.oauth_pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(state);
}

/* ---------------- 回环监听 ---------------- */

/// 在已绑定的 listener 上等待回调，返回授权码。
/// `state` 不匹配的请求直接 400 并继续等待。
async fn listen_for_code(listener: TcpListener, expected_state: &str) -> Result<String, String> {
    loop {
        let (stream, _) = tokio::time::timeout(CALLBACK_TIMEOUT, listener.accept())
            .await
            .map_err(|_| "等待授权回调超时，请重新开始授权".to_string())?
            .map_err(|error| format!("授权回调读取失败：{error}"))?;
        match handle_callback(stream, expected_state).await {
            CallbackOutcome::Pending => continue,
            CallbackOutcome::Code(code) => return Ok(code),
            CallbackOutcome::Cancelled(message) => return Err(message),
        }
    }
}

enum CallbackOutcome {
    /// 无关请求（如 favicon）或 state 不匹配，继续等待
    Pending,
    Code(String),
    Cancelled(String),
}

async fn handle_callback(mut stream: TcpStream, expected_state: &str) -> CallbackOutcome {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    // 读取请求头（GET 无 body），最多 16KB 防滥用
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                buffer.extend_from_slice(&chunk[..size]);
                if buffer.windows(4).any(|item| item == b"\r\n\r\n") || buffer.len() > 16 * 1024 {
                    break;
                }
            }
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let request_line = request.lines().next().unwrap_or("");
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };

    if path != "/" {
        let _ = respond(&mut stream, "400 Bad Request", "无效的回调请求").await;
        return CallbackOutcome::Pending;
    }
    let params = parse_query(query);
    let state = params.get("state").map(String::as_str).unwrap_or("");
    if state != expected_state {
        let _ = respond(&mut stream, "400 Bad Request", "state 不匹配，请返回 DBX 邮件重新授权").await;
        return CallbackOutcome::Pending;
    }
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| "用户取消了授权".to_string());
        let _ = respond(&mut stream, "200 OK", "授权未完成，请返回 DBX 邮件重试").await;
        return CallbackOutcome::Cancelled(format!("{error}：{description}"));
    }
    let Some(code) = params.get("code").cloned() else {
        let _ = respond(&mut stream, "400 Bad Request", "回调缺少授权码").await;
        return CallbackOutcome::Pending;
    };
    let _ = respond(&mut stream, "200 OK", "授权完成，请返回 DBX 邮件继续").await;
    CallbackOutcome::Code(code)
}

/// 解析 query string（只处理简单键值，值做 URL 解码）
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(urlencoding::decode(key).unwrap_or_default().into_owned(), urlencoding::decode(value).unwrap_or_default().into_owned());
    }
    map
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) -> std::io::Result<()> {
    let html = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>DBX 邮件授权</title>\
         </head><body style=\"font-family:system-ui;display:flex;align-items:center;justify-content:center;\
         height:100vh;margin:0\"><div style=\"text-align:center\"><h2>{message}</h2>\
         <p style=\"color:#666\">完成授权后可以关闭此页面。</p></div></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 附录 B 的固定向量
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(challenge_for(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn pkce_pair_is_url_safe_and_consistent() {
        let (verifier, challenge) = pkce_pair();
        assert!((43..=128).contains(&verifier.len()));
        assert!(!verifier.contains('+') && !verifier.contains('/') && !verifier.contains('='));
        assert_eq!(challenge_for(&verifier), challenge);
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let (verifier, challenge) = pkce_pair();
        let url = authorize_url("https://login.microsoftonline.com", "common", "client-1", "http://localhost:51111/", &verifier, "state-1");
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
        assert!(url.contains("client_id=client-1"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains(&format!("code_challenge={}", urlencoding::encode(&challenge))));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&*urlencoding::encode("offline_access")));
        assert!(url.contains(&*urlencoding::encode("Mail.Send")));
        assert!(url.contains("state=state-1"));
    }

    #[test]
    fn token_set_parses_response_and_keeps_old_refresh_when_absent() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"access_token":"at-1","refresh_token":"rt-2","expires_in":3600,"token_type":"Bearer"}"#,
        )
        .unwrap();
        let set = TokenSet::from_response(&value, "rt-old").unwrap();
        assert_eq!(set.access_token, "at-1");
        assert_eq!(set.refresh_token, "rt-2");
        assert!(set.expires_at > crate::now_ms());

        let no_refresh: serde_json::Value =
            serde_json::from_str(r#"{"access_token":"at-2","expires_in":60}"#).unwrap();
        let set = TokenSet::from_response(&no_refresh, "rt-old").unwrap();
        assert_eq!(set.refresh_token, "rt-old");
    }

    #[test]
    fn query_parser_decodes_values() {
        let map = parse_query("code=a%2Fb&state=s-1&empty=");
        assert_eq!(map.get("code").unwrap(), "a/b");
        assert_eq!(map.get("state").unwrap(), "s-1");
        assert_eq!(map.get("empty").unwrap(), "");
    }

    #[tokio::test]
    async fn loopback_listener_returns_code_and_rejects_wrong_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected = "state-ok".to_string();
        let task = tokio::spawn(async move { listen_for_code(listener, &expected).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // state 不匹配 → 400 且继续等待
        let mut wrong = TcpStream::connect(address).await.unwrap();
        wrong
            .write_all(b"GET /?code=x&state=bad HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        wrong.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));

        // 正确 state → 返回 code
        let mut ok = TcpStream::connect(address).await.unwrap();
        ok.write_all(b"GET /?code=abc&state=state-ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        ok.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        drop(ok);

        let code = task.await.unwrap().unwrap();
        assert_eq!(code, "abc");
    }

    #[tokio::test]
    async fn loopback_listener_reports_user_cancel() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { listen_for_code(listener, "state-ok").await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /?error=access_denied&error_description=user%20cancel&state=state-ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        drop(stream);
        let error = task.await.unwrap().unwrap_err();
        assert!(error.contains("access_denied"));
    }
}

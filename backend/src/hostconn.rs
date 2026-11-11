//! DBX 宿主连接管理（connection-provider）生命周期。
//!
//! 宿主在连接管理器里创建的邮件连接走这里：`connection/test` 探测、
//! `connection/connect` 幂等建会话并复用既有同步引擎、
//! `connection/disconnect` 停会话但保留本地缓存。
//!
//! 关键语义（详见 docs/IMPL_PLAN_CONNECTION_PROVIDER.zh-CN.md）：
//! - Secret 只出现在生命周期请求的 `connection` 对象里，本模块把密码放进
//!   **内存会话表**（`App::host_sessions`），绝不写入 SecretStore / SQLite；
//!   `App::load_password` 会话优先，同步引擎因此零改动。
//! - `runtime.host` / `runtime.port` 是宿主代理后的 IMAP 最终逻辑端点，
//!   持久化为账户行的 `imap_host/imap_port`（同步引擎只读库）；
//!   SMTP 由 sidecar 直接出网（宿主只代理单一端点）。
//! - Graph 双模式：`auth_mode=browser` 只建授权壳（浏览器授权仍在插件内向导
//!   完成）；`auth_mode=app` 纯后台接入（client_credentials），client secret 走
//!   binding:"secret" → 内存会话，连接后即可直接同步。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::accounts;
use crate::db;
use crate::model::Account;
use crate::sync;
use crate::App;

/// 一条宿主连接的活跃会话。密码与 client secret 只在内存里，随 disconnect 一起丢弃。
pub struct HostSession {
    pub connection_id: String,
    pub account_id: i64,
    /// 宿主 Secret Store 下发的密码（IMAP/SMTP 共用），不落盘
    pub password: String,
    /// 纯后台 Graph 账户的 client secret（binding:"secret" 下发），不落盘
    pub client_secret: String,
}

/// 从生命周期参数解析出的建会话所需信息
#[derive(Debug)]
pub struct Spec {
    pub connection_id: String,
    pub account: Account,
    pub password: String,
    /// 纯后台 Graph 的 client secret（仅存内存会话）
    pub client_secret: String,
}

/* ---------------- 会话表（connect 幂等的核心，独立出来便于单测） ---------------- */

/// 插入会话；同 `connection.id` 已存在时覆盖并返回 true（幂等语义）
pub(crate) fn upsert_session(
    sessions: &mut HashMap<String, HostSession>,
    session: HostSession,
) -> bool {
    sessions.insert(session.connection_id.clone(), session).is_some()
}

/// 移除会话，返回被移除的会话（不存在则 None，disconnect 幂等）
pub(crate) fn remove_session(
    sessions: &mut HashMap<String, HostSession>,
    connection_id: &str,
) -> Option<HostSession> {
    sessions.remove(connection_id)
}

/// 供 `App::load_password` 优先查：宿主连接会话的密码（内存）
pub fn session_password(app: &App, account_id: i64) -> Option<String> {
    let sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions
        .values()
        .find(|session| session.account_id == account_id)
        .map(|session| session.password.clone())
}

/// 供 `oauth::access_token` 纯后台路径优先查：宿主连接会话的 client_secret（内存）
pub fn session_client_secret(app: &App, account_id: i64) -> Option<String> {
    let sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions
        .values()
        .find(|session| session.account_id == account_id)
        .map(|session| session.client_secret.clone())
        .filter(|secret| !secret.is_empty())
}

/* ---------------- 生命周期参数解析（纯函数，可单测） ---------------- */

fn obj<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 数字字段：接受 number，容忍字符串数字；非法/缺失返回 0
fn number(value: &Value, key: &str) -> i64 {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// binding:"config" 字段（external_config.<key>，缺失/形状不对时当缺省）
fn config_text(connection: &Value, key: &str) -> String {
    text(obj(connection, "external_config"), key)
}

fn config_number(connection: &Value, key: &str) -> i64 {
    number(obj(connection, "external_config"), key)
}

/// binding:"secret" 字段（connection_secrets.<key>），回退标准 password 字段
fn secret_text(connection: &Value, key: &str) -> String {
    let value = text(obj(connection, "connection_secrets"), key);
    if value.is_empty() {
        text(connection, "password")
    } else {
        value
    }
}

/// 生命周期参数 → 账户参数（与 `accounts::account_from_params` 同形的 Account）
pub fn spec_from_lifecycle(params: &Value) -> Result<Spec, String> {
    let connection = obj(params, "connection");
    if !connection.is_object() {
        return Err("缺少 connection 对象".into());
    }
    let connection_id = text(connection, "id");
    if connection_id.is_empty() {
        return Err("缺少 connection.id".into());
    }

    let username = text(connection, "username");

    // 邮箱是本地账户行的唯一标识；缺省时接受「用户名即邮箱」的常见形态
    let email = {
        let value = config_text(connection, "email");
        if value.is_empty() {
            let fallback = username.clone();
            if fallback.contains('@') {
                fallback
            } else {
                return Err("请填写邮箱地址".into());
            }
        } else {
            value
        }
    };

    // kind=graph：Microsoft 365 / Outlook.com（Graph）账户。
    // browser：宿主表单只建账户壳，浏览器交互授权仍在插件内向导 /「重新授权」里完成；
    // app：纯后台接入（client_credentials），secret 走 binding:"secret"（内存会话），
    //      连接后即可直接同步，无需任何浏览器交互。
    let kind = {
        let value = config_text(connection, "kind");
        if value.is_empty() {
            "imap".into()
        } else {
            value.to_lowercase()
        }
    };
    if kind == "graph" {
        let client_id = config_text(connection, "client_id");
        if client_id.is_empty() {
            return Err("请填写 Azure 应用的 Client ID（授权在插件内完成）".into());
        }
        let tenant_id = {
            let value = config_text(connection, "tenant_id");
            if value.is_empty() {
                "common".into()
            } else {
                value
            }
        };
        let auth_mode = {
            let value = config_text(connection, "auth_mode");
            if value.is_empty() {
                "browser".into()
            } else {
                value.to_lowercase()
            }
        };
        let (auth_type, client_secret, user_id, sync_enabled) = if auth_mode == "app" {
            let secret = secret_text(connection, "client_secret");
            if secret.is_empty() {
                return Err("纯后台接入需要填写 Azure 应用的 Client Secret".into());
            }
            // 应用令牌无 /me 语义：留空时按「邮箱即 UPN」的常见形态兜底
            let user = {
                let value = config_text(connection, "user_id");
                if value.is_empty() {
                    email.clone()
                } else {
                    value
                }
            };
            (crate::model::AUTH_TYPE_APP_ONLY.to_string(), secret, user, true)
        } else {
            ("oauth2".to_string(), String::new(), String::new(), false)
        };
        let account = Account {
            id: 0,
            display_name: {
                let value = text(connection, "name");
                if value.is_empty() {
                    email.split('@').next().unwrap_or("").to_string()
                } else {
                    value
                }
            },
            email: email.clone(),
            username: if username.is_empty() {
                email.clone()
            } else {
                username
            },
            provider: "graph".into(),
            auth_type,
            tenant_id,
            client_id,
            user_id,
            // Graph 不走 IMAP/SMTP 传输，服务器字段仅作占位（与向导一致）
            imap_host: "graph.microsoft.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "graph.microsoft.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            signature: String::new(),
            color: "#e8660c".into(),
            // browser：未授权前不进周期巡检，避免后台报错；授权流程按需启用。
            // app：纯后台无需浏览器授权，连接后即可周期同步。
            sync_enabled,
            sync_interval: 5,
            last_sync_at: 0,
            last_error: String::new(),
            host_bound: true,
            host_connection_id: connection_id.clone(),
        };
        return Ok(Spec {
            connection_id,
            account,
            password: String::new(),
            client_secret,
        });
    }

    // IMAP 端点：runtime.*（宿主代理后的最终逻辑端点）优先，connection 兜底
    let runtime = obj(params, "runtime");
    let imap_host = {
        let value = text(runtime, "host");
        if value.is_empty() {
            text(connection, "host")
        } else {
            value
        }
    };
    if imap_host.is_empty() {
        return Err("缺少 IMAP 服务器地址".into());
    }
    let imap_port = {
        let value = number(runtime, "port");
        if value > 0 {
            value
        } else {
            number(connection, "port")
        }
        .clamp(1, 65535)
    } as u16;

    let username = text(connection, "username");

    if username.is_empty() {
        return Err("请填写登录用户名".into());
    }
    let password = secret_text(connection, "password");
    if password.is_empty() {
        return Err("缺少密码".into());
    }

    let smtp_host = config_text(connection, "smtp_host");
    if smtp_host.is_empty() {
        return Err("请填写 SMTP 服务器地址".into());
    }
    let smtp_port = {
        let value = config_number(connection, "smtp_port");
        if value > 0 {
            value
        } else {
            465
        }
        .clamp(1, 65535)
    } as u16;

    let account = Account {
        id: 0,
        display_name: {
            let value = text(connection, "name");
            if value.is_empty() {
                email.split('@').next().unwrap_or("").to_string()
            } else {
                value
            }
        },
        email,
        username,
        provider: "imap".into(),
        auth_type: "password".into(),
        tenant_id: String::new(),
        client_id: String::new(),
        user_id: String::new(),
        imap_host,
        imap_port,
        imap_security: accounts::normalize_security(&config_text(connection, "security")),
        smtp_host,
        smtp_port,
        smtp_security: accounts::normalize_security(&config_text(connection, "smtp_security")),
        signature: String::new(),
        color: "#e8660c".into(),
        // 周期巡检不感知内存会话，host 账户一律关掉自动同步；
        // 会话存活期间手动「立即同步」照常可用
        sync_enabled: false,
        sync_interval: 5,
        last_sync_at: 0,
        last_error: String::new(),
        host_bound: true,
        host_connection_id: connection_id.clone(),
    };

    Ok(Spec {
        connection_id,
        account,
        password,
        client_secret: String::new(),
    })
}

/* ---------------- 三个生命周期方法 ---------------- */

/// `connection/test`：复用 `sync::test_account`（IMAP 登录 + LIST + INBOX
/// STATUS + SMTP 探测）。按契约以 `{success, message}` 表达结果，
/// 即使探测失败也不抛 RPC error。
pub fn test(app: Arc<App>, params: &Value) -> Value {
    let spec = match spec_from_lifecycle(params) {
        Ok(spec) => spec,
        Err(error) => return json!({ "success": false, "message": error }),
    };
    // Graph 无 IMAP 端点可探测
    if spec.account.provider == "graph" {
        // 纯后台：换一次应用令牌即验证（凭据只比对，不回显）
        if spec.account.auth_type == crate::model::AUTH_TYPE_APP_ONLY {
            let fetched = app.rt.block_on(crate::oauth::client_credentials_token(
                &crate::oauth::authority_base(),
                &spec.account.tenant_id,
                &spec.account.client_id,
                &spec.client_secret,
            ));
            return match fetched {
                Ok(_) => json!({
                    "success": true,
                    "message": format!("应用令牌获取成功；纯后台访问 {}", spec.account.user_id),
                }),
                Err(error) => json!({ "success": false, "message": format!("连接失败：{error}") }),
            };
        }
        // 浏览器授权壳：授权与首拉都在插件内完成
        return json!({
            "success": true,
            "message": "Graph 账户无需连接测试；请在插件内完成 Microsoft 授权",
        });
    }
    match app.rt.block_on(sync::test_account(&spec.account, &spec.password)) {
        Ok(lines) => json!({ "success": true, "message": lines.join("；") }),
        Err(error) => json!({ "success": false, "message": format!("连接失败：{error}") }),
    }
}

/// `connection/connect`：幂等。以 `connection.id` 为 key 建立会话，
/// 复用文件夹发现与同步引擎；密码只进内存会话表。
pub fn connect(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let spec = spec_from_lifecycle(params)?;

    // 幂等：同 connection.id 已有活跃会话且账户行还在 → 直接成功返回
    if let Some(account_id) = {
        let sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(&spec.connection_id).map(|s| s.account_id)
    } {
        let existing = {
            let conn = app.db();
            db::account_get(&conn, account_id)
        };
        if let Ok(Some(row)) = existing {
            if row.host_bound {
                // 幂等返回前重应用 lifecycle spec：宿主编辑表单后会对同一连接重发
                // connect，端点/用户名/Graph 配置必须跟着更新；会话凭据随后刷新
                let account_id = ensure_account_row(&app, &spec)?;
                {
                    let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
                    upsert_session(
                        &mut sessions,
                        HostSession {
                            connection_id: spec.connection_id.clone(),
                            account_id,
                            password: spec.password.clone(),
                            client_secret: spec.client_secret.clone(),
                        },
                    );
                }
                return Ok(json!({
                    "success": true,
                    "message": format!("已连接 {}", row.email),
                }));
            }
        }
        // 账户行被用户删掉或解除绑定：清掉旧会话，走正常建立流程
        let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
        remove_session(&mut sessions, &spec.connection_id);
    }

    let account_id = ensure_account_row(&app, &spec)?;

    // 先入会话表：discover / sync 取密码走 load_password → 内存会话优先；
    // 纯后台 Graph 换令牌走 load_client_secret → 同样内存会话优先
    {
        let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
        upsert_session(
            &mut sessions,
            HostSession {
                connection_id: spec.connection_id.clone(),
                account_id,
                password: spec.password.clone(),
                client_secret: spec.client_secret.clone(),
            },
        );
    }

    // Graph 无 IMAP 端点可探测
    if spec.account.provider == "graph" {
        if spec.account.auth_type == crate::model::AUTH_TYPE_APP_ONLY {
            // 纯后台：换一次应用令牌作会话验证；失败回滚会话（账户行保留便于重试）
            let fetched = app.rt.block_on(crate::oauth::client_credentials_token(
                &crate::oauth::authority_base(),
                &spec.account.tenant_id,
                &spec.account.client_id,
                &spec.client_secret,
            ));
            if let Err(error) = fetched {
                let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
                remove_session(&mut sessions, &spec.connection_id);
                return Err(format!("连接失败：{error}"));
            }
            // 后台首拉，不阻塞 connect 返回；secret 取自内存会话
            {
                let app = Arc::clone(&app);
                std::thread::spawn(move || {
                    let _ = app.rt.block_on(sync::sync_account(
                        Arc::clone(&app),
                        account_id,
                        sync::SyncOptions::default(),
                    ));
                });
            }
            return Ok(json!({
                "success": true,
                "message": format!("已连接 {}（纯后台，访问 {}）", spec.account.email, spec.account.user_id),
            }));
        }
        // 浏览器授权壳：文件夹引导与首拉在插件内授权后进行
        return Ok(json!({
            "success": true,
            "message": format!("已连接 {}；请在插件内完成 Microsoft 授权", spec.account.email),
        }));
    }

    // 文件夹发现当作「会话可用」的验证；失败则回滚会话（账户行保留便于重试）
    if let Err(error) = accounts::discover(Arc::clone(&app), account_id) {
        let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
        remove_session(&mut sessions, &spec.connection_id);
        return Err(format!("连接失败：{error}"));
    }

    // 后台首拉，不阻塞 connect 返回；密码取自内存会话
    {
        let app = Arc::clone(&app);
        std::thread::spawn(move || {
            let _ = app.rt.block_on(sync::sync_account(
                Arc::clone(&app),
                account_id,
                sync::SyncOptions::default(),
            ));
        });
    }

    Ok(json!({
        "success": true,
        "message": format!("已连接 {}", spec.account.email),
    }))
}

/// 确保账户行存在并绑定为 host 来源：按 host_connection_id → 按同 email →
/// 新建。同一邮箱与向导账户共享同一行（本地缓存也只有一份）。
fn ensure_account_row(app: &App, spec: &Spec) -> Result<i64, String> {
    // 先按宿主 connection.id 找，再按邮箱兜底；两查共用同一把 db 锁
    // （Mutex 不可重入，二次 app.db() 会死锁）
    let existing = {
        let conn = app.db();
        match db::account_by_host(&conn, &spec.connection_id) {
            Ok(row) => row,
            Err(error) => return Err(format!("读取账户失败：{error}")),
        }
        .or_else(|| {
            db::account_by_email(&conn, &spec.account.email)
                .map_err(|error| format!("读取账户失败：{error}"))
                .ok()
                .flatten()
        })
    };
    let conn = app.db();
    match existing {
        Some(row) => {
            // provider 必须一致：IMAP 连接不得接管 Graph 账户，反之亦然
            if row.provider == "graph" && spec.account.provider != "graph" {
                return Err(
                    "该邮箱已作为 Microsoft 365（Graph）账户存在，请在插件内用向导管理".into(),
                );
            }
            if row.provider != "graph" && spec.account.provider == "graph" {
                return Err(
                    "该邮箱已作为 IMAP 账户存在，无法改为 Microsoft 365（Graph）连接".into(),
                );
            }
            if row.provider == "graph" {
                if spec.account.auth_type == crate::model::AUTH_TYPE_APP_ONLY {
                    // 纯后台连接：宿主表单显式给出接入方式，落库 auth_type/user_id/tenant/client
                    db::account_apply_host(&conn, row.id, &spec.account, &spec.connection_id)
                        .map_err(|error| format!("保存账户失败：{error}"))?;
                } else {
                    // 浏览器授权壳：保留原 tenant/client 与授权状态，仅绑定连接标记
                    // （不动 OAuth 凭据），保证 byConnection 查得到该账户
                    db::account_bind_host(&conn, row.id, &spec.connection_id)
                        .map_err(|error| format!("保存账户失败：{error}"))?;
                }
                return Ok(row.id);
            }
            db::account_apply_host(&conn, row.id, &spec.account, &spec.connection_id)
                .map_err(|error| format!("保存账户失败：{error}"))?;
            Ok(row.id)
        }
        None => {
            let id = db::account_insert(&conn, &spec.account, "")
                .map_err(|error| format!("保存账户失败：{error}"))?;
            db::account_apply_host(&conn, id, &spec.account, &spec.connection_id)
                .map_err(|error| format!("保存账户失败：{error}"))?;
            Ok(id)
        }
    }
}

/// `connection/disconnect`：幂等。移除内存会话（密码随之丢弃）并复位 host
/// 标记；账户行与全部本地缓存保留（离线仍可读）。
pub fn disconnect(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let connection_id = text(obj(params, "connection"), "id");
    if connection_id.is_empty() {
        return Err("缺少 connection.id".into());
    }
    let removed = {
        let mut sessions = app.host_sessions.lock().unwrap_or_else(|e| e.into_inner());
        remove_session(&mut sessions, &connection_id)
    };
    if let Some(session) = removed {
        let conn = app.db();
        db::account_unbind_host(&conn, session.account_id)
            .map_err(|error| format!("保存账户失败：{error}"))?;
    }
    Ok(json!({ "success": true }))
}

/* ---------------- 单测 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造一份与宿主 `plugin_connection_params` 同形的生命周期参数
    fn lifecycle(connection_id: &str, overrides: Value) -> Value {
        let mut params = json!({
            "provider": { "id": "com.mjj3610.mail.connection", "databaseType": "mail" },
            "connection": {
                "id": connection_id,
                "name": "工作邮箱",
                "db_type": "plugin",
                "host": "imap.example.com",
                "port": 1993,
                "username": "alice@example.com",
                "password": "",
                "external_config": {
                    "email": "alice@example.com",
                    "security": "tls",
                    "smtp_host": "smtp.example.com",
                    "smtp_port": 1465,
                    "smtp_security": "starttls"
                },
                "connection_secrets": { "password": "app-password" }
            },
            "runtime": { "host": "127.0.0.1", "port": 44915 }
        });
        if let Some(patch) = overrides.as_object() {
            for (key, value) in patch {
                params[key] = value.clone();
            }
        }
        params
    }

    #[test]
    fn spec_maps_runtime_endpoint_and_config_fields() {
        let spec = spec_from_lifecycle(&lifecycle("conn-1", Value::Null)).expect("解析");
        // IMAP 走 runtime 代理端点
        assert_eq!(spec.account.imap_host, "127.0.0.1");
        assert_eq!(spec.account.imap_port, 44915);
        assert_eq!(spec.account.imap_security, "tls");
        // SMTP 来自 external_config
        assert_eq!(spec.account.smtp_host, "smtp.example.com");
        assert_eq!(spec.account.smtp_port, 1465);
        assert_eq!(spec.account.smtp_security, "starttls");
        assert_eq!(spec.account.email, "alice@example.com");
        assert_eq!(spec.account.display_name, "工作邮箱");
        assert_eq!(spec.account.username, "alice@example.com");
        assert_eq!(spec.account.provider, "imap");
        assert!(!spec.account.sync_enabled);
        assert!(spec.account.host_bound);
        assert_eq!(spec.account.host_connection_id, "conn-1");
        // 密码来自 secret 落点，且只进内存 Spec
        assert_eq!(spec.password, "app-password");
    }

    #[test]
    fn graph_kind_builds_oauth_account_without_password() {
        let params = json!({
            "connection": {
                "id": "conn-graph-1",
                "name": "工作账户",
                "external_config": {
                    "kind": "graph",
                    "email": "bob@outlook.com",
                    "client_id": "11111111-2222-3333-4444-555555555555",
                },
                "connection_secrets": {},
            },
            "runtime": { "host": "127.0.0.1", "port": 44915 }
        });
        let spec = spec_from_lifecycle(&params).expect("解析");
        assert_eq!(spec.account.provider, "graph");
        assert_eq!(spec.account.auth_type, "oauth2");
        assert_eq!(spec.account.tenant_id, "common");
        assert_eq!(
            spec.account.client_id,
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(spec.account.email, "bob@outlook.com");
        assert_eq!(spec.account.username, "bob@outlook.com");
        assert_eq!(spec.account.host_connection_id, "conn-graph-1");
        assert!(spec.account.host_bound);
        assert!(!spec.account.sync_enabled);
        // 无密码：授权在插件内完成，Spec 不携带凭据
        assert!(spec.password.is_empty());
        assert!(spec.client_secret.is_empty());
    }

    #[test]
    fn graph_kind_requires_client_id() {
        let params = json!({
            "connection": {
                "id": "conn-graph-2",
                "external_config": { "kind": "graph", "email": "bob@outlook.com" },
            },
        });
        let error = spec_from_lifecycle(&params).unwrap_err();
        assert!(error.contains("Client ID"));
    }

    #[test]
    fn graph_app_mode_builds_app_only_account_with_secret() {
        let params = json!({
            "connection": {
                "id": "conn-graph-app-1",
                "name": "工作账户",
                "external_config": {
                    "kind": "graph",
                    "auth_mode": "app",
                    "email": "bob@outlook.com",
                    "client_id": "11111111-2222-3333-4444-555555555555",
                    "tenant_id": "11111111-2222-3333-4444-999999999999",
                    "user_id": "bob@outlook.com",
                },
                "connection_secrets": { "client_secret": "client-secret-value" },
            },
        });
        let spec = spec_from_lifecycle(&params).expect("解析");
        assert_eq!(spec.account.provider, "graph");
        assert_eq!(spec.account.auth_type, crate::model::AUTH_TYPE_APP_ONLY);
        assert_eq!(spec.account.user_id, "bob@outlook.com");
        assert_eq!(spec.account.tenant_id, "11111111-2222-3333-4444-999999999999");
        assert!(spec.account.sync_enabled, "纯后台连接连上即可周期同步");
        // 密码通道保持为空；client_secret 走独立字段
        assert!(spec.password.is_empty());
        assert_eq!(spec.client_secret, "client-secret-value");
    }

    #[test]
    fn graph_app_mode_user_id_falls_back_to_email() {
        let params = json!({
            "connection": {
                "id": "conn-graph-app-2",
                "external_config": {
                    "kind": "graph",
                    "auth_mode": "app",
                    "email": "bob@outlook.com",
                    "client_id": "11111111-2222-3333-4444-555555555555",
                },
                "connection_secrets": { "client_secret": "client-secret-value" },
            },
        });
        let spec = spec_from_lifecycle(&params).expect("解析");
        assert_eq!(spec.account.user_id, "bob@outlook.com");
        assert_eq!(spec.account.tenant_id, "common");
        assert_eq!(spec.account.auth_type, crate::model::AUTH_TYPE_APP_ONLY);
    }

    #[test]
    fn graph_app_mode_requires_client_secret() {
        let params = json!({
            "connection": {
                "id": "conn-graph-app-3",
                "external_config": {
                    "kind": "graph",
                    "auth_mode": "app",
                    "email": "bob@outlook.com",
                    "client_id": "11111111-2222-3333-4444-555555555555",
                },
            },
        });
        let error = spec_from_lifecycle(&params).unwrap_err();
        assert!(error.contains("Client Secret"));
    }

    #[test]
    fn spec_applies_defaults_and_fallbacks() {
        let params = json!({
            "connection": {
                "id": "conn-2",
                "host": "imap.example.com",
                "port": 9993,
                "username": "bob@example.com",
                "password": "plain-fallback",
                "external_config": { "smtp_host": "smtp.example.com" }
            },
            "runtime": {}
        });
        let spec = spec_from_lifecycle(&params).expect("解析");
        // runtime 缺失 → 回退 connection.host/port；smtp 端口/加密走默认
        assert_eq!(spec.account.imap_host, "imap.example.com");
        assert_eq!(spec.account.imap_port, 9993);
        assert_eq!(spec.account.smtp_port, 465);
        assert_eq!(spec.account.smtp_security, "tls");
        // secret 落点缺失 → 回退标准 password 字段
        assert_eq!(spec.password, "plain-fallback");
        // email 缺省回退「用户名即邮箱」；display_name 回退 local-part
        assert_eq!(spec.account.email, "bob@example.com");
        assert_eq!(spec.account.display_name, "bob");
    }

    #[test]
    fn spec_rejects_incomplete_lifecycle() {
        let cases = [
            // 缺 connection 对象
            json!({ "runtime": { "host": "h", "port": 1 } }),
            // 缺 connection.id
            json!({ "connection": { "host": "h", "port": 1, "username": "u", "password": "p", "external_config": { "email": "a@b.c", "smtp_host": "s" } } }),
            // 缺 IMAP 端点（runtime 与 connection 都没有 host）
            json!({ "connection": { "id": "x", "username": "u", "password": "p", "external_config": { "email": "a@b.c", "smtp_host": "s" } } }),
            // 缺用户名
            json!({ "connection": { "id": "x", "host": "h", "port": 1, "password": "p", "external_config": { "email": "a@b.c", "smtp_host": "s" } } }),
            // 缺密码
            json!({ "connection": { "id": "x", "host": "h", "port": 1, "username": "u", "external_config": { "email": "a@b.c", "smtp_host": "s" } } }),
            // 缺邮箱（用户名也不是邮箱形态）
            json!({ "connection": { "id": "x", "host": "h", "port": 1, "username": "alice", "password": "p", "external_config": { "smtp_host": "s" } } }),
            // 缺 SMTP 服务器
            json!({ "connection": { "id": "x", "host": "h", "port": 1, "username": "u", "password": "p", "external_config": { "email": "a@b.c" } } }),
        ];
        for params in cases {
            let error = spec_from_lifecycle(&params).expect_err("应拒绝不完整参数");
            assert!(!error.contains("app-password") && !error.contains("plain-fallback"));
        }
    }

    /// test 失败路径：连不存在的端口要得到明确错误，且错误里没有密码
    #[test]
    fn probe_failure_reports_error_without_secrets() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let account = Account {
            id: 0,
            email: "alice@example.com".into(),
            display_name: "alice".into(),
            username: "alice@example.com".into(),
            provider: "imap".into(),
            auth_type: "password".into(),
            tenant_id: String::new(),
            client_id: String::new(),
            user_id: String::new(),
            imap_host: "127.0.0.1".into(),
            imap_port: 1, // 保留端口，本机必然拒绝
            imap_security: "none".into(),
            smtp_host: "127.0.0.1".into(),
            smtp_port: 1,
            smtp_security: "none".into(),
            signature: String::new(),
            color: "#e8660c".into(),
            sync_enabled: false,
            sync_interval: 5,
            last_sync_at: 0,
            last_error: String::new(),
            host_bound: true,
            host_connection_id: "conn-test".into(),
        };
        let error = runtime
            .block_on(sync::test_account(&account, "app-password"))
            .expect_err("不可达端口应失败");
        assert!(!error.contains("app-password"));
    }

    #[test]
    fn session_table_is_idempotent() {
        let mut sessions = HashMap::new();
        let make = |connection_id: &str, account_id: i64, password: &str| HostSession {
            connection_id: connection_id.to_string(),
            account_id,
            password: password.to_string(),
            client_secret: String::new(),
        };
        // 首次 connect：新会话
        assert!(!upsert_session(&mut sessions, make("conn-1", 7, "pw-1")));
        assert_eq!(sessions.len(), 1);
        // 重复 connect（同 connection.id）：覆盖而非重复建
        assert!(upsert_session(&mut sessions, make("conn-1", 7, "pw-2")));
        assert_eq!(sessions.len(), 1);
        // 同一账户被两条连接持有：两条会话并存
        assert!(!upsert_session(&mut sessions, make("conn-2", 7, "pw-1")));
        assert_eq!(sessions.len(), 2);
        // 按账户查密码命中任一活跃会话
        let found = sessions
            .values()
            .find(|session| session.account_id == 7)
            .map(|session| session.password.clone());
        assert!(found.is_some());
        // disconnect 幂等：第二次移除返回 None
        assert!(remove_session(&mut sessions, "conn-1").is_some());
        assert!(remove_session(&mut sessions, "conn-1").is_none());
        assert!(remove_session(&mut sessions, "unknown").is_none());
    }
}

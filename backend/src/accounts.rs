//! 账户管理：预设服务商、新增/修改/删除、连接检测、文件夹发现。

use std::sync::Arc;

use rusqlite::params;
use serde_json::{json, Value};

use crate::db;
use crate::imap::{ImapClient, Login};
use crate::model::{Account, ProviderPreset};
use crate::sync;
use crate::App;

/// 账户颜色（按加入顺序循环，和企鹅邮件一样用彩色圆点区分账户）
const COLORS: [&str; 8] = [
    "#e8660c", "#3584e4", "#2ec27e", "#9141ac", "#e01b24", "#e5a50a", "#2190a4", "#c061cb",
];

/// 常见服务商预设
pub fn presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            key: "gmail".into(),
            label: "Gmail".into(),
            provider: "imap".into(),
            domains: vec!["gmail.com".into(), "googlemail.com".into()],
            imap_host: "imap.gmail.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            hint: "需要在 Google 账号中开启两步验证并生成「应用专用密码」".into(),
        },
        ProviderPreset {
            key: "msgraph".into(),
            label: "Microsoft 365 / Outlook.com（Graph）".into(),
            provider: "graph".into(),
            domains: vec![
                "outlook.com".into(),
                "hotmail.com".into(),
                "live.com".into(),
                "microsoft.com".into(),
                "office365.com".into(),
            ],
            imap_host: "".into(),
            imap_port: 0,
            imap_security: "tls".into(),
            smtp_host: "".into(),
            smtp_port: 0,
            smtp_security: "tls".into(),
            hint: "需要先在 Azure Portal 注册应用并填入 Client ID，然后在浏览器中完成授权（企业租户可能需要管理员同意）".into(),
        },
        ProviderPreset {
            key: "outlook".into(),
            label: "Outlook / Microsoft 365".into(),
            provider: "imap".into(),
            domains: vec!["outlook.com".into(), "hotmail.com".into(), "live.com".into(), "office365.com".into()],
            imap_host: "outlook.office365.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.office365.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            hint: "微软已停用基本认证，此预设仅适用于少数仍开启密码登录的场景；推荐改用 Microsoft 365（Graph）预设".into(),
        },
        ProviderPreset {
            key: "qq".into(),
            label: "QQ 邮箱".into(),
            provider: "imap".into(),
            domains: vec!["qq.com".into(), "foxmail.com".into()],
            imap_host: "imap.qq.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.qq.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            hint: "需要在 QQ 邮箱设置中开启 IMAP/SMTP 并使用「授权码」作为密码".into(),
        },
        ProviderPreset {
            key: "exmail".into(),
            label: "腾讯企业邮".into(),
            provider: "imap".into(),
            domains: vec!["exmail.qq.com".into()],
            imap_host: "imap.exmail.qq.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.exmail.qq.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            hint: "企业邮通常要求使用客户端专用密码".into(),
        },
        ProviderPreset {
            key: "163".into(),
            label: "网易 163 / 126".into(),
            provider: "imap".into(),
            domains: vec!["163.com".into(), "126.com".into(), "yeah.net".into()],
            imap_host: "imap.163.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.163.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            hint: "需要在网页版开启 IMAP/SMTP，并使用客户端授权码".into(),
        },
        ProviderPreset {
            key: "aliyun".into(),
            label: "阿里云邮箱".into(),
            provider: "imap".into(),
            domains: vec!["aliyun.com".into(), "aliyun.cn".into()],
            imap_host: "imap.aliyun.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.aliyun.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            hint: "使用邮箱登录密码或客户端专用密码".into(),
        },
        ProviderPreset {
            key: "icloud".into(),
            label: "iCloud 邮箱".into(),
            provider: "imap".into(),
            domains: vec!["icloud.com".into(), "me.com".into(), "mac.com".into()],
            imap_host: "imap.mail.me.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.me.com".into(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            hint: "需要 Apple ID 的 App 专用密码".into(),
        },
        ProviderPreset {
            key: "yahoo".into(),
            label: "Yahoo Mail".into(),
            provider: "imap".into(),
            domains: vec!["yahoo.com".into()],
            imap_host: "imap.mail.yahoo.com".into(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: "smtp.mail.yahoo.com".into(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            hint: "需要生成 App Password".into(),
        },
    ]
}

/// 按邮箱域名匹配预设
pub fn preset_for(email: &str) -> Option<ProviderPreset> {
    let domain = email.rsplit('@').next().unwrap_or("").to_lowercase();
    presets().into_iter().find(|preset| {
        preset
            .domains
            .iter()
            .any(|value| domain == *value || domain.ends_with(&format!(".{value}")))
    })
}

/// 从请求参数构造账户（不含密码）
pub fn account_from_params(params: &Value, id: i64) -> Result<Account, String> {
    let email = crate::param_str(params, "email").trim().to_string();
    if email.is_empty() || !email.contains('@') {
        return Err("请填写完整的邮箱地址".into());
    }
    let mut imap_host = crate::param_str(params, "imapHost").trim().to_string();
    let mut smtp_host = crate::param_str(params, "smtpHost").trim().to_string();
    let preset = preset_for(&email);
    // Graph 账户不走 IMAP/SMTP，无需服务器地址（后续会填占位常量）
    let provider_param = crate::param_str(params, "provider");
    if provider_param != "graph" && (imap_host.is_empty() || smtp_host.is_empty()) && preset.is_none() {
        return Err("请填写 IMAP 与 SMTP 服务器地址".into());
    }
    if let Some(preset) = &preset {
        if imap_host.is_empty() {
            imap_host = preset.imap_host.clone();
        }
        if smtp_host.is_empty() {
            smtp_host = preset.smtp_host.clone();
        }
    }
    let provider = {
        // 显式 provider 参数优先（Graph 向导不依赖域名预设匹配）
        if provider_param == "graph" || provider_param == "imap" {
            provider_param
        } else if provider_param.is_empty() {
            preset.as_ref().map(|item| item.provider.clone()).unwrap_or_else(|| "imap".into())
        } else {
            provider_param
        }
    };
    let tenant_id = crate::param_str(params, "tenantId").trim().to_string();
    let client_id = crate::param_str(params, "clientId").trim().to_string();
    // Graph 账户不走 IMAP/SMTP 传输，服务器字段仅作占位（数据库 NOT NULL）
    if provider == "graph" {
        imap_host = "graph.microsoft.com".into();
        smtp_host = "graph.microsoft.com".into();
    }
    let username = {
        let value = crate::param_str(params, "username").trim().to_string();
        if value.is_empty() {
            email.clone()
        } else {
            value
        }
    };
    let imap_port = crate::param_int(
        params,
        "imapPort",
        preset.as_ref().map(|item| item.imap_port as i64).unwrap_or(993),
    )
    .clamp(1, 65535) as u16;
    let smtp_port = crate::param_int(
        params,
        "smtpPort",
        preset.as_ref().map(|item| item.smtp_port as i64).unwrap_or(465),
    )
    .clamp(1, 65535) as u16;
    let imap_security = normalize_security(&{
        let value = crate::param_str(params, "imapSecurity");
        if value.is_empty() {
            preset.as_ref().map(|item| item.imap_security.clone()).unwrap_or_else(|| "tls".into())
        } else {
            value
        }
    });
    let smtp_security = normalize_security(&{
        let value = crate::param_str(params, "smtpSecurity");
        if value.is_empty() {
            preset.as_ref().map(|item| item.smtp_security.clone()).unwrap_or_else(|| "tls".into())
        } else {
            value
        }
    });
    let display_name = {
        let value = crate::param_str(params, "displayName").trim().to_string();
        if value.is_empty() {
            email.split('@').next().unwrap_or("").to_string()
        } else {
            value
        }
    };
    Ok(Account {
        id,
        email,
        display_name,
        username,
        provider: provider.clone(),
        auth_type: if provider == "graph" {
            // 纯后台接入（client_credentials）：无 oauthState 且给了 clientSecret + userId
            let is_app_only = crate::param_str(params, "oauthState").is_empty()
                && !crate::param_str(params, "clientSecret").trim().is_empty()
                && !crate::param_str(params, "userId").trim().is_empty();
            if is_app_only { crate::model::AUTH_TYPE_APP_ONLY.into() } else { "oauth2".into() }
        } else {
            "password".into()
        },
        tenant_id,
        client_id,
        user_id: crate::param_str(params, "userId").trim().to_string(),
        imap_host,
        imap_port,
        imap_security,
        smtp_host,
        smtp_port,
        smtp_security,
        signature: crate::param_str(params, "signature"),
        color: {
            let value = crate::param_str(params, "color");
            if value.is_empty() {
                COLORS[0].to_string()
            } else {
                value
            }
        },
        sync_enabled: params
            .get("syncEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        sync_interval: crate::param_int(params, "syncInterval", 5).clamp(0, 240),
        last_sync_at: 0,
        last_error: String::new(),
        host_bound: false,
        host_connection_id: String::new(),
    })
}

/// 加密方式归一化（hostconn 的宿主连接解析也用它）
pub(crate) fn normalize_security(value: &str) -> String {
    match value.to_lowercase().as_str() {
        "starttls" => "starttls".into(),
        "none" | "plain" => "none".into(),
        _ => "tls".into(),
    }
}

/// 新增账户：校验 → 保存密码/授权凭据 → 写库 → 尝试发现文件夹
pub fn add(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let account = account_from_params(params, 0)?;
    // Graph 账户：凭据是 OAuth refresh_token（从 pending 会话认领）
    let client_secret_param = crate::param_str(params, "clientSecret");
    let credential = if account.provider == "graph" {
        if account.client_id.trim().is_empty() {
            return Err("请填写 Azure 应用的 Client ID".into());
        }
        if account.tenant_id.trim().is_empty() {
            return Err("请填写租户 ID（不确定时填 common）".into());
        }
        let state = crate::param_str(params, "oauthState");
        if state.is_empty() {
            // 纯后台接入（client_credentials）：无浏览器交互，直接访问 userId 的邮箱
            if client_secret_param.trim().is_empty() {
                return Err("请先完成浏览器授权，或填入 Client Secret 以纯后台接入".into());
            }
            if account.user_id.trim().is_empty() {
                return Err("纯后台接入需要填写目标邮箱（userId）".into());
            }
            String::new()
        } else {
            let refresh_token = crate::oauth::claim(&app, &state)?;
            refresh_token
        }
    } else {
        let password = crate::param_str(params, "password");
        if password.trim().is_empty() {
            return Err("请填写邮箱密码或授权码".into());
        }
        password
    };

    {
        let conn = app.db();
        if db::account_by_email(&conn, &account.email)
            .map_err(|error| format!("读取账户失败：{error}"))?
            .is_some()
        {
            return Err("这个邮箱已经添加过了".into());
        }
    }

    if account.provider != "graph" && params.get("verify").and_then(Value::as_bool).unwrap_or(true) {
        app.rt
            .block_on(sync::test_account(&account, &credential))
            .map_err(|error| format!("连接检测失败：{error}"))?;
    }
    // 浏览器授权的 Graph 账户：验证即授权本身（refresh_token 已到手），无需额外探测；
    // 纯后台账户：验证 = 换应用令牌 + 探测目标邮箱的文件夹
    if account.provider == "graph" && credential.is_empty() && params.get("verify").and_then(Value::as_bool).unwrap_or(true) {
        app.rt
            .block_on(async {
                let authority = crate::oauth::authority_base();
                let set = crate::oauth::client_credentials_token(&authority, &account.tenant_id, &account.client_id, client_secret_param.trim()).await?;
                crate::graph::GraphClient::new(set.access_token, &account.user_id).folders().await?;
                Ok::<(), String>(())
            })
            .map_err(|error| format!("后台接入验证失败：{error}"))?;
    }

    let reference = if account.provider == "graph" {
        format!("oauth2:{}", account.email)
    } else {
        format!("mail:{}", account.email)
    };
    let vault = if credential.is_empty() {
        // 纯后台接入：没有 refresh_token；client_secret 存独立通道（引用在插入后落库）
        app.secrets
            .save(&format!("graphsecret:{}", account.email), client_secret_param.trim())
            .map_err(|error| format!("保存应用凭据失败：{error}"))?
    } else {
        app.secrets
            .save(&reference, &credential)
            .map_err(|error| if account.provider == "graph" { format!("保存授权凭据失败：{error}") } else { format!("保存密码失败：{error}") })?
    };

    let mut account = account;
    let color = {
        let conn = app.db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap_or(0);
        COLORS[(count as usize) % COLORS.len()].to_string()
    };
    account.color = color;

    let id = {
        let conn = app.db();
        db::account_insert(&conn, &account, &reference)
            .map_err(|error| format!("保存账户失败：{error}"))?
    };
    account.id = id;

    // client_secret 引用落库：纯后台接入（secret 已在 vault 分支保存）与
    // 机密客户端（浏览器授权 + clientSecret，这里保存）都要落
    if account.provider == "graph" && !client_secret_param.trim().is_empty() {
        let secret_reference = format!("graphsecret:{}", account.email);
        if !credential.is_empty() {
            app.secrets
                .save(&secret_reference, client_secret_param.trim())
                .map_err(|error| format!("保存应用凭据失败：{error}"))?;
        }
        let conn = app.db();
        db::account_set_client_secret_ref(&conn, id, &secret_reference)
            .map_err(|error| format!("保存账户失败：{error}"))?;
    }

    // 文件夹发现失败不阻塞新增：可能只是暂时离线
    let mut warnings = Vec::new();
    if let Err(error) = discover(app.clone(), id) {
        warnings.push(format!("文件夹尚未同步：{error}"));
    }

    Ok(json!({
        "account": account,
        "vault": vault,
        "warnings": warnings,
    }))
}

/// 修改账户（可选更新密码）
pub fn update(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let id = crate::param_int(params, "id", 0);
    if id <= 0 {
        return Err("缺少账户 ID".into());
    }
    let mut account = account_from_params(params, id)?;
    {
        let conn = app.db();
        let existing = db::account_get(&conn, id)
            .map_err(|error| format!("读取账户失败：{error}"))?
            .ok_or_else(|| "账户不存在".to_string())?;
        if account.color.is_empty() {
            account.color = existing.color;
        }
        if !params.get("signature").is_some() {
            account.signature = existing.signature;
        }
    db::account_update(&conn, &account).map_err(|error| format!("保存账户失败：{error}"))?;
    }
    if account.provider == "graph" {
        // Graph 账户：重新授权（oauthState 认领新 refresh_token）或仅更新 tenant/clientId
        let state = crate::param_str(params, "oauthState");
        if !state.is_empty() {
            let refresh = crate::oauth::claim(&app, &state)?;
            let reference = format!("oauth2:{}", account.email);
            app.secrets
                .save(&reference, &refresh)
                .map_err(|error| format!("保存授权凭据失败：{error}"))?;
        }
        // 机密客户端：允许更新 clientSecret（旧引用指向的值被覆盖）
        let client_secret_param = crate::param_str(params, "clientSecret");
        if !client_secret_param.trim().is_empty() {
            let secret_reference = format!("graphsecret:{}", account.email);
            app.secrets
                .save(&secret_reference, client_secret_param.trim())
                .map_err(|error| format!("保存应用凭据失败：{error}"))?;
            let conn = app.db();
            db::account_set_client_secret_ref(&conn, id, &secret_reference)
                .map_err(|error| format!("保存账户失败：{error}"))?;
        }
        return Ok(json!({ "account": account }));
    }
    let password = crate::param_str(params, "password");
    if !password.trim().is_empty() {
        let reference = format!("mail:{}", account.email);
        app.secrets
            .save(&reference, &password)
            .map_err(|error| format!("保存密码失败：{error}"))?;
    }
    Ok(json!({ "account": account }))
}

/// 删除账户：本地缓存、密码一并清理
pub fn remove(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let id = crate::param_int(params, "id", 0);
    let account = {
        let conn = app.db();
        db::account_get(&conn, id)
            .map_err(|error| format!("读取账户失败：{error}"))?
            .ok_or_else(|| "账户不存在".to_string())?
    };
    app.secrets.delete(&format!("mail:{}", account.email));
    app.secrets.delete(&format!("oauth2:{}", account.email));
    app.secrets.delete(&format!("graphsecret:{}", account.email));
    {
        let conn = app.db();
        db::account_delete(&conn, id).map_err(|error| format!("删除账户失败：{error}"))?;
    }
    let _ = std::fs::remove_dir_all(app.data_dir.join("raw").join(id.to_string()));
    Ok(json!({ "deleted": true }))
}

/// 连接检测（不写库）
pub fn test(app: Arc<App>, params: &Value) -> Result<Value, String> {
    let account = account_from_params(params, 0)?;
    if account.provider == "graph" {
        // Graph 账户用已保存的授权凭据验证（向导流程里授权本身即是验证）
        let lines = sync::test_account_graph(Arc::clone(&app), &account)
            .map_err(|error| format!("连接失败：{error}"))?;
        return Ok(json!({ "ok": true, "lines": lines }));
    }
    let password = crate::param_str(params, "password");
    if password.trim().is_empty() {
        return Err("请填写密码或授权码".into());
    }
    let lines = app
        .rt
        .block_on(sync::test_account(&account, &password))
        .map_err(|error| format!("连接失败：{error}"))?;
    Ok(json!({ "ok": true, "lines": lines }))
}

/// 发现并保存文件夹（新增账户后调用，也可手动重试）
pub fn discover(app: Arc<App>, account_id: i64) -> Result<Value, String> {
    let account = {
        let conn = app.db();
        db::account_get(&conn, account_id)
            .map_err(|error| format!("读取账户失败：{error}"))?
            .ok_or_else(|| "账户不存在".to_string())?
    };
    if account.provider == "graph" {
        sync::graph_discover(Arc::clone(&app), &account)
            .map_err(|error| format!("文件夹发现失败：{error}"))?;
        return Ok(json!({ "ok": true }));
    }
    let password = app.load_password(account_id)?;
    let login = Login {
        host: account.imap_host.clone(),
        port: account.imap_port,
        security: account.imap_security.clone(),
        username: account.username.clone(),
        password,
    };
    let remote = app
        .rt
        .block_on(async {
            let mut client = ImapClient::open(&login).await?;
            let mailboxes = client.list().await?;
            let mut counts = Vec::new();
            for mailbox in mailboxes.iter().filter(|item| item.selectable) {
                if let Ok(value) = client.status(&mailbox.name).await {
                    counts.push((mailbox.name.clone(), value));
                }
            }
            client.logout().await;
            Ok::<_, String>((mailboxes, counts))
        })?;

    let conn = app.db();
    for (index, mailbox) in remote.0.iter().enumerate() {
        let _ = db::mailbox_upsert(
            &conn,
            account_id,
            &mailbox.name,
            &mailbox.delimiter,
            &mailbox.role,
            mailbox.selectable,
            index as i64,
            "",
        );
    }
    for (name, (total, unread)) in &remote.1 {
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM mailboxes WHERE account_id = ?1 AND name = ?2",
                params![account_id, name],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = id {
            let _ = db::mailbox_set_counts(&conn, id, *total, *unread);
        }
    }
    let list = db::mailbox_list(&conn, account_id).map_err(|error| format!("读取文件夹失败：{error}"))?;
    Ok(json!({ "mailboxes": list }))
}

/// 按角色取文件夹 id（背景同步用）
pub fn mailbox_ids_for(conn: &rusqlite::Connection, account_id: i64, roles: &[&str]) -> Vec<i64> {
    let mut ids = Vec::new();
    for role in roles {
        if let Ok(id) = conn.query_row(
            "SELECT id FROM mailboxes WHERE account_id = ?1 AND role = ?2 AND selectable = 1 LIMIT 1",
            params![account_id, role],
            |row| row.get::<_, i64>(0),
        ) {
            ids.push(id);
        }
    }
    ids
}

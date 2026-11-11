//! 账户密码保管。
//!
//! 首选系统凭据库（Windows 凭据管理器 / macOS Keychain / Linux Secret Service）；
//! 凭据库不可用时退回本地 AES-256-GCM 加密文件，密钥保存在插件数据目录，
//! 权限尽量收紧到当前用户。密码从不写入日志，也不随账户信息返回前端。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use rand::RngCore;

const SERVICE: &str = "dbx-mail";
const KEY_FILE: &str = "secrets.key";
const STORE_FILE: &str = "secrets.bin";

/// 密码保管器
pub struct SecretStore {
    dir: PathBuf,
    /// 凭据库是否可用；不可用时全部走本地加密文件
    keyring_ok: bool,
}

impl SecretStore {
    /// 在插件数据目录下初始化
    pub fn new(dir: &Path) -> Self {
        // debug/测试可强制本地加密文件通道：无人值守跑真实验收时，
        // 新二进制首次读取钥匙串会触发 macOS 授权弹窗并挂起
        #[cfg(debug_assertions)]
        if std::env::var("DBX_MAIL_SECRET_FILE").ok().as_deref() == Some("1") {
            return Self {
                dir: dir.to_path_buf(),
                keyring_ok: false,
            };
        }
        // 先用一次写入探测凭据库是否可用（Windows 上服务不可用会直接报错）
        let keyring_ok = match keyring::Entry::new(SERVICE, "__probe__") {
            Ok(entry) => {
                let ok = entry.set_password("1").is_ok();
                if ok {
                    let _ = entry.delete_credential();
                }
                ok
            }
            Err(_) => false,
        };
        Self {
            dir: dir.to_path_buf(),
            keyring_ok,
        }
    }

    /// 保管方式（前端可展示「系统凭据库 / 本地加密」）
    pub fn backend(&self) -> &'static str {
        if self.keyring_ok {
            "keyring"
        } else {
            "local"
        }
    }

    /// 保存密码；返回实际使用的保管方式
    pub fn save(&self, reference: &str, password: &str) -> Result<&'static str, String> {
        if self.keyring_ok {
            let entry = keyring::Entry::new(SERVICE, reference)
                .map_err(|error| format!("凭据库不可用：{error}"))?;
            match entry.set_password(password) {
                Ok(()) => return Ok("keyring"),
                Err(error) => eprintln!("[dbx-mail] 系统凭据库写入失败，转本地加密：{error}"),
            }
        }
        self.save_local(reference, password)?;
        Ok("local")
    }

    /// 读取密码；找不到时返回空字符串
    pub fn load(&self, reference: &str) -> Result<String, String> {
        if self.keyring_ok {
            if let Ok(entry) = keyring::Entry::new(SERVICE, reference) {
                match entry.get_password() {
                    Ok(password) => return Ok(password),
                    Err(keyring::Error::NoEntry) => {}
                    Err(error) => eprintln!("[dbx-mail] 系统凭据库读取失败：{error}"),
                }
            }
        }
        self.load_local(reference)
    }

    /// 删除密码（忽略不存在）
    pub fn delete(&self, reference: &str) {
        if self.keyring_ok {
            if let Ok(entry) = keyring::Entry::new(SERVICE, reference) {
                let _ = entry.delete_credential();
            }
        }
        let mut store = self.read_local();
        if store.remove(reference).is_some() {
            let _ = self.write_local(&store);
        }
    }

    /* ---------------- 本地加密兜底 ---------------- */

    fn key(&self) -> Result<[u8; 32], String> {
        let path = self.dir.join(KEY_FILE);
        if let Ok(bytes) = fs::read(&path) {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(key);
            }
        }
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        fs::create_dir_all(&self.dir).map_err(|error| format!("创建数据目录失败：{error}"))?;
        let mut file = fs::File::create(&path).map_err(|error| format!("写入密钥失败：{error}"))?;
        file.write_all(&key).map_err(|error| format!("写入密钥失败：{error}"))?;
        restrict_permissions(&path);
        Ok(key)
    }

    fn read_local(&self) -> HashMap<String, String> {
        fs::read_to_string(self.dir.join(STORE_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn write_local(&self, store: &HashMap<String, String>) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|error| format!("创建数据目录失败：{error}"))?;
        let path = self.dir.join(STORE_FILE);
        let raw = serde_json::to_string(store).map_err(|error| format!("序列化失败：{error}"))?;
        fs::write(&path, raw).map_err(|error| format!("写入密码库失败：{error}"))?;
        restrict_permissions(&path);
        Ok(())
    }

    fn save_local(&self, reference: &str, password: &str) -> Result<(), String> {
        let key = self.key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度非法".to_string())?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let encrypted = cipher
            .encrypt(&nonce, password.as_bytes())
            .map_err(|_| "加密密码失败".to_string())?;
        let mut packed = nonce.to_vec();
        packed.extend_from_slice(&encrypted);
        let encoded = base64::engine::general_purpose::STANDARD.encode(packed);
        let mut store = self.read_local();
        store.insert(reference.to_string(), encoded);
        self.write_local(&store)
    }

    fn load_local(&self, reference: &str) -> Result<String, String> {
        let store = self.read_local();
        let Some(encoded) = store.get(reference) else {
            return Ok(String::new());
        };
        let packed = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "密码库内容损坏".to_string())?;
        if packed.len() < 13 {
            return Err("密码库内容损坏".into());
        }
        let (nonce_bytes, ciphertext) = packed.split_at(12);
        let key = self.key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "密钥长度非法".to_string())?;
        let plain = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| "解密密码失败".to_string())?;
        String::from_utf8(plain).map_err(|_| "密码编码非法".to_string())
    }
}

/// 收紧文件权限：Unix 下 0600；Windows 依赖用户配置目录的默认 ACL
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

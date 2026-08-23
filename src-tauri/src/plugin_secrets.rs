//! Claude 插件敏感配置的系统密钥库适配。
//!
//! 生产运行只支持 macOS Keychain 和 Windows Credential Manager；测试继续使用
//! `claude_plugins::InMemorySecretStore`，因此本模块没有任何测试会写入真实系统密钥库。

use crate::claude_plugins::{ClaudePluginError, Result, SecretStore};
use keyring::{Entry, Error as KeyringError};
use serde_json::Value;

/// KeenCode Claude 插件敏感配置使用的系统密钥库服务名。
const KEYRING_SERVICE: &str = "com.keencode.desktop.claude-plugins";

/// 使用 macOS Keychain / Windows Credential Manager 保存 Claude 插件敏感配置。
#[derive(Debug, Default)]
pub(crate) struct SystemSecretStore;

impl SystemSecretStore {
    /// 创建一个不携带敏感值的系统密钥库条目。
    fn entry(key: &str) -> Result<Entry> {
        Entry::new(KEYRING_SERVICE, key)
            .map_err(|_| ClaudePluginError::Invalid("无法访问系统密钥库".to_owned()))
    }

    /// 将密钥库错误转换为不包含服务名、键名或敏感值的插件错误。
    fn keyring_error(operation: &str) -> ClaudePluginError {
        ClaudePluginError::Invalid(format!("系统密钥库{operation}失败"))
    }
}

impl SecretStore for SystemSecretStore {
    /// 序列化后写入系统密钥库；序列化或密钥库错误均使用脱敏错误。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()> {
        let password = serde_json::to_string(value)
            .map_err(|_| ClaudePluginError::Invalid("敏感插件配置 JSON 序列化失败".to_owned()))?;
        let entry = Self::entry(key)?;
        entry
            .set_password(&password)
            .map_err(|_| Self::keyring_error("写入"))
    }

    /// 从系统密钥库读取并解析 JSON；不存在的条目表示尚未配置。
    fn get_json(&self, key: &str) -> Result<Option<Value>> {
        let entry = Self::entry(key)?;
        let password = match entry.get_password() {
            Ok(password) => password,
            Err(KeyringError::NoEntry) => return Ok(None),
            Err(_) => return Err(Self::keyring_error("读取")),
        };
        serde_json::from_str(&password).map(Some).map_err(|_| {
            ClaudePluginError::Invalid("系统密钥库中的敏感插件配置 JSON 无效".to_owned())
        })
    }

    /// 删除系统密钥库条目；不存在时按幂等删除处理。
    fn delete(&mut self, key: &str) -> Result<()> {
        let entry = Self::entry(key)?;
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(_) => Err(Self::keyring_error("删除")),
        }
    }
}

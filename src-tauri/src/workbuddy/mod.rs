pub mod data;
pub mod ipc_client;
pub mod standalone;

pub const MESSAGES: [(&str, &str); 7] = [
    ("AUTH_REQUIRED", "请先在 WorkBuddy 中登录一次，再刷新积分。"),
    ("AUTH_EXPIRED", "WorkBuddy 登录凭据已过期，请打开 WorkBuddy 重新登录后再查询。未自动刷新或修改登录状态。"),
    ("UNSUPPORTED_RUNTIME", "当前 WorkBuddy 版本暂不支持退出后的积分查询。"),
    ("UNSUPPORTED_ACCOUNT", "暂不支持此 WorkBuddy 企业账号的独立积分查询。"),
    ("NETWORK_ERROR", "WorkBuddy 积分查询失败，请检查网络后重试。"),
    ("SESSION_CHANGED", "WorkBuddy 正在更新本机登录凭据，将自动重新查询。"),
    ("QUERY_TIMEOUT", "WorkBuddy 积分查询超时，请检查网络后重试。"),
];

pub fn message(code: &str) -> Option<&'static str> {
    MESSAGES.iter().find(|(key, _)| *key == code).map(|(_, value)| *value)
}

pub fn failure(code: &str) -> crate::error::AppError {
    crate::error::AppError::new(code, message(code).unwrap_or("WorkBuddy 查询失败。"))
}

#[derive(Debug, Clone)]
pub enum WorkBuddyResult {
    Credits(Value),
    Models(Vec<serde_json::Value>),
    Identity { account_key: String },
    Checkin { account_key: String, outcome: String, credit: Option<f64>, streak: Option<f64> },
}

use serde_json::Value;

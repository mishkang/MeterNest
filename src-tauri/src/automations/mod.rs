pub mod codex_client;
pub mod scheduler;
pub mod system_wake;

// automations/errors.js
pub const MESSAGES: [(&str, &str); 15] = [
    ("ACTION_FAILED", "上次执行失败，请检查账号或网络后重试。"),
    ("AUTH_REQUIRED", "请先在对应客户端登录。"),
    ("AUTH_EXPIRED", "登录已过期，请在客户端重新登录。"),
    ("SESSION_CHANGED", "查询期间账号或登录状态发生变化，请重试。"),
    ("NETWORK_ERROR", "网络请求失败，请检查连接后重试。"),
    ("CHECKIN_OWN_APP", "签到需要客户端设备验证，请在 WorkBuddy 客户端完成签到。"),
    ("UNSUPPORTED_ACCOUNT", "当前账号不支持此操作。"),
    ("UNSUPPORTED_RUNTIME", "当前客户端版本暂不支持此操作。"),
    ("CODEX_MODEL_UNAVAILABLE", "未找到当前 Codex 账号可用的模型，请更新客户端后重试。"),
    ("CODEX_QUOTA_UNAVAILABLE", "对应的 Codex 额度窗口暂不可确认，未发送预热请求，将稍后重新检查。"),
    ("CODEX_UNCERTAIN", "预热请求结果尚未确认，为避免重复消耗，本次不自动重发。请刷新额度后确认。"),
    ("CODEX_LIMIT", "Codex 当前额度不可用，请等待恢复后重试。"),
    ("CODEX_ENDPOINT_UNSUPPORTED", "当前 Codex 使用了不受支持的自定义地址；为避免凭据被发送到未知服务，自动预热已拒绝。"),
    ("SOURCE_HIDDEN", "该来源已隐藏，自动任务已暂停。"),
    ("STATE_WRITE_FAILED", "无法保存任务记录，请检查配置目录；若操作已发出，请先刷新确认结果。"),
];

pub fn action_message(code: &str) -> &'static str {
    MESSAGES
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, value)| *value)
        .unwrap_or("网络请求失败，请检查连接后重试。")
}

pub fn normalize_code(code: &str) -> &'static str {
    MESSAGES
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(key, _)| *key)
        .unwrap_or("NETWORK_ERROR")
}

pub fn action_error(code: &str) -> crate::error::AppError {
    crate::error::AppError::new(code, action_message(code))
}

pub fn safe_message(code: &str) -> String {
    action_message(normalize_code(code)).to_string()
}

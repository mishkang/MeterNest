use crate::prefs::{enabled_tray_source, normalize_enabled_providers};
use crate::util::locale_number;
use serde_json::Value;

/// tray-title.js trayValueTitle.
pub fn tray_value_title(state: &Value, preferences: &Value) -> String {
    if !preferences.get("showTrayValue").and_then(Value::as_bool).unwrap_or(false) {
        return String::new();
    }
    let enabled = normalize_enabled_providers(
        state
            .get("enabledProviders")
            .or_else(|| preferences.get("enabledProviders"))
            .cloned()
            .unwrap_or_default(),
    );
    let source = preferences.get("trayValueSource").and_then(Value::as_str).unwrap_or("codex");
    if enabled_tray_source(source, &enabled) == "workbuddy" {
        let workbuddy = state.get("workbuddy").cloned().unwrap_or(Value::Null);
        let balance = workbuddy.get("account").and_then(|account| account.get("balance")).and_then(Value::as_f64);
        let error = workbuddy.get("error").and_then(Value::as_str).unwrap_or("");
        return match balance {
            Some(value) if error.is_empty() && value.is_finite() => locale_number(value, 2, 0),
            _ => "—".to_string(),
        };
    }
    let account = state.get("account").cloned().unwrap_or(Value::Null);
    let bucket = account
        .get("buckets")
        .and_then(Value::as_array)
        .and_then(|buckets| {
            buckets
                .iter()
                .find(|bucket| bucket.get("id").and_then(Value::as_str) == Some("codex"))
                .or_else(|| buckets.first())
        });
    let remaining = bucket
        .and_then(|bucket| bucket.get("primary"))
        .and_then(|primary| primary.get("remainingPercent"))
        .and_then(Value::as_f64);
    let error = state.get("accountError").and_then(Value::as_str).unwrap_or("");
    match remaining {
        Some(value) if error.is_empty() && value.is_finite() => format!("{}%", value),
        _ => "—".to_string(),
    }
}

/// hover.js hoverSummary.
pub fn hover_summary(state: &Value, language: &str) -> Value {
    use crate::i18n::{format, text};
    let account = state.get("account").cloned().unwrap_or(Value::Null);
    let bucket = account
        .get("buckets")
        .and_then(Value::as_array)
        .and_then(|buckets| {
            buckets
                .iter()
                .find(|bucket| bucket.get("id").and_then(Value::as_str) == Some("codex"))
                .or_else(|| buckets.first())
        });
    let remaining = bucket
        .and_then(|bucket| bucket.get("primary"))
        .and_then(|primary| primary.get("remainingPercent"))
        .and_then(Value::as_f64);
    let account_error = state.get("accountError").and_then(Value::as_str).unwrap_or("");
    let account_loading = state.get("accountLoading").and_then(Value::as_bool).unwrap_or(false);
    let workbuddy = state.get("workbuddy").cloned().unwrap_or(Value::Null);
    let balance = workbuddy.get("account").and_then(|account| account.get("balance")).and_then(Value::as_f64);
    let workbuddy_error = workbuddy.get("error").and_then(Value::as_str).unwrap_or("");
    let workbuddy_loading = workbuddy.get("loading").and_then(Value::as_bool).unwrap_or(false);
    let enabled = normalize_enabled_providers(state.get("enabledProviders").cloned().unwrap_or_default());
    let mut summary = serde_json::Map::new();
    if enabled.get("codex").and_then(Value::as_bool).unwrap_or(false) {
        summary.insert(
            "codex".to_string(),
            Value::String(match remaining {
                Some(value) if value.is_finite() => {
                    let suffix = if !account_error.is_empty() { " · 上次结果" } else { "" };
                    format(language, "剩余 {0}%{1}", &[&value.to_string(), text(language, suffix)])
                }
                _ => {
                    if account_loading {
                        text(language, "正在查询…").to_string()
                    } else {
                        text(language, "暂不可读").to_string()
                    }
                }
            }),
        );
    }
    if enabled.get("workbuddy").and_then(Value::as_bool).unwrap_or(false) {
        summary.insert(
            "workbuddy".to_string(),
            Value::String(match balance {
                Some(value) if value.is_finite() => {
                    let suffix = if !workbuddy_error.is_empty() { " · 上次结果" } else { "" };
                    format(language, "{0} 积分{1}", &[&locale_number(value, 2, 0), text(language, suffix)])
                }
                _ => {
                    if workbuddy_loading {
                        text(language, "正在查询…").to_string()
                    } else {
                        text(language, "暂不可读").to_string()
                    }
                }
            }),
        );
    }
    Value::Object(summary)
}

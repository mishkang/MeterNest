use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

// provider-settings.js
pub fn default_enabled_providers() -> Value {
    json!({ "codex": true, "workbuddy": true })
}

pub fn normalize_enabled_providers(value: Value) -> Value {
    let codex = value.get("codex").and_then(Value::as_bool);
    let workbuddy = value.get("workbuddy").and_then(Value::as_bool);
    match (codex, workbuddy) {
        (Some(c), Some(w)) if c || w => json!({ "codex": c, "workbuddy": w }),
        _ => default_enabled_providers(),
    }
}

pub fn validate_enabled_providers(value: Value) -> Result<Value, String> {
    let codex = value.get("codex").and_then(Value::as_bool);
    let workbuddy = value.get("workbuddy").and_then(Value::as_bool);
    match (codex, workbuddy) {
        (Some(c), Some(w)) => {
            if !(c || w) {
                Err("至少保留一个显示来源".to_string())
            } else {
                Ok(json!({ "codex": c, "workbuddy": w }))
            }
        }
        _ => Err("显示来源设置无效".to_string()),
    }
}

pub fn enabled_tray_source(source: &str, enabled: &Value) -> String {
    if (source == "codex" || source == "workbuddy") && enabled.get(source).and_then(Value::as_bool).unwrap_or(false) {
        source.to_string()
    } else if enabled.get("codex").and_then(Value::as_bool).unwrap_or(false) {
        "codex".to_string()
    } else {
        "workbuddy".to_string()
    }
}

// refresh-intervals.js
pub const DEFAULT_REFRESH_MINUTES: [(&str, i64); 4] =
    [("codex", 1), ("workbuddy", 5), ("feed", 10), ("models", 10)];

fn is_minutes(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                int >= 1
            } else {
                false
            }
        }
        _ => false,
    }
}

pub fn normalize_refresh_minutes(value: Value) -> Value {
    let mut result = Map::new();
    for (key, fallback) in DEFAULT_REFRESH_MINUTES {
        let minutes = value.get(key).filter(|item| is_minutes(item)).and_then(Value::as_i64).unwrap_or(fallback);
        result.insert(key.to_string(), json!(minutes));
    }
    Value::Object(result)
}

pub fn validate_refresh_minutes(value: Value) -> Result<Value, String> {
    if !value.is_object() {
        return Err("刷新间隔必须是不小于 1 的正整数（分钟）".to_string());
    }
    for (key, _) in DEFAULT_REFRESH_MINUTES {
        if key == "models" && value.get(key).is_none() {
            continue;
        }
        let item = value.get(key).cloned().unwrap_or(Value::Null);
        if !is_minutes(&item) {
            return Err("刷新间隔必须是不小于 1 的正整数（分钟）".to_string());
        }
    }
    Ok(normalize_refresh_minutes(value))
}

// automations/preferences.js
pub fn default_automation() -> Value {
    json!({
        "codex": { "onReset": false, "daily": false, "wake": false, "times": ["06:00", "11:02", "16:05"] },
        "workbuddy": { "daily": false }
    })
}

fn clock(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let valid = |slice: &[u8], max: u8| {
        slice.iter().all(|b| b.is_ascii_digit()) && slice.iter().fold(0u32, |acc, b| acc * 10 + (b - b'0') as u32) < max as u32
    };
    valid(&bytes[0..2], 24) && valid(&bytes[3..5], 60)
}

pub fn normalize_automation(value: Value) -> Value {
    let codex = value.get("codex").cloned().unwrap_or(Value::Null);
    let times: Vec<String> = codex
        .get("times")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let times = if times.len() <= 12 && times.iter().all(|time| clock(time)) {
        let mut unique: Vec<String> = BTreeMap::from_iter(times.iter().cloned().map(|t| (t, ()) ))
            .into_keys()
            .collect();
        unique.sort();
        unique
    } else {
        ["06:00", "11:02", "16:05"].iter().map(|s| s.to_string()).collect()
    };
    json!({
        "codex": {
            "wake": codex.get("wake").and_then(Value::as_bool).unwrap_or(false),
            "notify": codex.get("notify").and_then(Value::as_bool).unwrap_or(false),
            "onReset": codex.get("onReset").and_then(Value::as_bool).unwrap_or(false),
            "daily": codex.get("daily").and_then(Value::as_bool).unwrap_or(false) && !times.is_empty(),
            "times": times
        },
        "workbuddy": { "daily": value.get("workbuddy").and_then(|v| v.get("daily")).and_then(Value::as_bool).unwrap_or(false), "notify": value.get("workbuddy").and_then(|v| v.get("notify")).and_then(Value::as_bool).unwrap_or(false) }
    })
}

pub fn validate_automation(value: Value) -> Result<Value, String> {
    let invalid = "自动任务设置无效：时间须为不重复的 HH:mm，每日定时至少保留一个时间，最多 12 个。".to_string();
    let codex = value.get("codex").cloned().unwrap_or(Value::Null);
    let workbuddy = value.get("workbuddy").cloned().unwrap_or(Value::Null);
    if codex.is_null() || workbuddy.is_null() {
        return Err(invalid);
    }
    if let Some(wake) = codex.get("wake") {
        if !wake.is_boolean() {
            return Err(invalid);
        }
    }
    for field in ["onReset", "daily"] {
        match codex.get(field) {
            Some(item) if item.is_boolean() => {}
            _ => return Err(invalid),
        }
    }
    match workbuddy.get("daily") {
        Some(item) if item.is_boolean() => {}
        _ => return Err(invalid),
    }
    let times = match codex.get("times").and_then(Value::as_array) {
        Some(times) => times.clone(),
        None => return Err(invalid),
    };
    if times.len() > 12 {
        return Err(invalid);
    }
    let mut seen = std::collections::HashSet::new();
    for time in &times {
        match time.as_str() {
            Some(text) if clock(text) => {
                if !seen.insert(text.to_string()) {
                    return Err(invalid);
                }
            }
            _ => return Err(invalid),
        }
    }
    if codex.get("daily").and_then(Value::as_bool).unwrap_or(false) && times.is_empty() {
        return Err(invalid);
    }
    Ok(normalize_automation(value))
}

// settings.js
pub fn defaults() -> Value {
    json!({
        "language": "system",
        "showResetFeed": true,
        "showWorkBuddyHistory": true,
        "openAtLogin": false,
        "networkMode": "proxy",
        "proxyUrl": "http://127.0.0.1:7897",
        "showTrayValue": false,
        "trayValueSource": "codex",
        "refreshMinutes": normalize_refresh_minutes(Value::Null),
        "enabledProviders": default_enabled_providers(),
        "automation": default_automation()
    })
}

fn validate_proxy_url(raw: &str) -> Result<String, String> {
    let proxy_url = raw.trim();
    if proxy_url.len() > 2048 {
        return Err("代理地址过长".to_string());
    }
    let parsed = url::Url::parse(proxy_url)
        .map_err(|_| "请输入完整代理地址，例如 http://127.0.0.1:7897".to_string())?;
    let scheme = parsed.scheme();
    let allowed = matches!(scheme, "http" | "https" | "socks5" | "socks4");
    let host_empty = parsed.host_str().is_none() || parsed.host_str() == Some("");
    let has_credentials = parsed.username() != "" || parsed.password().is_some();
    if !allowed
        || host_empty
        || has_credentials
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err("代理需使用 HTTP、HTTPS、SOCKS4 或 SOCKS5 地址，不含账号、路径或查询参数".to_string());
    }
    Ok(proxy_url.to_string())
}

pub fn validate_settings(value: &Value) -> Result<Value, String> {
    if !value.is_object() {
        return Err("设置格式无效".to_string());
    }
    let open_at_login = value.get("openAtLogin").and_then(Value::as_bool);
    let network_mode = value.get("networkMode").and_then(Value::as_str);
    let proxy_raw = value.get("proxyUrl").and_then(Value::as_str);
    match (open_at_login, network_mode, proxy_raw) {
        (Some(login), Some(mode), Some(proxy)) => {
            if mode != "proxy" && mode != "direct" {
                return Err("设置格式无效".to_string());
            }
            let show_tray_value = value.get("showTrayValue").and_then(Value::as_bool).unwrap_or(false);
            let tray_value_source = value
                .get("trayValueSource")
                .and_then(Value::as_str)
                .filter(|item| *item == "codex" || *item == "workbuddy")
                .unwrap_or("codex")
                .to_string();
            let show_reset_feed = value.get("showResetFeed").and_then(Value::as_bool).unwrap_or(true);
            let show_workbuddy_history = value.get("showWorkBuddyHistory").and_then(Value::as_bool).unwrap_or(true);
            if !show_tray_value_valid(value) || tray_value_source.is_empty() {
                return Err("菜单栏显示设置无效".to_string());
            }
            if value.get("showResetFeed").map(|item| !item.is_boolean()).unwrap_or(false) {
                return Err("额度动态显示设置无效".to_string());
            }
            if value.get("showWorkBuddyHistory").map(|item| !item.is_boolean()).unwrap_or(false) {
                return Err("积分与模型显示设置无效".to_string());
            }
            let enabled_providers = validate_enabled_providers(value.get("enabledProviders").cloned().unwrap_or(default_enabled_providers()))?;
            let refresh_minutes = validate_refresh_minutes(value.get("refreshMinutes").cloned().unwrap_or(Value::Null))?;
            let proxy_url = if mode == "proxy" || !proxy.trim().is_empty() {
                validate_proxy_url(proxy)?
            } else {
                proxy.trim().to_string()
            };
            let automation = validate_automation(value.get("automation").cloned().unwrap_or(default_automation()))?;
            let tray_source = enabled_tray_source(&tray_value_source, &enabled_providers);
            let language = match value.get("language") {
                None => "system",
                Some(value) => value.as_str().filter(|language| crate::i18n::valid_preference(language))
                    .ok_or_else(|| "界面语言无效".to_string())?,
            };
            Ok(json!({
                "language": language,
                "showResetFeed": show_reset_feed,
                "showWorkBuddyHistory": show_workbuddy_history,
                "openAtLogin": login,
                "networkMode": mode,
                "proxyUrl": proxy_url,
                "showTrayValue": show_tray_value,
                "trayValueSource": tray_source,
                "refreshMinutes": refresh_minutes,
                "enabledProviders": enabled_providers,
                "automation": automation
            }))
        }
        _ => Err("设置格式无效".to_string()),
    }
}

fn show_tray_value_valid(value: &Value) -> bool {
    match value.get("showTrayValue") {
        Some(item) => item.is_boolean(),
        None => true,
    }
}

/// settings.js read(): defaults merged with saved values and normalization.
pub fn read_preferences(saved: &Value) -> Value {
    let mut merged = defaults();
    if let Some(object) = saved.as_object() {
        for (key, value) in object {
            match key.as_str() {
                "showResetFeed" | "showWorkBuddyHistory" => {
                    merged[key] = json!(value.as_bool() != Some(false));
                }
                "openAtLogin" | "showTrayValue" => {
                    if let Some(flag) = value.as_bool() {
                        merged[key] = json!(flag);
                    }
                }
                "language" => {
                    if value.as_str().is_some_and(crate::i18n::valid_preference) {
                        merged[key] = value.clone();
                    }
                }
                "networkMode" => {
                    if let Some(mode) = value.as_str().filter(|mode| matches!(*mode, "proxy" | "direct")) {
                        merged[key] = json!(mode);
                    }
                }
                "proxyUrl" => {
                    if let Some(url) = value.as_str().filter(|url| url.len() <= 2048) {
                        merged[key] = json!(url.trim());
                    }
                }
                "trayValueSource" => {
                    if let Some(source) = value.as_str().filter(|source| matches!(*source, "codex" | "workbuddy")) {
                        merged[key] = json!(source);
                    }
                }
                "enabledProviders" | "refreshMinutes" | "automation" => {
                    merged[key] = value.clone();
                }
                _ => {}
            }
        }
    }
    let mode = merged.get("networkMode").and_then(Value::as_str).unwrap_or("proxy");
    let proxy = merged.get("proxyUrl").and_then(Value::as_str).unwrap_or("");
    if mode == "proxy" && validate_proxy_url(proxy).is_err() {
        merged["proxyUrl"] = defaults()["proxyUrl"].clone();
    }
    let enabled = normalize_enabled_providers(merged.get("enabledProviders").cloned().unwrap_or(Value::Null));
    merged["enabledProviders"] = enabled.clone();
    let source = merged.get("trayValueSource").and_then(Value::as_str).unwrap_or("codex").to_string();
    merged["trayValueSource"] = json!(enabled_tray_source(&source, &enabled));
    merged["refreshMinutes"] = normalize_refresh_minutes(merged.get("refreshMinutes").cloned().unwrap_or(Value::Null));
    merged["automation"] = normalize_automation(merged.get("automation").cloned().unwrap_or(Value::Null));
    merged
}

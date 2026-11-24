use crate::error::AppError;
use crate::util::{now_ms, to_iso};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

static BEIJING_DATETIME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?$").unwrap());

pub const SUMMARY_PATH: &str = "/billing/meter/get-user-resource-summary";
pub const PAID_PATH: &str = "/billing/meter/get-user-resource-paid-packages";
pub const FREE_PATH: &str = "/billing/meter/get-user-resource-free-packages";

const CODES: [(&str, &[&str]); 4] = [
    ("base", &["002_AkiJS3ZHF5", "005_maRGyrHhw1", "003_FAnt7lcmRT", "023_4xbGhMrE6q", "026_BaESVICNoi", "027_0FCGVA6vSa", "008_cfWoLwvjU4", "035_ArVxJcGDsm", "039_KRcQj7wUat", "040_mi9rCYg46x", "001_PqouKr6QWV", "006_DbXS0lrypC"]),
    ("reward", &["007_nzdH5h4Nl0", "029_6wCGEWquYy", "030_BjSt89qTvr"]),
    ("bonus", &["028_NtpWi0jzXs", "037_WxOD3MpI2o"]),
    ("purchased", &["009_0XmEQc2xOf", "038_OhvqZtiPKr", "036_lupO5WgNdG"]),
];

fn category(code: &str) -> &'static str {
    for (kind, codes) in CODES {
        if codes.iter().any(|item| format!("TCACA_code_{}", item) == code) {
            return kind;
        }
    }
    "other"
}

fn is_paid(code: &str) -> bool {
    if category(code) == "purchased" {
        return true;
    }
    // base codes are the paid-path subset, matching workbuddy-data.js.
    CODES[0].1.iter().any(|item| format!("TCACA_code_{}", item) == code)
}

fn known(code: &str) -> bool {
    category(code) != "other"
}

pub fn label(kind: &str) -> &'static str {
    match kind {
        "base" => "套餐基础积分",
        "reward" => "平台奖励积分",
        "purchased" => "购买积分",
        "bonus" => "套餐赠送积分",
        _ => "其他积分",
    }
}

fn count(value: &Value) -> Result<f64, AppError> {
    let invalid = || AppError::message("WorkBuddy 积分格式已变化");
    match value {
        Value::Number(number) => {
            let number = number.as_f64().ok_or_else(invalid)?;
            if number.is_finite() && number >= 0.0 {
                Ok(number)
            } else {
                Err(invalid())
            }
        }
        Value::String(string) => {
            if string.trim().is_empty() {
                return Err(invalid());
            }
            let number: f64 = string.trim().parse().map_err(|_| invalid())?;
            if number.is_finite() && number >= 0.0 {
                Ok(number)
            } else {
                Err(invalid())
            }
        }
        _ => Err(invalid()),
    }
}

/// workbuddy-data.js billingTime: seconds/milliseconds or Beijing datetime text.
pub fn billing_time(value: &Value) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    let millis = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(string) if !string.trim().is_empty() => {
            if string.trim().chars().all(|c| c.is_ascii_digit()) {
                string.trim().parse::<f64>().ok()
            } else {
                let text = string.trim().replace(' ', "T");
                let parsed = if BEIJING_DATETIME_RE.is_match(&text) {
                    chrono::DateTime::parse_from_rfc3339(&format!("{}+08:00", text)).ok()
                } else {
                    chrono::DateTime::parse_from_rfc3339(&text).ok()
                    .or_else(|| chrono::DateTime::parse_from_rfc3339(&format!("{}Z", text)).ok())
                };
                return parsed.map(|time| time.with_timezone(&chrono::Utc).timestamp_millis());
            }
        }
        _ => None,
    }?;
    let millis = if millis < 1e12 { millis * 1000.0 } else { millis };
    let millis = millis as i64;
    if millis > 0 && millis < 8640000000000000 {
        Some(millis)
    } else {
        None
    }
}

/// workbuddy-data.js normalizeSummary.
pub fn normalize_summary(envelope: &Value, identity: &str) -> Result<Value, AppError> {
    let invalid = || AppError::message("WorkBuddy 积分响应暂不可用");
    if envelope.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(invalid());
    }
    let packages = envelope
        .get("data")
        .and_then(|data| data.get("Packages"))
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut breakdown = serde_json::Map::new();
    for kind in ["base", "reward", "purchased", "bonus", "other"] {
        breakdown.insert(kind.to_string(), json!(0.0));
    }
    let mut balance = 0.0;
    for item in packages {
        let value = count(item.get("CycleRemainCapacity").unwrap_or(&Value::Null))?;
        balance += value;
        let code = item.get("PackageCode").and_then(Value::as_str).unwrap_or("");
        let kind = category(code);
        let current = breakdown.get(kind).and_then(Value::as_f64).unwrap_or(0.0);
        breakdown.insert(kind.to_string(), json!(current + value));
    }
    if !balance.is_finite() {
        return Err(AppError::message("WorkBuddy 积分格式已变化"));
    }
    let identity: String = identity.chars().take(200).collect();
    let plan = envelope
        .get("data")
        .and_then(|data| data.get("SubscriptionPackageName"))
        .and_then(Value::as_str)
        .map(|value| value.chars().take(100).collect::<String>())
        .unwrap_or_default();
    let mut object = Map::new();
    object.insert("status".to_string(), json!("ready"));
    object.insert("identity".to_string(), json!(identity));
    object.insert("plan".to_string(), json!(plan));
    object.insert("balance".to_string(), json!(balance));
    object.insert("breakdown".to_string(), Value::Object(breakdown));
    Ok(Value::Object(object))
}

pub fn normalize_details(resources: &[Value], now: i64, expired: bool) -> Result<Vec<Value>, AppError> {
    let mut seen = std::collections::HashSet::new();
    let mut details: Vec<(i64, Value)> = Vec::new();
    for resource in resources {
        if resource.get("CapacityType").and_then(Value::as_i64) == Some(4) {
            continue;
        }
        let status = resource.get("Status").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        if expired {
            if !matches!(status, Some(2) | Some(3)) {
                continue;
            }
        } else if !matches!(status, None | Some(0) | Some(3)) {
            continue;
        }
        let expires = billing_time(resource.get("DeductionEndTime").unwrap_or(&Value::Null))
            .or_else(|| billing_time(resource.get("ExpiredTime").unwrap_or(&Value::Null)))
            .or_else(|| billing_time(resource.get("CycleEndTime").unwrap_or(&Value::Null)));
        let starts = billing_time(resource.get("DeductionStartTime").unwrap_or(&Value::Null))
            .or_else(|| billing_time(resource.get("CycleStartTime").unwrap_or(&Value::Null)));
        let expires = match expires {
            Some(value) => value,
            _ => continue,
        };
        if if expired { expires > now } else { expires <= now } {
            continue;
        }
        if let Some(starts) = starts {
            if starts > now {
                continue;
            }
        }
        let total_field = if expired { "CapacitySizePrecise" } else { "CycleCapacitySizePrecise" };
        let remaining_field = if expired { "CapacityRemainPrecise" } else { "CycleCapacityRemainPrecise" };
        let total = count(resource.get(total_field).or(resource.get("CycleCapacitySizePrecise")).unwrap_or(&Value::Null))?;
        let remaining = count(resource.get(remaining_field).or(resource.get("CycleCapacityRemainPrecise")).unwrap_or(&Value::Null))?;
        if total == 0.0 {
            continue;
        }
        let used_field = if expired { "CapacityUsedPrecise" } else { "CycleCapacityUsedPrecise" };
        let used = match resource.get(used_field).or(resource.get("CycleCapacityUsedPrecise")) {
            Some(value) if !value.is_null() => count(value)?,
            _ => (total - remaining).max(0.0),
        };
        if remaining > total {
            return Err(AppError::message("WorkBuddy 积分明细格式已变化"));
        }
        let id = match resource.get("ResourceId").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return Err(AppError::message("WorkBuddy 积分明细标识缺失")),
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let kind = category(resource.get("PackageCode").and_then(Value::as_str).unwrap_or(""));
        details.push((
            expires,
            json!({
                "kind": kind,
                "name": label(kind),
                "total": total,
                "remaining": remaining,
                "used": used,
                "expiresAt": to_iso(expires)
            }),
        ));
    }
    details.sort_by(|(a, _), (b, _)| if expired { b.cmp(a) } else { a.cmp(b) });
    Ok(details.into_iter().map(|(_, value)| value).collect())
}

fn beijing_time(time: i64) -> String {
    crate::util::beijing_time(time)
}

/// workbuddy-data.js fetchCreditDetails: paginated paid/free detail queries.
pub async fn fetch_credit_details<F>(request: F, summary: &Value, now: i64, expired: bool) -> Result<Vec<Value>, AppError>
where
    F: Fn(&str, Value) -> futures::future::BoxFuture<'static, Result<Value, crate::error::AppError>>,
{
    let packages = summary
        .get("data")
        .and_then(|data| data.get("Packages"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut package_codes: Vec<String> = packages
        .iter()
        .filter_map(|item| item.get("PackageCode").and_then(Value::as_str))
        .filter(|code| !code.is_empty())
        .map(str::to_string)
        .collect();
    if expired {
        for (_, codes) in CODES {
            for code in codes {
                package_codes.push(format!("TCACA_code_{}", code));
            }
        }
    }
    package_codes.sort();
    package_codes.dedup();
    if package_codes.is_empty() && !packages.is_empty() {
        return Err(AppError::message("WorkBuddy 积分明细分类暂不可用"));
    }
    let period_begin = now - 101 * 365 * 86400000;
    let mut rows: Vec<Value> = Vec::new();
    let paid: Vec<String> = package_codes.iter().filter(|code| is_paid(code) || !known(code)).cloned().collect();
    let free: Vec<String> = package_codes.iter().filter(|code| !is_paid(code)).cloned().collect();
    for (url, codes) in [(PAID_PATH, paid), (FREE_PATH, free)] {
        if codes.is_empty() {
            continue;
        }
        let mut collected: Vec<Value> = Vec::new();
        let mut previous_page_len = 0;
        for page in 1_u64.. {
            let mut body = json!({
                "PageNumber": page,
                "PageSize": 200,
                "PackageCodes": codes,
                "IsDisplayTotalInfo": true
            });
            if expired {
                body["Status"] = json!([2, 3]);
                body["PackageEndTimeRangeBegin"] = json!(beijing_time(period_begin));
                body["PackageEndTimeRangeEnd"] = json!(beijing_time(now));
                body["OrderBy"] = json!("endTime");
                body["SortBy"] = json!("desc");
            } else {
                body["Status"] = json!([0, 3]);
                body["OnlyValidPeriod"] = json!(true);
            }
            let result = request(url, body).await?;
            let accounts = result
                .get("data")
                .and_then(|data| data.get("Accounts"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if result.get("code").and_then(Value::as_i64) != Some(0) {
                return Err(AppError::message("WorkBuddy 积分明细暂不可用"));
            }
            let total = result.get("data").and_then(|data| data.get("TotalCount")).and_then(Value::as_i64);
            let page_len = accounts.len();
            let repeats = page_len > 0 && page_len == previous_page_len && collected.ends_with(&accounts);
            if repeats {
                return Err(AppError::message("WorkBuddy 积分明细分页重复"));
            }
            previous_page_len = page_len;
            collected.extend(accounts);
            let more = match total {
                Some(total) if total >= 0 => (collected.len() as i64) < total,
                _ => page_len >= 200,
            };
            if !more {
                break;
            }
            if page_len == 0 {
                return Err(AppError::message("WorkBuddy 积分明细分页不完整"));
            }
        }
        rows.extend(collected);
    }
    normalize_details(&rows, now, expired)
}

/// workbuddy-data.js displayCreditData: rebuild a bounded display payload.
pub fn display_credit_data(account: &Value) -> Result<Value, AppError> {
    let mut data = Map::new();
    if let Some(breakdown) = account.get("breakdown") {
        let mut rebuilt = Map::new();
        for kind in ["base", "reward", "purchased", "bonus", "other"] {
            rebuilt.insert(kind.to_string(), json!(count(breakdown.get(kind).unwrap_or(&Value::Null))?));
        }
        data.insert("breakdown".to_string(), Value::Object(rebuilt));
    }
    for key in ["creditDetails", "expiredCreditDetails"] {
        if let Some(items) = account.get(key).and_then(Value::as_array) {
            let mut rebuilt = Vec::new();
            for item in items {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("other");
                let kind = if ["base", "reward", "purchased", "bonus", "other"].contains(&kind) { kind } else { "other" };
                let expires = billing_time(item.get("expiresAt").unwrap_or(&Value::Null))
                    .ok_or_else(|| AppError::message("WorkBuddy 积分明细日期无效"))?;
                rebuilt.push(json!({
                    "kind": kind,
                    "name": label(kind),
                    "total": count(item.get("total").unwrap_or(&Value::Null))?,
                    "remaining": count(item.get("remaining").unwrap_or(&Value::Null))?,
                    "used": count(item.get("used").unwrap_or(&Value::Null))?,
                    "expiresAt": to_iso(expires)
                }));
            }
            data.insert(key.to_string(), Value::Array(rebuilt));
        }
    }
    for key in ["detailsError", "expiredDetailsError"] {
        if account.get(key).map(|value| !value.is_null()).unwrap_or(false) {
            data.insert(key.to_string(), json!("明细暂不可用，请刷新后重试。"));
        }
    }
    Ok(Value::Object(data))
}

/// workbuddy-data.js fetchCreditData for the IPC fallback path.
pub async fn fetch_credit_data<F>(request: F, summary: Value) -> Result<Value, AppError>
where
    F: Fn(&str, Value) -> futures::future::BoxFuture<'static, Result<Value, crate::error::AppError>> + Clone,
{
    let now = now_ms();
    let mut data = Map::new();
    let mut details: Result<Vec<Value>, AppError> = fetch_credit_details(request.clone(), &summary, now, false).await;
    match details {
        Ok(rows) => {
            data.insert("creditDetails".to_string(), Value::Array(rows));
        }
        Err(error) => {
            if error.code == "AUTH_EXPIRED" {
                return Err(error);
            }
            data.insert("detailsError".to_string(), json!("明细暂不可用，请刷新后重试。"));
        }
    }
    details = fetch_credit_details(request, &summary, now, true).await;
    match details {
        Ok(rows) => {
            data.insert("expiredCreditDetails".to_string(), Value::Array(rows));
        }
        Err(error) => {
            if error.code == "AUTH_EXPIRED" {
                return Err(error);
            }
            data.insert("expiredDetailsError".to_string(), json!("明细暂不可用，请刷新后重试。"));
        }
    }
    Ok(Value::Object(data))
}

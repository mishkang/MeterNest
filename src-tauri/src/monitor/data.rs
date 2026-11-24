use crate::error::AppError;
use crate::util::{parse_time, to_iso};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

static SCRIPT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<script\b([^>]*)>([\s\S]*?)</script>").unwrap());
static FEED_ID_ATTR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\bid\s*=\s*["']reset-feed-data["']"#).unwrap());

pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub home: &'static str,
}

pub const SOURCES: [Source; 1] = [Source {
    id: "reset-fm",
    name: "Codex Reset FM",
    url: "https://codex-reset.pro/",
    home: "https://codex-reset.pro/",
}];

fn text(value: &Value, max: usize) -> String {
    match value.as_str() {
        Some(string) => string.chars().take(max).collect::<String>().to_string(),
        None => String::new(),
    }
}

fn timestamp(value: &Value) -> Option<String> {
    let string = value.as_str()?;
    let millis = parse_time(string)?;
    Some(to_iso(millis))
}

fn safe_link(value: &Value) -> Option<String> {
    let string = value.as_str()?;
    if string.is_empty() || string.len() > 1000 {
        return None;
    }
    let url = url::Url::parse(string).ok()?;
    let host = url.host_str()?;
    if url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(host, "x.com" | "twitter.com" | "codex-reset.pro")
    {
        Some(url.to_string())
    } else {
        None
    }
}

pub fn parse_feed(body: &str, source: &Source) -> Result<Vec<Value>, AppError> {
    for captures in SCRIPT_RE.captures_iter(body) {
        let attributes = captures.get(1).map(|m| m.as_str()).unwrap_or("");
        if !FEED_ID_ATTR_RE.is_match(attributes) {
            continue;
        }
        let embedded = captures.get(2).map(|m| m.as_str()).unwrap_or("");
        let data: Value = serde_json::from_str(embedded.trim()).map_err(|_| AppError::message("数据格式已变化"))?;
        let events = data.get("events").cloned().unwrap_or(Value::Null);
        return parse_events(&events, source);
    }
    Err(AppError::message("数据格式已变化"))
}

fn parse_events(events: &Value, source: &Source) -> Result<Vec<Value>, AppError> {
    let list = events.as_array().ok_or_else(|| AppError::message("数据格式已变化"))?;
    let limited: Vec<&Value> = list.iter().take(500).collect();
    let mut records = Vec::new();
    for event in limited {
        let kind = event.get("tool").and_then(Value::as_str).unwrap_or("");
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "codex" || !matches!(event_type, "usage_reset" | "reset_card" | "credit_grant" | "policy") {
            continue;
        }
        let announced_at = timestamp(event.get("announcedAt").unwrap_or(&Value::Null));
        let completed_at = timestamp(event.get("completedAt").unwrap_or(&Value::Null));
        let at = announced_at.clone().or(completed_at.clone());
        let at = match at {
            Some(value) => value,
            None => continue,
        };
        let mut posts: Vec<Value> = event
            .get("posts")
            .and_then(Value::as_array)
            .map(|items| items.iter().take(64).map(|item| item.clone()).collect())
            .unwrap_or_default();
        posts.retain(|post| post.is_object());
        let mut parsed_posts: Vec<(i64, Value)> = Vec::new();
        for post in posts {
            let at = timestamp(post.get("at").unwrap_or(&Value::Null)).and_then(|value| parse_time(&value));
            let translation = text(post.get("translation").unwrap_or(&Value::Null), 1200).trim().to_string();
            let url = safe_link(post.get("url").unwrap_or(&Value::Null));
            let role = post
                .get("role")
                .and_then(Value::as_str)
                .filter(|role| ["announce", "update", "complete"].contains(role))
                .unwrap_or("")
                .to_string();
            if translation.is_empty() && url.is_none() {
                continue;
            }
            parsed_posts.push((
                at.unwrap_or(0),
                json!({
                    "at": at.map(|ms| to_iso(ms)),
                    "translation": translation,
                    "url": url,
                    "role": role
                }),
            ));
        }
        parsed_posts.sort_by_key(|(at, _)| *at);
        let parsed_posts: Vec<Value> = parsed_posts.into_iter().map(|(_, value)| value).collect();
        let selected: Vec<&Value> = parsed_posts.iter().rev().take(16).collect();
        let post = selected
            .iter()
            .find(|post| !post.get("translation").and_then(Value::as_str).unwrap_or("").is_empty() && post.get("url").is_some())
            .or_else(|| selected.iter().find(|post| post.get("url").is_some()));
        let id_source = match event.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => format!("{}:{}:{}", event_type, at, text(event.get("title").unwrap_or(&Value::Null), 200)),
        };
        let stage = event
            .get("stage")
            .and_then(Value::as_str)
            .filter(|stage| ["completed", "rolling_out", "scheduled"].contains(stage))
            .unwrap_or("unknown")
            .to_string();
        let mut title = text(event.get("title").unwrap_or(&Value::Null), 200);
        if title.is_empty() {
            title = "Codex 额度动态".to_string();
        }
        records.push(json!({
            "id": text(&Value::String(id_source), 200),
            "type": event_type,
            "title": title,
            "summary": text(event.get("summary").unwrap_or(&Value::Null), 800),
            "scope": text(event.get("scope").unwrap_or(&Value::Null), 120),
            "stage": stage,
            "at": at,
            "announcedAt": announced_at,
            "completedAt": completed_at,
            "updatedAt": timestamp(event.get("updatedAt").unwrap_or(&Value::Null)).unwrap_or_else(|| at.clone()),
            "posts": parsed_posts,
            "link": post.and_then(|value| safe_link(value.get("url").unwrap_or(&Value::Null))),
            "sources": [{ "id": source.id, "name": source.name, "url": source.home }]
        }));
    }
    if !list.is_empty() && records.is_empty() {
        return Err(AppError::message("没有可识别的 Codex 记录"));
    }
    Ok(records)
}

pub fn merge_records(groups: Vec<Vec<Value>>, count: usize) -> Vec<Value> {
    let mut records: Map<String, Value> = Map::new();
    for group in groups {
        for item in group {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            match records.get(&id) {
                None => {
                    records.insert(id, item);
                }
                Some(old) => {
                    let old_updated = old.get("updatedAt").and_then(Value::as_str).and_then(|v| parse_time(v)).unwrap_or(0);
                    let new_updated = item.get("updatedAt").and_then(Value::as_str).and_then(|v| parse_time(v)).unwrap_or(0);
                    let newer = if new_updated > old_updated { item.clone() } else { old.clone() };
                    let mut sources = Vec::new();
                    for source in [old.get("sources"), newer.get("sources")].into_iter().flatten() {
                        if let Some(list) = source.as_array() {
                            for entry in list {
                                let sid = entry.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                                if !sources.iter().any(|existing: &Value| existing.get("id").and_then(Value::as_str) == Some(sid.as_str())) {
                                    sources.push(entry.clone());
                                }
                            }
                        }
                    }
                    let mut merged = newer;
                    merged["sources"] = Value::Array(sources);
                    records.insert(id, merged);
                }
            }
        }
    }
    let mut list: Vec<Value> = records.into_values().collect();
    list.sort_by_key(|record| {
        let at = record.get("at").and_then(Value::as_str).and_then(|v| parse_time(v)).unwrap_or(0);
        std::cmp::Reverse(at)
    });
    list.truncate(count);
    list
}

pub fn encode_feed_cache(records: &[Value], updated_at: &str) -> Value {
    let events: Vec<Value> = records
        .iter()
        .take(5)
        .map(|record| {
            let posts = record.get("posts").and_then(Value::as_array).cloned().unwrap_or_default();
            let posts = if !posts.is_empty() {
                posts
            } else if let Some(link) = record.get("link").and_then(Value::as_str) {
                vec![json!({ "url": link })]
            } else {
                Vec::new()
            };
            json!({
                "id": record.get("id").cloned().unwrap_or(Value::Null),
                "tool": "codex",
                "type": record.get("type").cloned().unwrap_or(Value::Null),
                "title": record.get("title").cloned().unwrap_or(Value::Null),
                "summary": record.get("summary").cloned().unwrap_or(Value::Null),
                "scope": record.get("scope").cloned().unwrap_or(Value::Null),
                "stage": record.get("stage").cloned().unwrap_or(Value::Null),
                "announcedAt": record.get("announcedAt").cloned().unwrap_or(Value::Null),
                "completedAt": record.get("completedAt").cloned().unwrap_or(Value::Null),
                "updatedAt": record.get("updatedAt").cloned().unwrap_or(Value::Null),
                "posts": posts
            })
        })
        .collect();
    json!({ "version": 1, "updatedAt": updated_at, "events": events })
}

pub fn decode_feed_cache(value: Value, now: i64) -> Option<(Vec<Value>, String)> {
    let version = value.get("version").and_then(Value::as_i64);
    if version != Some(1) {
        return None;
    }
    let updated_at = value.get("updatedAt").and_then(Value::as_str)?;
    let updated_ms = parse_time(updated_at)?;
    if updated_ms > now + 60000 {
        return None;
    }
    let events = value.get("events")?.as_array()?;
    if events.is_empty() || events.len() > 5 {
        return None;
    }
    if serde_json::to_string(&value).map(|body| body.len()).unwrap_or(usize::MAX) > 262144 {
        return None;
    }
    let records = parse_events(&Value::Array(events.clone()), &SOURCES[0]).ok()?;
    if records.is_empty() {
        return None;
    }
    Some((merge_records(vec![records], 5), updated_at.to_string()))
}

fn normalize_window(value: &Value) -> Option<Value> {
    let used_percent = value.get("usedPercent").and_then(Value::as_f64)?;
    if !used_percent.is_finite() {
        return None;
    }
    let used = used_percent.clamp(0.0, 100.0);
    let remaining = ((100.0 - used) * 10.0).round() / 10.0;
    let window_duration = value.get("windowDurationMins").and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0);
    let resets_at = value.get("resetsAt").and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0);
    Some(json!({
        "usedPercent": used,
        "remainingPercent": remaining,
        "windowDurationMins": window_duration,
        "resetsAt": resets_at
    }))
}

/// data.js normalizeAccount: converts app-server account + rateLimits payloads.
pub fn normalize_account(account: &Value, limits: &Value) -> Value {
    if account.is_null() {
        return json!({
            "status": "signed-out",
            "message": "未读取到本机 Codex 登录账号。请先在 Codex 客户端或 CLI 中登录。",
            "buckets": []
        });
    }
    let account_type = account.get("type").and_then(Value::as_str).unwrap_or("");
    if account_type != "chatgpt" && account_type != "chatgptAuthTokens" {
        return json!({
            "status": "unsupported",
            "message": "当前本机 Codex 不是 ChatGPT 登录，无法读取套餐额度。",
            "buckets": []
        });
    }
    let mut entries: Vec<(String, Value)> = Vec::new();
    if let Some(by_id) = limits.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, bucket) in by_id {
            entries.push((id.clone(), bucket.clone()));
        }
    }
    if entries.is_empty() {
        if let Some(rate_limits) = limits.get("rateLimits") {
            let id = rate_limits.get("limitId").and_then(Value::as_str).unwrap_or("codex").to_string();
            entries.push((id, rate_limits.clone()));
        }
    }
    let buckets: Vec<Value> = entries
        .into_iter()
        .take(12)
        .map(|(id, bucket)| {
            let name = text(bucket.get("limitName").unwrap_or(&Value::Null), 100);
            let name = if name.is_empty() { text(&Value::String(id.clone()), 100) } else { name };
            json!({
                "id": text(&Value::String(id), 100),
                "name": name,
                "primary": normalize_window(bucket.get("primary").unwrap_or(&Value::Null)),
                "secondary": normalize_window(bucket.get("secondary").unwrap_or(&Value::Null))
            })
        })
        .filter(|bucket| {
            bucket.get("primary").map(|v| !v.is_null()).unwrap_or(false)
                || bucket.get("secondary").map(|v| !v.is_null()).unwrap_or(false)
        })
        .collect();
    let email = text(account.get("email").unwrap_or(&Value::Null), 320);
    let identity = if email.contains('@') { email } else { "本机 Codex 账号".to_string() };
    let reset_credits = limits
        .get("rateLimitResetCredits")
        .and_then(|v| v.get("availableCount"))
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0);
    if buckets.is_empty() {
        json!({
            "status": "unavailable",
            "identity": identity,
            "plan": text(account.get("planType").unwrap_or(&Value::Null), 80),
            "buckets": [],
            "resetCredits": reset_credits,
            "message": "服务未返回可用的额度窗口，不能视为剩余 0%。"
        })
    } else {
        json!({
            "status": "ready",
            "identity": identity,
            "plan": text(account.get("planType").unwrap_or(&Value::Null), 80),
            "buckets": buckets,
            "resetCredits": reset_credits,
            "message": ""
        })
    }
}

pub fn feed_error(error: &AppError) -> String {
    if error.code == "FEED_HTTP" {
        let status = error.status.unwrap_or(0);
        return match status {
            401 | 403 => format!("网站拒绝访问（HTTP {}）", status),
            429 => "网站请求限流（HTTP 429）".to_string(),
            _ => {
                if status > 0 {
                    format!("网站响应异常（HTTP {}）", status)
                } else {
                    "网站响应异常（HTTP 未知）".to_string()
                }
            }
        };
    }
    match error.code.as_str() {
        "FEED_TIMEOUT" => "连接超时".to_string(),
        "FEED_FORMAT" => "网站数据格式已变化".to_string(),
        "FEED_EMPTY" => "网站暂未提供记录".to_string(),
        _ => "网络连接失败，请检查代理".to_string(),
    }
}

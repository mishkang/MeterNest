use crate::automations::{action_error, normalize_code, safe_message};
use crate::core::Core;
use crate::error::AppError;
use crate::util::{beijing_day, local_slot, next_local_slot, now_ms, slot_key};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use tauri::{Emitter, Manager};

const FIVE_HOURS_MS: i64 = 5 * 60 * 60 * 1000;
const CODEX_SCAN_GAP_MS: i64 = 60_000;
const DAY: i64 = 86400000;

fn terminal_day(outcome: &str) -> bool {
    matches!(outcome, "claimed" | "done" | "inactive" | "ineligible" | "own-app")
}

fn signed_in_today(record: &Value, day: &str) -> bool {
    record["day"].as_str() == Some(day) && matches!(record["outcome"].as_str(), Some("claimed" | "done"))
}

fn warmup_completed_in_window(record: &Value, end: i64, now: i64) -> bool {
    let completed = record["warmupCompletedAt"].as_i64().unwrap_or(0);
    end > now && completed > 0 && completed <= now && completed >= end - FIVE_HOURS_MS
}

/// scheduler.js windowOf: the five-hour primary window with its sample.
pub fn window_of(account: &Value) -> Option<Value> {
    if account.get("status").and_then(Value::as_str) != Some("ready") {
        return None;
    }
    let buckets = account.get("buckets")?.as_array()?;
    let bucket = buckets.iter().find(|b| b.get("id").and_then(Value::as_str) == Some("codex")).or_else(|| buckets.first())?;
    let window = bucket.get("primary")?;
    if window.get("windowDurationMins").and_then(Value::as_f64) != Some(300.0) {
        return None;
    }
    let resets_at = window.get("resetsAt").and_then(Value::as_f64)?;
    if !resets_at.is_finite() || resets_at <= 0.0 {
        return None;
    }
    let used = window.get("usedPercent").and_then(Value::as_f64).unwrap_or(100.0 - window.get("remainingPercent").and_then(Value::as_f64).unwrap_or(0.0));
    let mut result = json!({ "end": (resets_at * 1000.0) as i64, "used": used });
    if let Some(sample) = account.get("quotaSample") {
        result["sample"] = sample.clone();
    }
    Some(result)
}

pub fn weekly_window_of(account: &Value) -> Option<Value> {
    if account.get("status").and_then(Value::as_str) != Some("ready") {
        return None;
    }
    let buckets = account.get("buckets")?.as_array()?;
    let bucket = buckets.iter().find(|b| b.get("id").and_then(Value::as_str) == Some("codex")).or_else(|| buckets.first())?;
    for key in ["primary", "secondary"] {
        if let Some(window) = bucket.get(key) {
            if window.get("windowDurationMins").and_then(Value::as_f64) == Some(10080.0) {
                if let Some(resets_at) = window.get("resetsAt").and_then(Value::as_f64) {
                    if resets_at.is_finite() && resets_at > 0.0 {
                        return Some(json!({ "end": (resets_at * 1000.0) as i64 }));
                    }
                }
            }
        }
    }
    None
}

fn duration_label(minutes: f64) -> String {
    if minutes <= 0.0 {
        return "未知".to_string();
    }
    if minutes.rem_euclid(10080.0) == 0.0 {
        format!("{} 周", (minutes / 10080.0) as i64)
    } else if minutes.rem_euclid(1440.0) == 0.0 {
        format!("{} 天", (minutes / 1440.0) as i64)
    } else if minutes.rem_euclid(60.0) == 0.0 {
        format!("{} 小时", (minutes / 60.0) as i64)
    } else {
        format!("{} 分钟", minutes)
    }
}

fn window_support(account: &Value) -> Option<Value> {
    if account.get("status").and_then(Value::as_str) != Some("ready") {
        return None;
    }
    let buckets = account.get("buckets")?.as_array()?;
    let mut durations = Vec::new();
    let mut primary_duration = None;
    for bucket in buckets {
        for key in ["primary", "secondary"] {
            let Some(duration) = bucket
                .get(key)
                .and_then(|window| window.get("windowDurationMins"))
                .and_then(Value::as_f64)
                .filter(|duration| duration.is_finite() && *duration > 0.0)
            else {
                continue;
            };
            if key == "primary" && primary_duration.is_none() {
                primary_duration = Some(duration);
            }
            durations.push(duration);
        }
    }
    // Match the windows actually used by the scheduler, not any arbitrary
    // primary/secondary duration in the response.
    let supported = window_of(account).is_some() || weekly_window_of(account).is_some();
    let duration = primary_duration.or_else(|| durations.first().copied());
    let message = if supported {
        String::new()
    } else {
        match duration {
            Some(duration) => format!("当前套餐仅提供 {} 额度窗口，不支持窗口到期预热。", duration_label(duration)),
            None => "未读取到 5 小时或每周额度窗口，当前套餐不支持窗口到期预热。".to_string(),
        }
    };
    Some(json!({
        "supported": supported,
        "durationMinutes": duration,
        "message": message
    }))
}

pub fn codex_window_supported(core: &Core) -> bool {
    core.scheduler.codex_window_support["supported"].as_bool().unwrap_or(true)
}

/// Use the timestamp captured BEFORE the quota read; compare in seconds.
fn is_idle_window(window: Option<&Value>, time: i64) -> bool {
    let Some(window) = window else { return false };
    if window.get("used").and_then(Value::as_f64) != Some(0.0) {
        return false;
    }
    let started_at = window
        .get("sample")
        .and_then(|sample| sample.get("startedAt"))
        .and_then(Value::as_i64)
        .unwrap_or(time);
    let end = window.get("end").and_then(Value::as_i64).unwrap_or(0);
    end.div_euclid(1000) - started_at.div_euclid(1000) >= 5 * 3600
}

pub struct Scheduler {
    pub disposed: bool,
    pub started: bool,
    pub startup_due: i64,
    pub timer_gen: u64,
    timer_due_at: Option<i64>,
    pub ticking: bool,
    pub preferences: Value,
    pub epochs: HashMap<String, u64>,
    pub database: Value,
    pub busy: HashMap<String, bool>,
    pub busy_manual: HashMap<String, bool>,
    pub state: Value,
    pub known_keys: HashMap<String, Option<String>>,
    pub last_scan_at: HashMap<String, i64>,
    pub codex_window_support: Value,
    pub display_keys: HashMap<String, Option<String>>,
    pub quota_version: Option<String>,
    codex_ready_account: Option<String>,
    codex_ready_scan: bool,
    codex_checked_windows: Vec<String>,
    pub restoring_workbuddy: bool,
    workbuddy_identity_stamp: Option<String>,
    pub status_checks: Vec<(String, Value)>,
}

impl Scheduler {
    pub fn new(preferences: Value, history: Value) -> Scheduler {
        let mut database = history;
        if !database.is_object() {
            database = json!({});
        }
        for provider in ["codex", "workbuddy"] {
            if !database.get(provider).map(|value| value.is_object()).unwrap_or(false) {
                database[provider] = json!({});
            }
        }
        let epochs = HashMap::from([("codex".to_string(), 0), ("workbuddy".to_string(), 0)]);
        Scheduler {
            disposed: false,
            started: false,
            startup_due: 0,
            timer_gen: 0,
            timer_due_at: None,
            ticking: false,
            preferences,
            epochs,
            database,
            busy: HashMap::new(),
            busy_manual: HashMap::new(),
            state: json!({
                "codex": { "running": false, "outcome": "idle" },
                "workbuddy": { "running": false, "outcome": "idle" }
            }),
            known_keys: HashMap::new(),
            last_scan_at: HashMap::new(),
            codex_window_support: json!({ "supported": true, "durationMinutes": null, "message": "" }),
            display_keys: HashMap::new(),
            quota_version: None,
            codex_ready_account: None,
            codex_ready_scan: false,
            codex_checked_windows: Vec::new(),
            restoring_workbuddy: false,
            workbuddy_identity_stamp: None,
            status_checks: Vec::new(),
        }
    }

    fn plan_timer(&mut self, deadline: i64) -> Option<u64> {
        if self.timer_due_at.is_some_and(|armed| armed <= deadline) {
            return None;
        }
        self.timer_due_at = Some(deadline);
        self.timer_gen += 1;
        Some(self.timer_gen)
    }

    fn take_timer(&mut self, generation: u64) -> bool {
        if self.timer_gen != generation || self.timer_due_at.take().is_none() {
            return false;
        }
        true
    }

    fn select_codex_account(&mut self, key: &str) -> bool {
        let previous = self.display_keys.get("codex").and_then(Option::as_deref);
        if previous == Some(key) {
            return false;
        }
        if previous.is_some() {
            *self.epochs.get_mut("codex").unwrap() += 1;
            self.last_scan_at.remove("codex");
        }
        self.display_keys.insert("codex".into(), Some(key.to_owned()));
        self.known_keys.remove("codex");
        self.codex_ready_account = None;
        self.codex_ready_scan = false;
        self.codex_checked_windows.clear();
        clear_codex_state(self);
        true
    }

    fn note_ready_codex_quota(&mut self, key: &str) {
        if self.codex_ready_account.as_deref() != Some(key) {
            self.codex_ready_account = Some(key.to_owned());
            self.codex_ready_scan = true;
        }
    }

    fn codex_scan_at(&self, windows: &[String], now: i64) -> i64 {
        if self.codex_ready_scan || windows.iter().any(|key| !self.codex_checked_windows.contains(key)) {
            return now;
        }
        self.last_scan_at.get("codex").copied().unwrap_or(0) + CODEX_SCAN_GAP_MS
    }
}

fn config(core: &Core) -> Value {
    crate::prefs::normalize_automation(core.scheduler.preferences.get("automation").cloned().unwrap_or(Value::Null))
}

fn scheduler_enabled(core: &Core, provider: &str) -> bool {
    crate::prefs::normalize_enabled_providers(core.scheduler.preferences.get("enabledProviders").cloned().unwrap_or_default())
        .get(provider)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn automatic(core: &Core, provider: &str) -> bool {
    let config = config(core);
    if provider == "codex" {
        (codex_window_supported(core) && config["codex"]["onReset"].as_bool().unwrap_or(false))
            || config["codex"]["daily"].as_bool().unwrap_or(false)
    } else {
        config["workbuddy"]["daily"].as_bool().unwrap_or(false)
    }
}

fn valid(core: &Core, provider: &str, epoch: u64, manual: bool) -> bool {
    !core.scheduler.disposed
        && scheduler_enabled(core, provider)
        && core.scheduler.epochs.get(provider).copied().unwrap_or(0) == epoch
        && (manual || automatic(core, provider))
}

pub fn snapshot(core: &Core) -> Value {
    let mut result = Map::new();
    for provider in ["codex", "workbuddy"] {
        let mut entry = core.scheduler.state[provider].clone();
        entry["paused"] = json!(!scheduler_enabled(core, provider));
        if provider == "codex" {
            if let Some(key) = core.scheduler.display_keys.get("codex").and_then(|key| key.as_deref()) {
                let history = record(core, "codex", key);
                let completed = history["warmupCompletedAt"].as_i64().filter(|time| *time > 0);
                entry["previousWarmupCompletedAt"] = history["previousWarmupCompletedAt"].clone();
                entry["warmupCompletedAt"] = json!(completed);
                entry["nextQuotaRecoveryAt"] = history["nextQuotaRecoveryAt"].clone();
            }
            let support = &core.scheduler.codex_window_support;
            entry["windowSupported"] = support.get("supported").cloned().unwrap_or(json!(true));
            entry["windowDurationMinutes"] = support.get("durationMinutes").cloned().unwrap_or(Value::Null);
            entry["windowMessage"] = support.get("message").cloned().unwrap_or(json!(""));
        }
        result.insert(provider.to_string(), entry);
    }
    Value::Object(result)
}

fn next_quota_recovery_at(account: &Value, now: i64) -> Option<i64> {
    let five_hour = window_of(account).and_then(|window| window["end"].as_i64());
    let weekly = weekly_window_of(account).and_then(|window| window["end"].as_i64());
    nearest_recovery_at(five_hour, weekly, now)
}

fn nearest_recovery_at(five_hour: Option<i64>, weekly: Option<i64>, after: i64) -> Option<i64> {
    [five_hour, weekly].into_iter().flatten().filter(|end| *end > after).min()
}

fn publish(core: &mut Core) {
    if core.scheduler.disposed {
        return;
    }
    let state = snapshot(core);
    crate::automations::system_wake::sync(core);
    let mut payload = state.clone();
    payload["wake"] = crate::automations::system_wake::snapshot(core);
    if core.settings.created {
        if let Some(window) = core.app.get_webview_window("settings") {
            let _ = window.emit_to("settings", "meternest-automation-updated", &payload);
        }
    }
}

fn record(core: &Core, provider: &str, key: &str) -> Value {
    if key.len() != 64 || !key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        return json!({});
    }
    let raw = core
        .scheduler
        .database
        .get(provider)
        .and_then(|p| p.get(key))
        .cloned()
        .unwrap_or(Value::Null);
    let number = |value: &Value, field: &str| -> i64 {
        value.get(field).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as i64).unwrap_or(0)
    };
    let outcome = raw.get("outcome").and_then(Value::as_str).unwrap_or("idle");
    let warmup_at = if number(&raw, "warmupAt") > 0 {
        number(&raw, "warmupAt")
    } else if matches!(outcome, "sent" | "confirmed" | "uncertain") && number(&raw, "lastAt") > 0 {
        number(&raw, "lastAt")
    } else {
        0
    };
    let last_successful = if number(&raw, "lastSuccessfulWarmupAt") > 0 {
        number(&raw, "lastSuccessfulWarmupAt")
    } else if matches!(outcome, "confirmed" | "sent") {
        warmup_at.max(number(&raw, "lastAt"))
    } else {
        0
    };
    let completed = number(&raw, "warmupCompletedAt").max(last_successful);
    let next_recovery = match raw.get("nextQuotaRecoveryAt") {
        Some(value) => value.as_i64().filter(|end| *end > 0),
        None => nearest_recovery_at(
            Some(number(&raw, "windowEnd")),
            Some(number(&raw, "weeklyWindowEnd")),
            if completed > 0 { completed } else { now_ms() },
        ),
    };
    json!({
        "keys": raw.get("keys").and_then(Value::as_array).map(|items| {
            let mut filtered: Vec<Value> = items.iter().filter(|k| k.is_string()).cloned().collect();
            if filtered.len() > 64 { filtered = filtered.split_off(filtered.len() - 64); }
            filtered
        }).unwrap_or_default(),
        "recoveryAt": if number(&raw, "recoveryAt") > 0 { number(&raw, "recoveryAt") } else { number(&raw, "windowEnd") },
        "windowEnd": number(&raw, "windowEnd"),
        "resetDue": number(&raw, "resetDue"),
        "weeklyWindowEnd": number(&raw, "weeklyWindowEnd"),
        "weeklyResetDue": number(&raw, "weeklyResetDue"),
        "retryAt": number(&raw, "retryAt"),
        "attempts": raw.get("attempts").and_then(Value::as_i64).filter(|v| *v >= 0).unwrap_or(0),
        "warmupAt": warmup_at,
        "previousWarmupCompletedAt": number(&raw, "previousWarmupCompletedAt"),
        "warmupCompletedAt": completed,
        "nextQuotaRecoveryAt": next_recovery,
        "lastSuccessfulWarmupAt": last_successful,
        "idleDue": raw.get("idleDue").and_then(Value::as_str).unwrap_or("").to_string(),
        "trigger": raw.get("trigger").and_then(Value::as_str).unwrap_or("").to_string(),
        "lastStartedAt": number(&raw, "lastStartedAt"),
        "attemptKey": raw.get("attemptKey").and_then(Value::as_str).unwrap_or("").to_string(),
        "day": raw.get("day").and_then(Value::as_str).unwrap_or("").to_string(),
        "outcome": outcome.to_string(),
        "errorCode": if crate::automations::MESSAGES.iter().any(|(code, _)| raw.get("errorCode").and_then(Value::as_str) == Some(code)) {
            raw.get("errorCode").and_then(Value::as_str).unwrap_or("").to_string()
        } else if outcome == "error" {
            "ACTION_FAILED".to_string()
        } else {
            String::new()
        },
        "lastAt": number(&raw, "lastAt"),
        "credit": raw.get("credit").and_then(Value::as_f64).filter(|v| v.is_finite())
    })
}

fn save(core: &mut Core, provider: &str, key: &str, value: Value) -> Result<(), AppError> {
    let mut next = core.scheduler.database.clone();
    next[provider][key] = value;
    let mut entries: Vec<(String, &Value)> = next[provider]
        .as_object()
        .map(|object| object.iter().filter(|(k, _)| k.len() == 64 && k.chars().all(|c| c.is_ascii_hexdigit())).map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();
    let recency = |(entry_key, value): &(String, &Value)| {
        let last = value["lastAt"].as_i64().unwrap_or(0);
        let started = value["lastStartedAt"].as_i64().unwrap_or(0);
        (entry_key == key, last.max(started))
    };
    entries.sort_by(|a, b| recency(b).cmp(&recency(a)));
    entries.truncate(32);
    let rebuilt: Map<String, Value> = entries.into_iter().map(|(k, v)| (k, v.clone())).collect();
    next[provider] = Value::Object(rebuilt);
    core.store.set("automationHistory", next.clone()).map_err(|_| action_error("STATE_WRITE_FAILED"))?;
    core.scheduler.database = next;
    Ok(())
}

fn finish_state(core: &mut Core, provider: &str, result: &Value, at: i64) {
    let failed = result.get("outcome").and_then(Value::as_str) == Some("error");
    let outcome = result.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string();
    let trigger = result.get("trigger").and_then(Value::as_str).unwrap_or("").to_string();
    let error_code = result.get("errorCode").and_then(Value::as_str).unwrap_or("ACTION_FAILED").to_string();
    let message = if failed || outcome == "uncertain" { safe_message(&error_code) } else { String::new() };
    let attempts = result.get("attempts").and_then(Value::as_i64).unwrap_or(0);
    let retry = if failed { retry_time(core, provider, result, now_ms()).unwrap_or(0) } else { 0 };
    let retry_stopped = failed
        && scheduler_enabled(core, provider)
        && automatic(core, provider)
        && trigger != "manual"
        && attempts >= 3;
    let credit = result.get("credit").and_then(Value::as_f64);
    let state = &mut core.scheduler.state[provider];
    state["outcome"] = json!(outcome);
    state["trigger"] = json!(trigger);
    state["at"] = json!(at);
    state["message"] = json!(message);
    state["retryAt"] = json!(if failed && retry > 0 { Some(retry) } else { None });
    state["retryStopped"] = json!(retry_stopped);
    state["credit"] = json!(credit);
}

fn clear_codex_state(scheduler: &mut Scheduler) {
    let state = &mut scheduler.state["codex"];
    // The old identity check still owns this flag until its callback returns.
    let checking = state["checking"].as_bool().unwrap_or(false);
    *state = json!({
        "checking": checking,
        "outcome": "idle", "trigger": "", "at": null, "message": "", "credit": null, "retryAt": null, "retryStopped": false
    });
}

fn select_codex_account(core: &mut Core, key: &str) -> bool {
    let changed = core.scheduler.select_codex_account(key);
    if changed {
        let mut history = record(core, "codex", key);
        if history["outcome"] == "error"
            && matches!(history["errorCode"].as_str(), Some("AUTH_REQUIRED" | "AUTH_EXPIRED" | "SESSION_CHANGED"))
        {
            // The previous login failure no longer applies to this session.
            history["retryAt"] = json!(0);
            history["attempts"] = json!(0);
            let _ = save(core, "codex", key, history);
        }
    }
    changed
}

fn due_codex_keys(core: &Core, r: &Value, time: i64) -> Vec<String> {
    let config = config(core);
    let mut keys: Vec<String> = Vec::new();
    if codex_window_supported(core) && config["codex"]["onReset"].as_bool().unwrap_or(false) {
        if let Some(idle_due) = r.get("idleDue").and_then(Value::as_str) {
            if !idle_due.is_empty() {
                keys.push(idle_due.to_string());
            }
        }
        let end = {
            let reset = r.get("resetDue").and_then(Value::as_i64).unwrap_or(0);
            let window = r.get("windowEnd").and_then(Value::as_i64).unwrap_or(0);
            if reset > 0 { reset } else { window }
        };
        let weekly_end = {
            let reset = r.get("weeklyResetDue").and_then(Value::as_i64).unwrap_or(0);
            let window = r.get("weeklyWindowEnd").and_then(Value::as_i64).unwrap_or(0);
            if reset > 0 { reset } else { window }
        };
        if end > 0 && end <= time {
            keys.push(format!("reset:{}", end));
        }
        if weekly_end > 0 && weekly_end <= time {
            keys.push(format!("weekly:{}", weekly_end));
        }
    }
    if config["codex"]["daily"].as_bool().unwrap_or(false) {
        if let Some(times) = config["codex"]["times"].as_array() {
            for clock in times.iter().filter_map(Value::as_str) {
                if let Some(slot) = local_slot(time, clock) {
                    if time >= slot && time - slot <= 3600000 {
                        keys.push(format!("time:{}", slot_key(slot)));
                    }
                }
            }
        }
    }
    let existing: Vec<String> = r
        .get("keys")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    keys.retain(|key| !existing.contains(key));
    keys
}

fn retry_time(core: &Core, provider: &str, r: &Value, time: i64) -> Option<i64> {
    if !scheduler_enabled(core, provider) || !automatic(core, provider) {
        return None;
    }
    if r.get("outcome").and_then(Value::as_str) != Some("error") || r.get("trigger").and_then(Value::as_str) == Some("manual") {
        return None;
    }
    let attempts = r.get("attempts").and_then(Value::as_i64).unwrap_or(0);
    let retry_at = r.get("retryAt").and_then(Value::as_i64).unwrap_or(0);
    if !(attempts > 0 && attempts < 3 && retry_at > time) {
        return None;
    }
    if provider == "workbuddy" {
        return if r.get("day").and_then(Value::as_str) == Some(beijing_day(time).as_str()) { Some(retry_at) } else { None };
    }
    let due = due_codex_keys(core, r, time);
    let joined = due.join("|");
    if !due.is_empty() && r.get("attemptKey").and_then(Value::as_str) == Some(joined.as_str()) {
        Some(retry_at)
    } else {
        None
    }
}

fn rotate_warmup_times(record: &mut Value, completed: i64, next_recovery: Option<i64>) {
    let previous = record["warmupCompletedAt"].as_i64().unwrap_or(0);
    if completed <= previous {
        return;
    }
    record["previousWarmupCompletedAt"] = json!(previous);
    record["warmupCompletedAt"] = json!(completed);
    record["nextQuotaRecoveryAt"] = json!(next_recovery);
}

fn observe_conversation_window(record: &mut Value, window: &Value, now: i64, next_recovery: Option<i64>) {
    let Some(end) = window["end"].as_i64().filter(|end| *end > FIVE_HOURS_MS) else {
        return;
    };
    let started = end - FIVE_HOURS_MS;
    if started <= now && window["used"].as_f64().is_some_and(|used| used > 0.0) {
        // A fixed window end yields the same start on every refresh. An app
        // warmup that already completed in this window is newer than its start.
        rotate_warmup_times(record, started, next_recovery);
    }
}

fn codex_reset_due(history: &Value, window: &Value, now: i64) -> i64 {
    let window_end = window["end"].as_i64().unwrap_or(0);
    if window_end <= now {
        return window_end;
    }
    let used = window["used"].as_f64().unwrap_or(0.0);
    if used > 0.0 || !window["sample"].is_null() && !is_idle_window(Some(window), now) {
        return 0;
    }
    let prior_end = history["windowEnd"].as_i64().unwrap_or(0);
    let processed = history["keys"].as_array().is_some_and(|keys| keys.contains(&json!(format!("reset:{prior_end}"))));
    if prior_end > 0 && prior_end <= now && !processed {
        return prior_end;
    }
    history["resetDue"].as_i64().unwrap_or(0)
}

fn observe(core: &mut Core, key: &str, account: &Value) -> Value {
    let window = window_of(account);
    let weekly = weekly_window_of(account);
    let mut r = record(core, "codex", key);
    let time = now_ms();
    let before = serde_json::to_string(&r).unwrap_or_default();
    let next_recovery = nearest_recovery_at(
        window.as_ref().and_then(|window| window["end"].as_i64()),
        weekly.as_ref().and_then(|window| window["end"].as_i64()),
        time,
    );
    if let Some(window) = &window {
        if !core.scheduler.busy.get("codex").copied().unwrap_or(false) {
            observe_conversation_window(&mut r, window, time, next_recovery);
        }
        let window_end = window.get("end").and_then(Value::as_i64).unwrap_or(0);
        let due = codex_reset_due(&r, window, time);
        r["windowEnd"] = json!(window_end);
        r["resetDue"] = json!(due);
        r["recoveryAt"] = json!(if window_end > time { window_end } else { 0 });
    }
    if let Some(weekly) = &weekly {
        let weekly_end = weekly.get("end").and_then(Value::as_i64).unwrap_or(0);
        let existing_keys: Vec<String> = r
            .get("keys")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let prior_weekly_end = r.get("weeklyWindowEnd").and_then(Value::as_i64).unwrap_or(0);
        if prior_weekly_end > 0 && time >= prior_weekly_end && !existing_keys.contains(&format!("weekly:{}", prior_weekly_end)) {
            r["weeklyResetDue"] = json!(prior_weekly_end);
        }
        if weekly_end <= time && !existing_keys.contains(&format!("weekly:{}", weekly_end)) {
            r["weeklyResetDue"] = json!(weekly_end);
        }
        r["weeklyWindowEnd"] = json!(weekly_end);
    }
    if r["nextQuotaRecoveryAt"].is_null() {
        r["nextQuotaRecoveryAt"] = json!(next_recovery);
    }
    if before != serde_json::to_string(&r).unwrap_or_default() {
        let _ = save(core, "codex", key, r.clone());
    }
    r
}

fn epoch_of(core: &Core, provider: &str) -> u64 {
    core.scheduler.epochs.get(provider).copied().unwrap_or(0)
}

fn quota_error_record(core: &mut Core, provider: &str, epoch: u64, key: &str, keys: &[String], error: AppError) {
    let mut failed = record(core, provider, key);
    if error.code != "STATE_WRITE_FAILED" {
        if provider == "codex" && error.code != "CODEX_UNCERTAIN" {
            if let Some(list) = failed.get_mut("keys").and_then(Value::as_array_mut) {
                list.retain(|item| !keys.iter().any(|key| item.as_str() == Some(key.as_str())));
            }
        }
        if provider == "codex" && error.code == "CODEX_UNCERTAIN" {
            failed["warmupAt"] = json!(now_ms());
        }
        if error.code == "CODEX_ENDPOINT_UNSUPPORTED" {
            failed["attempts"] = json!(3);
            failed["retryAt"] = json!(0);
        }
        failed["outcome"] = json!(if error.code == "CODEX_UNCERTAIN" { "uncertain" } else { "error" });
        failed["lastAt"] = json!(now_ms());
        failed["errorCode"] = json!(normalize_code(&error.code));
        let _ = save(core, provider, key, failed.clone());
    }
    let manual = core.scheduler.busy_manual.get(provider).copied().unwrap_or(false);
    if valid(core, provider, epoch, manual) {
        notify_action(core, provider, manual, safe_message(&error.code));
        if error.code == "STATE_WRITE_FAILED" {
            failed["outcome"] = json!("error");
        }
        failed["errorCode"] = json!(normalize_code(&error.code));
        finish_state(core, provider, &failed, now_ms());
    }
    perform_end(core, provider);
}

fn notify_action(core: &Core, provider: &str, manual: bool, message: String) {
    if !config(core)[provider]["notify"].as_bool().unwrap_or(false) {
        return;
    }
    let title = match (provider, manual) {
        ("codex", true) => "Codex 手动预热",
        ("codex", false) => "Codex 自动预热",
        ("workbuddy", true) => "WorkBuddy 手动签到",
        _ => "WorkBuddy 自动签到",
    };
    if core.packaged {
        let language = crate::i18n::resolve(&core.prefs());
        crate::macos::notifications::show(
            &core.app,
            crate::i18n::text(language, title),
            crate::i18n::text(language, &message).to_string(),
        );
    }
}

fn perform_end(core: &mut Core, provider: &str) {
    core.scheduler.busy.insert(provider.to_string(), false);
    core.scheduler.busy_manual.insert(provider.to_string(), false);
    core.scheduler.state[provider]["running"] = json!(false);
    publish(core);
    arm(core);
}

/// scheduler.js perform(): reserve → quota check → warmup → confirm.
pub fn perform(core: &mut Core, provider: &str, key: String, keys: Vec<String>, manual: bool) {
    if core.scheduler.busy.get(provider).copied().unwrap_or(false) {
        return;
    }
    let epoch = epoch_of(core, provider);
    let started_at = now_ms();
    let day = beijing_day(started_at);
    let mut r = record(core, provider, &key);
    if !manual && !valid(core, provider, epoch, false) {
        return;
    }
    if provider == "workbuddy" {
        let r_day = r.get("day").and_then(Value::as_str).unwrap_or("").to_string();
        let outcome = r.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string();
        if signed_in_today(&r, &day) {
            if !manual {
                if r["trigger"].as_str() == Some("daily") {
                    return;
                }
                r["trigger"] = json!("daily");
                if let Err(error) = save(core, provider, &key, r.clone()) {
                    eprintln!("Failed to save cached WorkBuddy check-in: {error:?}");
                    return;
                }
            }
            let last_at = r["lastAt"].as_i64().unwrap_or(0);
            finish_state(core, provider, &r, last_at);
            notify_action(core, provider, manual, "今天已经签到".to_string());
            publish(core);
            return;
        }
        if !manual && r_day == day && terminal_day(&outcome) {
            return;
        }
    }
    let existing_keys: Vec<String> = r
        .get("keys")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if !manual && provider == "codex" && keys.iter().all(|k| existing_keys.contains(k)) {
        return;
    }
    if provider == "codex" && warmup_completed_in_window(&r, r["windowEnd"].as_i64().unwrap_or(0), started_at) {
        finish_cached_warmup(core, &key, &keys, manual, r);
        return;
    }
    let previous = r.clone();
    core.scheduler.busy.insert(provider.to_string(), true);
    core.scheduler.busy_manual.insert(provider.to_string(), manual);
    core.scheduler.state[provider]["running"] = json!(true);
    core.scheduler.state[provider]["message"] = json!("");
    publish(core);
    // Reserve before any side effect.
    let trigger = if manual {
        "manual".to_string()
    } else if provider == "workbuddy" {
        "daily".to_string()
    } else if keys.iter().any(|k| k.starts_with("idle:")) {
        "startup-idle".to_string()
    } else {
        keys.join("|")
    };
    r["errorCode"] = json!("");
    r["lastStartedAt"] = json!(started_at);
    r["trigger"] = json!(trigger);
    r["retryAt"] = json!(started_at + if provider == "codex" { 300000 } else { 1800000 });
    r["attempts"] = json!(r.get("attempts").and_then(Value::as_i64).unwrap_or(0) + 1);
    if provider == "codex" {
        let mut merged: Vec<Value> = existing_keys.iter().map(|k| json!(k)).collect();
        for key in &keys {
            if !existing_keys.contains(key) {
                merged.push(json!(key));
            }
        }
        if merged.len() > 64 {
            merged = merged.split_off(merged.len() - 64);
        }
        r["keys"] = Value::Array(merged);
    } else {
        r["day"] = json!(day);
    }
    if let Err(error) = save(core, provider, &key, r) {
        quota_error_record(core, provider, epoch, &key, &keys, error);
        return;
    }
    if provider == "codex" {
        spawn_codex_phase(core, epoch, key, keys, manual, previous);
    } else {
        spawn_workbuddy_checkin(core, epoch, key, keys, manual);
    }
}

fn finish_cached_warmup(core: &mut Core, key: &str, keys: &[String], manual: bool, mut record: Value) {
    let mut processed = record["keys"].as_array().cloned().unwrap_or_default();
    for trigger in keys {
        let trigger = json!(trigger);
        if !processed.contains(&trigger) {
            processed.push(trigger);
        }
    }
    if processed.len() > 64 {
        processed.drain(..processed.len() - 64);
    }
    record["keys"] = json!(processed);
    if let Err(error) = save(core, "codex", key, record.clone()) {
        eprintln!("Failed to save cached Codex warmup: {error:?}");
        return;
    }
    let last_at = record["lastAt"].as_i64().unwrap_or(0);
    finish_state(core, "codex", &record, last_at);
    core.scheduler.state["codex"]["message"] = json!("当前窗口已完成预热");
    notify_action(core, "codex", manual, "当前窗口已完成预热".to_string());
}

fn spawn_codex_phase(core: &mut Core, epoch: u64, key: String, keys: Vec<String>, manual: bool, previous: Value) {
    let handle = core.handle.clone();
    let proxy = core.proxy_url();
    handle.clone().spawn_task(async move {
        let result = crate::monitor::local_client::read_codex_for_account(&proxy, &key).await;
        handle.post_job(Box::new(move |core: &mut Core| {
            if !valid(core, "codex", epoch, manual) {
                // No request was sent. Release this reservation without replacing newer quota observations.
                let _ = release_codex_reservation(core, &key, &previous);
                perform_end(core, "codex");
                return;
            }
            match result {
                Ok(quota) => codex_after_quota(core, epoch, key, keys, manual, quota, previous),
                Err(error) => quota_error_record(core, "codex", epoch, &key, &keys, error),
            }
        }));
    });
}

fn release_codex_reservation(core: &mut Core, key: &str, previous: &Value) -> Result<(), AppError> {
    let mut current = record(core, "codex", key);
    for field in ["errorCode", "lastStartedAt", "trigger", "retryAt", "attempts", "keys"] {
        current[field] = previous[field].clone();
    }
    save(core, "codex", key, current)
}

fn codex_skip_until(quota: &Value, window: Option<&Value>, keys: &[String], manual: bool, now: i64) -> Result<Option<i64>, AppError> {
    let daily = keys.iter().any(|key| key.starts_with("time:"));
    match quota["status"].as_str() {
        Some("ready") => {}
        Some("unavailable") if daily => return Ok(None),
        Some("unavailable") => return Err(action_error("CODEX_QUOTA_UNAVAILABLE")),
        _ => return Err(action_error("AUTH_REQUIRED")),
    }
    if manual || daily {
        return Ok(None);
    }
    if keys.iter().any(|key| key.starts_with("weekly:")) {
        return weekly_window_of(quota).map(|_| None).ok_or_else(|| action_error("CODEX_QUOTA_UNAVAILABLE"));
    }
    let window = window
        .filter(|window| window["used"].as_f64().is_some_and(f64::is_finite))
        .ok_or_else(|| action_error("CODEX_QUOTA_UNAVAILABLE"))?;
    let end = window["end"].as_i64().unwrap_or(0);
    Ok((end > now && !is_idle_window(Some(window), now)).then_some(end))
}

fn codex_after_quota(core: &mut Core, epoch: u64, key: String, keys: Vec<String>, manual: bool, quota: Value, previous: Value) {
    let window = window_of(&quota);
    if let Some(support) = window_support(&quota) {
        core.scheduler.codex_window_support = support;
    }
    if let Some(window) = &window {
        let now = now_ms();
        let mut history = previous.clone();
        let end = window["end"].as_i64().unwrap_or(0);
        let weekly_end = weekly_window_of(&quota).and_then(|weekly| weekly["end"].as_i64());
        observe_conversation_window(&mut history, window, now, nearest_recovery_at(Some(end), weekly_end, now));
        if warmup_completed_in_window(&history, end, now) {
            history["windowEnd"] = json!(end);
            finish_cached_warmup(core, &key, &keys, manual, history);
            perform_end(core, "codex");
            return;
        }
    }
    match codex_skip_until(&quota, window.as_ref(), &keys, manual, now_ms()) {
        Ok(Some(recovery_at)) => {
            let result = json!({ "outcome": "skipped", "recoveryAt": recovery_at });
            codex_complete(core, epoch, key, keys, manual, result);
            return;
        }
        Ok(None) => {}
        Err(error) => {
            quota_error_record(core, "codex", epoch, &key, &keys, error);
            return;
        }
    }
    if !manual && (crate::automations::system_wake::waiting_for_scheduled_resume(core)
        || crate::automations::system_wake::settling_deadline(core, now_ms()).is_some())
    {
        if let Err(error) = release_codex_reservation(core, &key, &previous) {
            quota_error_record(core, "codex", epoch, &key, &keys, error);
            return;
        }
        // A new scan will read the current account and quota again after waking.
        core.scheduler.codex_ready_scan = true;
        perform_end(core, "codex");
        return;
    }
    // warmup phase
    let handle = core.handle.clone();
    let proxy = core.proxy_url();
    let expected = key.clone();
    handle.clone().spawn_task(async move {
        let result = crate::automations::codex_client::warmup(&expected, if proxy.is_empty() { None } else { Some(proxy.as_str()) }).await;
        handle.post_job(Box::new(move |core: &mut Core| {
            match result {
                Ok(outcome) => {
                    let mut result = json!({ "outcome": "sent", "accountKey": outcome.account_key });
                    result["warmupAt"] = json!(now_ms());
                    if result.get("accountKey").and_then(Value::as_str) != Some(key.as_str()) {
                        quota_error_record(core, "codex", epoch, &key, &keys, action_error("SESSION_CHANGED"));
                        return;
                    }
                    result["lastSuccessfulWarmupAt"] = json!(now_ms());
                    result["recoveryAt"] = json!(0);
                    if !valid(core, "codex", epoch, manual) {
                        codex_complete(core, epoch, key, keys, manual, result);
                        return;
                    }
                    // A completed response is not proof the quota window started.
                    let handle = core.handle.clone();
                    let proxy = core.proxy_url();
                    let key_after = key.clone();
                    handle.clone().spawn_task(async move {
                        let read = crate::monitor::local_client::read_codex_for_account(&proxy, &key_after).await;
                        handle.post_job(Box::new(move |core: &mut Core| {
                            match read {
                                Ok(after) if valid(core, "codex", epoch, manual) => {
                                    let mut confirmed = false;
                                    let recovery = {
                                        let window = window_of(&after);
                                        match &window {
                                            Some(window) => {
                                                let end = window.get("end").and_then(Value::as_i64).unwrap_or(0);
                                                let used = window.get("used").and_then(Value::as_f64).unwrap_or(0.0);
                                                confirmed = end > now_ms() && used > 0.0;
                                                if end > now_ms() { end } else { 0 }
                                            }
                                            None => 0,
                                        }
                                    };
                                    observe(core, &key_after, &after);
                                    result["recoveryAt"] = json!(recovery);
                                    result["nextQuotaRecoveryAt"] = json!(next_quota_recovery_at(&after, now_ms()));
                                    result["outcome"] = json!(if confirmed { "confirmed" } else { "sent" });
                                    codex_complete(core, epoch, key_after, keys, manual, result);
                                }
                                _ => {
                                    codex_complete(core, epoch, key, keys, manual, result);
                                }
                            }
                        }));
                    });
                }
                Err(error) => {
                    quota_error_record(core, "codex", epoch, &key, &keys, error);
                }
            }
        }));
    });
}

fn codex_complete(core: &mut Core, epoch: u64, key: String, keys: Vec<String>, manual: bool, mut result: Value) {
    let mut r = record(core, "codex", &key);
    let next_recovery = result.as_object_mut().and_then(|fields| fields.remove("nextQuotaRecoveryAt")).and_then(|value| value.as_i64());
    if let Some(completed) = result["lastSuccessfulWarmupAt"].as_i64() {
        rotate_warmup_times(&mut r, completed, next_recovery);
    }
    for (field, value) in result.as_object().into_iter().flatten() {
        r[field] = value.clone();
    }
    r["errorCode"] = json!("");
    r["lastAt"] = json!(now_ms());
    r["retryAt"] = json!(0);
    r["attempts"] = json!(0);
    r["resetDue"] = json!(0);
    r["idleDue"] = json!("");
    if keys.iter().any(|k| k.starts_with("weekly:")) {
        r["weeklyResetDue"] = json!(0);
    }
    r.as_object_mut().map(|object| object.remove("accountKey"));
    if result.get("outcome").and_then(Value::as_str) == Some("skipped") {
        if let Some(list) = r.get_mut("keys").and_then(Value::as_array_mut) {
            list.retain(|item| {
                let text = item.as_str().unwrap_or("");
                !keys.iter().any(|k| text == k.as_str()) || !text.starts_with("idle:")
            });
        }
    }
    let last_at = r.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
    if let Err(error) = save(core, "codex", &key, r.clone()) {
        quota_error_record(core, "codex", epoch, &key, &keys, error);
        return;
    }
    if valid(core, "codex", epoch, manual) {
        let message = match r["outcome"].as_str() {
            Some("confirmed") => "预热成功",
            Some("sent") => "预热请求已完成",
            Some("skipped") => "当前窗口已开始，已跳过预热",
            _ => "预热处理完成",
        };
        notify_action(core, "codex", manual, message.to_string());
        finish_state(core, "codex", &r, last_at);
        let handle = core.handle.clone();
        handle.post_job(Box::new(move |core: &mut Core| {
            crate::monitor::refresh_source(core, "codex");
        }));
    }
    perform_end(core, "codex");
}

fn spawn_workbuddy_checkin(core: &mut Core, epoch: u64, key: String, keys: Vec<String>, manual: bool) {
    let handle = core.handle.clone();
    let app = core.app.clone();
    let expected = key.clone();
    handle.clone().spawn_task(async move {
        let mut client = crate::workbuddy::standalone::StandaloneClient::new(app);
        let result = client.checkin(&expected).await;
        handle.post_job(Box::new(move |core: &mut Core| {
            match result {
                Ok(crate::workbuddy::WorkBuddyResult::Checkin { account_key, outcome, credit, streak }) => {
                    if account_key != key {
                        quota_error_record(core, "workbuddy", epoch, &key, &keys, action_error("SESSION_CHANGED"));
                        return;
                    }
                    let day = beijing_day(now_ms());
                    let mut value = json!({
                        "outcome": outcome, "day": day, "lastAt": now_ms(),
                        "trigger": if manual { "manual" } else { "daily" }, "retryAt": 0, "attempts": 0
                    });
                    if let Some(credit) = credit {
                        value["credit"] = json!(credit);
                    }
                    if let Some(streak) = streak {
                        value["streak"] = json!(streak);
                    }
                    let mut r = record(core, "workbuddy", &key);
                    for (field, entry) in value.as_object().into_iter().flatten() {
                        r[field] = entry.clone();
                    }
                    r["errorCode"] = json!("");
                    let last_at = r.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
                    if let Err(error) = save(core, "workbuddy", &key, r.clone()) {
                        quota_error_record(core, "workbuddy", epoch, &key, &keys, error);
                        return;
                    }
                    if valid(core, "workbuddy", epoch, manual) {
                        let message = match outcome.as_str() {
                            "claimed" => "签到成功",
                            "done" => "今天已经签到",
                            "inactive" => "当前没有可参加的签到活动",
                            "ineligible" => "当前账号不符合签到条件",
                            "own-app" => "请在 WorkBuddy 客户端完成签到",
                            _ => "签到处理完成",
                        };
                        notify_action(core, "workbuddy", manual, message.to_string());
                        finish_state(core, "workbuddy", &r, last_at);
                    }
                    if valid(core, "workbuddy", epoch, manual) {
                        crate::monitor::refresh_workbuddy_after_checkin(core);
                        if outcome == "claimed" {
                            // The balance endpoint can lag behind the claim response.
                            core.handle.post_after(5_000, Box::new(crate::monitor::refresh_workbuddy_after_checkin));
                        }
                    }
                    perform_end(core, "workbuddy");
                }
                Ok(_) => {
                    quota_error_record(core, "workbuddy", epoch, &key, &keys, action_error("UNSUPPORTED_RUNTIME"));
                }
                Err(error) => {
                    if !valid(core, "workbuddy", epoch, manual) {
                        perform_end(core, "workbuddy");
                        return;
                    }
                    quota_error_record(core, "workbuddy", epoch, &key, &keys, error);
                }
            }
        }));
    });
}

/// scheduler.js scan(p, manual).
pub fn scan(core: &mut Core, provider: &str, manual: bool) {
    if core.scheduler.disposed {
        return;
    }
    if core.scheduler.state[provider]["checking"].as_bool().unwrap_or(false) {
        return;
    }
    if !scheduler_enabled(core, provider) {
        if manual {
            core.scheduler.state[provider]["outcome"] = json!("error");
            core.scheduler.state[provider]["message"] = json!(safe_message("SOURCE_HIDDEN"));
            publish(core);
        }
        return;
    }
    if !manual && !automatic(core, provider) {
        return;
    }
    if core.scheduler.busy.get(provider).copied().unwrap_or(false) {
        return;
    }
    let epoch = epoch_of(core, provider);
    core.scheduler.state[provider]["checking"] = json!(true);
    publish(core);
    let handle = core.handle.clone();
    let app = core.app.clone();
    let provider_owned = provider.to_string();
    handle.clone().spawn_task(async move {
        let identity = if provider_owned == "codex" {
            crate::automations::codex_client::read_credentials().await
                .map(|credentials| (credentials.account_key, None))
        } else {
            let mut client = crate::workbuddy::standalone::StandaloneClient::new(app);
            client.identity().await.map(|(key, stamp)| (key, Some(stamp)))
        };
        handle.post_job(Box::new(move |core: &mut Core| {
            core.scheduler.state[provider_owned.as_str()]["checking"] = json!(false);
            match identity {
                Ok((account_key, stamp)) if valid(core, &provider_owned, epoch, manual) => {
                    if let Some(stamp) = stamp {
                        core.scheduler.workbuddy_identity_stamp = Some(stamp);
                    }
                    scan_with_key(core, &provider_owned, account_key, epoch, manual);
                }
                Ok(_) => {}
                Err(error) => {
                    if !core.scheduler.disposed && epoch_of(core, &provider_owned) == epoch {
                        core.scheduler.state[provider_owned.as_str()]["outcome"] = json!("error");
                        core.scheduler.state[provider_owned.as_str()]["message"] = json!(safe_message(&error.code));
                        publish(core);
                    }
                }
            }
            publish(core);
            arm(core);
        }));
    });
}

fn scan_with_key(core: &mut Core, provider: &str, account_key: String, mut epoch: u64, manual: bool) {
    if !valid(core, provider, epoch, manual) {
        return;
    }
    if provider == "codex" {
        select_codex_account(core, &account_key);
        epoch = epoch_of(core, provider);
    }
    let r = record(core, provider, &account_key);
    let known = core.scheduler.known_keys.get(provider).and_then(|k| k.clone());
    if provider == "codex"
        && !manual
        && codex_window_supported(core)
        && config(core)["codex"]["onReset"].as_bool().unwrap_or(false)
        && known.as_deref() != Some(account_key.as_str())
    {
        // Read the returning or newly discovered account's quota before deciding.
        core.scheduler.state["codex"]["checking"] = json!(true);
        let handle = core.handle.clone();
        let proxy = core.proxy_url();
        let key = account_key.clone();
        let prior = r.clone();
        handle.clone().spawn_task(async move {
            let read = crate::monitor::local_client::read_codex_for_account(&proxy, &key).await;
            handle.post_job(Box::new(move |core: &mut Core| {
                core.scheduler.state["codex"]["checking"] = json!(false);
                if let Ok(quota) = read {
                    codex_startup_scan(core, epoch, key, quota, manual, prior);
                }
                publish(core);
                arm(core);
            }));
        });
        return;
    }
    scan_continue(core, provider, account_key, epoch, manual, r);
}

fn codex_startup_scan(core: &mut Core, epoch: u64, key: String, quota: Value, manual: bool, prior: Value) {
    if !valid(core, "codex", epoch, manual) {
        return;
    }
    if quota.get("status").and_then(Value::as_str) != Some("ready") {
        core.scheduler.state["codex"]["outcome"] = json!("error");
        core.scheduler.state["codex"]["message"] = json!(safe_message("AUTH_REQUIRED"));
        publish(core);
        return;
    }
    if let Some(support) = window_support(&quota) {
        core.scheduler.codex_window_support = support;
    }
    if core.scheduler.busy.get("codex").copied().unwrap_or(false) {
        return;
    }
    let fresh_record = record(core, "codex", &key);
    // A manual request may have completed while discovery awaited quotas.
    if prior.get("lastAt") != fresh_record.get("lastAt")
        || prior.get("lastStartedAt") != fresh_record.get("lastStartedAt")
        || prior.get("warmupAt") != fresh_record.get("warmupAt")
    {
        let outcome = fresh_record.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string();
        core.scheduler.known_keys.insert("codex".to_string(), if outcome == "error" { None } else { Some(key.clone()) });
        if fresh_record.get("lastAt").and_then(Value::as_i64).unwrap_or(0) > 0 {
            let last = fresh_record.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
            finish_state(core, "codex", &fresh_record, last);
        }
        return;
    }
    let r = observe(core, &key, &quota);
    let window = window_of(&quota);
    let prior_warmup_at = r.get("warmupAt").and_then(Value::as_i64).unwrap_or(0);
    let reset_due = r.get("resetDue").and_then(Value::as_i64).unwrap_or(0);
    let idle_due = r.get("idleDue").and_then(Value::as_str).unwrap_or("").to_string();
    if is_idle_window(window.as_ref(), now_ms()) && reset_due == 0 && idle_due.is_empty() && (prior_warmup_at == 0 || now_ms() - prior_warmup_at >= 5 * 3600000) {
        let mut updated = r.clone();
        if updated.get("outcome").and_then(Value::as_str) == Some("skipped") {
            if let Some(list) = updated.get_mut("keys").and_then(Value::as_array_mut) {
                list.retain(|item| !item.as_str().unwrap_or("").starts_with("idle:"));
            }
        }
        updated["idleDue"] = json!(format!("idle:{}", if prior_warmup_at > 0 { prior_warmup_at.to_string() } else { "initial".to_string() }));
        let _ = save(core, "codex", &key, updated);
    }
    let r = record(core, "codex", &key);
    scan_continue(core, "codex", key, epoch, manual, r);
}

fn scan_continue(core: &mut Core, provider: &str, account_key: String, _epoch: u64, manual: bool, r: Value) {
    core.scheduler.known_keys.insert(provider.to_string(), Some(account_key.clone()));
    if r.get("lastAt").and_then(Value::as_i64).unwrap_or(0) > 0 {
        let last = r.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
        finish_state(core, provider, &r, last);
    }
    let day = beijing_day(now_ms());
    if provider == "workbuddy" {
        if manual {
            perform(core, provider, account_key, Vec::new(), true);
            return;
        }
        let mut current = r.clone();
        if current.get("day").and_then(Value::as_str) != Some(day.as_str()) {
            current["day"] = json!(day);
            current["attempts"] = json!(0);
            current["retryAt"] = json!(0);
            current["outcome"] = json!("idle");
            let _ = save(core, provider, &account_key, current.clone());
        }
        if signed_in_today(&current, &day) {
            perform(core, provider, account_key, Vec::new(), false);
            return;
        }
        let outcome = current.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string();
        let retry_at = current.get("retryAt").and_then(Value::as_i64).unwrap_or(0);
        let attempts = current.get("attempts").and_then(Value::as_i64).unwrap_or(0);
        if terminal_day(&outcome) || retry_at > now_ms() || attempts >= 3 {
            return;
        }
        perform(core, provider, account_key, Vec::new(), false);
        return;
    }
    let due = due_codex_keys(core, &r, now_ms());
    if manual {
        perform(core, provider, account_key, due, true);
        return;
    }
    if due.is_empty() {
        return;
    }
    let attempt_key = due.join("|");
    let mut current = r.clone();
    if current.get("attemptKey").and_then(Value::as_str) != Some(attempt_key.as_str()) {
        current["attemptKey"] = json!(attempt_key);
        current["retryAt"] = json!(0);
        current["attempts"] = json!(0);
        let _ = save(core, provider, &account_key, current.clone());
    }
    let retry_at = current.get("retryAt").and_then(Value::as_i64).unwrap_or(0);
    let attempts = current.get("attempts").and_then(Value::as_i64).unwrap_or(0);
    if retry_at > now_ms() || attempts >= 3 {
        return;
    }
    perform(core, provider, account_key, due, false);
}

pub fn restore_workbuddy_state(core: &mut Core, force: bool) {
    if core.scheduler.disposed || !scheduler_enabled(core, "workbuddy") || core.scheduler.busy.get("workbuddy").copied().unwrap_or(false) {
        return;
    }
    if core.scheduler.restoring_workbuddy {
        return;
    }
    let epoch = epoch_of(core, "workbuddy");
    let has_terminal = core
        .scheduler
        .database
        .get("workbuddy")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .values()
                .any(|r| r.get("lastAt").and_then(Value::as_i64).unwrap_or(0) > 0 && terminal_day(r.get("outcome").and_then(Value::as_str).unwrap_or("")))
        })
        .unwrap_or(false);
    if !has_terminal {
        return;
    }
    core.scheduler.state["workbuddy"]["checking"] = json!(true);
    publish(core);
    core.scheduler.restoring_workbuddy = true;
    let handle = core.handle.clone();
    let app = core.app.clone();
    handle.clone().spawn_task(async move {
        let mut client = crate::workbuddy::standalone::StandaloneClient::new(app);
        let identity = client.identity().await;
        handle.post_job(Box::new(move |core: &mut Core| {
            let finish = |core: &mut Core| {
                core.scheduler.restoring_workbuddy = false;
                core.scheduler.state["workbuddy"]["checking"] = json!(false);
                publish(core);
            };
            if core.scheduler.disposed || !scheduler_enabled(core, "workbuddy") || epoch_of(core, "workbuddy") != epoch || core.scheduler.busy.get("workbuddy").copied().unwrap_or(false) {
                finish(core);
                return;
            }
            match identity {
                Err(_) => {
                    let state = &mut core.scheduler.state["workbuddy"];
                    *state = json!({ "outcome": "idle", "at": null, "credit": null, "message": "签到状态暂不可读，请稍后重试。", "retryAt": null, "retryStopped": false });
                    finish(core);
                }
                Ok((account_key, stamp)) => {
                    core.scheduler.workbuddy_identity_stamp = Some(stamp);
                    let changed = core.scheduler.known_keys.get("workbuddy").and_then(|k| k.as_deref()) != Some(account_key.as_str());
                    core.scheduler.known_keys.insert("workbuddy".to_string(), Some(account_key.clone()));
                    let day = beijing_day(now_ms());
                    let r = record(core, "workbuddy", &account_key);
                    let checked = core
                        .scheduler
                        .status_checks
                        .iter()
                        .find(|(key, _)| key == &account_key)
                        .map(|(_, value)| value.clone());
                    let cached_pending = checked
                        .as_ref()
                        .and_then(|value| value.get("day"))
                        .and_then(Value::as_str)
                        .map(|d| d == day)
                        .unwrap_or(false)
                        && checked.as_ref().and_then(|value| value.get("outcome")).and_then(Value::as_str) == Some("pending")
                        && checked.as_ref().and_then(|value| value.get("at")).and_then(Value::as_i64).map(|at| now_ms() - at < 300000).unwrap_or(false);
                    let r_day = r.get("day").and_then(Value::as_str).unwrap_or("").to_string();
                    let r_outcome = r.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string();
                    if r_day == day && r.get("lastAt").and_then(Value::as_i64).unwrap_or(0) > 0 && terminal_day(&r_outcome) {
                        let last = r.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
                        finish_state(core, "workbuddy", &r, last);
                    } else if changed || r_day != day {
                        let state = &mut core.scheduler.state["workbuddy"];
                        *state = json!({ "outcome": "idle", "at": null, "credit": null, "message": "", "retryAt": null, "retryStopped": false });
                        if cached_pending {
                            state["outcome"] = json!("pending");
                        }
                    }
                    let skip = r_day == day && matches!(r_outcome.as_str(), "claimed" | "done")
                        || !force && checked
                            .as_ref()
                            .and_then(|value| value.get("day"))
                            .and_then(Value::as_str)
                            .map(|d| d == day)
                            .unwrap_or(false)
                            && checked.as_ref().and_then(|value| value.get("at")).and_then(Value::as_i64).map(|at| now_ms() - at < 300000).unwrap_or(false);
                    if skip {
                        finish(core);
                        return;
                    }
                    core.scheduler.status_checks.retain(|(key, _)| key != &account_key);
                    core.scheduler.status_checks.push((account_key.clone(), json!({ "day": day, "at": now_ms() })));
                    if core.scheduler.status_checks.len() > 32 {
                        core.scheduler.status_checks.remove(0);
                    }
                    let handle = core.handle.clone();
                    let app = core.app.clone();
                    let key = account_key.clone();
                    handle.clone().spawn_task(async move {
                        let mut client = crate::workbuddy::standalone::StandaloneClient::new(app);
                        let result = client.checkin_status(&key).await;
                        handle.post_job(Box::new(move |core: &mut Core| {
                            match result {
                                Ok(crate::workbuddy::WorkBuddyResult::Checkin { account_key: result_key, outcome, credit, streak }) => {
                                    if core.scheduler.disposed || !scheduler_enabled(core, "workbuddy") || epoch_of(core, "workbuddy") != epoch || core.scheduler.busy.get("workbuddy").copied().unwrap_or(false) {
                                        finish(core);
                                        return;
                                    }
                                    if result_key != account_key {
                                        let state = &mut core.scheduler.state["workbuddy"];
                                        *state = json!({ "outcome": "idle", "at": null, "credit": null, "message": safe_message("SESSION_CHANGED"), "retryAt": null, "retryStopped": false });
                                        finish(core);
                                        return;
                                    }
                                    if beijing_day(now_ms()) != day {
                                        finish(core);
                                        return;
                                    }
                                    let mut value = json!({
                                        "outcome": outcome, "day": day, "lastAt": now_ms(),
                                        "trigger": "status", "retryAt": 0, "attempts": 0
                                    });
                                    if let Some(credit) = credit {
                                        value["credit"] = json!(credit);
                                    }
                                    if let Some(streak) = streak {
                                        value["streak"] = json!(streak);
                                    }
                                    let mut r = record(core, "workbuddy", &account_key);
                                    for (field, entry) in value.as_object().into_iter().flatten() {
                                        r[field] = entry.clone();
                                    }
                                    let _ = save(core, "workbuddy", &account_key, r.clone());
                                    let last = r.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
                                    finish_state(core, "workbuddy", &r, last);
                                    finish(core);
                                }
                                _ => {
                                    let state = &mut core.scheduler.state["workbuddy"];
                                    *state = json!({ "outcome": "idle", "at": null, "credit": null, "message": "签到状态暂不可读，请稍后重试。", "retryAt": null, "retryStopped": false });
                                    finish(core);
                                }
                            }
                        }));
                    });
                }
            }
        }));
    });
}

pub fn tick(core: &mut Core) {
    if core.scheduler.disposed || !core.scheduler.started || core.scheduler.ticking {
        return;
    }
    core.scheduler.ticking = true;
    core.scheduler.startup_due = 0;
    let time = now_ms();
    if let Some((_, windows)) = codex_scan_plan(core, time).filter(|(at, _)| *at <= time) {
        core.scheduler.codex_ready_scan = false;
        core.scheduler.codex_checked_windows = windows;
        core.scheduler.last_scan_at.insert("codex".into(), time);
        scan(core, "codex", false);
    }
    let last_workbuddy_scan = core.scheduler.last_scan_at.get("workbuddy").copied().unwrap_or(0);
    if time - last_workbuddy_scan >= 5 * 60_000 && workbuddy_scan_due(core, time) {
        core.scheduler.last_scan_at.insert("workbuddy".into(), time);
        scan(core, "workbuddy", false);
    }
    // Completion publishing happens through arm(); tick finishing locally.
    core.scheduler.ticking = false;
    publish(core);
    arm(core);
}

pub fn next_codex_due(core: &Core) -> Option<i64> {
    if core.scheduler.disposed || !scheduler_enabled(core, "codex") || !automatic(core, "codex") {
        return None;
    }
    let time = now_ms();
    let mut candidates: Vec<i64> = Vec::new();
    let config = config(core);
    if config["codex"]["daily"].as_bool().unwrap_or(false) {
        if let Some(times) = config["codex"]["times"].as_array() {
            for clock in times.iter().filter_map(Value::as_str) {
                if let Some(slot) = next_local_slot(time, clock) {
                    candidates.push(slot);
                }
            }
        }
    }
    if let Some(key) = core.scheduler.known_keys.get("codex").and_then(|k| k.clone()) {
        let r = record_const(core, "codex", &key);
        if codex_window_supported(core) && config["codex"]["onReset"].as_bool().unwrap_or(false) {
            for end in [r.get("windowEnd").and_then(Value::as_i64).unwrap_or(0), r.get("weeklyWindowEnd").and_then(Value::as_i64).unwrap_or(0)] {
                if end > time {
                    candidates.push(end);
                }
            }
        }
        if let Some(retry) = retry_time(core, "codex", &r, time) {
            candidates.push(retry);
        }
    }
    candidates.into_iter().min()
}

fn record_const(core: &Core, provider: &str, key: &str) -> Value {
    let raw = core
        .scheduler
        .database
        .get(provider)
        .and_then(|p| p.get(key))
        .cloned()
        .unwrap_or(Value::Null);
    let number = |field: &str| -> i64 { raw.get(field).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v as i64).unwrap_or(0) };
    json!({
        "windowEnd": number("windowEnd"),
        "weeklyWindowEnd": number("weeklyWindowEnd"),
        "retryAt": number("retryAt"),
        "attempts": raw.get("attempts").and_then(Value::as_i64).unwrap_or(0),
        "attemptKey": raw.get("attemptKey").and_then(Value::as_str).unwrap_or("").to_string(),
        "day": raw.get("day").and_then(Value::as_str).unwrap_or("").to_string(),
        "outcome": raw.get("outcome").and_then(Value::as_str).unwrap_or("idle").to_string(),
        "trigger": raw.get("trigger").and_then(Value::as_str).unwrap_or("").to_string(),
        "keys": raw.get("keys").cloned().unwrap_or(json!([])),
        "resetDue": number("resetDue"),
        "weeklyResetDue": number("weeklyResetDue"),
        "recoveryAt": number("recoveryAt"),
        "idleDue": raw.get("idleDue").and_then(Value::as_str).unwrap_or("").to_string(),
        "warmupAt": number("warmupAt")
    })
}

fn scan_available(core: &Core, provider: &str) -> bool {
    scheduler_enabled(core, provider)
        && automatic(core, provider)
        && !core.scheduler.state[provider]["checking"].as_bool().unwrap_or(false)
        && !core.scheduler.busy.get(provider).copied().unwrap_or(false)
}

fn workbuddy_scan_due(core: &Core, time: i64) -> bool {
    if !scan_available(core, "workbuddy") {
        return false;
    }
    let Some(key) = core.scheduler.known_keys.get("workbuddy").and_then(Option::as_deref) else {
        return true;
    };
    let current = record_const(core, "workbuddy", key);
    if crate::workbuddy::standalone::identity_stamp() != core.scheduler.workbuddy_identity_stamp
        || current["day"].as_str() != Some(beijing_day(time).as_str())
    {
        return true;
    }
    if signed_in_today(&current, &beijing_day(time)) {
        return current["trigger"].as_str() != Some("daily");
    }
    !terminal_day(current["outcome"].as_str().unwrap_or("idle"))
        && current["retryAt"].as_i64().unwrap_or(0) <= time
        && current["attempts"].as_i64().unwrap_or(0) < 3
}

fn codex_scan_plan(core: &Core, time: i64) -> Option<(i64, Vec<String>)> {
    if !scan_available(core, "codex") || crate::automations::system_wake::waiting_for_scheduled_resume(core) {
        return None;
    }
    let scan_at = |windows: &[String]| {
        core.scheduler.codex_scan_at(windows, time)
            .max(crate::automations::system_wake::settling_deadline(core, time).unwrap_or(0))
    };
    let Some(key) = core.scheduler.known_keys.get("codex").and_then(Option::as_deref) else {
        return Some((scan_at(&[]), Vec::new()));
    };
    let current = record_const(core, "codex", key);
    let due = due_codex_keys(core, &current, time);
    if due.is_empty() {
        return None;
    }
    let attempt_key = due.join("|");
    if current["attemptKey"].as_str() == Some(attempt_key.as_str())
        && (current["retryAt"].as_i64().unwrap_or(0) > time || current["attempts"].as_i64().unwrap_or(0) >= 3)
    {
        return None;
    }
    let windows = due.into_iter().filter(|key| key.starts_with("reset:") || key.starts_with("weekly:")).collect::<Vec<_>>();
    Some((scan_at(&windows), windows))
}

pub fn arm(core: &mut Core) {
    if core.scheduler.disposed || !core.scheduler.started {
        return;
    }
    let time = now_ms();
    let mut deadlines: Vec<i64> = vec![time + 60000];
    if core.scheduler.startup_due > time {
        deadlines.push(core.scheduler.startup_due);
    }
    if let Some(codex_due) = next_codex_due(core) {
        deadlines.push(codex_due);
    }
    if let Some((at, _)) = codex_scan_plan(core, time) {
        deadlines.push(at);
    }
    if scheduler_enabled(core, "workbuddy") && config(core)["workbuddy"]["daily"].as_bool().unwrap_or(false) {
        deadlines.push((time + 8 * 3600000).div_euclid(DAY) * DAY + DAY - 8 * 3600000);
    }
    let delay = (deadlines.into_iter().min().unwrap_or(time + 60000) - time).max(100);
    let Some(generation) = core.scheduler.plan_timer(time + delay) else {
        return;
    };
    let handle = core.handle.clone();
    handle.post_after(delay, Box::new(move |core: &mut Core| {
        if core.scheduler.take_timer(generation) {
            tick(core);
        }
    }));
}

pub fn update_settings(core: &mut Core) {
    let before = core.scheduler.preferences.clone();
    core.scheduler.preferences = core.prefs();
    for provider in ["codex", "workbuddy"] {
        let was = crate::prefs::normalize_automation(before.get("automation").cloned().unwrap_or(Value::Null));
        let now = config(core);
        let changed = serde_json::to_string(&was.get(provider)).unwrap_or_default() != serde_json::to_string(&now.get(provider)).unwrap_or_default();
        let busy_manual = core.scheduler.busy_manual.get(provider).copied().unwrap_or(false);
        if !scheduler_enabled(core, provider) || changed && !busy_manual {
            *core.scheduler.epochs.get_mut(provider).unwrap() += 1;
            if core.scheduler.busy.get(provider).copied().unwrap_or(false) {
                // In-flight requests are discarded by epoch checks.
            }
        }
    }
    publish(core);
    if core.scheduler.started {
        tick(core);
    }
    if core.scheduler.started && !automatic(core, "workbuddy") {
        restore_workbuddy_state(core, false);
    }
    arm(core);
}

pub fn on_quota_update(core: &mut Core, snapshot: Value) {
    if core.scheduler.disposed {
        return;
    }
    let updated_at = snapshot.get("updatedAt").and_then(Value::as_str).map(str::to_string);
    if updated_at.is_none() || updated_at.as_deref() == core.scheduler.quota_version.as_deref() {
        arm(core);
        return;
    }
    core.scheduler.quota_version = updated_at;
    let account = &snapshot["account"];
    if let Some(support) = window_support(account) {
        core.scheduler.codex_window_support = support;
    }
    let Some(key) = core.monitor.codex_quota_key.clone() else {
        arm(core);
        return;
    };
    let changed = select_codex_account(core, &key);
    if account["status"].as_str() == Some("ready") {
        core.scheduler.note_ready_codex_quota(&key);
    }
    if scheduler_enabled(core, "codex") {
        let result = observe(core, &key, account);
        if !core.scheduler.state["codex"]["running"].as_bool().unwrap_or(false)
            && (changed || matches!(result.get("outcome").and_then(Value::as_str), Some("confirmed") | Some("sent") | Some("skipped")))
        {
            let last = result.get("lastAt").and_then(Value::as_i64).unwrap_or(0);
            finish_state(core, "codex", &result, last);
        }
        publish(core);
    }
    let time = now_ms();
    if account["status"].as_str() == Some("ready") && codex_scan_plan(core, time).is_some_and(|(at, _)| at <= time) {
        tick(core);
    } else {
        arm(core);
    }
}

pub fn on_power_resume(core: &mut Core) {
    core.scheduler.codex_ready_scan = true;
    tick(core);
}

pub fn start(core: &mut Core) {
    if core.scheduler.started || core.scheduler.disposed {
        return;
    }
    core.scheduler.started = true;
    restore_codex_history(core);
    restore_workbuddy_state(core, false);
    core.scheduler.startup_due = now_ms();
    let handle = core.handle.clone();
    handle.post_after(0, Box::new(|core: &mut Core| {
        tick(core);
    }));
}

fn restore_codex_history(core: &mut Core) {
    let epoch = epoch_of(core, "codex");
    let handle = core.handle.clone();
    handle.clone().spawn_task(async move {
        let credentials = crate::automations::codex_client::read_credentials().await;
        handle.post_job(Box::new(move |core| {
            if core.scheduler.disposed || epoch_of(core, "codex") != epoch
                || core.scheduler.busy.get("codex").copied().unwrap_or(false)
                || core.scheduler.display_keys.get("codex").and_then(|key| key.as_ref()).is_some()
            {
                return;
            }
            if let Ok(credentials) = credentials {
                let key = credentials.account_key;
                let history = record(core, "codex", &key);
                select_codex_account(core, &key);
                if let Some(last) = history["lastAt"].as_i64().filter(|time| *time > 0) {
                    finish_state(core, "codex", &history, last);
                }
                publish(core);
            }
        }));
    });
}

pub fn run(core: &mut Core, provider: &str) -> Value {
    if provider != "codex" && provider != "workbuddy" {
        return snapshot(core);
    }
    scan(core, provider, true);
    snapshot(core)
}

pub fn dispose(core: &mut Core) {
    core.scheduler.disposed = true;
    core.scheduler.timer_due_at = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_success_expires_with_its_window_or_day() {
        let completed = 2 * FIVE_HOURS_MS;
        let history = json!({ "warmupCompletedAt": completed });
        let end = completed + FIVE_HOURS_MS;
        assert!(warmup_completed_in_window(&history, end, end - 1));
        assert!(!warmup_completed_in_window(&history, end, end));
        assert!(!warmup_completed_in_window(&history, end + 1, end - 1));
        assert!(!warmup_completed_in_window(&history, end, completed - 1));

        let checkin = json!({ "day": "2026-11-24", "outcome": "claimed" });
        assert!(signed_in_today(&checkin, "2026-11-24"));
        assert!(!signed_in_today(&checkin, "2026-11-25"));
        let ineligible = json!({ "day": "2026-11-24", "outcome": "ineligible" });
        assert!(!signed_in_today(&ineligible, "2026-11-24"));
    }

    #[test]
    fn codex_checks_first_ready_quota_and_new_window_without_waiting_for_heartbeat() {
        let mut scheduler = Scheduler::new(json!({}), json!({}));
        scheduler.select_codex_account("account-a");
        scheduler.last_scan_at.insert("codex".into(), 100_000);
        assert_eq!(scheduler.codex_scan_at(&[], 101_000), 160_000);

        scheduler.note_ready_codex_quota("account-a");
        assert_eq!(scheduler.codex_scan_at(&[], 101_000), 101_000);
        scheduler.codex_ready_scan = false;
        scheduler.note_ready_codex_quota("account-a");
        assert_eq!(scheduler.codex_scan_at(&[], 102_000), 160_000);

        let window = vec!["reset:103000".to_owned()];
        assert_eq!(scheduler.codex_scan_at(&window, 103_000), 103_000);
        scheduler.codex_checked_windows = window.clone();
        scheduler.last_scan_at.insert("codex".into(), 103_000);
        assert_eq!(scheduler.codex_scan_at(&window, 104_000), 163_000);
        assert_eq!(scheduler.codex_scan_at(&["weekly:105000".into()], 105_000), 105_000);

        scheduler.select_codex_account("account-b");
        scheduler.select_codex_account("account-a");
        scheduler.last_scan_at.insert("codex".into(), 106_000);
        scheduler.note_ready_codex_quota("account-a");
        assert_eq!(scheduler.codex_scan_at(&[], 107_000), 107_000);
    }

    #[test]
    fn returning_codex_account_rechecks_its_own_expired_window() {
        let key_a = "a".repeat(64);
        let key_b = "b".repeat(64);
        let a_end = 10 * FIVE_HOURS_MS;
        let return_at = a_end + 16 * 60_000;
        let b_end = return_at + 60 * 60_000;
        let mut scheduler = Scheduler::new(json!({}), json!({}));
        scheduler.database["codex"][&key_a] = json!({ "windowEnd": a_end, "keys": [] });
        scheduler.database["codex"][&key_b] = json!({ "windowEnd": b_end, "keys": [] });
        scheduler.select_codex_account(&key_a);
        scheduler.known_keys.insert("codex".into(), Some(key_a.clone()));
        assert!(scheduler.select_codex_account(&key_b));
        scheduler.known_keys.insert("codex".into(), Some(key_b.clone()));
        scheduler.last_scan_at.insert("codex".into(), return_at - 5_000);
        let b_epoch = scheduler.epochs["codex"];

        assert!(scheduler.select_codex_account(&key_a));
        assert!(scheduler.epochs["codex"] > b_epoch);
        assert!(!scheduler.known_keys.contains_key("codex"));
        assert!(!scheduler.last_scan_at.contains_key("codex"));
        let returning_window = json!({ "end": return_at + FIVE_HOURS_MS, "used": 0, "sample": { "startedAt": return_at } });
        assert_eq!(codex_reset_due(&scheduler.database["codex"][&key_a], &returning_window, return_at), a_end);
        assert_eq!(scheduler.database["codex"][&key_b]["windowEnd"], b_end);
        let active_window = json!({ "end": return_at + FIVE_HOURS_MS, "used": 2 });
        assert_eq!(codex_reset_due(&scheduler.database["codex"][&key_a], &active_window, return_at), 0);
        scheduler.database["codex"][&key_a]["keys"] = json!([format!("reset:{a_end}")]);
        assert_eq!(codex_reset_due(&scheduler.database["codex"][&key_a], &returning_window, return_at), 0);
    }

    #[test]
    fn refresh_cannot_postpone_an_armed_scheduler_deadline() {
        let mut scheduler = Scheduler::new(json!({}), json!({}));
        let heartbeat = scheduler.plan_timer(60_000).unwrap();
        assert_eq!(scheduler.plan_timer(75_000), None);
        assert_eq!(scheduler.plan_timer(60_000), None);

        let reset = scheduler.plan_timer(50_000).unwrap();
        assert_eq!(scheduler.plan_timer(110_000), None);
        assert!(!scheduler.take_timer(heartbeat));
        assert_eq!(scheduler.timer_due_at, Some(50_000));
        assert!(scheduler.take_timer(reset));
        assert!(!scheduler.take_timer(reset));
        assert!(scheduler.plan_timer(110_000).is_some());
    }

    #[test]
    fn warmup_times_rotate_once_per_observed_window_and_keep_request_completion() {
        let first = 10 * FIVE_HOURS_MS;
        let second = first + FIVE_HOURS_MS + 30_000;
        let mut history = json!({});
        rotate_warmup_times(&mut history, first, Some(first + FIVE_HOURS_MS));
        let window = json!({ "end": second + FIVE_HOURS_MS, "used": 1 });
        observe_conversation_window(&mut history, &window, second + 10_000, Some(second + FIVE_HOURS_MS));
        observe_conversation_window(&mut history, &window, second + 60_000, Some(second + 60_001));
        assert_eq!(history["previousWarmupCompletedAt"], first);
        assert_eq!(history["warmupCompletedAt"], second);
        assert_eq!(history["nextQuotaRecoveryAt"], second + FIVE_HOURS_MS);

        let third = second + 7 * DAY;
        let weekly_recovery = third + 60_000;
        rotate_warmup_times(&mut history, third, Some(weekly_recovery));
        let auto_window = json!({ "end": third - 1000 + FIVE_HOURS_MS, "used": 1 });
        observe_conversation_window(&mut history, &auto_window, third + 30_000, Some(third + FIVE_HOURS_MS));
        assert_eq!(history["previousWarmupCompletedAt"], second);
        assert_eq!(history["warmupCompletedAt"], third);
        assert_eq!(history["nextQuotaRecoveryAt"], weekly_recovery);
    }

    #[test]
    fn startup_recovers_a_used_window_even_if_it_already_expired() {
        let first = 10 * FIVE_HOURS_MS;
        let second = first + FIVE_HOURS_MS + 30_000;
        let end = second + FIVE_HOURS_MS;
        let mut history = json!({ "warmupCompletedAt": first });
        observe_conversation_window(&mut history, &json!({ "end": end, "used": 0 }), end + 1, None);
        assert_eq!(history["warmupCompletedAt"], first);
        observe_conversation_window(&mut history, &json!({ "end": end, "used": 1 }), end + 1, None);
        assert_eq!(history["previousWarmupCompletedAt"], first);
        assert_eq!(history["warmupCompletedAt"], second);

        let third = end + 30_000;
        rotate_warmup_times(&mut history, third, None);
        observe_conversation_window(&mut history, &json!({ "end": end, "used": 1 }), third + 1, None);
        assert_eq!(history["previousWarmupCompletedAt"], second);
        assert_eq!(history["warmupCompletedAt"], third);
    }

    #[test]
    fn next_recovery_uses_the_nearest_future_window() {
        let account = |five_hour, weekly| json!({ "status": "ready", "buckets": [{ "id": "codex",
            "primary": { "windowDurationMins": 300, "resetsAt": five_hour, "usedPercent": 10 },
            "secondary": { "windowDurationMins": 10080, "resetsAt": weekly }
        }] });
        assert_eq!(next_quota_recovery_at(&account(100, 200), 0), Some(100_000));
        assert_eq!(next_quota_recovery_at(&account(200, 100), 0), Some(100_000));
        assert_eq!(next_quota_recovery_at(&account(100, 200), 100_000), Some(200_000));
        assert_eq!(next_quota_recovery_at(&account(100, 200), 200_000), None);
    }

    #[test]
    fn scheduled_daily_warmup_does_not_require_a_five_hour_window() {
        let monthly_quota = json!({ "status": "ready", "buckets": [{ "id": "codex", "primary": {
            "windowDurationMins": 43200, "resetsAt": 1000, "usedPercent": 5
        } }] });
        let window = window_of(&monthly_quota);
        assert!(window.is_none());
        assert_eq!(codex_skip_until(&monthly_quota, window.as_ref(), &["time:08:00".into()], false, 1000).unwrap(), None);
        let error = codex_skip_until(&monthly_quota, window.as_ref(), &["reset:1000".into()], false, 1000).unwrap_err();
        assert_eq!(error.code, "CODEX_QUOTA_UNAVAILABLE");
        let daily = ["time:08:00".into()];
        assert_eq!(codex_skip_until(&json!({ "status": "unavailable" }), None, &daily, false, 1000).unwrap(), None);
        assert_eq!(codex_skip_until(&json!({ "status": "signed-out" }), None, &daily, false, 1000).unwrap_err().code, "AUTH_REQUIRED");
    }

    #[test]
    fn active_five_hour_window_only_blocks_the_window_trigger() {
        let quota = json!({ "status": "ready", "buckets": [{ "id": "codex", "primary": {
            "windowDurationMins": 300, "resetsAt": 10, "usedPercent": 5
        }, "secondary": { "windowDurationMins": 10080, "resetsAt": 20 } }] });
        let window = window_of(&quota);
        assert_eq!(codex_skip_until(&quota, window.as_ref(), &["reset:1000".into()], false, 1000).unwrap(), Some(10000));
        assert_eq!(codex_skip_until(&quota, window.as_ref(), &["weekly:1000".into()], false, 1000).unwrap(), None);
        let both = ["reset:1000".into(), "time:08:00".into()];
        assert_eq!(codex_skip_until(&quota, window.as_ref(), &both, false, 1000).unwrap(), None);
    }
}

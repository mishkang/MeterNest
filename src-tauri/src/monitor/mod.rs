pub mod codex_binary;
pub mod data;
pub mod feed;
pub mod hover;
pub mod local_client;
pub mod reset_notifications;
pub mod tray_icon;
pub mod tray_title;

use crate::core::Core;
use crate::error::AppError;
use crate::macos;
use crate::monitor::data::{decode_feed_cache, encode_feed_cache, feed_error, merge_records, SOURCES};
use crate::monitor::hover::{hover_update, HoverState};
use crate::monitor::local_client::read_codex_with_account;
use crate::monitor::reset_notifications::ResetNotifications;
use crate::monitor::tray_title::{hover_summary, tray_value_title};
use crate::util::{now_ms, to_iso};
use crate::workbuddy::standalone::StandaloneClient;
use serde_json::{json, Value};
use std::collections::HashMap;
use tauri::{Emitter, Manager};

const REFRESH_INTERVAL_MS: i64 = 10 * 60 * 1000;
const TRANSIENT_ERRORS: [&str; 6] = ["CODEX_TIMEOUT", "CODEX_CONNECTION", "CODEX_EXITED", "NETWORK_ERROR", "QUERY_TIMEOUT", "SESSION_CHANGED"];

pub struct Monitor {
    pub started: bool,
    pub disposed: bool,
    pub panel_created: bool,
    pub panel_loaded: bool,
    pub panel_visible: bool,
    open_panel_on_tray_release: bool,
    pub compact_heights: HashMap<String, i64>,
    pub network: bool,
    pub timer_gen: u64,
    pub pending: bool,
    pub pending_parts: u8,
    pub workbuddy_pending: bool,
    workbuddy_refresh_after_checkin: bool,
    pub models_pending: bool,
    pub workbuddy_failures: u32,
    pub enabled_providers: Value,
    pub show_reset_feed: bool,
    pub show_workbuddy_history: bool,
    pub provider_epoch: HashMap<String, u64>,
    pub refresh_minutes: Value,
    pub last_refresh_at: HashMap<String, Option<i64>>,
    pub next_refresh_at: HashMap<String, Option<i64>>,
    pub feed_groups: Vec<(String, Vec<Value>)>,
    pub state: Value,
    pub(crate) codex_quota_key: Option<String>,
    pub notifications: ResetNotifications,
    pub hover: HoverState,
}

impl Monitor {
    pub fn new(saved_preferences: &Value, notification_history: Value, feed_cache: Value) -> Monitor {
        let prefs = crate::prefs::read_preferences(saved_preferences);
        let enabled_providers = crate::prefs::normalize_enabled_providers(prefs.get("enabledProviders").cloned().unwrap_or_default());
        let show_reset_feed = prefs.get("showResetFeed").and_then(Value::as_bool).unwrap_or(true);
        let show_workbuddy_history = prefs.get("showWorkBuddyHistory").and_then(Value::as_bool).unwrap_or(true);
        let refresh_minutes = crate::prefs::normalize_refresh_minutes(prefs.get("refreshMinutes").cloned().unwrap_or(Value::Null));
        let provider = if enabled_providers.get("codex").and_then(Value::as_bool).unwrap_or(true) { "codex" } else { "workbuddy" };
        let state = json!({
            "showResetFeed": show_reset_feed,
            "showWorkBuddyHistory": show_workbuddy_history,
            "enabledProviders": enabled_providers.clone(),
            "refreshMinutes": refresh_minutes.clone(),
            "provider": provider,
            "loading": false,
            "accountLoading": false,
            "feedLoading": false,
            "records": [],
            "sources": [],
            "account": null,
            "accountError": "",
            "updatedAt": null,
            "feedUpdatedAt": null,
            "workbuddy": { "loading": false, "account": null, "error": "", "updatedAt": null },
            "models": { "loading": false, "items": [], "error": "", "updatedAt": null }
        });
        let mut monitor = Monitor {
            started: false,
            disposed: false,
            panel_created: false,
            panel_loaded: false,
            panel_visible: false,
            open_panel_on_tray_release: false,
            compact_heights: HashMap::new(),
            network: false,
            timer_gen: 0,
            pending: false,
            pending_parts: 0,
            workbuddy_pending: false,
            workbuddy_refresh_after_checkin: false,
            models_pending: false,
            workbuddy_failures: 0,
            enabled_providers,
            show_reset_feed,
            show_workbuddy_history,
            provider_epoch: HashMap::from([("codex".into(), 0), ("workbuddy".into(), 0), ("feed".into(), 0)]),
            refresh_minutes,
            last_refresh_at: HashMap::from([("codex".into(), None), ("feed".into(), None), ("workbuddy".into(), None), ("models".into(), None)]),
            next_refresh_at: HashMap::from([("codex".into(), None), ("feed".into(), None), ("workbuddy".into(), None), ("models".into(), None)]),
            feed_groups: Vec::new(),
            state,
            codex_quota_key: None,
            notifications: ResetNotifications::new(notification_history),
            hover: HoverState::default(),
        };
        // Cached public feed records display immediately.
        if show_reset_feed {
            if let Some((records, updated_at)) = decode_feed_cache(feed_cache, now_ms()) {
                monitor.state["records"] = Value::Array(records.clone());
                monitor.state["feedUpdatedAt"] = json!(updated_at);
                monitor.feed_groups.push((SOURCES[0].id.to_string(), records));
                monitor.state["sources"] = Value::Array(
                    SOURCES
                        .iter()
                        .map(|source| json!({ "name": source.name, "url": source.home, "ok": false, "error": "等待本次更新" }))
                        .collect(),
                );
            }
        }
        monitor
    }
}

fn epoch_of(core: &Core, provider: &str) -> u64 {
    *core.monitor.provider_epoch.get(provider).unwrap_or(&0)
}

fn enabled(core: &Core, provider: &str) -> bool {
    let monitor = &core.monitor;
    match provider {
        "feed" => {
            monitor.enabled_providers.get("codex").and_then(Value::as_bool).unwrap_or(false) && monitor.show_reset_feed
        }
        "models" => {
            monitor.enabled_providers.get("workbuddy").and_then(Value::as_bool).unwrap_or(false) && monitor.show_workbuddy_history
        }
        "codex" => monitor.enabled_providers.get("codex").and_then(Value::as_bool).unwrap_or(false),
        "workbuddy" => monitor.enabled_providers.get("workbuddy").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

fn valid_request(core: &Core, provider: &str, epoch: u64) -> bool {
    !core.monitor.disposed && enabled(core, provider) && epoch_of(core, provider) == epoch
}

fn interval_of(core: &Core, provider: &str) -> i64 {
    core.monitor
        .refresh_minutes
        .get(provider)
        .and_then(Value::as_i64)
        .unwrap_or(10)
        .max(1)
        .saturating_mul(60000)
}

pub fn snapshot(core: &Core) -> Value {
    core.monitor.state.clone()
}

pub fn publish(core: &mut Core) {
    if core.monitor.disposed {
        return;
    }
    let state = snapshot(core);
    if core.monitor.panel_created {
        if let Some(panel) = core.app.get_webview_window("panel") {
            let _ = panel.emit_to("panel", "codex-monitor-updated", &state);
        }
    }
    crate::automations::scheduler::on_quota_update(core, state.clone());
    hover_update(core, hover_summary(&state, crate::i18n::resolve(&core.prefs())));
    update_tray_title(core);
}

pub fn update_tray_title(core: &mut Core) {
    if core.monitor.disposed {
        return;
    }
    let prefs = core.prefs();
    let title = tray_value_title(&core.monitor.state, &prefs);
    let app = core.app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
            if title.is_empty() {
                macos::apply_tray_title(None, mtm);
            } else {
                macos::apply_tray_title(Some(&title), mtm);
            }
        }
    });
}

fn set_state_flag(state: &mut Value, key: &str, value: Value) {
    if state.get(key).map(|existing| existing != &value).unwrap_or(true) {
        state[key] = value;
    }
}

/// One deadline timer for both providers; manual refresh moves only that
/// provider's deadline and background reads never switch the visible tab.
fn schedule_refresh(core: &mut Core) {
    core.monitor.timer_gen += 1;
    let generation = core.monitor.timer_gen;
    if core.monitor.disposed {
        return;
    }
    let now = now_ms();
    let mut deadlines: Vec<i64> = Vec::new();
    let codex_enabled = core.monitor.enabled_providers.get("codex").and_then(Value::as_bool).unwrap_or(false);
    if codex_enabled && !core.monitor.pending {
        deadlines.push(core.monitor.next_refresh_at.get("codex").copied().flatten().unwrap_or(now));
        if enabled(core, "feed") {
            deadlines.push(core.monitor.next_refresh_at.get("feed").copied().flatten().unwrap_or(now));
        }
    }
    let workbuddy_enabled = core.monitor.enabled_providers.get("workbuddy").and_then(Value::as_bool).unwrap_or(false);
    if workbuddy_enabled && !core.monitor.workbuddy_pending {
        deadlines.push(core.monitor.next_refresh_at.get("workbuddy").copied().flatten().unwrap_or(now));
    }
    if enabled(core, "models") && !core.monitor.models_pending {
        deadlines.push(core.monitor.next_refresh_at.get("models").copied().flatten().unwrap_or(now));
    }
    if deadlines.is_empty() {
        return;
    }
    let delay = (deadlines.iter().copied().min().unwrap_or(now) - now).clamp(0, 2147483647);
    let handle = core.handle.clone();
    handle.post_after(delay, Box::new(move |core: &mut Core| {
        if core.monitor.timer_gen != generation {
            return;
        }
        refresh_background(core);
        schedule_refresh(core);
    }));
}

pub fn update_refresh_settings(core: &mut Core) {
    if core.monitor.disposed {
        return;
    }
    let prefs = core.prefs();
    let next = crate::prefs::normalize_refresh_minutes(prefs.get("refreshMinutes").cloned().unwrap_or(Value::Null));
    let mut changed = false;
    for provider in ["codex", "workbuddy", "feed", "models"] {
        let value = next.get(provider).and_then(Value::as_i64).unwrap_or(10);
        if core.monitor.refresh_minutes.get(provider).and_then(Value::as_i64) != Some(value) {
            changed = true;
            core.monitor.refresh_minutes[provider] = json!(value);
            // Preserve initial/login delays and running requests.
            if core.monitor.last_refresh_at.get(provider).copied().flatten().is_some() {
                let last = core.monitor.last_refresh_at.get(provider).copied().flatten().unwrap_or(0);
                core.monitor.next_refresh_at.insert(provider.to_string(), Some(last.saturating_add(interval_of(core, provider))));
            }
        }
    }
    if !changed {
        return;
    }
    core.monitor.state["refreshMinutes"] = core.monitor.refresh_minutes.clone();
    schedule_refresh(core);
    publish(core);
}

pub fn update_provider_settings(core: &mut Core) {
    if core.monitor.disposed {
        return;
    }
    let prefs = core.prefs();
    let next = crate::prefs::normalize_enabled_providers(prefs.get("enabledProviders").cloned().unwrap_or_default());
    let next_feed = prefs.get("showResetFeed").and_then(Value::as_bool).unwrap_or(true);
    let next_history = prefs.get("showWorkBuddyHistory").and_then(Value::as_bool).unwrap_or(true);
    let mut changed = false;
    if next_feed != core.monitor.show_reset_feed {
        changed = true;
        *core.monitor.provider_epoch.get_mut("feed").unwrap() += 1;
        core.monitor.last_refresh_at.insert("feed".into(), None);
        core.monitor.next_refresh_at.insert("feed".into(), None);
    }
    if next_history != core.monitor.show_workbuddy_history {
        changed = true;
        *core.monitor.provider_epoch.get_mut("workbuddy").unwrap() += 1;
        core.monitor.workbuddy_failures = 0;
        for key in ["workbuddy", "models"] {
            core.monitor.last_refresh_at.insert(key.into(), None);
            core.monitor.next_refresh_at.insert(key.into(), None);
        }
    }
    for source in ["codex", "workbuddy"] {
        let next_value = next.get(source).and_then(Value::as_bool).unwrap_or(true);
        let current = core.monitor.enabled_providers.get(source).and_then(Value::as_bool).unwrap_or(true);
        if next_value == current {
            continue;
        }
        changed = true;
        *core.monitor.provider_epoch.get_mut(source).unwrap() += 1;
        if source == "codex" {
            *core.monitor.provider_epoch.get_mut("feed").unwrap() += 1;
        }
        for key in if source == "codex" { ["codex", "feed"] } else { ["workbuddy", "models"] } {
            core.monitor.last_refresh_at.insert(key.into(), None);
            core.monitor.next_refresh_at.insert(key.into(), None);
        }
        if source == "workbuddy" {
            core.monitor.workbuddy_failures = 0;
        }
    }
    if !changed {
        return;
    }
    core.monitor.enabled_providers = next;
    core.monitor.show_reset_feed = next_feed;
    core.monitor.show_workbuddy_history = next_history;
    core.monitor.state["showResetFeed"] = json!(next_feed);
    core.monitor.state["showWorkBuddyHistory"] = json!(next_history);
    if !enabled(core, "models") {
        core.monitor.state["models"]["loading"] = json!(false);
    }
    if !enabled(core, "feed") {
        core.monitor.state["feedLoading"] = json!(false);
    }
    core.monitor.state["enabledProviders"] = core.monitor.enabled_providers.clone();
    let provider = core.monitor.state.get("provider").and_then(Value::as_str).unwrap_or("codex").to_string();
    if !enabled(core, &provider) {
        let fallback = if enabled(core, "codex") { "codex" } else { "workbuddy" };
        core.monitor.state["provider"] = json!(fallback);
    }
    crate::monitor::hover::hide(core);
    resize_panel(core);
    publish(core);
    schedule_refresh(core);
    // Newly enabled sources query now, even if another source has a delay.
    refresh_background(core);
}

pub fn refresh_background(core: &mut Core) {
    refresh_provider(core, "codex", false);
    refresh_provider(core, "workbuddy", false);
    refresh_models(core, false);
}

pub fn refresh_models(core: &mut Core, force: bool) -> Value {
    if core.monitor.disposed || !core.monitor.network || !enabled(core, "models") {
        return snapshot(core);
    }
    if core.monitor.models_pending {
        return snapshot(core);
    }
    let due = core.monitor.next_refresh_at.get("models").copied().flatten();
    if !force && due.map(|at| now_ms() < at).unwrap_or(false) {
        return snapshot(core);
    }
    let epoch = epoch_of(core, "workbuddy");
    core.monitor.state["models"]["loading"] = json!(true);
    publish(core);
    core.monitor.models_pending = true;
    let handle = core.handle.clone();
    let app = core.app.clone();
    handle.clone().spawn_task(async move {
        let result: Result<Vec<Value>, AppError> = async {
            let mut client = StandaloneClient::new(app.clone());
            client.models().await
        }
        .await;
        handle.post_job(Box::new(move |core: &mut Core| {
            let valid = valid_request(core, "workbuddy", epoch);
            core.monitor.models_pending = false;
            if valid {
                match result {
                    Ok(models) => {
                        core.monitor.state["models"]["items"] = Value::Array(models);
                        core.monitor.state["models"]["error"] = json!("");
                        core.monitor.state["models"]["updatedAt"] = json!(to_iso(now_ms()));
                    }
                    Err(error) => {
                        let clear = matches!(error.code.as_str(), "AUTH_REQUIRED" | "AUTH_EXPIRED" | "SESSION_CHANGED" | "UNSUPPORTED_ACCOUNT");
                        if clear {
                            core.monitor.state["models"]["items"] = json!([]);
                            core.monitor.state["models"]["updatedAt"] = json!(null);
                        }
                        let base = if crate::workbuddy::message(&error.code).is_some() {
                            crate::workbuddy::message(&error.code).unwrap().to_string()
                        } else {
                            "模型配置暂不可用，请检查网络或 WorkBuddy 版本后刷新。".to_string()
                        };
                        let has_items = core.monitor.state["models"]["items"].as_array().map(|items| !items.is_empty()).unwrap_or(false);
                        core.monitor.state["models"]["error"] = json!(if has_items { format!("{} 显示上次成功结果，当前倍率尚未确认。", base) } else { base });
                    }
                }
                core.monitor.last_refresh_at.insert("models".into(), Some(now_ms()));
                core.monitor.next_refresh_at.insert("models".into(), Some(now_ms().saturating_add(interval_of(core, "models"))));
            }
            core.monitor.state["models"]["loading"] = json!(false);
            publish(core);
            schedule_refresh(core);
        }));
    });
    snapshot(core)
}

pub fn refresh_workbuddy_after_checkin(core: &mut Core) {
    if core.monitor.workbuddy_pending {
        core.monitor.workbuddy_refresh_after_checkin = true;
    } else {
        refresh_workbuddy(core, true);
    }
}

pub fn refresh_workbuddy(core: &mut Core, force: bool) -> Value {
    if core.monitor.disposed || !core.monitor.network || !enabled(core, "workbuddy") {
        return snapshot(core);
    }
    if core.monitor.workbuddy_pending {
        return snapshot(core);
    }
    let due = core.monitor.next_refresh_at.get("workbuddy").copied().flatten();
    if !force && due.map(|at| now_ms() < at).unwrap_or(false) {
        return snapshot(core);
    }
    let epoch = epoch_of(core, "workbuddy");
    let include_details = core.monitor.show_workbuddy_history;
    core.monitor.state["workbuddy"]["loading"] = json!(true);
    publish(core);
    core.monitor.workbuddy_pending = true;
    let handle = core.handle.clone();
    let app = core.app.clone();
    handle.clone().spawn_task(async move {
        // One retry per query for transient failures, never a login refresh.
        let read = || async {
            let mut client = StandaloneClient::new(app.clone());
            client.credits(include_details).await
        };
        let result = match read().await {
            Ok(value) => Ok(value),
            Err(error) => {
                if TRANSIENT_ERRORS.contains(&error.code.as_str()) {
                    read().await.map_err(|retry| {
                        let mut retry_error = retry;
                        if retry_error.account_identity.is_none() {
                            retry_error.account_identity = error.account_identity.clone();
                        }
                        retry_error
                    })
                } else {
                    Err(error)
                }
            }
        };
        handle.post_job(Box::new(move |core: &mut Core| {
            let valid = valid_request(core, "workbuddy", epoch);
            let mut retry_delay: Option<i64> = None;
            if valid {
                match result {
                    Ok(crate::workbuddy::WorkBuddyResult::Credits(account)) => {
                        core.monitor.workbuddy_failures = 0;
                        core.monitor.state["workbuddy"]["account"] = account;
                        core.monitor.state["workbuddy"]["error"] = json!("");
                        core.monitor.state["workbuddy"]["updatedAt"] = json!(to_iso(now_ms()));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if TRANSIENT_ERRORS.contains(&error.code.as_str()) {
                            core.monitor.workbuddy_failures = (core.monitor.workbuddy_failures + 1).min(6);
                            retry_delay = Some((30000 * (1 << (core.monitor.workbuddy_failures - 1))).min(REFRESH_INTERVAL_MS));
                        } else {
                            core.monitor.workbuddy_failures = 0;
                        }
                        let cached = TRANSIENT_ERRORS.contains(&error.code.as_str())
                            && core.monitor.state["workbuddy"]["account"].get("status").and_then(Value::as_str) == Some("ready");
                        let message = if let Some(text) = crate::workbuddy::message(&error.code) {
                            text.to_string()
                        } else {
                            "积分暂不可读，请先在本机启动 WorkBuddy 并登录，再刷新。若仍失败，可能是网络异常或客户端接口已变化。".to_string()
                        };
                        if !cached {
                            core.monitor.state["workbuddy"]["account"] = json!(null);
                            core.monitor.state["workbuddy"]["updatedAt"] = json!(null);
                        }
                        core.monitor.state["workbuddy"]["error"] = json!(if cached { format!("{} 显示上次成功结果，当前积分尚未确认。", message) } else { message.clone() });
                        if matches!(error.code.as_str(), "AUTH_REQUIRED" | "AUTH_EXPIRED" | "SESSION_CHANGED" | "UNSUPPORTED_ACCOUNT") {
                            core.monitor.state["models"]["items"] = json!([]);
                            core.monitor.state["models"]["updatedAt"] = json!(null);
                            core.monitor.state["models"]["error"] = json!(message.clone());
                        }
                    }
                }
            }
            core.monitor.workbuddy_pending = false;
            let refresh_after_checkin = std::mem::take(&mut core.monitor.workbuddy_refresh_after_checkin);
            core.monitor.state["workbuddy"]["loading"] = json!(false);
            if valid {
                core.monitor.last_refresh_at.insert("workbuddy".into(), Some(now_ms()));
                core.monitor.next_refresh_at.insert("workbuddy".into(), Some(now_ms().saturating_add(retry_delay.unwrap_or_else(|| interval_of(core, "workbuddy")))));
            }
            publish(core);
            if refresh_after_checkin {
                refresh_workbuddy(core, true);
            }
            schedule_refresh(core);
        }));
    });
    snapshot(core)
}

pub fn refresh_provider(core: &mut Core, provider: &str, force: bool) -> Value {
    if core.monitor.disposed || !core.monitor.network {
        return snapshot(core);
    }
    if !enabled(core, provider) {
        return snapshot(core);
    }
    if provider == "workbuddy" {
        return refresh_workbuddy(core, force);
    }
    if core.monitor.pending {
        return snapshot(core);
    }
    let usage_due = force
        || core.monitor.next_refresh_at.get("codex").copied().flatten().is_none()
        || now_ms() >= core.monitor.next_refresh_at.get("codex").copied().flatten().unwrap_or(0);
    let feed_due = enabled(core, "feed")
        && (force
            || core.monitor.next_refresh_at.get("feed").copied().flatten().is_none()
            || now_ms() >= core.monitor.next_refresh_at.get("feed").copied().flatten().unwrap_or(0));
    if !usage_due && !feed_due {
        return snapshot(core);
    }
    let epoch = epoch_of(core, "codex");
    let feed_epoch = epoch_of(core, "feed");
    core.monitor.pending = true;
    core.monitor.pending_parts = (if usage_due { 1 } else { 0 }) + (if feed_due { 1 } else { 0 });
    set_state_flag(&mut core.monitor.state, "loading", json!(true));
    set_state_flag(&mut core.monitor.state, "accountLoading", json!(usage_due));
    set_state_flag(&mut core.monitor.state, "feedLoading", json!(feed_due));
    publish(core);
    if usage_due {
        let handle = core.handle.clone();
        let proxy = core.proxy_url();
        handle.clone().spawn_task(async move {
            let result = read_with_retry(&proxy).await;
            handle.post_job(Box::new(move |core: &mut Core| {
                let valid = valid_request(core, "codex", epoch);
                if valid {
                    match result {
                        Ok((account, key)) => {
                            core.monitor.codex_quota_key = key.filter(|_| matches!(account["status"].as_str(), Some("ready" | "unavailable")));
                            core.monitor.state["account"] = account;
                            core.monitor.state["accountError"] = json!("");
                            core.monitor.state["updatedAt"] = json!(to_iso(now_ms()));
                        }
                        Err(error) => {
                            let cached = TRANSIENT_ERRORS.contains(&error.code.as_str())
                                && core.monitor.state["account"].get("status").and_then(Value::as_str) == Some("ready")
                                && error
                                    .account_identity
                                    .as_deref()
                                    .map(|identity| {
                                        identity.is_empty() || Some(identity) == core.monitor.state["account"].get("identity").and_then(Value::as_str)
                                    })
                                    .unwrap_or(false);
                            if !cached {
                                core.monitor.codex_quota_key = None;
                                core.monitor.state["account"] = json!(null);
                                core.monitor.state["updatedAt"] = json!(null);
                            }
                            let message = crate::monitor::local_client::codex_failure_message(&error);
                            core.monitor.state["accountError"] = json!(if cached { format!("{} 显示上次成功结果，当前额度尚未确认。", message) } else { message.clone() });
                        }
                    }
                }
                core.monitor.state["accountLoading"] = json!(false);
                finish_pending_part(core, "codex", epoch, USAGE_FLAG_DONE);
            }));
        });
    }
    if feed_due {
        let handle = core.handle.clone();
        let proxy = core.proxy_url();
        handle.clone().spawn_task(async move {
            let mut results: Vec<Result<Vec<Value>, AppError>> = Vec::new();
            for source in SOURCES.iter() {
                results.push(crate::monitor::feed::fetch_source(source, &proxy).await);
            }
            handle.post_job(Box::new(move |core: &mut Core| {
                let valid = valid_request(core, "feed", feed_epoch);
                if valid {
                    let mut sources = Vec::new();
                    let mut new_groups: Vec<(&'static str, Vec<Value>)> = Vec::new();
                    let mut any_ok = false;
                    for (index, result) in results.into_iter().enumerate() {
                        let source = &SOURCES[index];
                        match result {
                            Ok(records) => {
                                any_ok = true;
                                new_groups.push((source.id, records));
                                sources.push(json!({ "name": source.name, "url": source.home, "ok": true, "error": "" }));
                            }
                            Err(error) => {
                                sources.push(json!({ "name": source.name, "url": source.home, "ok": false, "error": feed_error(&error) }));
                            }
                        }
                    }
                    // Successful fetches replace their source group; failures
                    // keep the previous records for the merge.
                    let mut all_groups: Vec<Vec<Value>> = Vec::new();
                    let mut merged_groups: Vec<(String, Vec<Value>)> = Vec::new();
                    for source in SOURCES.iter() {
                        if let Some((_, records)) = new_groups.iter().find(|(id, _)| *id == source.id) {
                            all_groups.push(records.clone());
                            merged_groups.push((source.id.to_string(), records.clone()));
                        } else if let Some((_, records)) = core.monitor.feed_groups.iter().find(|(id, _)| id == source.id) {
                            all_groups.push(records.clone());
                            merged_groups.push((source.id.to_string(), records.clone()));
                        }
                    }
                    if any_ok {
                        let previous = core.monitor.state["records"].clone();
                        let records = merge_records(all_groups, 5);
                        core.monitor.state["records"] = Value::Array(records.clone());
                        core.monitor.state["feedUpdatedAt"] = json!(to_iso(now_ms()));
                        core.monitor.feed_groups = merged_groups;
                        let notifications = core.monitor.notifications.observe(previous.as_array().map(|v| v.as_slice()).unwrap_or(&[]), &records);
                        if !notifications.is_empty() {
                            let history = core.monitor.notifications.history();
                            let _ = core.store.set("resetNotificationHistory", history);
                            for notification in notifications {
                                show_reset_notification(core, notification);
                            }
                        }
                        let cache = encode_feed_cache(&records, &to_iso(now_ms()));
                        let _ = core.store.set("resetFeedCache", cache);
                    }
                    core.monitor.state["sources"] = Value::Array(sources);
                }
                core.monitor.state["feedLoading"] = json!(false);
                finish_pending_part(core, "feed", feed_epoch, USAGE_FLAG_DONE);
            }));
        });
    }
    snapshot(core)
}

const USAGE_FLAG_DONE: bool = true;

fn finish_pending_part(core: &mut Core, provider: &str, epoch: u64, _flag: bool) {
    let valid_provider = valid_request(core, provider, epoch);
    if valid_provider {
        core.monitor.last_refresh_at.insert(provider.to_string(), Some(now_ms()));
        core.monitor.next_refresh_at.insert(provider.to_string(), Some(now_ms().saturating_add(interval_of(core, provider))));
    }
    if core.monitor.pending_parts > 0 {
        core.monitor.pending_parts -= 1;
    }
    if core.monitor.pending_parts == 0 {
        core.monitor.pending = false;
        core.monitor.state["loading"] = json!(false);
        publish(core);
    } else {
        publish(core);
    }
    schedule_refresh(core);
}

async fn read_with_retry(proxy: &str) -> Result<(Value, Option<String>), AppError> {
    match read_codex_with_account(proxy).await {
        Ok(value) => Ok(value),
        Err(error) => {
            if TRANSIENT_ERRORS.contains(&error.code.as_str()) {
                match read_codex_with_account(proxy).await {
                    Ok(value) => Ok(value),
                    Err(mut retry) => {
                        if retry.account_identity.is_none() {
                            retry.account_identity = error.account_identity.clone();
                        }
                        Err(retry)
                    }
                }
            } else {
                Err(error)
            }
        }
    }
}

fn show_reset_notification(core: &mut Core, payload: Value) {
    use tauri_plugin_notification::NotificationExt;
    let app = core.app.clone();
    let title = payload.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let body = payload.get("body").and_then(Value::as_str).unwrap_or("").to_string();
    let language = crate::i18n::resolve(&core.prefs());
    let title = crate::i18n::text(language, &title);
    let body = if let Some((record, description)) = body.rsplit_once('\n') {
        format!("{record}\n{}", crate::i18n::text(language, description))
    } else {
        body
    };
    let _ = app.notification().builder().title(title).body(body).show();
}

pub fn enabled_feed(core: &Core) -> bool {
    enabled(core, "feed")
}

pub fn select_provider(core: &mut Core, provider: &str, force: bool) -> Result<Value, AppError> {
    if core.monitor.disposed || !core.monitor.network {
        return Ok(snapshot(core));
    }
    if provider != "codex" && provider != "workbuddy" {
        return Err(AppError::message("不支持的额度来源"));
    }
    if !enabled(core, provider) {
        return Err(AppError::message("该来源已隐藏"));
    }
    let changed = core.monitor.state.get("provider").and_then(Value::as_str) != Some(provider);
    core.monitor.state["provider"] = json!(provider);
    if changed {
        resize_panel(core);
        publish(core);
    }
    Ok(refresh_provider(core, provider, force))
}

pub fn refresh(core: &mut Core, provider: &str) -> Value {
    if provider == "models" {
        return refresh_models(core, true);
    }
    if provider == "workbuddy" || provider == "codex" {
        let _ = select_provider(core, provider, true);
    }
    snapshot(core)
}

pub fn refresh_source(core: &mut Core, provider: &str) -> Value {
    if enabled(core, provider) {
        refresh_provider(core, provider, true)
    } else {
        snapshot(core)
    }
}

pub fn show_panel(core: &mut Core) {
    crate::monitor::hover::hide(core);
    if core.monitor.disposed {
        return;
    }
    core.monitor.open_panel_on_tray_release = false;
    if !crate::windows::ensure_panel_window(core) {
        return;
    }
    resize_panel(core);
    core.monitor.panel_visible = true;
    if let Some(panel) = core.app.get_webview_window("panel") {
        macos::panel::show(&panel);
    }
    if core.monitor.panel_loaded {
        if let Some(panel) = core.app.get_webview_window("panel") {
            let _ = panel.emit_to("panel", "codex-monitor-reset-expanded", ());
        }
    }
    let provider = core.monitor.state.get("provider").and_then(Value::as_str).unwrap_or("codex").to_string();
    let _ = select_provider(core, &provider, false);
    let app = core.app.clone();
    let handle = core.handle.clone();
    let _ = app.run_on_main_thread(move || {
        crate::macos::power::register_escape(handle);
    });
}

pub fn hide_panel(core: &mut Core) {
    core.monitor.open_panel_on_tray_release = false;
    if !core.monitor.panel_visible {
        return;
    }
    core.monitor.panel_visible = false;
    let app = core.app.clone();
    let _ = app.run_on_main_thread(crate::macos::power::unregister_escape);
    macos::panel::hide(&core.app);
}

pub fn toggle(core: &mut Core) {
    crate::monitor::hover::hide(core);
    if core.monitor.disposed {
        return;
    }
    if core.monitor.panel_visible {
        hide_panel(core);
        return;
    }
    show_panel(core);
}

pub fn on_tray_left_press(core: &mut Core) {
    crate::monitor::hover::hide(core);
    if core.monitor.panel_visible {
        hide_panel(core);
    } else {
        core.monitor.open_panel_on_tray_release = true;
    }
}

pub fn on_tray_left_release(core: &mut Core) {
    if std::mem::take(&mut core.monitor.open_panel_on_tray_release) {
        show_panel(core);
    }
}

fn cursor_point(app: &tauri::AppHandle) -> Option<(f64, f64)> {
    use std::sync::mpsc::channel;
    let (tx, rx) = channel();
    app.run_on_main_thread(move || {
        let value = objc2_foundation::MainThreadMarker::new().map(macos::cursor_screen_point);
        let _ = tx.send(value);
    })
    .ok()?;
    rx.recv().ok()?
}

pub fn resize_panel(core: &mut Core) {
    if !core.monitor.panel_created {
        return;
    }
    let provider = core.monitor.state.get("provider").and_then(Value::as_str).unwrap_or("codex").to_string();
    let compact = if provider == "codex" { !core.monitor.show_reset_feed } else { !core.monitor.show_workbuddy_history };
    let default_height = if enabled(core, "codex") && enabled(core, "workbuddy") { 400 } else { 360 };
    let height = if compact {
        core.monitor.compact_heights.get(&provider).copied().map(|h| h as f64).unwrap_or(default_height as f64)
    } else {
        744.0
    };
    let fallback_area = macos::ScreenFrame { x: 0.0, y: 0.0, width: 1440.0, height: 900.0, frame_x: 0.0, frame_y: 0.0, frame_width: 1440.0, frame_height: 900.0 };
    let (mut anchor, area) = match screen_geometry(&core.app) {
        Some(value) => value,
        None => ((0.0, 0.0, 1.0, 1.0), fallback_area),
    };
    if !(anchor.2 > 0.0 && anchor.3 > 0.0) {
        let (x, y) = cursor_point(&core.app).unwrap_or((0.0, 0.0));
        anchor = (x, y, 1.0, 1.0);
    }
    let bounds = crate::windows::position_panel(anchor, area, height);
    macos::panel::set_bounds(&core.app, bounds);
}

fn screen_geometry(app: &tauri::AppHandle) -> Option<((f64, f64, f64, f64), macos::ScreenFrame)> {
    use std::sync::mpsc::channel;
    let (tx, rx) = channel();
    app.run_on_main_thread(move || {
        let value = objc2_foundation::MainThreadMarker::new().and_then(|mtm| {
            let anchor = macos::tray_bounds(mtm)?;
            let area = macos::work_area_for(anchor, mtm);
            Some((anchor, area))
        });
        let _ = tx.send(value);
    })
    .ok()?;
    rx.recv().ok()?
}

pub fn on_escape_key(core: &mut Core) {
    if core.monitor.panel_visible {
        hide_panel(core);
    }
}

pub fn fit_panel_height(core: &mut Core, height: f64) -> Result<bool, AppError> {
    if !(240.0..=760.0).contains(&height) || height.is_nan() {
        return Err(AppError::message("额度窗口尺寸无效"));
    }
    let provider = core.monitor.state.get("provider").and_then(Value::as_str).unwrap_or("codex").to_string();
    let compact = if provider == "codex" { !core.monitor.show_reset_feed } else { !core.monitor.show_workbuddy_history };
    if compact {
        let current = core.monitor.compact_heights.get(&provider).copied().unwrap_or(0);
        if (current as f64 - height).abs() > 1.0 {
            core.monitor.compact_heights.insert(provider, height.ceil() as i64);
            resize_panel(core);
        }
    }
    Ok(true)
}

pub fn start(core: &mut Core, delay_initial_query: bool) -> bool {
    if core.monitor.started || core.monitor.disposed {
        return core.monitor.started;
    }
    if core.app.tray_by_id("main").is_none() {
        return false;
    }
    core.monitor.network = true;
    core.monitor.started = true;
    update_tray_title(core);
    if delay_initial_query {
        // Register the tray immediately, but let login-time networking/proxies
        // settle before any automatic query.
        let first_query_at = now_ms() + 10000;
        for provider in ["codex", "feed", "workbuddy", "models"] {
            core.monitor.next_refresh_at.insert(provider.to_string(), Some(first_query_at));
        }
        schedule_refresh(core);
    } else {
        refresh_background(core);
    }
    true
}

pub fn dispose(core: &mut Core) {
    if core.monitor.disposed {
        return;
    }
    core.monitor.disposed = true;
    core.monitor.timer_gen += 1;
    crate::monitor::hover::dispose(core);
    let app = core.app.clone();
    let _ = app.run_on_main_thread(crate::macos::power::unregister_escape);
    if core.monitor.panel_created {
        if let Some(panel) = core.app.get_webview_window("panel") {
            macos::panel::dispose(&panel);
            let _ = panel.destroy();
        }
    }
    core.monitor.panel_created = false;
    core.monitor.panel_visible = false;
    core.monitor.feed_groups.clear();
}


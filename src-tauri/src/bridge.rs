use crate::core::CoreHandle;
use crate::error::AppError;
use crate::monitor;
use serde_json::{json, Value};
use tauri::Manager;
use tauri::State;

type Reply = std::sync::mpsc::Sender<Result<Value, AppError>>;

fn call(
    state: State<CoreHandle>,
    window: &str,
    allowed: &[&str],
    job: impl FnOnce(&mut crate::core::Core, Reply) + Send + 'static,
) -> Result<Value, AppError> {
    if !allowed.contains(&window) {
        return Err(AppError::message("拒绝非授权窗口的请求"));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let window = window.to_string();
    state.post_job(Box::new(move |core| {
        if core.app.get_webview_window(&window).is_none() {
            let _ = tx.send(Err(AppError::message(format!("拒绝非{}窗口的请求", window))));
            return;
        }
        job(core, tx);
    }));
    rx.recv().map_err(|_| AppError::message("应用已退出"))?
}

#[tauri::command(async)]
pub fn codex_monitor_read(window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        let _ = tx.send(Ok(monitor::snapshot(core)));
    })
}

#[tauri::command(async)]
pub fn codex_monitor_select(provider: String, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        let _ = tx.send(monitor::select_provider(core, &provider, false));
    })
}

#[tauri::command(async)]
pub fn codex_monitor_refresh(provider: String, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        let _ = tx.send(Ok(monitor::refresh(core, &provider)));
    })
}

#[tauri::command(async)]
pub fn codex_monitor_fit(height: f64, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        let _ = tx.send(monitor::fit_panel_height(core, height).map(|value| json!(value)));
    })
    .map(|_| true)
}

#[tauri::command(async)]
pub fn codex_monitor_hide(window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        monitor::hide_panel(core);
        let _ = tx.send(Ok(json!(true)));
    })
    .map(|_| true)
}

#[tauri::command(async)]
pub fn codex_monitor_open(url: String, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["panel"], move |core, tx| {
        // Only feed home pages and record links may ever be opened.
        let feed_enabled = {
            let prefs = core.prefs();
            prefs.get("showResetFeed").and_then(Value::as_bool).unwrap_or(true)
                && core.monitor.enabled_providers.get("codex").and_then(Value::as_bool).unwrap_or(false)
        };
        let mut allowed: Vec<String> = Vec::new();
        if feed_enabled {
            for source in crate::monitor::data::SOURCES.iter() {
                allowed.push(source.home.to_string());
            }
            for record in core.monitor.state.get("records").and_then(Value::as_array).cloned().unwrap_or_default() {
                if let Some(link) = record.get("link").and_then(Value::as_str) {
                    if !link.is_empty() {
                        allowed.push(link.to_string());
                    }
                }
            }
        }
        if !allowed.contains(&url) {
            let _ = tx.send(Err(AppError::message("不允许打开此地址")));
            return;
        }
        let opened = crate::macos::open_url(&url);
        let _ = tx.send(if opened { Ok(json!(true)) } else { Err(AppError::message("不允许打开此地址")) });
    })
    .map(|_| true)
}

#[tauri::command(async)]
pub fn meternest_hover_ready(window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["hover"], move |core, tx| {
        crate::monitor::hover::on_page_ready(core);
        let _ = tx.send(Ok(json!(true)));
    })
    .map(|_| true)
}

#[tauri::command(async)]
pub fn meternest_hover_fit(width: f64, height: f64, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["hover"], move |core, tx| {
        let ok = crate::monitor::hover::fit(core, width, height);
        let _ = tx.send(Ok(json!(ok)));
    })
    .map(|_| true)
}

fn settings_snapshot(core: &mut crate::core::Core) -> Value {
    let prefs = core.prefs();
    let mut value = prefs.clone();
    value["version"] = json!(core.app.package_info().version.to_string());
    value["initialPage"] = json!(core.settings.selected_page);
    value["startupSupported"] = json!(core.packaged);
    value["wakeState"] = crate::automations::system_wake::snapshot(core);
    value["automationState"] = crate::automations::scheduler::snapshot(core);
    value
}

#[tauri::command(async)]
pub fn meternest_settings_read(window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["settings"], move |core, tx| {
        let _ = tx.send(Ok(settings_snapshot(core)));
    })
}

#[tauri::command(async)]
pub fn meternest_settings_save(value: Value, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["settings"], move |core, tx| {
        let validated = match crate::prefs::validate_settings(&value) {
            Ok(validated) => validated,
            Err(message) => {
                let _ = tx.send(Err(AppError::message(message)));
                return;
            }
        };
        if validated["automation"]["codex"]["wake"].as_bool().unwrap_or(false) && !crate::automations::system_wake::snapshot(core).get("installed").and_then(Value::as_bool).unwrap_or(false) {
            let _ = tx.send(Err(AppError::message("请先授权安装系统唤醒组件")));
            return;
        }
        if validated["automation"]["codex"]["onReset"].as_bool().unwrap_or(false)
            && !crate::automations::scheduler::codex_window_supported(core)
        {
            let message = core.scheduler.codex_window_support["message"].as_str().unwrap_or("当前套餐不支持窗口到期预热。");
            let _ = tx.send(Err(AppError::message(format!("{} 请关闭“每周和5小时窗口到期预热”后保存。", message))));
            return;
        }
        let previous = core.prefs();
        let open_at_login = validated["openAtLogin"].as_bool().unwrap_or(false);
        let previous_login = previous["openAtLogin"].as_bool().unwrap_or(false);
        let startup = (open_at_login != previous_login).then(|| crate::login_startup::LoginStartup::new(core.packaged));
        if let Some(startup) = &startup {
            if let Err(message) = startup.set_enabled(open_at_login) {
                let _ = tx.send(Err(AppError::message(message)));
                return;
            }
        }
        if let Err(error) = core.store.set("preferences", validated) {
            let message = if let Some(Err(rollback)) = startup.map(|startup| startup.set_enabled(previous_login)) {
                format!("{}。登录启动设置未能恢复：{}", error.message, rollback)
            } else {
                error.message
            };
            let _ = tx.send(Err(AppError::message(message)));
            return;
        }
        // onChanged: propagate to monitor, tray, scheduler and wake.
        crate::monitor::update_provider_settings(core);
        crate::monitor::update_tray_title(core);
        crate::monitor::update_refresh_settings(core);
        crate::automations::scheduler::update_settings(core);
        crate::automations::system_wake::sync(core);
        let _ = tx.send(Ok(settings_snapshot(core)));
    })
}

#[tauri::command(async)]
pub async fn meternest_notification_authorize(window: tauri::WebviewWindow) -> Result<bool, AppError> {
    if window.label() != "settings" {
        return Err(AppError::message("无权执行此操作"));
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let app = window.app_handle().clone();
    app.run_on_main_thread(move || unsafe {
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2::msg_send;
        use std::sync::Mutex;
        let sender = Mutex::new(Some(sender));
        let callback = block2::RcBlock::new(move |granted: Bool, _error: *mut AnyObject| {
            if let Some(sender) = sender.lock().unwrap().take() {
                let _ = sender.send(granted.as_bool());
            }
        });
        let center: *mut AnyObject = msg_send![AnyClass::get(c"UNUserNotificationCenter").expect("UNUserNotificationCenter"), currentNotificationCenter];
        let block: &block2::Block<dyn Fn(Bool, *mut AnyObject)> = &*callback;
        let _: () = msg_send![center, requestAuthorizationWithOptions: 7_usize, completionHandler: block];
    }).map_err(|_| AppError::message("无法请求系统通知权限"))?;
    receiver.await.map_err(|_| AppError::message("无法获取系统通知权限"))
}

#[tauri::command(async)]
pub fn meternest_settings_fit(height: f64, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<bool, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["settings"], move |core, tx| {
        if !height.is_finite() || !(100.0..=1200.0).contains(&height) {
            let _ = tx.send(Err(AppError::message("设置窗口尺寸无效")));
            return;
        }
        if let Some(settings) = core.app.get_webview_window("settings") {
            let scale = settings.scale_factor().unwrap_or(1.0);
            let bounds = settings.outer_size().ok().and_then(|size| settings.outer_position().ok().map(|position| (position, size)));
            if let Some((position, size)) = bounds {
                let position = position.to_logical::<f64>(scale);
                let size = size.to_logical::<f64>(scale);
                let work = {
                    let (tx2, rx2) = std::sync::mpsc::channel();
                    let app = core.app.clone();
                    let _ = app.run_on_main_thread(move || {
                        let value = objc2_foundation::MainThreadMarker::new().and_then(|mtm| {
                            let monitor = crate::macos::work_area_for((position.x, position.y, size.width, size.height), mtm);
                            Some(monitor)
                        });
                        let _ = tx2.send(value);
                    });
                    rx2.recv().ok()
                };
                let area = work.flatten().unwrap_or(crate::macos::ScreenFrame { x: 0.0, y: 0.0, width: 1440.0, height: 900.0, frame_x: 0.0, frame_y: 0.0, frame_width: 1440.0, frame_height: 900.0 });
                let inner_height = settings.inner_size().map(|inner| inner.to_logical::<f64>(scale).height).unwrap_or(size.height);
                let frame_height = (size.height - inner_height).max(0.0);
                let height = height.min(area.height - frame_height);
                let y = area.y.max(position.y.min(area.y + area.height - height - frame_height));
                let _ = settings.set_size(tauri::LogicalSize::new(700.0, height));
                let _ = settings.set_position(tauri::LogicalPosition::new(position.x, y));
            }
        }
        let _ = tx.send(Ok(json!(true)));
    })
    .map(|_| true)
}

#[tauri::command(async)]
pub fn meternest_wake_manage(action: String, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    if window_label != "settings" {
        return Err(AppError::message("拒绝非授权窗口的请求"));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    state.post_job(Box::new(move |core| {
        if core.app.get_webview_window(&window_label).is_none() {
            let _ = tx.send(Err(AppError::message(format!("拒绝非{}窗口的请求", window_label))));
            return;
        }
        let context = match crate::automations::system_wake::prepare_manage(core, &action) {
            Ok(context) => context,
            Err(message) => {
                let _ = tx.send(Err(AppError::message(message)));
                return;
            }
        };
        let handle = core.handle.clone();
        std::thread::spawn(move || {
            let result = crate::automations::system_wake::execute_manage(context);
            handle.post_job(Box::new(move |core| {
                let reply = crate::automations::system_wake::finish_manage(core, result);
                if action == "uninstall" && reply.is_ok() {
                    // Uninstalling the component also clears the wake preference.
                    let mut prefs = core.store.get("preferences");
                    if prefs.get("automation").and_then(|a| a.get("codex")).and_then(|c| c.get("wake")).and_then(Value::as_bool) == Some(true) {
                        prefs["automation"]["codex"]["wake"] = json!(false);
                        let _ = core.store.set("preferences", prefs.clone());
                        crate::automations::scheduler::update_settings(core);
                    }
                }
                let _ = tx.send(reply.map_err(AppError::message));
            }));
        });
    }));
    rx.recv().map_err(|_| AppError::message("应用已退出"))?
}

#[tauri::command(async)]
pub fn meternest_automation_state(window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    let window_label = window.label().to_string();
    call(state, &window_label, &["settings"], move |core, tx| {
        crate::automations::scheduler::restore_workbuddy_state(core, true);
        let mut value = crate::automations::scheduler::snapshot(core);
        value["wake"] = crate::automations::system_wake::snapshot(core);
        let _ = tx.send(Ok(value));
    })
}

#[tauri::command(async)]
pub fn meternest_automation_run(source: String, window: tauri::WebviewWindow, state: State<CoreHandle>) -> Result<Value, AppError> {
    if source != "codex" && source != "workbuddy" {
        return Err(AppError::message("无效的任务来源"));
    }
    let window_label = window.label().to_string();
    call(state, &window_label, &["settings"], move |core, tx| {
        let value = crate::automations::scheduler::run(core, &source);
        let _ = tx.send(Ok(value));
    })
}

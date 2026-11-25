#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod i18n;
mod automations;
mod bridge;
mod constants;
mod core;
mod error;
mod login_startup;
mod macos;
mod monitor;
mod paths;
mod prefs;
mod store;
mod util;
mod windows;
mod workbuddy;

use crate::core::{Core, CoreHandle, SettingsState};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::Manager;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let tray_only = argv.iter().any(|arg| arg == login_startup::TRAY_ONLY_ARG);
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A repeated launch hands control to the running instance.
            if argv.iter().any(|arg| arg == login_startup::TRAY_ONLY_ARG) {
                return;
            }
            if let Some(state) = app.try_state::<CoreHandle>() {
                state.post_job(Box::new(|core: &mut Core| {
                    monitor::toggle(core);
                }));
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            bridge::codex_monitor_read,
            bridge::codex_monitor_select,
            bridge::codex_monitor_refresh,
            bridge::codex_monitor_fit,
            bridge::codex_monitor_hide,
            bridge::codex_monitor_open,
            bridge::meternest_hover_ready,
            bridge::meternest_hover_fit,
            bridge::meternest_settings_read,
            bridge::meternest_settings_save,
            bridge::meternest_settings_fit,
            bridge::meternest_notification_authorize,
            bridge::meternest_wake_manage,
            bridge::meternest_automation_state,
            bridge::meternest_automation_run
        ])
        .setup(move |app| {
            // LSUIElement: menu-bar only, no Dock icon.
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let store = match store::Store::open() {
                Ok(store) => store,
                Err(error) => {
                    config_failure_dialog(&error);
                    app.handle().exit(1);
                    return Ok(());
                }
            };
            build_application_menu(app)?;
            let (core, receiver) = build_core(app.handle().clone(), util::is_packaged(), store);
            let handle = core.handle.clone();
            let login_startup = login_startup::LoginStartup::new(util::is_packaged());
            let at_login = tray_only || login_startup.was_opened_at_login(&argv);
            app.manage(handle);
            build_tray(app, i18n::resolve(&core.prefs()))?;
            // Power & event monitors run on the main thread.
            let monitor_handle = app.state::<CoreHandle>().inner().clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
                    macos::power::install_workspace_monitors(monitor_handle, mtm);
                }
            });
            std::thread::spawn(move || core::run_loop(core, receiver));
            // Notification click opens the Codex quick panel (reset-notifications.js).
            {
                use tauri_plugin_notification::NotificationExt;
                let notify_handle = app.state::<CoreHandle>().inner().clone();
                let _ = app.notification().on_action(move |_action| {
                    notify_handle.post_job(Box::new(|core: &mut Core| {
                        if monitor::enabled_feed(core) {
                            core.monitor.state["provider"] = serde_json::json!("codex");
                            monitor::show_panel(core);
                        }
                    }));
                });
            }
            let state = app.state::<CoreHandle>();
            state.post_job(Box::new(move |core: &mut Core| {
                let ready = monitor::start(core, at_login);
                core.ready = ready;
                if ready {
                    automations::scheduler::start(core);
                    automations::system_wake::sync(core);
                    if core.prefs().get("openAtLogin").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                        let handle = core.handle.clone();
                        handle.spawn_task(async move {
                            let _ = tokio::task::spawn_blocking(|| {
                                login_startup::LoginStartup::new(true).reconcile()
                            })
                            .await;
                        });
                    }
                } else {
                    // No menu bar entry: offer settings or quit, never a headless app.
                    let choice = failure_dialog();
                    if choice == 0 {
                        core.settings.selected_page = "general".into();
                        windows::ensure_settings_window(core);
                    } else {
                        core.app.exit(0);
                    }
                }
            }));
            Ok(())
        })
        .on_window_event(|_window, _event| {})
        .build(tauri::generate_context!())
        .expect("MeterNest 启动失败")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = app.try_state::<CoreHandle>() {
                    state.post_job(Box::new(|core: &mut Core| {
                        automations::system_wake::dispose(core);
                        automations::scheduler::dispose(core);
                        monitor::dispose(core);
                    }));
                    // Give the core loop a moment to flush dispose jobs.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
        });
}

fn build_core(app: tauri::AppHandle, packaged: bool, store: store::Store) -> (Core, std::sync::mpsc::Receiver<core::Job>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = CoreHandle { tx };
    let preferences = store.get("preferences");
    let monitor = monitor::Monitor::new(&preferences, store.get("resetNotificationHistory"), store.get("resetFeedCache"));
    let scheduler = automations::scheduler::Scheduler::new(preferences, store.get("automationHistory"));
    let wake = automations::system_wake::SystemWake::new(packaged);
    let core = Core {
        app,
        handle,
        store,
        monitor,
        scheduler,
        wake,
        settings: SettingsState {
            selected_page: "general".into(),
            created: false,
            loaded: false,
            pending_navigate: None,
        },
        ready: false,
        packaged,
    };
    (core, rx)
}

fn config_failure_dialog(error: &error::AppError) {
    let script = r#"on run argv
        display dialog (item 1 of argv) with title "MeterNest" buttons {"退出 MeterNest"} default button 1 with icon stop
    end run"#;
    let message = format!("无法读取或备份 MeterNest 的配置文件。请检查用户资料库中 Application Support/MeterNest 的权限后重试。\n\n{}", error);
    let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", script, "--", &message]).output();
}

fn build_application_menu(app: &tauri::App) -> tauri::Result<()> {
    let about = MenuItem::with_id(app, "about", "关于 MeterNest", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings-menu", "设置…", true, Some("CmdOrCtrl+,"))?;
    let quit = PredefinedMenuItem::quit(app, None)?;
    let app_menu = Submenu::with_items(app, "MeterNest", true, &[&about, &settings, &PredefinedMenuItem::separator(app)?, &quit])?;
    let edit_menu = Submenu::with_items(
        app,
        "编辑",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let menu = Menu::with_items(app, &[&app_menu, &edit_menu])?;
    app.set_menu(menu)?;
    let handle = app.handle().clone();
    app.on_menu_event(move |app, event| {
        let state = app.state::<CoreHandle>();
        match event.id().as_ref() {
            "about" => state.post_job(Box::new(|core: &mut Core| {
                core.settings.selected_page = "about".into();
                windows::ensure_settings_window(core);
            })),
            "settings-menu" => state.post_job(Box::new(|core: &mut Core| {
                core.settings.selected_page = "general".into();
                windows::ensure_settings_window(core);
            })),
            _ => {
                let _ = handle;
            }
        }
    });
    Ok(())
}

fn set_tray_menu(app: &tauri::AppHandle, language: &str) -> tauri::Result<()> {
    let view = MenuItem::with_id(app, "tray-view", i18n::text(language, "额度速览"), true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "tray-refresh", i18n::text(language, "刷新数据"), true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "tray-settings", i18n::text(language, "设置"), true, None::<&str>)?;
    let about = MenuItem::with_id(app, "tray-about", i18n::text(language, "关于"), true, None::<&str>)?;
    let quit = PredefinedMenuItem::quit(app, Some(i18n::text(language, "退出")))?;
    let menu = Menu::with_items(
        app,
        &[
            &view,
            &refresh,
            &settings,
            &about,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let Some(tray) = app.tray_by_id("main") else {
        return Ok(());
    };
    tray.set_menu(Some(menu))
}

fn build_tray(app: &tauri::App, language: &str) -> tauri::Result<()> {
    set_tray_menu(app.handle(), language)?;
    let Some(tray) = app.tray_by_id("main") else {
        return Ok(());
    };
    tray.set_show_menu_on_left_click(false)?;
    tray.on_menu_event(|app, event| {
        let state = app.state::<CoreHandle>();
        match event.id().as_ref() {
            "tray-view" => state.post_job(Box::new(|core: &mut Core| {
                monitor::show_panel(core);
            })),
            "tray-refresh" => state.post_job(Box::new(|core: &mut Core| {
                let provider = core.monitor.state.get("provider").and_then(serde_json::Value::as_str).unwrap_or("codex").to_string();
                monitor::refresh(core, &provider);
            })),
            "tray-settings" => state.post_job(Box::new(|core: &mut Core| {
                core.settings.selected_page = "general".into();
                windows::ensure_settings_window(core);
            })),
            "tray-about" => state.post_job(Box::new(|core: &mut Core| {
                core.settings.selected_page = "about".into();
                windows::ensure_settings_window(core);
            })),
            _ => {}
        }
    });
    let app_handle = app.handle().clone();
    tray.on_tray_icon_event(move |_tray, event| {
        if let TrayIconEvent::Click { button, button_state, .. } = event {
            let state = app_handle.state::<CoreHandle>();
            match (button, button_state) {
                (MouseButton::Left, MouseButtonState::Down) => {
                    // Record whether we should open before the tray changes focus.
                    state.post_job(Box::new(|core: &mut Core| {
                        monitor::on_tray_left_press(core);
                    }));
                }
                (MouseButton::Left, MouseButtonState::Up) => {
                    state.post_job(Box::new(|core: &mut Core| {
                        monitor::on_tray_left_release(core);
                    }));
                }
                (MouseButton::Right, MouseButtonState::Down) => {
                    state.post_job(Box::new(|core: &mut Core| {
                        monitor::hover::hide(core);
                        monitor::hide_panel(core);
                    }));
                }
                _ => {}
            }
        } else if matches!(event, TrayIconEvent::Enter { .. }) {
            let state = app_handle.state::<CoreHandle>();
            state.post_job(Box::new(|core: &mut Core| {
                monitor::hover::enter(core);
            }));
        } else if matches!(event, TrayIconEvent::Leave { .. }) {
            let state = app_handle.state::<CoreHandle>();
            state.post_job(Box::new(|core: &mut Core| {
                monitor::hover::hide(core);
            }));
        }
    });
    // Upgrade the tray icon to a multi-representation template NSImage.
    let _ = app.run_on_main_thread(move || {
        if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
            let reps: Vec<(u32, Vec<u8>)> = [1u32, 2, 3]
                .iter()
                .map(|scale| (*scale, monitor::tray_icon::rasterize(*scale)))
                .collect();
            macos::apply_tray_icon(&reps, mtm);
        }
    });
    Ok(())
}

/// The Electron failure dialog: open settings or quit.
fn failure_dialog() -> i32 {
    let script = r#"display dialog "无法创建菜单栏图标。请检查菜单栏是否可用，然后重新启动 MeterNest。也可以先打开设置检查配置。" with title "MeterNest" buttons {"打开设置", "退出 MeterNest"} default button 1 cancel button 2 with icon stop"#;
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output();
    match output {
        Ok(result) => {
            if result.status.success() {
                0
            } else {
                1
            }
        }
        Err(_) => 1,
    }
}

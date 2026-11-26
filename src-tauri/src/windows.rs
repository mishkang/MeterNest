use crate::core::Core;
use crate::macos;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const PANEL_WIDTH: f64 = 360.0;
pub const PANEL_HEIGHT: f64 = 530.0;
// Transparent space for the CSS shadow. Keep in sync with panel.css.
const PANEL_SHADOW_SIDE: f64 = 24.0;
const PANEL_SHADOW_BOTTOM: f64 = 32.0;

pub fn position_panel(anchor: (f64, f64, f64, f64), area: macos::ScreenFrame, max_height: f64) -> (f64, f64, f64, f64) {
    // Fit the visible panel in the work area and its shadow on the display.
    let left = area.x.max(area.frame_x + PANEL_SHADOW_SIDE);
    let right = (area.x + area.width).min(area.frame_x + area.frame_width - PANEL_SHADOW_SIDE);
    let top = area.y.max(area.frame_y);
    let bottom = (area.y + area.height).min(area.frame_y + area.frame_height - PANEL_SHADOW_BOTTOM);
    let width = PANEL_WIDTH.min(right - left);
    let height = max_height.min(bottom - top);
    let x = (anchor.0 + anchor.2 / 2.0 - width / 2.0).round().clamp(left, right - width);
    // The transparent padding receives mouse events too. Keep it off the tray.
    let below = (anchor.1 + anchor.3).max(area.y);
    let preferred = if below + height <= bottom { below } else { anchor.1 - PANEL_SHADOW_BOTTOM - height };
    let y = preferred.clamp(top, bottom - height);
    (x - PANEL_SHADOW_SIDE, y, width + 2.0 * PANEL_SHADOW_SIDE, height + PANEL_SHADOW_BOTTOM)
}

/// hover.js positioning: tooltip kept in the work area near the anchor.
pub fn position_hover(anchor: (f64, f64, f64, f64), area: macos::ScreenFrame, width: f64, height: f64) -> (f64, f64) {
    let width = width.min(area.width - 12.0);
    let x = (area.x + 6.0).max(((anchor.0 + anchor.2 / 2.0 - width / 2.0).round()).min(area.x + area.width - width - 6.0));
    // The HTML adds 4px of transparent shadow space; keep the visible edge
    // about 6px below the menu bar including that space.
    let gap = 2.0;
    let below = anchor.1 + anchor.3 + gap;
    let y = (area.y + gap).max(if below + height <= area.y + area.height - gap { below } else { anchor.1 - height - gap }.min(area.y + area.height - height - gap));
    (x, y)
}

pub fn ensure_panel_window(core: &mut Core) -> bool {
    if core.monitor.panel_created {
        return true;
    }
    let app = core.app.clone();
    let window = WebviewWindowBuilder::new(&app, "panel", WebviewUrl::App("codex-monitor/panel.html".into()))
        .title(crate::i18n::text(crate::i18n::resolve(&core.prefs()), "额度速览"))
        .initialization_script(crate::i18n::initialization_script(core))
        .initialization_script("document.addEventListener('contextmenu', event => event.preventDefault(), true)")
        .inner_size(PANEL_WIDTH + 2.0 * PANEL_SHADOW_SIDE, PANEL_HEIGHT + PANEL_SHADOW_BOTTOM)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .fullscreen(false)
        .skip_taskbar(true)
        .accept_first_mouse(true)
        // Let the rounded web content define the visible edge of the window.
        .shadow(false)
        .transparent(true)
        .visible(false)
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                if let Some(state) = window.app_handle().try_state::<crate::core::CoreHandle>() {
                    state.post_job(Box::new(|core: &mut Core| {
                        core.monitor.panel_loaded = true;
                        crate::monitor::publish(core);
                    }));
                }
            }
        })
        .build();
    match window {
        Ok(window) => {
            if !macos::panel::attach(&window) {
                let _ = window.destroy();
                return false;
            }
            core.monitor.panel_created = true;
            core.monitor.panel_loaded = false;
            true
        }
        Err(_) => false,
    }
}

/// settings.js show(page): create or focus the settings window.
pub fn ensure_settings_window(core: &mut Core) {
    let app = core.app.clone();
    let page = core.settings.selected_page.clone();
    if core.settings.created {
        if let Some(window) = app.get_webview_window("settings") {
            if !core.settings.loaded {
                core.settings.pending_navigate = Some(page);
            } else {
                let _ = window.emit_to("settings", "meternest-settings-navigate", &page);
            }
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.unminimize();
        }
        return;
    }
    let builder = WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings/index.html".into()))
        .title(format!("MeterNest · {}", crate::i18n::text(crate::i18n::resolve(&core.prefs()), "设置")))
        .initialization_script(crate::i18n::initialization_script(core))
        .initialization_script("document.addEventListener('contextmenu', event => event.preventDefault(), true)")
        .inner_size(700.0, 700.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .fullscreen(false)
        .background_color(tauri::window::Color(245, 247, 250, 255))
        .visible(false)
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let handle = window.app_handle();
                let state = handle.state::<crate::core::CoreHandle>();
                let app = handle.clone();
                state.post_job(Box::new(move |core: &mut Core| {
                    core.settings.loaded = true;
                    if let Some(window) = app.get_webview_window("settings") {
                        let page = core.settings.pending_navigate.take().unwrap_or_else(|| core.settings.selected_page.clone());
                        let _ = window.emit_to("settings", "meternest-settings-navigate", &page);
                    }
                }));
            }
        });
    if let Ok(window) = builder.build() {
        core.settings.created = true;
        core.settings.loaded = false;
        core.settings.pending_navigate = Some(page);
        let state = core.handle.clone();
        window.on_window_event(move |event| {
            match event {
                tauri::WindowEvent::Focused(true) => {
                    // Clicking the settings window collapses the quick panel.
                    state.post_job(Box::new(|core: &mut Core| {
                        if core.monitor.panel_visible
                            && core.app.get_webview_window("settings").is_some_and(|settings| settings.is_focused().unwrap_or(false))
                        {
                            crate::monitor::hide_panel(core);
                        }
                    }));
                }
                tauri::WindowEvent::Destroyed => {
                    state.post_job(Box::new(|core: &mut Core| {
                        core.settings.created = false;
                        core.settings.loaded = false;
                        core.settings.pending_navigate = None;
                        if !core.ready {
                            core.app.exit(0);
                        }
                    }));
                }
                _ => {}
            }
        });
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Create the transparent hover tooltip window (once).
pub fn ensure_hover_window(core: &mut Core) -> bool {
    if core.monitor.hover.created {
        return true;
    }
    let app = core.app.clone();
    let window = WebviewWindowBuilder::new(&app, "hover", WebviewUrl::App("codex-monitor/hover.html".into()))
        .title(crate::i18n::text(crate::i18n::resolve(&core.prefs()), "MeterNest 额度提示"))
        .initialization_script(crate::i18n::initialization_script(core))
        .initialization_script("document.addEventListener('contextmenu', event => event.preventDefault(), true)")
        .inner_size(160.0, 42.0)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .fullscreen(false)
        .skip_taskbar(true)
        .shadow(false)
        .transparent(true)
        .focusable(false)
        .visible(false)
        .build();
    match window {
        Ok(window) => {
            macos::configure_hover_window(&window);
            let _ = window.set_ignore_cursor_events(true);
            core.monitor.hover.created = true;
            core.monitor.hover.loaded = false;
            true
        }
        Err(_) => false,
    }
}

pub fn set_hover_bounds(window: &tauri::WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    let _ = window.set_size(tauri::LogicalSize::new(width, height));
    let _ = window.set_position(tauri::LogicalPosition::new(x, y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_shadow_stays_below_the_menu_bar() {
        let area = macos::ScreenFrame {
            x: 0.0, y: 30.0, width: 1440.0, height: 820.0,
            frame_x: 0.0, frame_y: 0.0, frame_width: 1440.0, frame_height: 900.0,
        };
        let anchor = (700.0, 0.0, 42.0, 30.0);
        for height in [370.0, 744.0] {
            let (_, y, _, _) = position_panel(anchor, area, height);
            assert_eq!(y, anchor.1 + anchor.3);
        }
    }
}

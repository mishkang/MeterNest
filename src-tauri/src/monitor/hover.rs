use crate::core::Core;
use crate::macos;
use serde_json::Value;
use tauri::{Emitter, Manager};

/// hover.js: a 200ms-delayed tooltip below the tray icon. The window ignores
/// mouse events and never takes focus.
#[derive(Default)]
pub struct HoverState {
    pub created: bool,
    pub loaded: bool,
    pub visible: bool,
    pub active: bool,
    pub timer_gen: u64,
}

pub fn enter(core: &mut Core) {
    if core.monitor.disposed || core.monitor.hover.active || !can_show(core) {
        return;
    }
    core.monitor.hover.active = true;
    core.monitor.hover.timer_gen += 1;
    let generation = core.monitor.hover.timer_gen;
    core.handle.post_after(200, Box::new(move |core: &mut Core| {
        if core.monitor.hover.timer_gen != generation || core.monitor.disposed {
            return;
        }
        show(core);
    }));
}

fn can_show(core: &Core) -> bool {
    !core.monitor.disposed && core.app.tray_by_id("main").is_some() && !core.monitor.panel_visible
}

pub fn hide(core: &mut Core) {
    core.monitor.hover.active = false;
    core.monitor.hover.timer_gen += 1;
    if core.monitor.hover.visible {
        core.monitor.hover.visible = false;
        if core.monitor.hover.created {
            if let Some(window) = core.app.get_webview_window("hover") {
                macos::order_out(&window);
            }
        }
    }
}

pub fn show(core: &mut Core) {
    if core.monitor.disposed || !core.monitor.hover.active || !can_show(core) {
        return;
    }
    if !core.monitor.hover.created {
        if !crate::windows::ensure_hover_window(core) {
            return;
        }
    }
    if !core.monitor.hover.loaded {
        // The page reports readiness via the meternest_hover_ready command.
        return;
    }
    request_update(core);
}

/// hover.js update(): ask the page to re-render, measure and report its box.
pub fn request_update(core: &mut Core) {
    if !core.monitor.hover.created || !core.monitor.hover.loaded {
        return;
    }
    if let Some(window) = core.app.get_webview_window("hover") {
        let summary = crate::monitor::tray_title::hover_summary(&core.monitor.state);
        let _ = window.emit_to("hover", "meternest-hover-updated", &summary);
    }
}

/// Called on every publish while the tooltip is visible.
pub fn hover_update(core: &mut Core, summary: Value) {
    let _ = summary;
    if core.monitor.hover.visible {
        request_update(core);
    }
}

/// The page measured itself; position the window and reveal it.
pub fn fit(core: &mut Core, width: f64, height: f64) -> bool {
    if !core.monitor.hover.created {
        return false;
    }
    if !width.is_finite() || !height.is_finite() || !(40.0..=1000.0).contains(&width) || !(20.0..=600.0).contains(&height) {
        return false;
    }
    let Some((anchor, area)) = hover_geometry(&core.app) else {
        return false;
    };
    if let Some(window) = core.app.get_webview_window("hover") {
        let (x, y) = crate::windows::position_hover(anchor, area, width, height);
        crate::windows::set_hover_bounds(&window, x, y, width, height);
        if core.monitor.hover.active && can_show(core) && !core.monitor.hover.visible {
            core.monitor.hover.visible = true;
            macos::order_front_regardless(&window);
        }
        return true;
    }
    false
}

pub fn on_page_ready(core: &mut Core) {
    core.monitor.hover.loaded = true;
    if core.monitor.hover.active {
        show(core);
    }
}

fn hover_geometry(app: &tauri::AppHandle) -> Option<((f64, f64, f64, f64), macos::ScreenFrame)> {
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

pub fn dispose(core: &mut Core) {
    hide(core);
    if core.monitor.hover.created {
        if let Some(window) = core.app.get_webview_window("hover") {
            let _ = window.destroy();
        }
    }
    core.monitor.hover.created = false;
    core.monitor.hover.visible = false;
    core.monitor.hover.loaded = false;
}

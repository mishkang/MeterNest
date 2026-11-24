use tauri::Manager;

/// Resolve bundled resources both in dev (manifest-relative) and packaged
/// (resource dir) builds.
pub fn resource_path(app: &tauri::AppHandle, relative: &str) -> Option<std::path::PathBuf> {
    if let Ok(dir) = app.path().resource_dir() {
        // Tauri keeps the `resources/` prefix for entries declared as
        // `resources/**/*`, while individual entries such as `bin/...` are
        // copied at the resource root. Accept both layouts.
        for candidate in [dir.join(relative), dir.join("resources").join(relative)] {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join(relative);
    if dev.exists() {
        return Some(dev);
    }
    None
}

/// The privileged wake helper binary.
pub fn wake_helper_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    resource_path(app, "bin/meternest-wake-helper")
        .or_else(|| {
            let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin/meternest-wake-helper");
            dev.exists().then_some(dev)
        })
}

pub fn workbuddy_query_script(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    resource_path(app, "workbuddy/codex-monitor/workbuddy-query.cjs")
}

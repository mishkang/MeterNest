use crate::automations::scheduler::Scheduler;
use crate::automations::system_wake::SystemWake;
use crate::monitor::Monitor;
use crate::prefs::read_preferences;
use crate::store::Store;
use serde_json::Value;
use std::sync::mpsc;

/// A unit of work executed on the single core loop, mirroring the Electron
/// main process event loop. Async IO happens on the Tauri runtime and posts
/// results back as jobs.
pub type Job = Box<dyn FnOnce(&mut Core) + Send>;

#[derive(Clone)]
pub struct CoreHandle {
    pub tx: mpsc::Sender<Job>,
}

impl CoreHandle {
    pub fn post_job(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// setTimeout equivalent: schedule a job onto the core loop.
    pub fn post_after(&self, ms: i64, job: Job) {
        let handle = self.clone();
        tauri::async_runtime::spawn(async move {
            if ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            }
            handle.post_job(job);
        });
    }

    pub fn spawn_task<F>(&self, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tauri::async_runtime::spawn(task);
    }
}

pub struct Core {
    pub app: tauri::AppHandle,
    pub handle: CoreHandle,
    pub store: Store,
    pub monitor: Monitor,
    pub scheduler: Scheduler,
    pub wake: SystemWake,
    pub settings: SettingsState,
    pub ready: bool,
    pub packaged: bool,
}

/// settings.js window bookkeeping.
pub struct SettingsState {
    pub selected_page: String,
    pub created: bool,
    pub loaded: bool,
    /// A queued navigation for a window that is still loading.
    pub pending_navigate: Option<String>,
}

impl Core {
    pub fn prefs(&self) -> Value {
        read_preferences(&self.store.get("preferences"))
    }

    /// The proxy URL used by Codex/feed queries ('' for direct).
    pub fn proxy_url(&self) -> String {
        let prefs = self.prefs();
        if prefs.get("networkMode").and_then(Value::as_str) == Some("direct") {
            return String::new();
        }
        prefs
            .get("proxyUrl")
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:7897")
            .to_string()
    }
}

/// powerMonitor 'resume' + network recovery entry point.
pub fn on_system_resume(core: &mut Core, resumed_at: i64) {
    crate::automations::system_wake::resume(core, resumed_at);
    crate::automations::scheduler::on_power_resume(core);
    crate::monitor::refresh_background(core);
}

/// Launch the core loop thread. The sender stays alive in the AppHandle side.
pub fn run_loop(mut core: Core, rx: mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        job(&mut core);
        if core.ready {
            // nothing special
        }
    }
}

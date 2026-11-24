use crate::core::Core;
use crate::macos::power::PowerAssertion;
use crate::util::now_ms;
use serde_json::{json, Value};

const WAKE_LEAD_MS: i64 = 30_000;
const SYSTEM_WAKE_SETTLE_MS: i64 = 30_000;
const WAKE_EVENT_GRACE_MS: i64 = 15_000;

/// system-wake.js wakePlan.
pub fn wake_plan(preferences: &Value, deadline: Option<i64>, now: i64, pid: i32) -> Value {
    let codex = crate::prefs::normalize_automation(preferences.get("automation").cloned().unwrap_or(Value::Null))["codex"].clone();
    let enabled = preferences.get("enabledProviders")
        .and_then(|p| p.get("codex"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
        && codex.get("wake").and_then(Value::as_bool).unwrap_or(false)
        && (codex.get("onReset").and_then(Value::as_bool).unwrap_or(false) || codex.get("daily").and_then(Value::as_bool).unwrap_or(false))
        && deadline.map(|deadline| deadline > now && deadline <= now + 8 * 86400000).unwrap_or(false);
    json!({
        "version": 1,
        "enabled": enabled,
        "pid": pid,
        "wakeAt": if enabled { (now + 2000).max(deadline.unwrap_or(0) - WAKE_LEAD_MS) } else { 0 },
        "deadline": if enabled { deadline } else { Some(0) }
    })
}

pub struct SystemWake {
    pub available: bool,
    pub error: String,
    pub hold: Option<PowerAssertion>,
    pub hold_timer_gen: u64,
    pub guard_timer_gen: u64,
    pub guarded_deadline: i64,
    pub planned_deadline: i64,
    pub planned_wake: i64,
    pub last_plan: String,
    pub disposed: bool,
    settle_until: Option<i64>,
    sleep_plan: Option<(i64, i64)>,
}

impl SystemWake {
    pub fn new(packaged: bool) -> SystemWake {
        let uid = unsafe { libc::getuid() };
        let home_ok = dirs::home_dir().map(|home| home.to_string_lossy().starts_with("/Users/")).unwrap_or(false);
        SystemWake {
            available: home_ok && uid >= 501 && packaged,
            error: String::new(),
            hold: None,
            hold_timer_gen: 0,
            guard_timer_gen: 0,
            guarded_deadline: 0,
            planned_deadline: 0,
            planned_wake: 0,
            last_plan: String::new(),
            disposed: false,
            settle_until: None,
            sleep_plan: None,
        }
    }
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn app_bundle() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.ancestors().nth(3)?.to_path_buf();
    bundle.to_string_lossy().ends_with(".app").then_some(bundle)
}

fn paths() -> (String, String, String, String, i32) {
    let user = uid();
    let home = dirs::home_dir().map(|home| home.to_string_lossy().to_string()).unwrap_or_default();
    let plan = format!("{}/Library/Application Support/MeterNest/wake-plan.json", home);
    (format!("com.meternest.wake.{}", user), format!("/Library/PrivilegedHelperTools/com.meternest.wake.{}", user), format!("/Library/LaunchDaemons/com.meternest.wake.{}.plist", user), plan, std::process::id() as i32)
}

fn installed() -> bool {
    let (_, helper, plist, _, _) = paths();
    for file in [helper, plist] {
        let Ok(stat) = std::fs::symlink_metadata(&file) else {
            return false;
        };
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if !stat.is_file() || stat.is_symlink() || stat.uid() != 0 || stat.permissions().mode() & 0o022 != 0 {
            return false;
        }
    }
    true
}

pub fn snapshot(core: &Core) -> Value {
    let status = {
        let file = format!("/var/db/com.meternest.wake.{}.json", uid());
        std::fs::read_to_string(&file)
            .ok()
            .filter(|_| {
                std::fs::symlink_metadata(&file)
                    .map(|stat| {
                        use std::os::unix::fs::MetadataExt;
                        stat.is_file() && stat.uid() == 0 && stat.size() < 4096
                    })
                    .unwrap_or(false)
            })
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .unwrap_or(Value::Null)
    };
    let outcome = status.get("outcome").and_then(Value::as_str).unwrap_or("");
    let scheduled_at = status
        .get("wakeAt")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > now_ms() as f64)
        .filter(|_| outcome == "scheduled")
        .map(|value| value as i64);
    json!({
        "available": core.wake.available,
        "installed": installed(),
        "error": core.wake.error,
        "scheduledAt": scheduled_at,
        "outcome": if ["scheduled", "cancelled", "error"].contains(&outcome) { outcome } else { "idle" }
    })
}

fn wake_enabled(core: &Core) -> bool {
    let prefs = core.prefs();
    let codex = crate::prefs::normalize_automation(prefs.get("automation").cloned().unwrap_or(Value::Null))["codex"].clone();
    let window_supported = crate::automations::scheduler::codex_window_supported(core);
    core.wake.available
        && installed()
        && prefs.get("enabledProviders").and_then(|p| p.get("codex")).and_then(Value::as_bool).unwrap_or(true)
        && codex.get("wake").and_then(Value::as_bool).unwrap_or(false)
        && ((window_supported && codex.get("onReset").and_then(Value::as_bool).unwrap_or(false))
            || codex.get("daily").and_then(Value::as_bool).unwrap_or(false))
}

pub fn settling_deadline(core: &Core, now: i64) -> Option<i64> {
    core.wake.settle_until.filter(|at| *at > now)
}

pub fn waiting_for_scheduled_resume(core: &Core) -> bool {
    core.wake.sleep_plan.is_some()
}

pub fn prepare_for_sleep(core: &mut Core, sleeping_at: i64) {
    let wake_at = core.wake.planned_wake;
    core.wake.sleep_plan = (wake_at > sleeping_at && wake_enabled(core))
        .then_some((wake_at, core.wake.planned_deadline));
}

fn scheduled_resume_deadline(wake_at: i64, task_at: i64, resumed_at: i64) -> Option<i64> {
    (wake_at > 0 && (wake_at..=task_at + WAKE_EVENT_GRACE_MS).contains(&resumed_at))
        .then_some(resumed_at + SYSTEM_WAKE_SETTLE_MS)
}

fn release(core: &mut Core) {
    core.wake.hold_timer_gen += 1;
    core.wake.guarded_deadline = 0;
    if core.wake.hold.is_some() {
        if let Some(mut hold) = core.wake.hold.take() {
            hold.stop();
        }
    }
}

fn protect(core: &mut Core, deadline: i64) {
    if core.wake.guarded_deadline == deadline {
        return;
    }
    release(core);
    if let Some(assertion) = PowerAssertion::start() {
        core.wake.hold = Some(assertion);
        core.wake.guarded_deadline = deadline;
        let generation = core.wake.hold_timer_gen;
        core.handle.post_after(180000, Box::new(move |core: &mut Core| {
            if core.wake.hold_timer_gen == generation {
                release(core);
            }
        }));
    }
}

fn write_plan(core: &mut Core, plan: &Value) -> Result<(), String> {
    let value = serde_json::to_string(plan).unwrap_or_default();
    if core.wake.last_plan == value {
        return Ok(());
    }
    let (_, _, _, plan_file, _) = paths();
    let directory = std::path::Path::new(&plan_file)
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_default();
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let temporary = format!("{}.tmp-{}", plan_file, std::process::id());
    std::fs::write(&temporary, &value).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, &plan_file).map_err(|error| error.to_string())?;
    core.wake.last_plan = value;
    Ok(())
}

pub fn sync(core: &mut Core) {
    if core.wake.disposed {
        return;
    }
    core.wake.guard_timer_gen += 1;
    let deadline = crate::automations::scheduler::next_codex_due(core);
    let now = now_ms();
    let mut plan = wake_plan(&core.prefs(), deadline, now, std::process::id() as i32);
    if plan.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        let plan_deadline = plan.get("deadline").and_then(Value::as_i64).unwrap_or(0);
        if core.wake.planned_deadline == plan_deadline {
            plan["wakeAt"] = json!(core.wake.planned_wake);
        } else {
            core.wake.planned_deadline = plan_deadline;
            core.wake.planned_wake = plan.get("wakeAt").and_then(Value::as_i64).unwrap_or(0);
        }
    } else {
        core.wake.planned_deadline = 0;
        core.wake.planned_wake = 0;
    }
    if !wake_enabled(core) {
        plan["enabled"] = json!(false);
    }
    if let Err(_error) = write_plan(core, &plan) {
        core.wake.error = "无法保存系统唤醒计划，请检查配置目录。".to_string();
    } else {
        core.wake.error = String::new();
    }
    if !wake_enabled(core) {
        release(core);
        core.wake.settle_until = None;
        core.wake.sleep_plan = None;
        core.wake.guarded_deadline = 0;
        return;
    }
    if plan.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        let deadline = plan.get("deadline").and_then(Value::as_i64).unwrap_or(0);
        let delay = deadline - WAKE_LEAD_MS - now_ms();
        if delay <= 0 {
            protect(core, deadline);
        } else {
            let generation = core.wake.guard_timer_gen;
            core.handle.post_after(delay, Box::new(move |core: &mut Core| {
                if core.wake.guard_timer_gen == generation {
                    sync(core);
                }
            }));
        }
    }
}

pub fn resume(core: &mut Core, resumed_at: i64) {
    if core.wake.disposed {
        return;
    }
    let sleep_plan = core.wake.sleep_plan.take();
    let deadline = crate::automations::scheduler::next_codex_due(core);
    let now = now_ms();
    let plan = wake_plan(&core.prefs(), deadline, now, std::process::id() as i32);
    if wake_enabled(core) {
        core.wake.settle_until = sleep_plan.and_then(|(wake_at, task_at)| scheduled_resume_deadline(wake_at, task_at, resumed_at));
        let plan_deadline = plan.get("deadline").and_then(Value::as_i64).unwrap_or(0);
        if let (Some(_), Some((_, task_at))) = (core.wake.settle_until, sleep_plan) {
            release(core);
            protect(core, task_at);
        } else if plan.get("enabled").and_then(Value::as_bool).unwrap_or(false) && plan_deadline - now_ms() <= 90000 {
            protect(core, plan_deadline);
        } else if core.wake.planned_deadline > 0 && (core.wake.planned_deadline - now_ms()).abs() <= 180000 {
            protect(core, core.wake.planned_deadline);
        }
    }
    sync(core);
}

/// Everything needed to run the privileged install/uninstall away from the
/// core loop so the admin prompt does not freeze the rest of the app.
pub struct WakeManage {
    app: tauri::AppHandle,
    action: String,
    label: String,
    helper: String,
    plist: String,
    plan_file: String,
    pid: i32,
    home: String,
}

pub fn prepare_manage(core: &Core, action: &str) -> Result<WakeManage, String> {
    if !core.wake.available || !matches!(action, "install" | "uninstall") {
        return Err("请在已安装的 macOS 正式应用中授权系统唤醒。".to_string());
    }
    let (label, helper, plist, plan_file, pid) = paths();
    let home = dirs::home_dir().map(|home| home.to_string_lossy().to_string()).unwrap_or_default();
    Ok(WakeManage {
        app: core.app.clone(),
        action: action.to_string(),
        label,
        helper,
        plist,
        plan_file,
        pid,
        home,
    })
}

pub fn execute_manage(context: WakeManage) -> Result<(), String> {
    let staging = std::env::temp_dir().join(format!("meternest-wake-authorize-{}", std::process::id()));
    std::fs::create_dir_all(&staging).map_err(|_| "系统授权未完成或组件安装失败，请重试。".to_string())?;
    let script = if context.action == "install" {
        let source = crate::paths::wake_helper_path(&context.app)
            .filter(|path| path.is_file())
            .ok_or_else(|| "缺少系统唤醒组件，请重新安装 MeterNest。".to_string())?;
        let bundle = app_bundle().ok_or_else(|| "无法校验 MeterNest 应用签名。".to_string())?;
        let source_path = source.canonicalize().map_err(|_| "缺少系统唤醒组件，请重新安装 MeterNest。".to_string())?;
        let bundle_path = bundle.canonicalize().unwrap_or(bundle);
        if !source_path.starts_with(&bundle_path) {
            return Err("系统唤醒组件不在应用包内，已拒绝安装。".to_string());
        }
        let verify = format!("/usr/bin/codesign --verify --strict {}", quote(&bundle_path.to_string_lossy()));
        let payload = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>{}</string><string>{}</string></array><key>RunAtLoad</key><true/><key>StartInterval</key><integer>15</integer><key>WatchPaths</key><array><string>{}</string></array><key>ProcessType</key><string>Background</string></dict></plist>",
            context.label,
            context.helper,
            uid(),
            xml(&context.home),
            xml(&context.plan_file)
        );
        let staged = staging.join("wake.plist");
        std::fs::write(&staged, payload).map_err(|_| "系统授权未完成或组件安装失败，请重试。".to_string())?;
        format!(
            "{5}\n/bin/launchctl bootout system {0} >/dev/null 2>&1 || true\n/usr/bin/install -d -o root -g wheel -m 755 /Library/PrivilegedHelperTools\n/usr/bin/install -o root -g wheel -m 755 {1} {2}\n/usr/bin/install -o root -g wheel -m 644 {3} {4}\n/bin/launchctl bootstrap system {4}",
            quote(&context.plist),
            quote(&source_path.to_string_lossy()),
            quote(&context.helper),
            quote(&staged.to_string_lossy()),
            quote(&context.plist),
            verify
        )
    } else {
        format!(
            "{0} {1} {2}\n/bin/launchctl bootout system {3} >/dev/null 2>&1 || true\n/bin/rm -f {0} {3} {4}",
            quote(&context.helper),
            uid(),
            quote(&context.home),
            quote(&context.plist),
            quote(&format!("/var/db/com.meternest.wake.{}.json", uid()))
        )
    };
    let _ = write_plan_value(&context.plan_file, &json!({ "version": 1, "enabled": false, "pid": context.pid, "wakeAt": 0, "deadline": 0 }));
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv\ndo shell script (item 1 of argv) with administrator privileges\nend run",
            "--",
            &format!("set -e\n{}", script),
        ])
        .output()
        .map_err(|_| "系统授权未完成或组件安装失败，请重试。".to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    if !output.status.success() {
        return Err("系统授权未完成或组件安装失败，请重试。".to_string());
    }
    Ok(())
}

pub fn finish_manage(core: &mut Core, result: Result<(), String>) -> Result<Value, String> {
    match result {
        Ok(()) => {
            core.wake.error = String::new();
            core.wake.last_plan = String::new();
            sync(core);
            Ok(snapshot(core))
        }
        Err(message) => Err(message),
    }
}

fn write_plan_value(plan_file: &str, plan: &Value) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(plan_file).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(plan_file, serde_json::to_string(plan).unwrap_or_default())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn xml(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

pub fn dispose(core: &mut Core) {
    core.wake.disposed = true;
    core.wake.hold_timer_gen += 1;
    release(core);
    core.wake.settle_until = None;
    let (_, _, _, plan_file, _) = paths();
    let _ = write_plan_value(&plan_file, &json!({ "version": 1, "enabled": false, "pid": std::process::id() as i32, "wakeAt": 0, "deadline": 0 }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_plan_is_thirty_seconds_before_the_deadline() {
        let prefs = json!({ "automation": { "codex": { "onReset": true, "wake": true } } });
        let plan = wake_plan(&prefs, Some(120_000), 60_000, 123);
        assert_eq!(plan["wakeAt"], 90_000);
        assert_eq!(plan["deadline"], 120_000);
        assert_eq!(wake_plan(&prefs, Some(120_000), 120_000, 123)["enabled"], false);
    }

    #[test]
    fn settling_only_follows_a_system_resume_near_the_scheduled_wake() {
        assert_eq!(scheduled_resume_deadline(90_000, 120_000, 91_000), Some(121_000));
        assert_eq!(scheduled_resume_deadline(90_000, 120_000, 89_999), None);
        assert_eq!(scheduled_resume_deadline(90_000, 120_000, 122_000), Some(152_000));
        assert_eq!(scheduled_resume_deadline(90_000, 120_000, 150_000), None);
        assert_eq!(scheduled_resume_deadline(0, 0, 120_000), None);
    }
}

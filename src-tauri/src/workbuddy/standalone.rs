use super::data::display_credit_data;
use super::ipc_client;
use super::WorkBuddyResult;
use crate::error::AppError;
use crate::paths::workbuddy_query_script;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::time::{timeout, Duration};

const MAX_OUTPUT: usize = 1024 * 1024;
const TIMEOUT_MS: u64 = 12000;

pub struct StandaloneClient {
    app: AppHandle,
    disposed: bool,
    identity_cache: Option<(String, String)>, // stamp → accountKey
}

/// workbuddy-standalone.js findRuntime.
pub async fn find_runtime() -> Option<(PathBuf, PathBuf)> {
    let home = dirs::home_dir()?;
    let mut apps = vec![PathBuf::from("/Applications/WorkBuddy.app")];
    apps.push(home.join("Applications/WorkBuddy.app"));
    for app in apps {
        let binary = app.join("Contents/MacOS/Electron");
        let root = app.join("Contents/Resources/app.asar");
        let Ok(exe) = tokio::fs::metadata(&binary).await else { continue };
        let Ok(archive) = tokio::fs::metadata(&root).await else { continue };
        if exe.is_file() && (archive.is_file() || archive.is_dir()) {
            return Some((binary, root));
        }
    }
    None
}

pub fn shared_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join("Library/Application Support/CodeBuddyExtension/Data/Public"))
}

pub fn identity_stamp() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let file = shared_dir()?.join("auth/workbuddy-desktop.info");
    if std::fs::exists(file.with_extension("info.logged-out")).ok()? {
        return None;
    }
    let stat = std::fs::symlink_metadata(file).ok()?;
    if !stat.is_file() {
        return None;
    }
    Some(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        stat.dev(), stat.ino(), stat.mtime(), stat.mtime_nsec(), stat.ctime(), stat.ctime_nsec(), stat.len()
    ))
}

pub fn config_dir() -> PathBuf {
    std::env::var("WORKBUDDY_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".workbuddy")))
        .unwrap_or_else(|| PathBuf::from(".workbuddy"))
}

impl StandaloneClient {
    pub fn new(app: AppHandle) -> Self {
        StandaloneClient { app, disposed: false, identity_cache: None }
    }

    /// workbuddy-standalone.js read(kind, expectedAccountKey).
    pub async fn read(&mut self, kind: &str, expected_account_key: Option<&str>) -> Result<WorkBuddyResult, AppError> {
        if self.disposed {
            return Err(AppError::message("WorkBuddy 查询已取消"));
        }
        let runtime = find_runtime().await;
        if self.disposed {
            return Err(AppError::message("WorkBuddy 查询已取消"));
        }
        let runtime = match runtime {
            Some(runtime) => runtime,
            None => {
                if matches!(kind, "credits" | "summary") {
                    return ipc_client::read(&self.app, kind == "summary").await.map(WorkBuddyResult::Credits);
                }
                return Err(super::failure("UNSUPPORTED_RUNTIME"));
            }
        };
        self.run_query(kind, expected_account_key, &runtime).await
    }

    async fn run_query(&mut self, kind: &str, expected_account_key: Option<&str>, runtime: &(PathBuf, PathBuf)) -> Result<WorkBuddyResult, AppError> {
        let script = workbuddy_query_script(&self.app).ok_or_else(|| super::failure("UNSUPPORTED_RUNTIME"))?;
        let home = dirs::home_dir().ok_or_else(|| super::failure("UNSUPPORTED_RUNTIME"))?;
        let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for key in ["HOME", "PATH", "TMPDIR", "TMP"] {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.to_string(), value);
            }
        }
        env.insert("ELECTRON_RUN_AS_NODE".to_string(), "1".to_string());
        let child = Command::new(&runtime.0)
            .arg(&script)
            .current_dir(&home)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| super::failure("UNSUPPORTED_RUNTIME"))?;
        let mut payload = json!({
            "root": runtime.1.to_string_lossy(),
            "sharedDir": shared_dir().map(|path| path.to_string_lossy().to_string()),
            "configDir": config_dir().to_string_lossy().to_string()
        });
        if kind != "credits" {
            payload["kind"] = json!(kind);
        }
        if matches!(kind, "checkin" | "checkin-status") {
            payload["expectedAccountKey"] = json!(expected_account_key);
        }
        let body = serde_json::to_string(&payload).unwrap_or_default();
        let output = collect_query_output(child, body.as_bytes(), kind == "credits").await?;
        self.parse_output(kind, &output)
    }

    fn parse_output(&self, kind: &str, output: &[u8]) -> Result<WorkBuddyResult, AppError> {
        let message: Value = serde_json::from_slice(output).map_err(|_| super::failure("UNSUPPORTED_RUNTIME"))?;
        if message.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = message.get("code").and_then(Value::as_str).unwrap_or("");
            if super::message(code).is_some() {
                return Err(super::failure(code));
            }
            return Err(super::failure("UNSUPPORTED_RUNTIME"));
        }
        let account = message.get("account").cloned().unwrap_or(Value::Null);
        if matches!(kind, "identity" | "checkin" | "checkin-status") {
            let account_key = account.get("accountKey").and_then(Value::as_str).unwrap_or("").to_string();
            let valid = account_key.len() == 64 && account_key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
            if !valid {
                return Err(super::failure("UNSUPPORTED_RUNTIME"));
            }
            if kind == "identity" {
                return Ok(WorkBuddyResult::Identity { account_key });
            }
            let allowed: Vec<&str> = if kind == "checkin-status" {
                vec!["pending", "done", "inactive", "ineligible", "own-app"]
            } else {
                vec!["claimed", "done", "inactive", "ineligible", "own-app"]
            };
            let outcome = account.get("outcome").and_then(Value::as_str).unwrap_or("").to_string();
            if !allowed.contains(&outcome.as_str()) {
                return Err(super::failure("UNSUPPORTED_RUNTIME"));
            }
            let credit = account.get("credit").and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0);
            let streak = account.get("streak").and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0);
            return Ok(WorkBuddyResult::Checkin { account_key, outcome, credit, streak });
        }
        if kind == "models" {
            let models = account.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
            if models.is_empty() {
                return Err(super::failure("UNSUPPORTED_RUNTIME"));
            }
            return Ok(WorkBuddyResult::Models(models));
        }
        let status = account.get("status").and_then(Value::as_str).unwrap_or("");
        let balance = account.get("balance").and_then(Value::as_f64);
        let identity = account.get("identity").and_then(Value::as_str).unwrap_or("").to_string();
        let plan = account.get("plan").and_then(Value::as_str).unwrap_or("").to_string();
        let identity_valid = account.get("identity").map(|v| v.is_string()).unwrap_or(false);
        let plan_valid = account.get("plan").map(|v| v.is_string()).unwrap_or(false);
        if status != "ready"
            || !identity_valid
            || !plan_valid
            || !matches!(balance, Some(value) if value.is_finite() && value >= 0.0)
        {
            return Err(super::failure("UNSUPPORTED_RUNTIME"));
        }
        let mut payload = json!({
            "status": "ready",
            "identity": identity.chars().take(200).collect::<String>(),
            "plan": plan.chars().take(100).collect::<String>(),
            "balance": balance.unwrap_or(0.0)
        });
        if let Some(map) = payload.as_object_mut() {
            let display = display_credit_data(&account)?;
            if let Some(fields) = display.as_object() {
                for (key, value) in fields {
                    map.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(WorkBuddyResult::Credits(payload))
    }

    /// workbuddy-standalone.js identity(): stat-stamped account key cache.
    pub async fn identity(&mut self) -> Result<(String, String), AppError> {
        if self.disposed {
            return Err(super::failure("AUTH_REQUIRED"));
        }
        let stamp = identity_stamp().ok_or_else(|| super::failure("AUTH_REQUIRED"))?;
        if let Some((cached, account_key)) = &self.identity_cache {
            if cached == &stamp {
                return Ok((account_key.clone(), stamp));
            }
        }
        let key = match self.read("identity", None).await? {
            WorkBuddyResult::Identity { account_key } => account_key,
            _ => return Err(super::failure("UNSUPPORTED_RUNTIME")),
        };
        if identity_stamp().as_deref() != Some(stamp.as_str()) {
            return Err(super::failure("SESSION_CHANGED"));
        }
        self.identity_cache = Some((stamp.clone(), key.clone()));
        Ok((key, stamp))
    }

    pub async fn credits(&mut self, include_details: bool) -> Result<WorkBuddyResult, AppError> {
        let kind = if include_details { "credits" } else { "summary" };
        match self.read(kind, None).await {
            Ok(result) => Ok(result),
            Err(error) => {
                // Only capability/version failures use IPC when the desktop is
                // running. Expired/revoked credentials never trigger fallback.
                if !self.disposed && error.code == "UNSUPPORTED_RUNTIME" {
                    match ipc_client::read(&self.app, !include_details).await {
                        Ok(value) => Ok(WorkBuddyResult::Credits(value)),
                        Err(_) => Err(error),
                    }
                } else {
                    Err(error)
                }
            }
        }
    }

    pub async fn models(&mut self) -> Result<Vec<Value>, AppError> {
        match self.read("models", None).await? {
            WorkBuddyResult::Models(models) => Ok(models),
            _ => Err(super::failure("UNSUPPORTED_RUNTIME")),
        }
    }

    pub async fn checkin_status(&mut self, expected: &str) -> Result<WorkBuddyResult, AppError> {
        self.read("checkin-status", Some(expected)).await
    }

    pub async fn checkin(&mut self, expected: &str) -> Result<WorkBuddyResult, AppError> {
        self.read("checkin", Some(expected)).await
    }

}

async fn collect_query_output(mut child: Child, body: &[u8], include_credit_details: bool) -> Result<Vec<u8>, AppError> {
    let unsupported = || super::failure("UNSUPPORTED_RUNTIME");
    let mut stdin = child.stdin.take().ok_or_else(unsupported)?;
    let stdout = child.stdout.take().ok_or_else(unsupported)?;
    let (timeout_ms, max_output) = if include_credit_details { (300_000, 64 * 1024 * 1024) } else { (TIMEOUT_MS, MAX_OUTPUT) };
    timeout(Duration::from_millis(timeout_ms), async {
        stdin.write_all(body).await.map_err(|_| unsupported())?;
        drop(stdin);
        let mut output = Vec::new();
        stdout.take((max_output + 1) as u64).read_to_end(&mut output).await.map_err(|_| unsupported())?;
        if output.len() > max_output || !child.wait().await.map_err(|_| unsupported())?.success() {
            return Err(unsupported());
        }
        Ok(output)
    }).await.map_err(|_| super::failure("QUERY_TIMEOUT"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_a_helper_that_closes_stdout_before_exiting() {
        let child = Command::new("/bin/sh")
            .args(["-c", "cat >/dev/null; printf result; exec 1>&-; sleep 0.1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn().unwrap();
        assert_eq!(collect_query_output(child, b"request", false).await.unwrap(), b"result");
    }
}

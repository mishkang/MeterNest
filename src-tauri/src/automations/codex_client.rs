use super::action_error;
use crate::error::AppError;
use crate::monitor::codex_binary::find_codex;
use crate::util::{jwt_decode, now_ms, sha256_hex};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DEFAULT_BASE: &str = "https://chatgpt.com/backend-api/codex";

static BASE_URL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?m)^\s*openai_base_url\s*=\s*["']([^"']+)["']"#).unwrap());
static AUTH_STORE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?m)^\s*cli_auth_credentials_store\s*=\s*["'](file|keyring|auto|ephemeral)["']"#).unwrap());
static VERSION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\d+\.\d+\.\d+").unwrap());

pub struct Credentials {
    pub token: String,
    pub account_id: String,
    pub account_key: String,
    pub revision: String,
}

fn getuid() -> u32 {
    unsafe { libc::getuid() }
}

/// codex-client.js readOwned: O_NOFOLLOW, ownership and permission checks.
fn read_owned(path: &Path) -> Result<String, AppError> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Read;
    let mut file = std::fs::File::options().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let stat = file.metadata()?;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    if !stat.is_file()
        || stat.size() > 1024 * 1024
        || stat.uid() != getuid()
        || stat.permissions().mode() & 0o022 != 0
    {
        return Err(action_error("AUTH_REQUIRED"));
    }
    let mut body = String::new();
    file.by_ref().take(1024 * 1024 + 1).read_to_string(&mut body)?;
    if body.len() > 1024 * 1024 {
        return Err(action_error("AUTH_REQUIRED"));
    }
    Ok(body)
}

pub fn codex_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    match std::env::var("CODEX_HOME") {
        Ok(value) if !value.trim().is_empty() => home.join(value.trim()),
        _ => home.join(".codex"),
    }
}

fn read_base_url() -> Result<url::Url, AppError> {
    let config = codex_home().join("config.toml");
    let body = match std::fs::symlink_metadata(&config) {
        Ok(_) => read_owned(&config)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return parse_base_url(DEFAULT_BASE),
        Err(error) => return Err(error.into()),
    };
    let raw = BASE_URL_RE.captures(&body).and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim()).filter(|value| !value.is_empty());
    parse_base_url(raw.unwrap_or(DEFAULT_BASE))
}

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

fn parse_base_url(raw: &str) -> Result<url::Url, AppError> {
    let url = url::Url::parse(raw).map_err(|_| action_error("CODEX_ENDPOINT_UNSUPPORTED"))?;
    let scheme = url.scheme();
    let path = url.path().trim_end_matches('/');
    let has_credentials = !url.username().is_empty() || url.password().is_some();
    let official = scheme == "https" && url.host_str() == Some("chatgpt.com") && url.port().is_none();
    let loopback = is_loopback(&url) && matches!(scheme, "http" | "https");
    if has_credentials
        || url.query().is_some()
        || url.fragment().is_some()
        || path != "/backend-api/codex"
        || !(official || loopback)
    {
        return Err(action_error("CODEX_ENDPOINT_UNSUPPORTED"));
    }
    Ok(url)
}

/// codex-client.js credentials: file / keyring / auto auth storage.
pub async fn read_credentials() -> Result<Credentials, AppError> {
    let root = codex_home();
    let mut mode = "file".to_string();
    let config_toml = root.join("config.toml");
    if config_toml.exists() || std::fs::symlink_metadata(&config_toml).is_ok() {
        match read_owned(&config_toml) {
            Ok(body) => {
                if let Some(captures) = AUTH_STORE_RE.captures(&body) {
                    mode = captures.get(1).map(|m| m.as_str()).unwrap_or("file").to_string();
                }
            }
            Err(_) => return Err(action_error("AUTH_REQUIRED")),
        }
    }
    if mode == "ephemeral" {
        return Err(action_error("UNSUPPORTED_RUNTIME"));
    }
    let mut raw: Option<String> = None;
    if mode == "keyring" || mode == "auto" {
        let canonical = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let account = format!("cli|{}", &sha256_hex(canonical.to_string_lossy().as_bytes())[..16]);
        let mut command = tokio::process::Command::new("/usr/bin/security");
        command
            .args(["find-generic-password", "-s", "Codex Auth", "-a", &account, "-w"])
            .kill_on_drop(true)
            .stderr(std::process::Stdio::null());
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
            .await
            .map_err(|_| action_error("AUTH_REQUIRED"))?;
        match output {
            Ok(result) if result.status.success() => {
                raw = Some(String::from_utf8_lossy(&result.stdout).trim_end_matches('\n').to_string());
            }
            Ok(result) => {
                let code = result.status.code().unwrap_or(-1);
                if mode != "auto" || code != 44 {
                    return Err(action_error("AUTH_REQUIRED"));
                }
            }
            Err(_) => return Err(action_error("AUTH_REQUIRED")),
        }
    }
    let raw = match raw {
        Some(value) => value,
        None => read_owned(&root.join("auth.json"))?,
    };
    let auth: Value = serde_json::from_str(&raw).map_err(|_| action_error("AUTH_REQUIRED"))?;
    if let Some(auth_mode) = auth.get("auth_mode").and_then(Value::as_str) {
        if !auth_mode.is_empty() && auth_mode != "chatgpt" {
            return Err(action_error("AUTH_REQUIRED"));
        }
    }
    let tokens = auth.get("tokens").cloned().unwrap_or(Value::Null);
    let token = tokens.get("access_token").and_then(Value::as_str).unwrap_or("").to_string();
    if token.is_empty() {
        return Err(action_error("AUTH_REQUIRED"));
    }
    let claims = jwt_decode(&token);
    let id_claims = jwt_decode(tokens.get("id_token").and_then(Value::as_str).unwrap_or(""));
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if token.len() > 65536 || token.contains('\n') || token.contains('\r') || account_id.is_empty() || account_id.contains('\n') || account_id.contains('\r') {
        return Err(action_error("AUTH_REQUIRED"));
    }
    let expires = claims.get("exp").and_then(Value::as_f64);
    if let Some(exp) = expires {
        if exp.is_finite() && exp * 1000.0 <= now_ms() as f64 + 5000.0 {
            return Err(action_error("AUTH_EXPIRED"));
        }
    }
    let sub = id_claims
        .get("sub")
        .and_then(Value::as_str)
        .or(claims.get("sub").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    Ok(Credentials {
        account_key: sha256_hex(format!("codex:{}:{}", account_id, sub).as_bytes()),
        revision: sha256_hex(token.as_bytes()),
        token,
        account_id,
    })
}

async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<String, AppError> {
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(action_error("AUTH_EXPIRED"));
    }
    if status == 429 {
        return Err(action_error("CODEX_LIMIT"));
    }
    if status >= 400 {
        return Err(action_error("NETWORK_ERROR"));
    }
    if response.content_length().map(|length| length > limit as u64).unwrap_or(false) {
        return Err(action_error("NETWORK_ERROR"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| action_error("NETWORK_ERROR"))? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(action_error("NETWORK_ERROR"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).to_string())
}

/// codex-client.js pickModel.
fn pick_model(models: &[Value]) -> Option<Value> {
    let usable: Vec<&Value> = models
        .iter()
        .filter(|model| {
            let name = model
                .get("slug")
                .or_else(|| model.get("model"))
                .or_else(|| model.get("id"));
            name.map(|value| value.is_string()).unwrap_or(false) && model.get("hidden").and_then(Value::as_bool) != Some(true)
        })
        .collect();
    usable
        .iter()
        .find(|model| {
            let name = model_name(model);
            name.contains("mini") || name.contains("luna")
        })
        .or_else(|| {
            usable
                .iter()
                .find(|model| model.get("isDefault").and_then(Value::as_bool).unwrap_or(false) || model.get("is_default").and_then(Value::as_bool).unwrap_or(false))
        })
        .or_else(|| usable.first())
        .map(|model| (*model).clone())
}

fn model_name(model: &Value) -> &str {
    model
        .get("slug")
        .or_else(|| model.get("model"))
        .or_else(|| model.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

async fn binary_version() -> Option<String> {
    let binary = find_codex()?;
    let mut command = tokio::process::Command::new(&binary);
    command
        .arg("--version")
        .kill_on_drop(true)
        .stderr(std::process::Stdio::null());
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
        .await
        .ok()?
        .ok()
        .filter(|result| result.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    VERSION_RE.find(&text).map(|m| m.as_str().to_string())
}

#[derive(Debug, Clone)]
pub struct WarmupOutcome {
    pub account_key: String,
}

/// codex-client.js warmup(): one fixed minimal text request via the
/// Responses API; no tools, files or conversation history.
pub async fn warmup(expected_account_key: &str, proxy: Option<&str>) -> Result<WarmupOutcome, AppError> {
    let auth = read_credentials().await?;
    if auth.account_key != expected_account_key {
        return Err(action_error("SESSION_CHANGED"));
    }
    let base = read_base_url()?;
    let version = binary_version().await.ok_or_else(|| action_error("UNSUPPORTED_RUNTIME"))?;
    // Never send a loopback request, which can carry a token over HTTP, through a proxy.
    let proxy = if is_loopback(&base) { "" } else { proxy.unwrap_or_default() };
    let client = crate::util::http_client(proxy, std::time::Duration::from_secs(25))
        .map_err(|_| action_error("NETWORK_ERROR"))?;
    let base = base.as_str().trim_end_matches('/');
    let headers = |route: &str, extra: Option<&str>| {
        let mut map = reqwest::header::HeaderMap::new();
        let insert = |map: &mut reqwest::header::HeaderMap, name: &str, value: &str| {
            if let (Ok(name), Ok(value)) = (reqwest::header::HeaderName::try_from(name), reqwest::header::HeaderValue::from_str(value)) {
                map.insert(name, value);
            }
        };
        insert(&mut map, "Authorization", &format!("Bearer {}", auth.token));
        insert(&mut map, "ChatGPT-Account-Id", &auth.account_id);
        insert(&mut map, "OpenAI-Beta", "responses=experimental");
        insert(&mut map, "originator", "codex_cli_rs");
        insert(&mut map, "version", &version);
        insert(&mut map, "User-Agent", &format!("codex_cli_rs/{}", version));
        if let Some(extra) = extra {
            for line in extra.split('\n') {
                if let Some((name, value)) = line.split_once(':') {
                    insert(&mut map, name.trim(), value.trim());
                }
            }
        }
        let _ = route;
        map
    };
    let mut sent = false;
    let result: Result<WarmupOutcome, AppError> = async {
        let models_url = format!("{}/models?client_version={}", base, urlencoding(&version));
        let response = client.get(&models_url).headers(headers("models", None)).send().await.map_err(|_| action_error("NETWORK_ERROR"))?;
        let body = read_body(response, 1024 * 1024).await?;
        let list: Value = serde_json::from_str(&body).map_err(|_| action_error("NETWORK_ERROR"))?;
        let models: Vec<Value> = list
            .get("models")
            .and_then(Value::as_array)
            .or_else(|| list.get("data").and_then(Value::as_array))
            .cloned()
            .unwrap_or_default();
        let model = pick_model(&models).ok_or_else(|| action_error("CODEX_MODEL_UNAVAILABLE"))?;
        let current = read_credentials().await?;
        if current.account_key != auth.account_key || current.revision != auth.revision {
            return Err(action_error("SESSION_CHANGED"));
        }
        let levels: Vec<Value> = model
            .get("supported_reasoning_levels")
            .or_else(|| model.get("supportedReasoningEfforts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let efforts: Vec<String> = levels
            .iter()
            .map(|level| {
                level
                    .get("effort")
                    .or_else(|| level.get("reasoningEffort"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        let effort = if efforts.iter().any(|e| e == "low") {
            Some("low")
        } else if efforts.iter().any(|e| e == "minimal") {
            Some("minimal")
        } else {
            efforts.first().map(String::as_str)
        };
        let model_name = model
            .get("slug")
            .or_else(|| model.get("model"))
            .or_else(|| model.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut request = json!({
            "model": model_name,
            "instructions": "Reply with OK only.",
            "input": [{ "role": "user", "type": "message", "content": [{ "type": "input_text", "text": "Hi. Reply OK only." }] }],
            "stream": true,
            "store": false,
            "tools": [],
            "tool_choice": "none"
        });
        if let Some(effort) = effort {
            request["reasoning"] = json!({ "effort": effort });
        }
        sent = true;
        let response = client
            .post(format!("{}/responses", base))
            .headers(headers("responses", Some("Content-Type: application/json\nAccept: text/event-stream")))
            .body(serde_json::to_string(&request).unwrap_or_default())
            .send()
            .await
            .map_err(|_| action_error("NETWORK_ERROR"))?;
        let reply = read_body(response, 1024 * 1024).await?;
        let mut completed = false;
        let mut failed = false;
        for line in reply.split('\n') {
            let line = line.trim_end_matches('\r');
            if !line.starts_with("data:") {
                continue;
            }
            let payload = line[5..].trim();
            if let Ok(event) = serde_json::from_str::<Value>(payload) {
                let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                if event_type == "response.completed" {
                    let status = event.get("response").and_then(|response| response.get("status")).and_then(Value::as_str).unwrap_or("completed");
                    if status == "completed" || status.is_empty() {
                        completed = true;
                    }
                }
                if matches!(event_type, "error" | "response.failed" | "response.incomplete") {
                    failed = true;
                }
            }
        }
        if !completed || failed {
            return Err(action_error("NETWORK_ERROR"));
        }
        Ok(WarmupOutcome { account_key: auth.account_key.clone() })
    }
    .await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if sent && !matches!(error.code.as_str(), "AUTH_EXPIRED" | "CODEX_LIMIT" | "SESSION_CHANGED" | "ACTION_CANCELLED") {
                Err(action_error("CODEX_UNCERTAIN"))
            } else {
                Err(error)
            }
        }
    }
}

fn urlencoding(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => result.push(byte as char),
            other => result.push_str(&format!("%{:02X}", other)),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_endpoints_are_limited_to_the_official_service_and_loopback() {
        assert!(!is_loopback(&parse_base_url(DEFAULT_BASE).unwrap()));
        for host in ["localhost", "127.0.0.1", "[::1]"] {
            let url = parse_base_url(&format!("http://{host}:8080/backend-api/codex/")).unwrap();
            assert!(is_loopback(&url));
        }
        for endpoint in ["https://test.invalid/backend-api/codex", "http://chatgpt.com/backend-api/codex", "http://localhost/backend-api/codex?redirect=1"] {
            assert_eq!(parse_base_url(endpoint).unwrap_err().code, "CODEX_ENDPOINT_UNSUPPORTED");
        }
    }
}

use super::data::{fetch_credit_data, display_credit_data, normalize_summary, PAID_PATH, FREE_PATH, SUMMARY_PATH};
use crate::error::AppError;
use crate::util::now_ms;
use base64::Engine;
use hmac::{Hmac, Mac};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

const MAX_FRAME: usize = 1024 * 1024;
const UNAVAILABLE: &str = "请先在本机启动 WorkBuddy 并登录，再刷新积分。";
static SOCKET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^b-[a-f0-9]+\.sock$").unwrap());

type HmacSha256 = Hmac<Sha256>;

fn proof(ticket: &str, role_server: bool, endpoint: &str, client_nonce: &str, server_nonce: &str) -> String {
    let parts = [
        if role_server { "wbipc-s" } else { "wbipc-c" }.to_string(),
        "1".to_string(),
        endpoint.to_string(),
        client_nonce.to_string(),
        server_nonce.to_string(),
    ];
    let mut transcript: Vec<u8> = Vec::new();
    for part in parts {
        let body = part.as_bytes();
        transcript.extend_from_slice(&(body.len() as u32).to_be_bytes());
        transcript.extend_from_slice(body);
    }
    let mut mac = HmacSha256::new_from_slice(ticket.as_bytes()).expect("hmac key");
    mac.update(&transcript);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn config_dir() -> PathBuf {
    std::env::var("WORKBUDDY_CONFIG_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".workbuddy")))
        .unwrap_or_else(|| PathBuf::from(".workbuddy"))
}

fn cached_identity() -> String {
    let mut identity = String::new();
    if let Ok(body) = std::fs::read_to_string(config_dir().join("storage/skeleton/account-snapshot.json")) {
        if body.len() < 65536 {
            if let Ok(value) = serde_json::from_str::<Value>(&body) {
                identity = value
                    .get("primary")
                    .and_then(|primary| primary.get("nickname"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    identity
}

struct Discovery {
    endpoint: String,
    ticket: String,
}

fn lstat(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok()
}

fn owned_private(metadata: &std::fs::Metadata, uid: u32, mask: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    metadata.uid() == uid && metadata.permissions().mode() & mask == 0
}

fn read_discovery(uid: u32) -> Result<Discovery, AppError> {
    let file = config_dir().join("wbipc/endpoint.json");
    let unavailable = || AppError::message(UNAVAILABLE);
    let stat = lstat(&file).ok_or_else(unavailable)?;
    if !stat.is_file() || stat.is_symlink() || stat.len() > 8192 || !owned_private(&stat, uid, 0o077) {
        return Err(unavailable());
    }
    let body = std::fs::read_to_string(&file).map_err(|_| unavailable())?;
    let discovery: Value = serde_json::from_str(&body).map_err(|_| AppError::message("invalid discovery"))?;
    let ticket = discovery.get("ticket").and_then(Value::as_str).unwrap_or("").to_string();
    let endpoint = discovery.get("endpoint").and_then(Value::as_str).unwrap_or("").to_string();
    let ticket_valid = ticket.len() >= 32
        && ticket.len() <= 128
        && ticket.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ticket_valid || endpoint.is_empty() {
        return Err(AppError::message("invalid discovery"));
    }
    let path = PathBuf::from(&endpoint);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if !path.is_absolute()
        || parent.file_name().map(|name| name != "wbipc").unwrap_or(true)
        || !SOCKET_RE.is_match(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
    {
        return Err(AppError::message("invalid socket"));
    }
    let socket_stat = lstat(&path).ok_or_else(unavailable)?;
    let parent_stat = lstat(&parent).ok_or_else(unavailable)?;
    use std::os::unix::fs::FileTypeExt;
    if !socket_stat.file_type().is_socket() || !owned_private(&socket_stat, uid, 0o077) || !parent_stat.is_dir() || parent_stat.is_symlink() || !owned_private(&parent_stat, uid, 0o077) {
        return Err(AppError::message("untrusted socket"));
    }
    Ok(Discovery { endpoint, ticket })
}

struct Session {
    stream: UnixStream,
    buffer: Vec<u8>,
    done: bool,
}

impl Session {
    async fn send(&mut self, value: &Value) -> Result<(), AppError> {
        let body = format!("{}\n", serde_json::to_string(value).unwrap_or_default());
        self.stream
            .write_all(body.as_bytes())
            .await
            .map_err(|_| AppError::message(UNAVAILABLE))
    }

    async fn next_message(&mut self) -> Result<Value, AppError> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=index).collect();
                let text = String::from_utf8_lossy(&line).trim().to_string();
                if text.is_empty() {
                    continue;
                }
                return serde_json::from_str(&text).map_err(|_| AppError::message("WorkBuddy 积分读取失败，请确认已登录；客户端接口变化时需更新适配。"));
            }
            if self.done {
                return Err(AppError::message(UNAVAILABLE));
            }
            let mut chunk = vec![0u8; 65536];
            let size = self.stream.read(&mut chunk).await.map_err(|_| AppError::message(UNAVAILABLE))?;
            if size == 0 {
                self.done = true;
                continue;
            }
            self.buffer.extend_from_slice(&chunk[..size]);
            if self.buffer.len() > MAX_FRAME {
                return Err(AppError::message("oversized frame"));
            }
        }
    }
}

/// workbuddy-client.js read({includeDetails}).
pub async fn read(_app: &AppHandle, summary_only: bool) -> Result<Value, AppError> {
    let uid = unsafe { libc::getuid() };
    let discovery = read_discovery(uid)?;
    let connect = UnixStream::connect(&discovery.endpoint)
        .await
        .map_err(|_| AppError::message(UNAVAILABLE))?;
    let nonce: String = {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    };
    let mut session = Session { stream: connect, buffer: Vec::new(), done: false };
    let ticket_id: String = {
        use sha2::Digest;
        let mut hasher = Sha256::new();
        hasher.update(discovery.ticket.as_bytes());
        let digest = hasher.finalize();
        digest.iter().take(8).map(|byte| format!("{:02x}", byte)).collect()
    };
    session.send(&json!({
        "type": "session_hello", "protocol_min": 1, "protocol_max": 1,
        "client_nonce": nonce, "ticket_id": ticket_id,
        "client": { "kind": "desktop", "id": "meternest-quota" }
    }))
    .await?;
    let failure = || AppError::message("WorkBuddy 积分读取失败，请确认已登录；客户端接口变化时需更新适配。");
    let result = timeout(Duration::from_millis(10000), async {
        // challenge
        let challenge = session.next_message().await?;
        if challenge.get("type").and_then(Value::as_str) != Some("session_challenge")
            || challenge.get("protocol").and_then(Value::as_i64) != Some(1)
            || challenge.get("server_nonce").and_then(Value::as_str).map(|n| n.is_empty() || n.len() > 128).unwrap_or(true)
        {
            return Err(failure());
        }
        let server_nonce = challenge.get("server_nonce").and_then(Value::as_str).unwrap_or("").to_string();
        let expected = proof(&discovery.ticket, true, &discovery.endpoint, &nonce, &server_nonce);
        let actual = challenge.get("server_proof").and_then(Value::as_str).unwrap_or("");
        if actual.len() != expected.len() || actual != expected {
            return Err(failure());
        }
        session.send(&json!({ "type": "session_prove", "client_proof": proof(&discovery.ticket, false, &discovery.endpoint, &nonce, &server_nonce) })).await?;
        // ack
        let ack = session.next_message().await?;
        let pipes = ack.get("pipes").and_then(Value::as_array).cloned().unwrap_or_default();
        let has_pipe = pipes.iter().any(|pipe| pipe.as_str() == Some("wb.request"));
        if ack.get("type").and_then(Value::as_str) != Some("session_hello_ack") || ack.get("protocol").and_then(Value::as_i64) != Some(1) || !has_pipe {
            return Err(failure());
        }
        session.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "broker/GetPipe", "params": { "pipe": "wb.request" }, "mode": "call" })).await?;
        // pipe
        let pipe = session.next_message().await?;
        if pipe.get("id").and_then(Value::as_i64) != Some(1)
            || pipe.get("error").map(|e| !e.is_null()).unwrap_or(false)
            || pipe.get("result").and_then(|result| result.get("channel")).and_then(Value::as_str) != Some("c:wb.request")
        {
            return Err(failure());
        }
        let methods = pipe.get("result").and_then(|result| result.get("methods")).and_then(Value::as_array).cloned().unwrap_or_default();
        if !methods.iter().any(|method| method.as_str() == Some("http.fetch")) {
            return Err(failure());
        }
        // summary
        session.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "c:wb.request/http.fetch", "mode": "call", "params": {
            "method": "POST", "path": SUMMARY_PATH, "headers": { "content-type": "application/json" }, "body_b64": "e30=" } })).await?;
        let summary_message = session.next_message().await?;
        let summary_body = extract_body(&summary_message, 2)?;
        let summary: Value = serde_json::from_slice(&summary_body).map_err(|_| failure())?;
        if summary_only {
            return normalize_summary(&summary, &cached_identity());
        }
        // details through the same socket
        let next_id = Arc::new(std::sync::atomic::AtomicI64::new(3));
        let session = Arc::new(Mutex::new(session));
        let request_session = session.clone();
        let request = move |url: &str, body: Value| {
            let session = request_session.clone();
            let next_id = next_id.clone();
            let url = url.to_string();
            Box::pin(async move {
                let mut guard = session.lock().await;
                if url != PAID_PATH && url != FREE_PATH {
                    return Err(AppError::message("不支持的积分查询"));
                }
                let id = next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let payload = json!({
                    "jsonrpc": "2.0", "id": id, "method": "c:wb.request/http.fetch", "mode": "call", "params": {
                        "method": "POST", "path": url, "headers": { "content-type": "application/json" },
                        "body_b64": base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap_or_default()) }
                });
                guard.send(&payload).await?;
                let message = tokio::time::timeout(Duration::from_millis(6000), guard.next_message())
                    .await
                    .map_err(|_| AppError::message("积分明细查询超时"))??;
                let body = extract_body(&message, id)?;
                serde_json::from_slice(&body).map_err(|_| AppError::message("明细请求失败"))
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, AppError>> + Send>>
        };
        let details = fetch_credit_data(request, summary.clone()).await;
        match details {
            Ok(value) => Ok(json!({ "summary": summary, "details": value })),
            Err(_) => Ok(json!({ "summary": summary, "detailsError": "积分明细暂不可用，请刷新后重试。" })),
        }
    })
    .await;
    let payload = result.map_err(|_| AppError::message("WorkBuddy 积分查询超时，请稍后刷新"))??;
    // identity: optional cached nickname
    let identity = cached_identity();
    let summary = payload.get("summary").cloned().unwrap_or(Value::Null);
    let mut account = normalize_summary(&summary, &identity)?;
    if let Some(map) = account.as_object_mut() {
        let display = payload.get("details").cloned().unwrap_or(Value::Null);
        let display = if display.is_null() {
            let _ = now_ms();
            json!({ "detailsError": "积分明细暂不可用，请刷新后重试。" })
        } else {
            display
        };
        if let Some(display) = display_credit_data(&display).ok().and_then(|value| value.as_object().cloned()) {
            for (key, value) in display {
                map.insert(key, value);
            }
        }
    }
    Ok(account)
}

fn extract_body(message: &Value, id: i64) -> Result<Vec<u8>, AppError> {
    let failure = || AppError::message("request failed");
    if message.get("id").and_then(Value::as_i64) != Some(id) {
        return Err(AppError::message("unexpected response"));
    }
    if message.get("error").map(|e| !e.is_null()).unwrap_or(false) {
        return Err(failure());
    }
    let result = message.get("result").ok_or_else(failure)?;
    if result.get("status").and_then(Value::as_i64) != Some(200) {
        return Err(failure());
    }
    let body = result.get("body_b64").and_then(Value::as_str).ok_or_else(failure)?;
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| failure())
}

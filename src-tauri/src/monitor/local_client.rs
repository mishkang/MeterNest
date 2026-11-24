use super::codex_binary::find_codex;
use super::data::normalize_account;
use crate::automations::{action_error, codex_client};
use crate::error::AppError;
use crate::util::now_ms;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{timeout, Duration};

pub fn codex_failure_message(error: &AppError) -> String {
    match error.code.as_str() {
        "CODEX_NOT_FOUND" => "未找到 Codex 桌面客户端内置的只读接口或本机 CLI。请确认已安装客户端，安装后完全退出并重启 MeterNest。".to_string(),
        "CODEX_START_FAILED" => "已找到 Codex，但无法启动其只读接口。请检查客户端安装是否完整，或是否被系统安全策略阻止。".to_string(),
        "CODEX_TIMEOUT" => "Codex 额度查询超时。请检查代理地址，并确认 Codex 客户端当前可以正常连接服务。".to_string(),
        "CODEX_EXITED" => "Codex 只读接口提前退出。请检查客户端是否能正常运行，或更新客户端后重试。".to_string(),
        "CODEX_CONNECTION" => "Codex 只读接口连接中断，请刷新重试。".to_string(),
        "CODEX_UNSUPPORTED" => "当前 Codex 版本不支持所需的额度查询接口，请更新 Codex 客户端。".to_string(),
        "CODEX_AUTH_REQUIRED" => "Codex 登录状态不可用。请在 Codex 桌面客户端确认登录后，再刷新额度。".to_string(),
        "CODEX_REQUEST_FAILED" => {
            if error.status == Some(2) {
                "已读取到 Codex 登录账号，但额度请求失败。请检查代理及客户端登录是否有效。".to_string()
            } else {
                "Codex 账号读取失败。请在客户端确认登录和网络状态后重试。".to_string()
            }
        }
        _ => "本机 Codex 额度暂不可读。请确认已登录、客户端已安装且代理可用。".to_string(),
    }
}

fn local_error(code: &str, message: &str) -> AppError {
    AppError::new(code, message)
}

const TIMEOUT_MS: u64 = 12000;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

static AUTH_STATUS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b40[13]\b").unwrap());
static AUTH_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(access|refresh)[ _-]*token.*(?:expired|invalid)").unwrap());

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> std::io::Result<()> {
    let body = format!("{}\n", serde_json::to_string(message).unwrap_or_default());
    stdin.write_all(body.as_bytes()).await
}

/// local-client.js read(): initialize → account/read → account/rateLimits/read
/// over the discovered `codex app-server` stdio protocol. Read-only.
pub async fn read_codex_with_account(proxy: &str) -> Result<(Value, Option<String>), AppError> {
    let before = codex_client::read_credentials().await.ok().map(|auth| (auth.account_key, auth.revision));
    let started_at = now_ms();
    let mut quota = read_codex(proxy).await?;
    let completed_at = now_ms();
    let after = codex_client::read_credentials().await.ok().map(|auth| (auth.account_key, auth.revision));
    if before != after {
        return Err(action_error("SESSION_CHANGED"));
    }
    quota["quotaSample"] = json!({ "startedAt": started_at, "completedAt": completed_at });
    Ok((quota, after.map(|(key, _)| key)))
}

pub async fn read_codex_for_account(proxy: &str, expected: &str) -> Result<Value, AppError> {
    let (quota, key) = read_codex_with_account(proxy).await?;
    if key.as_deref() != Some(expected) {
        return Err(action_error("SESSION_CHANGED"));
    }
    Ok(quota)
}

async fn read_codex(proxy: &str) -> Result<Value, AppError> {
    let binary = find_codex().ok_or_else(|| local_error("CODEX_NOT_FOUND", "未找到本机 Codex 客户端或 CLI"))?;
    let home = dirs::home_dir().ok_or_else(|| local_error("CODEX_NOT_FOUND", "未找到本机 Codex 客户端或 CLI"))?;
    let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (key, value) in std::env::vars() {
        let lower = key.to_ascii_lowercase();
        if lower == "http_proxy" || lower == "https_proxy" || lower == "all_proxy" {
            continue;
        }
        if !proxy.is_empty() && lower == "no_proxy" {
            continue;
        }
        env.insert(key, value);
    }
    if !proxy.is_empty() {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.insert(key.to_string(), proxy.to_string());
        }
    }
    let mut child: Child = Command::new(&binary)
        .arg("app-server")
        .current_dir(&home)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| local_error("CODEX_START_FAILED", "无法启动本机 Codex 只读接口"))?;

    let mut stdin = child.stdin.take().ok_or_else(|| local_error("CODEX_START_FAILED", "无法启动本机 Codex 只读接口"))?;
    let stdout = child.stdout.take().ok_or_else(|| local_error("CODEX_START_FAILED", "无法启动本机 Codex 只读接口"))?;
    let mut reader = BufReader::new(stdout.take((MAX_OUTPUT + 1) as u64));
    let mut account: Value = Value::Null;
    let mut expected_id: i64 = 0;
    let init = json!({
        "id": 0,
        "method": "initialize",
        "params": { "clientInfo": { "name": "meternest_usage_monitor", "title": "MeterNest Usage Monitor", "version": "1.0.0" } }
    });
    let result = timeout(Duration::from_millis(TIMEOUT_MS), async {
        write_message(&mut stdin, &init).await.map_err(|_| local_error("CODEX_CONNECTION", "本机 Codex 接口连接中断"))?;
        let mut buffer = String::new();
        let mut total = 0usize;
        loop {
            let mut line = String::new();
            let size = reader.read_line(&mut line).await.map_err(|_| local_error("CODEX_CONNECTION", "本机 Codex 接口连接中断"))?;
            if size == 0 {
                return Err(local_error("CODEX_EXITED", "本机 Codex 接口提前结束"));
            }
            total += size;
            if total > MAX_OUTPUT {
                return Err(AppError::message("Codex 返回的数据过大"));
            }
            buffer.push_str(&line);
            while let Some(newline) = buffer.find('\n') {
                let message_line: String = buffer.drain(..=newline).collect();
                let message_line = message_line.trim().to_string();
                if message_line.is_empty() {
                    continue;
                }
                let message: Value = match serde_json::from_str(&message_line) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if message.get("id").and_then(Value::as_i64) != Some(expected_id) {
                    continue;
                }
                if let Some(error) = message.get("error").filter(|e| !e.is_null()) {
                    let detail = error.get("message").and_then(Value::as_str).unwrap_or("");
                    let code = error.get("code").and_then(Value::as_i64);
                    let error_code = if code == Some(-32601) || code == Some(-32602) {
                        "CODEX_UNSUPPORTED"
                    } else if is_auth_failure(detail) {
                        "CODEX_AUTH_REQUIRED"
                    } else {
                        "CODEX_REQUEST_FAILED"
                    };
                    let mut failure = local_error(error_code, "本机 Codex 额度暂不可读，请检查登录状态或稍后重试");
                    failure.status = Some(expected_id); // request stage marker
                    if !account.is_null() {
                        let identity = normalize_account(&account, &Value::Null).get("identity").and_then(Value::as_str).map(str::to_string);
                        failure.account_identity = identity;
                    }
                    return Err(failure);
                }
                if expected_id == 0 {
                    expected_id = 1;
                    let _ = write_message(&mut stdin, &json!({ "method": "initialized", "params": {} })).await;
                    let _ = write_message(&mut stdin, &json!({ "id": 1, "method": "account/read", "params": { "refreshToken": false } })).await;
                } else if expected_id == 1 {
                    account = message.get("result").and_then(|result| result.get("account")).cloned().unwrap_or(Value::Null);
                    let account_type = account.get("type").and_then(Value::as_str).unwrap_or("");
                    if account.is_null() || (account_type != "chatgpt" && account_type != "chatgptAuthTokens") {
                        return Ok(normalize_account(&account, &Value::Null));
                    }
                    expected_id = 2;
                    let _ = write_message(&mut stdin, &json!({ "id": 2, "method": "account/rateLimits/read", "params": {} })).await;
                } else {
                    let limits = message.get("result").cloned().unwrap_or(Value::Null);
                    return Ok(normalize_account(&account, &limits));
                }
            }
        }
    })
    .await;
    let _ = child.kill().await;
    let value = result.map_err(|_| local_error("CODEX_TIMEOUT", "本机 Codex 额度读取超时，请检查代理或登录状态"))??;
    Ok(value)
}

fn is_auth_failure(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("not authenticated") || lower.contains("unauthorized") {
        return true;
    }
    if AUTH_STATUS_RE.is_match(detail) {
        return true;
    }
    AUTH_TOKEN_RE.is_match(detail)
}

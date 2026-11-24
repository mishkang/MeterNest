use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
use std::time::Duration;

pub fn http_client(proxy: &str, timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .no_proxy();
    if !proxy.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    builder.build()
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// JS `new Date(time + 8*3600000).toISOString().slice(0, 10)`
pub fn beijing_day(time: i64) -> String {
    let offset = FixedOffset::east_opt(8 * 3600).unwrap();
    match offset.timestamp_millis_opt(time).single() {
        Some(t) => t.format("%Y-%m-%d").to_string(),
        None => String::new(),
    }
}

/// `new Date(time + 8 * 3600000).toISOString().slice(0, 19).replace('T', ' ')`
pub fn beijing_time(time: i64) -> String {
    let offset = FixedOffset::east_opt(8 * 3600).unwrap();
    match offset.timestamp_millis_opt(time).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => String::new(),
    }
}

/// Local wall-clock slot for "HH:mm" on the given timestamp (JS setHours).
pub fn local_slot(time: i64, clock: &str) -> Option<i64> {
    let date = Local.timestamp_millis_opt(time).single()?.date_naive();
    local_slot_on(date, clock)
}

pub fn next_local_slot(time: i64, clock: &str) -> Option<i64> {
    let today = Local.timestamp_millis_opt(time).single()?.date_naive();
    (0..=2).find_map(|days| {
        let date = today.checked_add_days(chrono::Days::new(days))?;
        local_slot_on(date, clock).filter(|slot| *slot >= time)
    })
}

fn local_slot_on(date: chrono::NaiveDate, clock: &str) -> Option<i64> {
    let (hour, minute) = clock.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    let naive = date.and_hms_opt(hour, minute, 0)?;
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(value) => Some(value.timestamp_millis()),
        // Local time can occur twice when clocks fall back. Choose the first instant.
        chrono::LocalResult::Ambiguous(a, b) => Some(a.timestamp_millis().min(b.timestamp_millis())),
        chrono::LocalResult::None => None,
    }
}

/// JS `date.getFullYear()...` local key.
pub fn slot_key(time: i64) -> String {
    let local: DateTime<Local> = Local.timestamp_millis_opt(time).single().unwrap_or_else(|| Local::now());
    local.format("%Y-%m-%d %H:%M").to_string()
}

/// Number.toLocaleString('zh-CN', {maximumFractionDigits: 2}) —
/// half-away-from-zero rounding, trailing zeros trimmed, comma grouping.
pub fn locale_number(value: f64, max_fraction: usize, min_fraction: usize) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    let mut formatted = format!("{:.*}", max_fraction, magnitude);
    if max_fraction > 0 {
        let point = formatted.find('.').unwrap_or(formatted.len());
        let decimals = formatted.len() - point - 1;
        if decimals < min_fraction {
            formatted.push_str(&"0".repeat(min_fraction - decimals));
        } else {
            while formatted.len() - formatted.find('.').unwrap() - 1 > min_fraction && formatted.ends_with('0') {
                formatted.pop();
            }
            if formatted.ends_with('.') {
                formatted.pop();
            }
        }
    }
    let (whole, fraction) = match formatted.split_once('.') {
        Some((w, f)) => (w.to_string(), Some(f.to_string())),
        None => (formatted.clone(), None),
    };
    let mut grouped = String::new();
    let digits: Vec<char> = whole.chars().collect();
    for (index, digit) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*digit);
    }
    let nonzero = formatted.parse::<f64>().map(|value| value != 0.0).unwrap_or(false);
    let mut result = String::new();
    if negative && nonzero {
        result.push('-');
    }
    result.push_str(&grouped);
    if let Some(fraction) = fraction {
        if !fraction.is_empty() {
            result.push('.');
            result.push_str(&fraction);
        }
    }
    result
}

/// JS `Date.parse` acceptance for ISO-ish strings → milliseconds, when valid & positive.
pub fn parse_time(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.with_timezone(&Utc).timestamp_millis())
        .or_else(|| DateTime::parse_from_rfc3339(&format!("{}Z", value)).ok().map(|d| d.with_timezone(&Utc).timestamp_millis()))
        .filter(|ms| *ms > 0)
}

/// ISO string with milliseconds, like JS `new Date(...).toISOString()`.
pub fn to_iso(time: i64) -> String {
    match Utc.timestamp_millis_opt(time).single() {
        Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => String::new(),
    }
}

pub fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

/// Base64url without padding for JWT parts.
pub fn jwt_decode(token: &str) -> serde_json::Value {
    let payload = token.split('.').nth(1).unwrap_or("");
    use base64::Engine;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    serde_json::from_slice(&engine.decode(payload.as_bytes()).unwrap_or_default()).unwrap_or(serde_json::Value::Null)
}


/// Packaged (.app) detection: bundled builds live under Contents/MacOS.
pub fn is_packaged() -> bool {
    std::env::current_exe()
        .map(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn explicit_proxy_and_direct_mode_use_different_http_targets() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for target in ["http://meternest.invalid/preview", "/preview"] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut request = String::new();
                socket.read_line(&mut request).await.unwrap();
                assert_eq!(request.trim_end(), format!("GET {target} HTTP/1.1"));
                let mut line = String::new();
                loop {
                    line.clear();
                    assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
                    if line == "\r\n" { break; }
                }
                socket.get_mut().write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
            }
        });
        let timeout = Duration::from_secs(3);
        let proxied = http_client(&format!("http://{address}"), timeout).unwrap();
        assert_eq!(proxied.get("http://meternest.invalid/preview").send().await.unwrap().status(), 204);
        let direct = http_client("", timeout).unwrap();
        assert_eq!(direct.get(format!("http://{address}/preview")).send().await.unwrap().status(), 204);
        server.await.unwrap();
    }
}

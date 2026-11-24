use super::data::parse_feed;
use super::data::Source;
use crate::error::AppError;
use serde_json::Value;

/// index.js fetchSource/readBody: GET with no-store, redirect rejected,
/// one retry on connect error or 5xx, 10s abort, 2MB body cap.
pub async fn fetch_source(source: &Source, proxy: &str) -> Result<Vec<Value>, AppError> {
    let client = crate::util::http_client(proxy, std::time::Duration::from_secs(10))
        .map_err(|_| AppError::new("FEED_NETWORK", "数据源连接失败"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(10000);
    let mut response = None;
    for attempt in 0..2 {
        let request = client
            .get(source.url)
            .header("Accept", "text/html")
            .header("Cache-Control", "no-cache")
            .timeout(std::time::Duration::from_millis(deadline.saturating_duration_since(tokio::time::Instant::now()).as_millis().max(1000) as u64));
        let result = request.send().await;
        match result {
            Ok(ok) => {
                let status = ok.status().as_u16();
                if attempt == 0 && status >= 500 && tokio::time::Instant::now() < deadline {
                    continue;
                }
                response = Some(ok);
                break;
            }
            Err(_) => {
                if attempt == 0 && tokio::time::Instant::now() < deadline {
                    continue;
                }
                return Err(AppError::new("FEED_NETWORK", "数据源连接失败"));
            }
        }
    }
    let response = response.ok_or_else(|| AppError::new("FEED_TIMEOUT", "数据源连接失败"))?;
    let status = response.status().as_u16();
    if status >= 400 {
        return Err(AppError::with_status("FEED_HTTP", "数据源暂不可用", status as i64));
    }
    let limit = 2 * 1024 * 1024;
    if response.content_length().map(|length| length > limit as u64).unwrap_or(false) {
        return Err(AppError::message("数据源响应过大"));
    }
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|_| AppError::new("FEED_NETWORK", "数据源连接失败"))? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(AppError::message("数据源响应过大"));
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).to_string();
    let records = parse_feed(&text, source).map_err(|_| AppError::new("FEED_FORMAT", "数据格式已变化"))?;
    if records.is_empty() {
        return Err(AppError::new("FEED_EMPTY", "来源暂未提供记录"));
    }
    Ok(records)
}


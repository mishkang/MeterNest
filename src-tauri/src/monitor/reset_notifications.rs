use serde_json::{json, Value};
use std::collections::VecDeque;

const PENDING_STAGES: [&str; 2] = ["scheduled", "rolling_out"];
const MAX_IDS: usize = 100;

/// reset-notifications.js: observe public feed transitions only.
pub struct ResetNotifications {
    sent: VecDeque<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_all_completions_at_once_and_remembers_them_after_reload() {
        let record = |id, stage| json!({ "id": id, "type": "usage_reset", "stage": stage });
        let previous = [record("a", "scheduled"), record("b", "rolling_out")];
        let latest = [record("a", "completed"), record("b", "completed"), record("c", "completed")];
        let mut notifications = ResetNotifications::new(Value::Null);
        let delivered = notifications.observe(&previous, &latest);
        assert_eq!(delivered.iter().map(|entry| entry["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "b"]);
        let mut restarted = ResetNotifications::new(notifications.history());
        assert!(restarted.observe(&previous, &latest).is_empty());
    }
}

impl ResetNotifications {
    pub fn new(history: Value) -> Self {
        let mut sent = VecDeque::new();
        if history.get("version").and_then(Value::as_i64) == Some(1) {
            if let Some(ids) = history.get("ids").and_then(Value::as_array) {
                if ids.len() <= MAX_IDS
                    && ids.iter().all(|id| {
                        id.as_str().map(|id| !id.is_empty() && id.len() <= 200).unwrap_or(false)
                    })
                {
                    for id in ids.iter().filter_map(Value::as_str) {
                        if !sent.iter().any(|entry| entry == id) {
                            sent.push_back(id.to_string());
                        }
                    }
                }
            }
        }
        ResetNotifications { sent }
    }

    /// Returns notifications for all tracked records that have just completed.
    pub fn observe(&mut self, previous: &[Value], records: &[Value]) -> Vec<Value> {
        let mut notifications = Vec::new();
        for latest in records {
            if latest.get("type").and_then(Value::as_str) != Some("usage_reset")
                || latest.get("stage").and_then(Value::as_str) != Some("completed")
            {
                continue;
            }
            let Some(id) = latest.get("id").and_then(Value::as_str) else {
                continue;
            };
            if self.sent.iter().any(|entry| entry == id) {
                continue;
            }
            let Some(before) = previous.iter().find(|item| item.get("id").and_then(Value::as_str) == Some(id)) else {
                continue;
            };
            let stage_before = before.get("stage").and_then(Value::as_str).unwrap_or("");
            if !PENDING_STAGES.contains(&stage_before) {
                continue;
            }
            self.sent.push_back(id.to_string());
            if self.sent.len() > MAX_IDS {
                self.sent.pop_front();
            }
            notifications.push(json!({
                "title": "Codex 额度重置已完成",
                "body": format!("{}\nCodex Reset FM 已将此动态标记为已完成。点击查看额度速览。", latest.get("title").and_then(Value::as_str).unwrap_or("")),
                "id": latest.get("id").cloned().unwrap_or(Value::Null)
            }));
        }
        notifications
    }

    pub fn history(&self) -> Value {
        json!({ "version": 1, "ids": self.sent })
    }

}

use crate::error::AppError;
use serde_json::{json, Value};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// electron-store compatible JSON store at
/// ~/Library/Application Support/MeterNest/meternest.json
pub struct Store {
    path: PathBuf,
    data: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_write_does_not_change_memory_or_next_save() {
        let dir = std::env::temp_dir().join(format!("meternest-store-test-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("meternest.json");
        let mut store = Store { path: path.clone(), data: json!({ "preferences": "original" }) };
        // Force rename to fail even when tests run as root.
        std::fs::create_dir(&path).unwrap();
        assert!(store.set("preferences", json!("uncommitted")).is_err());
        assert_eq!(store.get("preferences"), json!("original"));
        std::fs::remove_dir(&path).unwrap();

        store.set("cache", json!(42)).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, json!({ "preferences": "original", "cache": 42 }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_json_is_preserved_and_io_errors_are_not_treated_as_missing_files() {
        let dir = std::env::temp_dir().join(format!("meternest-store-test-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        assert!(Store::open_at(dir.clone()).is_err());
        let path = dir.join("meternest.json");
        std::fs::write(&path, b"[42]").unwrap();
        let mut store = Store::open_at(path.clone()).unwrap();
        let backup = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        assert_ne!(backup, path);
        assert_eq!(std::fs::read(&backup).unwrap(), b"[42]");
        store.set("cache", json!(42)).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"[42]");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

impl Store {
    pub fn open() -> Result<Store, AppError> {
        let home = dirs::home_dir().ok_or_else(|| AppError::message("无法定位用户目录"))?;
        Self::open_at(home.join("Library/Application Support/MeterNest/meternest.json"))
    }

    fn open_at(path: PathBuf) -> Result<Store, AppError> {
        let data = match std::fs::read(&path) {
            Ok(body) => match serde_json::from_slice::<Value>(&body) {
                Ok(value) if value.is_object() => value,
                _ => {
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_millis())
                        .unwrap_or(0);
                    std::fs::rename(&path, path.with_extension(format!("json.corrupt-{}", stamp)))?;
                    json!({})
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => json!({}),
            Err(error) => return Err(error.into()),
        };
        Ok(Store { path, data })
    }

    pub fn get(&self, key: &str) -> Value {
        self.data.get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn set(&mut self, key: &str, value: Value) -> Result<(), AppError> {
        let mut next = if self.data.is_object() { self.data.clone() } else { json!({}) };
        next[key] = value;
        self.flush(&next)?;
        self.data = next;
        Ok(())
    }

    fn flush(&self, data: &Value) -> Result<(), AppError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(data).map_err(|error| AppError::message(error.to_string()))?;
        let temporary = self.path.with_extension(format!("json.tmp-{:016x}", rand::random::<u64>()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        let result = file.write_all(&body).and_then(|()| std::fs::rename(&temporary, &self.path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(Into::into)
    }
}

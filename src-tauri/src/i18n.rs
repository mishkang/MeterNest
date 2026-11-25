use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::LazyLock};
use tauri::{Emitter, Manager};

type Catalog = BTreeMap<String, [String; 5]>;
static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("i18n.json")).expect("valid translation catalog")
});

const LANGUAGES: [&str; 5] = ["en", "fr", "ja", "ko", "zh-Hant"];

pub fn valid_preference(value: &str) -> bool {
    matches!(value, "system" | "zh-Hans") || LANGUAGES.contains(&value)
}

fn supported(tag: &str) -> Option<&'static str> {
    let tag = tag.to_ascii_lowercase().replace('_', "-");
    let mut parts = tag.split('-');
    match parts.next()? {
        "zh" => {
            let subtags: Vec<_> = parts.collect();
            if subtags.contains(&"hans") {
                Some("zh-Hans")
            } else if subtags.iter().any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo")) {
                Some("zh-Hant")
            } else {
                Some("zh-Hans")
            }
        }
        "en" => Some("en"),
        "fr" => Some("fr"),
        "ja" => Some("ja"),
        "ko" => Some("ko"),
        _ => None,
    }
}

pub fn resolve(preference: &Value) -> &'static str {
    let choice = preference["language"].as_str().unwrap_or("system");
    if choice != "system" {
        return supported(choice).unwrap_or("zh-Hans");
    }
    objc2_foundation::NSLocale::preferredLanguages()
        .iter()
        .find_map(|tag| supported(&tag.to_string()))
        .unwrap_or("zh-Hans")
}

fn index(language: &str) -> Option<usize> {
    LANGUAGES.iter().position(|item| *item == language)
}

pub fn text<'a>(language: &str, key: &'a str) -> &'a str {
    index(language)
        .and_then(|column| CATALOG.get(key).map(|row| row[column].as_str()))
        .unwrap_or(key)
}

pub fn format(language: &str, key: &str, values: &[&str]) -> String {
    let mut translated = text(language, key).to_owned();
    for (index, value) in values.iter().enumerate() {
        translated = translated.replace(&format!("{{{index}}}"), value);
    }
    translated
}

pub fn payload(language: &str) -> Value {
    let strings: BTreeMap<&str, &str> = index(language)
        .map(|column| {
            CATALOG.iter()
                .map(|(key, row)| (key.as_str(), row[column].as_str()))
                .collect()
        })
        .unwrap_or_default();
    json!({ "language": language, "strings": strings })
}

pub fn initialization_script(core: &crate::core::Core) -> String {
    format!("window.meterNestLocale = {};", payload(resolve(&core.prefs())))
}

pub fn apply(core: &crate::core::Core) {
    let language = resolve(&core.prefs());
    let _ = crate::set_tray_menu(&core.app, language);
    if let Some(window) = core.app.get_webview_window("settings") {
        let _ = window.set_title(&format!("MeterNest · {}", text(language, "设置")));
    }
    if let Some(window) = core.app.get_webview_window("panel") {
        let _ = window.set_title(text(language, "额度速览"));
    }
    for label in ["panel", "hover"] {
        if let Some(window) = core.app.get_webview_window(label) {
            let _ = window.emit("meternest-locale", payload(language));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_keep_explicit_language_and_migrate_old_settings() {
        let old = json!({"openAtLogin": true});
        assert_eq!(crate::prefs::read_preferences(&old)["language"], "system");

        for language in ["system", "zh-Hans", "zh-Hant", "en", "fr", "ja", "ko"] {
            let mut prefs = crate::prefs::defaults();
            prefs["language"] = json!(language);
            assert_eq!(crate::prefs::validate_settings(&prefs).unwrap()["language"], language);
            assert_eq!(crate::prefs::read_preferences(&prefs)["language"], language);
        }
    }

    #[test]
    fn resolves_supported_languages_and_chinese_scripts() {
        for (tag, expected) in [
            ("en-GB", "en"),
            ("fr-CA", "fr"),
            ("ja-JP", "ja"),
            ("ko-KR", "ko"),
            ("zh_CN", "zh-Hans"),
            ("zh-Hans-HK", "zh-Hans"),
            ("zh-Hant-HK", "zh-Hant"),
            ("zh-TW", "zh-Hant"),
        ] {
            assert_eq!(supported(tag), Some(expected));
        }
        assert_eq!(supported("de-DE"), None);
        assert!(!valid_preference("en-GB"));
    }
}

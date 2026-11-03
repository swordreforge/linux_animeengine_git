//! Deterministic localization — ported from `anima_i18n_static.py`.
//!
//! No network translator: every Steam platform language is represented by a
//! stable locale code and lookups come from the bundled
//! `anima_translations.json` catalog (`{ "<lang>": { "<english>": "<text>" } }`).
//! Missing keys fall back to the English source.
//!
//! The Python build used `QLocale.system()`; on Linux we read the C locale
//! (`LANG` / `LC_ALL` / `LC_MESSAGES`) instead.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::paths::{atomic_write_json, user_data_dir};

/// The catalog maps language code -> (English source -> translated text).
pub type Catalog = HashMap<String, HashMap<String, String>>;

/// Steam platform languages: `(code, native name)`.
pub const STEAM_LANGUAGES: &[(&str, &str)] = &[
    ("ar", "العربية"),
    ("bg", "Български"),
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("cs", "Čeština"),
    ("da", "Dansk"),
    ("nl", "Nederlands"),
    ("en", "English"),
    ("fi", "Suomi"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("hu", "Magyar"),
    ("id", "Bahasa Indonesia"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("ms", "Bahasa Melayu"),
    ("no", "Norsk"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("pt-BR", "Português (Brasil)"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("es", "Español (España)"),
    ("es-419", "Español (Latinoamérica)"),
    ("sv", "Svenska"),
    ("th", "ไทย"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("vi", "Tiếng Việt"),
];

/// Catalog version reported by [`I18n::status`].
pub const CATALOG_VERSION: &str = "2026-09-13-v4.7-final";

const CATALOG_FILE: &str = "anima_translations.json";
const PREFS_FILE: &str = "language.json";

/// Latin-American region codes mapped to `es-419`.
const LATAM_REGIONS: &[&str] = &[
    "AR", "BO", "BR", "BZ", "CL", "CO", "CR", "CU", "DO", "EC", "SV", "GT", "HN", "MX", "NI", "PA",
    "PY", "PE", "PR", "UY", "VE", "419",
];

fn steam_api_to_web(name: &str) -> Option<&'static str> {
    Some(match name {
        "arabic" => "ar",
        "bulgarian" => "bg",
        "schinese" => "zh-CN",
        "tchinese" => "zh-TW",
        "czech" => "cs",
        "danish" => "da",
        "dutch" => "nl",
        "english" => "en",
        "finnish" => "fi",
        "french" => "fr",
        "german" => "de",
        "greek" => "el",
        "hungarian" => "hu",
        "indonesian" => "id",
        "italian" => "it",
        "japanese" => "ja",
        "koreana" => "ko",
        "malay" => "ms",
        "norwegian" => "no",
        "polish" => "pl",
        "portuguese" => "pt",
        "brazilian" => "pt-BR",
        "romanian" => "ro",
        "russian" => "ru",
        "spanish" => "es",
        "latam" => "es-419",
        "swedish" => "sv",
        "thai" => "th",
        "turkish" => "tr",
        "ukrainian" => "uk",
        "vietnamese" => "vi",
        _ => return None,
    })
}

/// Whether `code` is one of the supported catalog locales.
pub fn is_supported(code: &str) -> bool {
    STEAM_LANGUAGES.iter().any(|(c, _)| *c == code)
}

/// Normalize an arbitrary locale string or Steam API language name to a
/// supported code, falling back to `"en"`.
pub fn normalize_language(value: &str) -> String {
    let raw = value.trim().replace('_', "-");
    if raw.is_empty() {
        return "en".to_string();
    }
    let lower = raw.to_ascii_lowercase();

    if let Some(code) = steam_api_to_web(&lower) {
        return code.to_string();
    }
    if let Some((code, _)) = STEAM_LANGUAGES
        .iter()
        .find(|(code, _)| code.to_ascii_lowercase() == lower)
    {
        return (*code).to_string();
    }
    if lower.starts_with("zh") {
        return if ["tw", "hk", "mo", "hant"].iter().any(|x| lower.contains(x)) {
            "zh-TW"
        } else {
            "zh-CN"
        }
        .to_string();
    }
    if lower.starts_with("pt") {
        return if lower.contains("br") { "pt-BR" } else { "pt" }.to_string();
    }
    if lower.starts_with("es") {
        let region = raw.to_ascii_uppercase();
        let region = region.split('-').nth(1).unwrap_or("");
        return if LATAM_REGIONS.contains(&region) {
            "es-419"
        } else {
            "es"
        }
        .to_string();
    }

    let base = lower.split('-').next().unwrap_or(lower.as_str());
    let base = match base {
        "nb" | "nn" => "no",
        "iw" => "he",
        other => other,
    };
    if is_supported(base) {
        base.to_string()
    } else {
        "en".to_string()
    }
}

/// Default location of the persisted language choice.
pub fn default_prefs_path() -> PathBuf {
    user_data_dir().join(PREFS_FILE)
}

/// Environment variable that overrides the language for QA launches.
const LANGUAGE_OVERRIDE_ENV: &str = "ANIMAENGINE_LANGUAGE";

/// Detect the active language: explicit override, saved preference, Steam, then
/// the system locale, then English.
pub fn detect_language(prefs_path: &Path) -> String {
    if let Ok(value) = std::env::var(LANGUAGE_OVERRIDE_ENV)
        && !value.trim().is_empty()
    {
        return normalize_language(&value);
    }

    if let Some(code) = std::fs::read_to_string(prefs_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json.get("language")?.as_str().map(normalize_language))
    {
        return code;
    }

    if let Ok(value) = std::env::var("SteamAppLanguage")
        && !value.trim().is_empty()
    {
        return normalize_language(&value);
    }

    if let Some(locale) = system_locale() {
        return normalize_language(&locale);
    }
    "en".to_string()
}

/// First non-empty, non-`C` locale from `LC_ALL` / `LC_MESSAGES` / `LANG`,
/// stripped of codeset and modifier (`zh_CN.UTF-8@euro` -> `zh_CN`).
fn system_locale() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if value.is_empty() || value == "C" || value == "POSIX" {
                continue;
            }
            let base = value.split(['.', '@']).next().unwrap_or(value);
            if !base.is_empty() {
                return Some(base.to_string());
            }
        }
    }
    None
}

/// Candidate catalog locations, in priority order (deduplicated).
pub fn catalog_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("ANIMAENGINE_TRANSLATIONS")
        && !path.trim().is_empty()
    {
        candidates.push(PathBuf::from(path));
    }

    let exe = crate::paths::exe_dir();
    candidates.push(exe.join(CATALOG_FILE));
    candidates.push(exe.join("_internal").join(CATALOG_FILE));

    if let Ok(dir) = std::env::var("ANIMAENGINE_RUNTIME_DIR")
        && !dir.trim().is_empty()
    {
        let dir = PathBuf::from(dir);
        candidates.push(dir.join(CATALOG_FILE));
        candidates.push(dir.join("_internal").join(CATALOG_FILE));
    }

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// Load a catalog file; an unreadable or malformed file yields an empty catalog.
pub fn load_catalog(path: &Path) -> Catalog {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Diagnostic snapshot for Settings/QA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationStatus {
    pub language: String,
    pub catalog_version: String,
    pub entries: usize,
    pub catalog_path: Option<String>,
}

/// Loaded catalog + active language.
pub struct I18n {
    catalog: Catalog,
    language: String,
    catalog_path: Option<PathBuf>,
    prefs_path: PathBuf,
}

impl I18n {
    /// Discover the catalog and detect the language (the normal entry point).
    pub fn discover() -> Self {
        let mut catalog = Catalog::new();
        let mut catalog_path = None;
        for candidate in catalog_candidates() {
            if candidate.is_file() {
                let loaded = load_catalog(&candidate);
                if !loaded.is_empty() {
                    catalog = loaded;
                    catalog_path = Some(candidate);
                    break;
                }
            }
        }

        let prefs_path = default_prefs_path();
        let language = detect_language(&prefs_path);
        Self {
            catalog,
            language,
            catalog_path,
            prefs_path,
        }
    }

    /// Build from an in-memory catalog and explicit language (no persistence).
    pub fn from_parts(catalog: Catalog, language: impl AsRef<str>) -> Self {
        Self {
            catalog,
            language: normalize_language(language.as_ref()),
            catalog_path: None,
            prefs_path: default_prefs_path(),
        }
    }

    /// Build with an explicit preferences path (used by `set_language`).
    pub fn with_prefs(catalog: Catalog, language: impl AsRef<str>, prefs_path: PathBuf) -> Self {
        Self {
            catalog,
            language: normalize_language(language.as_ref()),
            catalog_path: None,
            prefs_path,
        }
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn catalog_path(&self) -> Option<&Path> {
        self.catalog_path.as_deref()
    }

    /// Number of entries for the active language.
    pub fn entries(&self) -> usize {
        self.catalog.get(&self.language).map_or(0, HashMap::len)
    }

    /// Translate `source`, falling back to the source string.
    pub fn tr(&self, source: &str) -> String {
        if source.is_empty() || self.language == "en" {
            return source.to_string();
        }
        self.catalog
            .get(&self.language)
            .and_then(|table| table.get(source))
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| source.to_string())
    }

    /// Persist an explicit language choice and apply it. UI text should be
    /// re-read after this (the Python build applied it on next start).
    pub fn set_language(&mut self, code: &str) -> Result<()> {
        let code = normalize_language(code);
        atomic_write_json(&self.prefs_path, &serde_json::json!({ "language": code }))?;
        self.language = code;
        Ok(())
    }

    pub fn status(&self) -> TranslationStatus {
        TranslationStatus {
            language: self.language.clone(),
            catalog_version: CATALOG_VERSION.to_string(),
            entries: self.entries(),
            catalog_path: self
                .catalog_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_locales_and_steam_names() {
        assert_eq!(normalize_language(""), "en");
        assert_eq!(normalize_language("en"), "en");
        assert_eq!(normalize_language("en-US"), "en");
        assert_eq!(normalize_language("zh_CN"), "zh-CN");
        assert_eq!(normalize_language("zh_TW"), "zh-TW");
        assert_eq!(normalize_language("zh-Hant"), "zh-TW");
        assert_eq!(normalize_language("pt_BR"), "pt-BR");
        assert_eq!(normalize_language("pt_PT"), "pt");
        assert_eq!(normalize_language("es_MX"), "es-419");
        assert_eq!(normalize_language("es_ES"), "es");
        assert_eq!(normalize_language("schinese"), "zh-CN");
        assert_eq!(normalize_language("koreana"), "ko");
        assert_eq!(normalize_language("brazilian"), "pt-BR");
        assert_eq!(normalize_language("latam"), "es-419");
        assert_eq!(normalize_language("nb"), "no");
        assert_eq!(normalize_language("klingon"), "en");
    }

    #[test]
    fn tr_falls_back_to_source() {
        let mut de = HashMap::new();
        de.insert("Hello".to_string(), "Hallo".to_string());
        let mut catalog = Catalog::new();
        catalog.insert("de".to_string(), de);

        let i18n = I18n::from_parts(catalog, "de");
        assert_eq!(i18n.tr("Hello"), "Hallo");
        assert_eq!(i18n.tr("Missing"), "Missing");
        assert_eq!(i18n.entries(), 1);
    }

    #[test]
    fn tr_english_is_identity() {
        let mut catalog = Catalog::new();
        catalog.insert(
            "en".to_string(),
            HashMap::from([("Hello".to_string(), "IGNORED".to_string())]),
        );
        let i18n = I18n::from_parts(catalog, "en");
        assert_eq!(i18n.tr("Hello"), "Hello");
    }

    #[test]
    fn loads_catalog_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anima_translations.json");
        std::fs::write(
            &path,
            r#"{"fr":{"Hello":"Bonjour"},"de":{"Hello":"Hallo"}}"#,
        )
        .unwrap();

        let catalog = load_catalog(&path);
        let i18n = I18n::from_parts(catalog, "fr");
        assert_eq!(i18n.tr("Hello"), "Bonjour");
    }

    #[test]
    fn malformed_catalog_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(load_catalog(&path).is_empty());
    }

    #[test]
    fn set_language_persists_choice() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("language.json");
        let mut i18n = I18n::with_prefs(Catalog::new(), "en", prefs.clone());

        i18n.set_language("zh_TW").unwrap();
        assert_eq!(i18n.language(), "zh-TW");

        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prefs).unwrap()).unwrap();
        assert_eq!(saved["language"], "zh-TW");

        assert_eq!(detect_language(&prefs), "zh-TW");
    }
}

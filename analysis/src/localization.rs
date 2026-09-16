use std::collections::HashMap;
use std::sync::RwLock;

static ACTIVE_LANGUAGE: RwLock<&'static str> = RwLock::new("english");
static LOCALIZATIONS: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);
#[cfg(not(target_arch = "wasm32"))]
static EXTRA_SEARCH_PATHS: RwLock<Vec<std::path::PathBuf>> = RwLock::new(Vec::new());

#[cfg(target_arch = "wasm32")]
include!(concat!(env!("OUT_DIR"), "/embedded_localizations.rs"));

pub fn get_active_language() -> &'static str {
    *ACTIVE_LANGUAGE.read().unwrap()
}

pub fn set_active_language(lang: &'static str) {
    if *ACTIVE_LANGUAGE.read().unwrap() != lang {
        let mut active_lock = ACTIVE_LANGUAGE.write().unwrap();
        if *active_lock != lang {
            *active_lock = lang;
            let mut loc_lock = LOCALIZATIONS.write().unwrap();
            *loc_lock = None; // clear cache to force reload
        }
    }
}

/// Canonical form of a localization token: lowercase, no `#`.
///
/// The `#` is a lookup-time sigil, not part of the token name — that is Valve's
/// own convention, and the game's shipped files (`dod_english.txt`,
/// `valve_english.txt`, `gameui_english.txt`) all store keys bare. AMXX files
/// have no prefix by format. Normalizing on both insert and lookup means a file
/// using either style resolves identically.
fn normalize_key(key: &str) -> String {
    key.trim().trim_start_matches('#').to_lowercase()
}

/// Registers an additional `localizations/` folder for `load_pass` to scan,
/// on top of its built-in cwd- and walk-up-from-exe-relative guesses. Needed
/// for a packaged Tauri build: `translate_key`'s normal search only finds the
/// repo-root `localizations/` folder when the running binary lives inside the
/// source tree (dev/debug builds), so the installed app registers its bundled
/// resource directory here at startup instead. Clears the cache so a later
/// `translate_key` call reloads.
#[cfg(not(target_arch = "wasm32"))]
pub fn add_localization_search_path(dir: std::path::PathBuf) {
    EXTRA_SEARCH_PATHS.write().unwrap().push(dir);
    *LOCALIZATIONS.write().unwrap() = None;
}

pub fn translate_key(key: &str) -> Option<String> {
    let lookup_key = normalize_key(key);

    // 1. Try to read from cache first with a read lock
    {
        let read_lock = LOCALIZATIONS.read().unwrap();
        if let Some(ref map) = *read_lock {
            return map.get(&lookup_key).cloned();
        }
    }

    // 2. If it is None, acquire write lock to initialize it
    let mut write_lock = LOCALIZATIONS.write().unwrap();
    if write_lock.is_none() {
        let active = *ACTIVE_LANGUAGE.read().unwrap();
        *write_lock = Some(load_localizations_from_disk(active));
    }
    write_lock.as_ref().unwrap().get(&lookup_key).cloned()
}

fn get_amxx_code(lang: &str) -> &str {
    match lang {
        "german" => "de",
        "french" => "fr",
        "spanish" => "es",
        "russian" => "ru",
        "serbian" => "sr",
        "turkish" => "tr",
        "swedish" => "sv",
        "danish" => "da",
        "polish" => "pl",
        "dutch" => "nl",
        "portuguese" => "pt",
        "brazilian" => "bp",
        "czech" => "cz",
        "finnish" => "fi",
        "bulgarian" => "bg",
        "romanian" => "ro",
        "hungarian" => "hu",
        "lithuanian" => "lt",
        "slovak" => "sk",
        "macedonian" => "mk",
        "croatian" => "hr",
        "bosnian" => "bs",
        "chinese" => "cn",
        "albanian" => "al",
        _ => "en",
    }
}

fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut quotes_indices = Vec::new();
    let mut escaped = false;

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !escaped {
            escaped = true;
        } else {
            if c == '"' && !escaped {
                quotes_indices.push(i);
            }
            escaped = false;
        }
        i += 1;
    }

    if quotes_indices.len() >= 4 {
        let key: String = chars[quotes_indices[0] + 1..quotes_indices[1]]
            .iter()
            .collect();
        let val: String = chars[quotes_indices[2] + 1..quotes_indices[3]]
            .iter()
            .collect();
        Some((key, val))
    } else {
        None
    }
}

fn parse_localization_content(content: &str, map: &mut HashMap<String, String>, target_lang: &str) {
    let mut current_lang = "en".to_string(); // default to en in case there are no headers

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        // Check for language section like [en], [de]
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_lang = trimmed[1..trimmed.len() - 1].trim().to_lowercase();
            continue;
        }

        // Skip Valve KeyValues blocks
        if trimmed.starts_with('{') || trimmed.starts_with('}') {
            continue;
        }

        // Try parsing as Valve KeyValues first (quoted key and value)
        if let Some((key, val)) = parse_kv_line(trimmed) {
            let key_clean = normalize_key(&key);
            map.insert(key_clean, val);
        } else if current_lang == target_lang
            && let Some(pos) = trimmed.find('=') {
                let key = normalize_key(&trimmed[..pos]);
                let val = trimmed[pos + 1..].trim().to_string();
                if !key.is_empty() {
                    map.insert(key, val);
                }
            }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_localizations_from_disk(active_lang: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let amxx_code = get_amxx_code(active_lang);

    // Pass 1: English baseline
    load_pass(&mut map, "english", "en");

    // Pass 2: Active language overlay (if not English)
    if active_lang != "english" {
        load_pass(&mut map, active_lang, amxx_code);
    }

    map
}

#[cfg(not(target_arch = "wasm32"))]
fn scan_dir_recursive(
    dir: &std::path::Path,
    map: &mut HashMap<String, String>,
    filter_lang: &str,
    amxx_code: &str,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                scan_dir_recursive(&path, map, filter_lang, amxx_code);
            } else if path.is_file() && path.extension().map(|s| s == "txt").unwrap_or(false) {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                let should_load = if name.contains('_') {
                    name.ends_with(&format!("_{}.txt", filter_lang))
                } else {
                    true
                };

                if should_load
                    && let Ok(content) = read_to_string_lossy_utf16_or_utf8(&path) {
                        parse_localization_content(&content, map, amxx_code);
                    }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_pass(map: &mut HashMap<String, String>, filter_lang: &str, amxx_code: &str) {
    let mut paths = vec![
        std::path::PathBuf::from("localizations"),
        std::path::PathBuf::from("../localizations"),
    ];
    paths.extend(EXTRA_SEARCH_PATHS.read().unwrap().iter().cloned());

    if let Ok(exe_path) = std::env::current_exe() {
        // A single `.parent()` hop only finds a sibling `localizations/`
        // folder for binaries that run from the workspace root. Debug/dev
        // builds run from deeply nested target dirs (e.g. the Tauri app's
        // `desktop-studio/src-tauri/target/debug/`) whose exe directory is
        // several levels below the workspace-root `localizations/` folder
        // that ships beside the top-level Cargo.toml — every `translate_key`
        // call silently returned `None` there (weapon names rendered blank).
        // Walk up looking for it instead of assuming a fixed depth.
        let mut dir = exe_path.parent();
        for _ in 0..6 {
            match dir {
                Some(d) => {
                    paths.push(d.join("localizations"));
                    dir = d.parent();
                }
                None => break,
            }
        }
    }

    for base_path in paths {
        if base_path.is_dir()
            && base_path
                .file_name()
                .map(|n| n == "localizations")
                .unwrap_or(false)
        {
            scan_dir_recursive(&base_path, map, filter_lang, amxx_code);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_to_string_lossy_utf16_or_utf8(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.len() >= 2 {
        if bytes[0] == 0xFF && bytes[1] == 0xFE {
            // UTF-16 LE
            let u16_chars: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_le_bytes(*chunk))
                .collect();
            return Ok(String::from_utf16_lossy(&u16_chars));
        } else if bytes[0] == 0xFE && bytes[1] == 0xFF {
            // UTF-16 BE
            let u16_chars: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_be_bytes(*chunk))
                .collect();
            return Ok(String::from_utf16_lossy(&u16_chars));
        }
    }

    match String::from_utf8(bytes.clone()) {
        Ok(s) => Ok(s),
        Err(_) => {
            let has_nulls = bytes.iter().enumerate().any(|(i, &b)| b == 0 && i % 2 == 1);
            if has_nulls && bytes.len() % 2 == 0 {
                let u16_chars: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_le_bytes(*chunk))
                    .collect();
                Ok(String::from_utf16_lossy(&u16_chars))
            } else {
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn load_localizations_from_disk(active_lang: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let amxx_code = get_amxx_code(active_lang);

    // Pass 1: English baseline
    load_pass_embedded(&mut map, "english", "en");

    // Pass 2: Active language overlay (if not English)
    if active_lang != "english" {
        load_pass_embedded(&mut map, active_lang, amxx_code);
    }

    map
}

#[cfg(target_arch = "wasm32")]
fn load_pass_embedded(map: &mut HashMap<String, String>, filter_lang: &str, amxx_code: &str) {
    for (name, content) in EMBEDDED_LOCALIZATIONS {
        let name_lower = name.to_lowercase();
        let should_load = if name_lower.contains('_') {
            name_lower.ends_with(&format!("_{}.txt", filter_lang))
        } else {
            true
        };

        if should_load {
            parse_localization_content(content, map, amxx_code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_kv_line() {
        let line = "\"#Game_joined_team\"\t\t\"%s1 joined team %s2\"";
        let parsed = parse_kv_line(line);
        assert_eq!(
            parsed,
            Some((
                "#Game_joined_team".to_string(),
                "%s1 joined team %s2".to_string()
            ))
        );

        let line_escaped = "\"#Game_test\"\t\t\"This has a \\\"quote\\\" inside\"";
        let parsed_escaped = parse_kv_line(line_escaped);
        assert_eq!(
            parsed_escaped,
            Some((
                "#Game_test".to_string(),
                "This has a \\\"quote\\\" inside".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_localization_content() {
        let content = r##"
            "lang"
            {
                "Language" "English"
                "Tokens"
                {
                    "#Game_joined_team"        "%s1 joined team %s2"
                    "Game_join"                "%s1 joined the game"
                    // this is a comment
                    "#Game_disconnected"       "%s1 disconnected"
                }
            }
        "##;

        let mut map = HashMap::new();
        parse_localization_content(content, &mut map, "en");

        // Keys are canonicalized on insert: lowercased, `#` stripped. A file
        // written either way lands in the map identically.
        assert_eq!(
            map.get("game_joined_team"),
            Some(&"%s1 joined team %s2".to_string())
        );
        assert_eq!(
            map.get("game_join"),
            Some(&"%s1 joined the game".to_string())
        );
        assert_eq!(
            map.get("game_disconnected"),
            Some(&"%s1 disconnected".to_string())
        );

        // Nothing is ever stored with the sigil.
        assert!(map.keys().all(|k| !k.starts_with('#')));
    }

    #[test]
    fn test_parse_amxx_localization() {
        let content = r#"
            [en]
            CHO_FIN_EXT = Choosing finished. Current map will be extended to next %.0f minutes
            CHO_FIN_NEXT = Choosing finished. The nextmap will be %s
            
            [de]
            CHO_FIN_EXT = Auswahl beendet. Laufende Map wird um %.0f Minuten verlängert.
        "#;

        let mut map = HashMap::new();
        parse_localization_content(content, &mut map, "en");

        // AMXX keys carry no prefix by format, and land in canonical form.
        assert_eq!(
            map.get("cho_fin_ext"),
            Some(
                &"Choosing finished. Current map will be extended to next %.0f minutes".to_string()
            )
        );
        assert_eq!(map.get("#cho_fin_ext"), None);
        assert_eq!(
            map.get("cho_fin_next"),
            Some(&"Choosing finished. The nextmap will be %s".to_string())
        );

        // The [de] section must not leak into an [en] load.
        assert!(!map.values().any(|v| v.contains("Auswahl beendet")));
    }

    #[test]
    fn test_real_localization_loading() {
        // Force reload from disk
        set_active_language("english");
        let score_allies = translate_key("#game_score_allie_points");
        assert_eq!(score_allies.as_deref(), Some("Allies score %s1 points."));

        let joined_team = translate_key("#game_joined_team");
        assert_eq!(joined_team.as_deref(), Some("*%s1 joined %s2"));

        let class_kar = translate_key("#class_axis_kar98");
        assert_eq!(class_kar.as_deref(), Some("Grenadier"));
    }

    /// Callers pass keys both ways — `#game_joined_team` from chat handling,
    /// `weapon.k98` from the weapon table — so a lookup must resolve either
    /// query form. Before keys were canonicalized, a `#`-prefixed query could
    /// never reach the game files, which store their 1,190 tokens bare.
    #[test]
    fn test_translate_key_resolves_both_prefix_forms() {
        set_active_language("english");

        // Stored bare, queried both ways.
        assert_eq!(
            translate_key("#game_joined_team").as_deref(),
            Some("*%s1 joined %s2")
        );
        assert_eq!(
            translate_key("game_joined_team").as_deref(),
            Some("*%s1 joined %s2")
        );

        // Stored prefixed, queried both ways.
        assert_eq!(translate_key("#weapon.k98").as_deref(), Some("K98"));
        assert_eq!(translate_key("weapon.k98").as_deref(), Some("K98"));

        // Case is normalized on the way in.
        assert_eq!(translate_key("#Game_Joined_Team").as_deref(), Some("*%s1 joined %s2"));

        assert_eq!(translate_key("#definitely_not_a_real_key"), None);
    }
}

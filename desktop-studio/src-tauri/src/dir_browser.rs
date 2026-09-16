// dir_browser.rs — filesystem browsing backend for the Demo Analyzer's
// Explorer sidebar: a native drive/folder tree plus the non-recursive
// contents (subfolders + `.dem` files) of whichever single folder is
// selected. Mirrors the `dev` branch egui GUI's SidePanel::left explorer +
// browser.rs's `desktop_files` list (see native/src/bin/gui/tree.rs's
// get_native_roots/get_subdirs/get_demo_map_name/count_demo_files and
// scan_demo_folders_async) — see docs/archive/tauri_parity_audit.md Area 3 for the
// full corrected design this was rebuilt against.

use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct DirEntryLite {
    pub name: String,
    pub path: String,
    /// Non-recursive count of `.dem` files directly inside this folder —
    /// drives the tree's 📂/📁 icon + "(N)" suffix.
    pub demo_count: usize,
}

#[derive(Serialize)]
pub struct DemoFileEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_unix_secs: f64,
    pub map_name: String,
    pub demo_type: String,
}

#[derive(Serialize)]
pub struct DirListing {
    pub path: Option<String>,
    pub parent: Option<String>,
    pub subdirs: Vec<DirEntryLite>,
    pub demos: Vec<DemoFileEntry>,
}

fn is_dem_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("dem"))
        .unwrap_or(false)
}

/// Non-recursive count of `.dem` files directly inside `dir`. Mirrors dev's
/// `tree.rs::count_demo_files` exactly.
fn count_demo_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| {
                    let p = e.path();
                    !p.is_dir() && is_dem_file(&p)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Reads the map name directly out of the demo file header — a 276-byte
/// `HLDEMO` header read, no demo parsing. Mirrors dev's
/// `tree.rs::get_demo_map_name` exactly (including its `"-"` fallback).
fn get_demo_map_name(path: &Path) -> String {
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut header = [0u8; 276];
        if file.read_exact(&mut header).is_ok() && &header[0..6] == b"HLDEMO" {
            let map_bytes = &header[16..];
            let len = map_bytes.iter().position(|&c| c == 0).unwrap_or(260);
            return String::from_utf8_lossy(&map_bytes[..len]).into_owned();
        }
    }
    "-".to_string()
}

/// Filename heuristic fallback — dev has no header-read equivalent for demo
/// type either, see `tree.rs:159-164`.
fn demo_type_from_name(name: &str) -> String {
    if name.to_lowercase().contains("hltv") {
        "HLTV".to_string()
    } else {
        "POV".to_string()
    }
}

fn native_roots() -> Vec<DirEntryLite> {
    #[cfg(target_os = "windows")]
    {
        (b'A'..=b'Z')
            .filter_map(|letter| {
                let drive = format!("{}:\\", letter as char);
                let p = PathBuf::from(&drive);
                if p.is_dir() {
                    let demo_count = count_demo_files(&p);
                    Some(DirEntryLite { name: drive.clone(), path: drive, demo_count })
                } else {
                    None
                }
            })
            .collect()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let root = PathBuf::from("/");
        let demo_count = count_demo_files(&root);
        vec![DirEntryLite { name: "/".to_string(), path: "/".to_string(), demo_count }]
    }
}

/// Lists the immediate subfolders and `.dem` files of `path`. `path: None`
/// returns the drive roots (Windows) or `/` (elsewhere) as the top of the tree.
#[tauri::command]
pub fn browse_directory(path: Option<String>) -> Result<DirListing, String> {
    let Some(path) = path else {
        return Ok(DirListing {
            path: None,
            parent: None,
            subdirs: native_roots(),
            demos: Vec::new(),
        });
    };

    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(crate::messages::not_a_directory(&path));
    }

    let mut subdirs = Vec::new();
    let mut demos = Vec::new();

    let entries = std::fs::read_dir(&dir).map_err(|e| crate::messages::failed_to_read_file(&path, e))?;
    for entry in entries.filter_map(Result::ok) {
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        if entry_path.is_dir() {
            if name.starts_with('.') || name.starts_with('$') {
                continue;
            }
            let demo_count = count_demo_files(&entry_path);
            subdirs.push(DirEntryLite { name, path: entry_path.to_string_lossy().into_owned(), demo_count });
        } else if is_dem_file(&entry_path) {
            let meta = entry.metadata().ok();
            let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified_unix_secs = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let map_name = get_demo_map_name(&entry_path);
            let demo_type = demo_type_from_name(&name);
            demos.push(DemoFileEntry {
                name,
                path: entry_path.to_string_lossy().into_owned(),
                size_bytes,
                modified_unix_secs,
                map_name,
                demo_type,
            });
        }
    }

    subdirs.sort_by_key(|a| a.name.to_lowercase());
    demos.sort_by_key(|a| a.name.to_lowercase());

    // Drive roots (e.g. "C:\") have no useful parent; going "up" from one
    // should return to the drive-list root instead of erroring on `.parent()`.
    let parent = dir.parent().map(|p| p.to_string_lossy().into_owned());

    Ok(DirListing { path: Some(path), parent, subdirs, demos })
}

/// A sensible starting folder for the picker — the user's Documents
/// directory, falling back to their home directory.
#[tauri::command]
pub fn default_browse_dir() -> Option<String> {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .map(|p| p.to_string_lossy().into_owned())
}

/// Non-recursive `.dem` count for a single folder — used by the Explorer
/// sidebar's Quick Links rows (Pinned/Recent/Local), which don't otherwise
/// need a full directory listing.
#[tauri::command]
pub fn count_demo_files_in_folder(path: String) -> usize {
    count_demo_files(&PathBuf::from(path))
}

fn is_drive_root(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        let s = path.to_string_lossy();
        s.len() <= 3 && s.ends_with(":\\")
    }
    #[cfg(not(target_os = "windows"))]
    {
        path == Path::new("/")
    }
}

#[derive(Serialize)]
pub struct DemoFolderHit {
    pub path: String,
    pub demo_count: usize,
}

const SCAN_SKIP_DIRS: [&str; 5] = ["target", "node_modules", ".git", "src", "assets"];

/// Bounded background scan for folders containing at least one `.dem` file,
/// rooted at `root` (falling back to the default browse dir). Mirrors dev's
/// `tree.rs::scan_demo_folders_async` bounds exactly: depth-limited to 4,
/// capped at 2000 folders checked, skips `.`/`$`-prefixed and
/// target/node_modules/.git/src/assets dirs. Feeds the Explorer sidebar's
/// "Local" Quick Links tier.
#[tauri::command]
pub async fn scan_demo_folders(root: Option<String>) -> Result<Vec<DemoFolderHit>, String> {
    crate::messages::spawn_blocking_result(tokio::task::spawn_blocking(move || {
        let root_dir = root
            .map(PathBuf::from)
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        if !root_dir.is_dir() {
            return Vec::new();
        }

        let mut folders = Vec::new();
        let walk_recursive = !is_drive_root(&root_dir);
        let mut stack = vec![(root_dir, 0usize)];
        let max_depth = 4;
        let mut folders_checked = 0;
        let max_folders_checked = 2000;

        while let Some((dir, depth)) = stack.pop() {
            folders_checked += 1;
            if folders_checked > max_folders_checked {
                break;
            }

            let mut demo_count = 0;
            let mut subdirs = Vec::new();

            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    if path.is_dir() {
                        if walk_recursive && depth < max_depth
                            && let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                let name_lower = name.to_lowercase();
                                if !name.starts_with('.')
                                    && !name.starts_with('$')
                                    && !SCAN_SKIP_DIRS.contains(&name_lower.as_str())
                                {
                                    subdirs.push(path);
                                }
                            }
                    } else if is_dem_file(&path) {
                        demo_count += 1;
                    }
                }
            }

            if demo_count > 0 {
                folders.push(DemoFolderHit { path: dir.to_string_lossy().into_owned(), demo_count });
            }

            subdirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            for subdir in subdirs {
                stack.push((subdir, depth + 1));
            }
        }

        folders.sort_by(|a, b| a.path.cmp(&b.path));
        folders
    }))
    .await
}

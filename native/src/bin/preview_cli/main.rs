//! Headless preview builder: turns demos into `<stem>_preview.dem` with a
//! bookmark on every highlight.
//!
//! ## `--player` on an HLTV demo
//!
//! An HLTV demo holds every player's highlights, so a preview of one is a wall
//! of bookmarks for twelve people. Naming a player narrows them to that one,
//! which is what makes the Event List usable for finding somebody's rounds.
//!
//! It does **not** move the camera, and cannot: DoD 1.3's client implements
//! DRC commands 1-10 only, so the two that aim a spectator camera --
//! DRC_CMD_CHASE (11) and DRC_CMD_INEYE (12) -- are discarded unread. That was
//! tried, and read out of client.dll afterwards; see
//! docs/goldsrc_client_dll_internals.md. Jumping to a bookmark still lands on
//! the right moment, but the camera is wherever the auto-director left it.
//!
//! Meaningless for a POV demo, which is already one player's, so it is refused
//! there rather than silently ignored.

use std::io::{self, Write};

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let (args, requested_player) = split_player_argument(&raw_args);
    let mut input_paths: Vec<String> = Vec::new();
    let mut is_interactive = false;

    if args.len() <= 1 {
        is_interactive = true;
        println!("Drag and drop demo files or directories into this window and press Enter.");
        println!("(To exit without processing, leave blank and press Enter)");
        print!("> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let mut trimmed = input.trim();

            // Strip PowerShell evaluation operator if present
            if trimmed.starts_with("& ") {
                trimmed = trimmed[2..].trim();
            }

            let mut current = String::new();
            let mut active_quote: Option<char> = None;

            for c in trimmed.chars() {
                if let Some(q) = active_quote {
                    if c == q {
                        active_quote = None; // Closing quote
                    } else {
                        current.push(c);
                    }
                } else if c == '"' || c == '\'' {
                    active_quote = Some(c); // Opening quote
                } else if c == ' ' {
                    if !current.is_empty() {
                        input_paths.push(current.clone());
                        current.clear();
                    }
                } else {
                    current.push(c);
                }
            }
            if !current.is_empty() {
                input_paths.push(current);
            }
        }

        if input_paths.is_empty() {
            return;
        }
    } else {
        input_paths = args[1..].to_vec();
    }

    let patcher_config = native::patch::PatcherConfig::default();
    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut processed = 0usize;
    let mut skipped = 0usize;

    for input in &input_paths {
        let path = std::path::PathBuf::from(input);

        if path.is_dir() {
            // ── Directory input: scan all .dem files inside ──────────────────
            let output_dir = path.join("previews");
            if let Err(e) = std::fs::create_dir_all(&output_dir) {
                eprintln!("Error creating output directory: {:?} - {}", output_dir, e);
                continue;
            }
            println!("Created directory: {:?}", output_dir);

            let entries = match std::fs::read_dir(&path) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to read directory: {:?} - {}", path, e);
                    continue;
                }
            };

            for entry in entries.flatten() {
                let file_path = entry.path();
                if !file_path.is_file() {
                    continue;
                }
                let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext.to_lowercase() != "dem" {
                    continue;
                }
                process_demo(&file_path, &output_dir, &patcher_config, requested_player, &cancel_token, &mut processed, &mut skipped);
            }
        } else if path.is_file() {
            // ── Individual file input ────────────────────────────────────────
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.to_lowercase() != "dem" {
                eprintln!("Skipped: {:?} — Not a .dem file", path.file_name().unwrap_or_default());
                skipped += 1;
                continue;
            }

            let output_dir = path
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .join("previews");

            if let Err(e) = std::fs::create_dir_all(&output_dir) {
                eprintln!("Error creating output directory: {:?} - {}", output_dir, e);
                skipped += 1;
                continue;
            }
            println!("Created directory: {:?}", output_dir);

            process_demo(&path, &output_dir, &patcher_config, requested_player, &cancel_token, &mut processed, &mut skipped);
        } else {
            eprintln!("Skipped: {:?} — Path not accessible", path);
            skipped += 1;
        }
    }

    println!(
        "\nBatch Complete\n  Processed: {}  |  Skipped: {}",
        processed, skipped
    );

    if is_interactive {
        println!("\nPress Enter to exit...");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

/// Pulls `--player <name|index>` out of the argument list, leaving the rest
/// for the drag-and-drop path parser to treat as paths.
///
/// Hand-rolled rather than pulled in as a dependency: this binary takes paths
/// and now one flag, and it is dropped-on as often as it is typed.
fn split_player_argument(args: &[String]) -> (Vec<String>, Option<&str>) {
    let mut kept = Vec::with_capacity(args.len());
    let mut player = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--player" | "-p" if i + 1 < args.len() => {
                player = Some(args[i + 1].as_str());
                i += 2;
            }
            other if other.starts_with("--player=") => {
                player = Some(&args[i]["--player=".len()..]);
                i += 1;
            }
            _ => {
                kept.push(args[i].clone());
                i += 1;
            }
        }
    }
    (kept, player)
}

/// Whether a streak belongs to the player named on the command line.
///
/// Accepts an entity index as well as a name, because DoD names are full of
/// clan tags and punctuation that a shell would fight over -- the index is
/// printed alongside every name in the roster listing for exactly that reason.
fn streak_matches_player(streak: &native::patch::CaptureStreak, want: &str) -> bool {
    if let Ok(index) = want.parse::<usize>() {
        return streak.player_index == index;
    }
    streak
        .target_player
        .as_deref()
        .is_some_and(|name| name.to_lowercase().contains(&want.to_lowercase()))
}

/// Lists who has highlights in an HLTV demo, so `--player` has something to
/// name. Sorted by kills, since that is what a preview is being built to find.
fn print_roster(streaks: &[native::patch::CaptureStreak]) {
    let mut by_player: std::collections::HashMap<usize, (String, usize, u32)> =
        std::collections::HashMap::new();
    for s in streaks {
        let entry = by_player
            .entry(s.player_index)
            .or_insert_with(|| (s.target_player.clone().unwrap_or_else(|| "<unnamed>".into()), 0, 0));
        entry.1 += 1;
        entry.2 += s.kill_count as u32;
    }
    let mut rows: Vec<_> = by_player.into_iter().collect();
    rows.sort_by_key(|(_, (_, _, kills))| std::cmp::Reverse(*kills));

    println!("  players with highlights (pass one to --player, by name or index):");
    for (index, (name, streaks, kills)) in rows {
        println!("    {index:>3}  {kills:>3} kills in {streaks:>2} streaks   {name}");
    }
}

fn process_demo(
    path: &std::path::Path,
    output_dir: &std::path::Path,
    patcher_config: &native::patch::PatcherConfig,
    requested_player: Option<&str>,
    cancel_token: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    processed: &mut usize,
    skipped: &mut usize,
) {
    let original_filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    println!("Processing: {}", original_filename);

    let (_tickrate, mut streaks, is_pov, local_player_idx, _playback_frames, _match_start, _frame_times) =
        match native::patch::scan_demo_for_highlights(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  Skipped: {} — Scan error: {}", original_filename, e);
                *skipped += 1;
                return;
            }
        };

    // A POV demo already records the only camera it has, and its highlights are
    // filtered to the recording player anyway, so there is nothing for
    // `--player` to do. Say so instead of silently ignoring it, or a mistyped
    // name looks like it worked.
    let selected_player = if is_pov {
        streaks.retain(|s| Some(s.player_index) == local_player_idx);
        if requested_player.is_some() {
            println!("  Note: {original_filename} is a POV demo — --player ignored (its camera is already one player's)");
        }
        None
    } else {
        requested_player
    };

    if streaks.is_empty() {
        println!("  Skipped: {} — No highlights found", original_filename);
        *skipped += 1;
        return;
    }

    if let Some(want) = selected_player {
        let roster = streaks.clone();
        streaks.retain(|s| streak_matches_player(s, want));
        if streaks.is_empty() {
            println!("  Skipped: {original_filename} — no highlights for a player matching \"{want}\"");
            print_roster(&roster);
            *skipped += 1;
            return;
        }
    } else if !is_pov {
        // Not an error -- the all-players preview is still what most runs want.
        // But an HLTV preview bookmarks kills the auto-director may never have
        // been pointed at, so it is worth knowing the option exists.
        println!("  HLTV demo: bookmarking all players. Re-run with --player to narrow it to one.");
        print_roster(&streaks);
    }

    // Taken before `streaks` is moved into the builder.
    let chosen_player = selected_player.map(|_| {
        let s = &streaks[0];
        (s.player_index, s.target_player.clone().unwrap_or_else(|| format!("p{}", s.player_index)))
    });

    let mut jobs = native::patch::build_preview_patch_jobs(streaks, Some(output_dir));

    if let Some((index, name)) = &chosen_player {
        for job in &mut jobs {
            job.target_player = Some(name.clone());
            // Distinguish one player's preview from another's, through the same
            // sanitizer the builder uses -- `playdemo`/`viewdemo` targets have a
            // ~40 character budget and break silently past it, so the combined
            // stem has to be re-fitted rather than just appended to.
            let stem = std::path::Path::new(&job.source_demo)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let safe = native::patch::playdemo_safe_stem(&format!("{stem}_{index}"));
            job.output_demo = output_dir.join(format!("{safe}_preview.dem"));
        }
        println!("  Bookmarking only {name} (index {index}) — the camera stays with the auto-director");
    }

    for job in &jobs {
        let new_filename = job.output_demo
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let patcher = native::patch::StreamPatcher::new(&job.source_demo, &job.output_demo);
        match patcher.patch(job, patcher_config, cancel_token) {
            Ok(()) => {
                println!("  Saved: {}", new_filename);
                *processed += 1;
            }
            Err(e) => {
                eprintln!("  Error writing file {}: {}", new_filename, e);
                *skipped += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_player_argument;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn player_flag_is_removed_from_the_paths() {
        // argv[0] is the exe, and the caller indexes args[1..] for paths, so
        // the flag has to come out without disturbing that offset.
        let a = args(&["preview_cli.exe", "--player", "8", "demo.dem"]);
        let (kept, player) = split_player_argument(&a);
        assert_eq!(player, Some("8"));
        assert_eq!(kept, args(&["preview_cli.exe", "demo.dem"]));
    }

    #[test]
    fn player_flag_accepts_its_spellings() {
        for form in [
            vec!["x", "-p", "stealth", "d.dem"],
            vec!["x", "--player", "stealth", "d.dem"],
            vec!["x", "--player=stealth", "d.dem"],
        ] {
            let a = args(&form);
            let (kept, player) = split_player_argument(&a);
            assert_eq!(player, Some("stealth"), "{form:?}");
            assert_eq!(kept, args(&["x", "d.dem"]), "{form:?}");
        }
    }

    #[test]
    fn a_trailing_player_flag_with_no_value_is_kept_as_a_path() {
        // Better a "not accessible" complaint naming the flag than silently
        // consuming the last argument and building an unfiltered preview.
        let a = args(&["x", "demo.dem", "--player"]);
        let (kept, player) = split_player_argument(&a);
        assert_eq!(player, None);
        assert_eq!(kept, args(&["x", "demo.dem", "--player"]));
    }

    #[test]
    fn paths_survive_untouched_when_no_flag_is_given() {
        let a = args(&["x", "one.dem", "two.dem"]);
        let (kept, player) = split_player_argument(&a);
        assert_eq!(player, None);
        assert_eq!(kept, a);
    }
}

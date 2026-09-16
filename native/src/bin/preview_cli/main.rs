use std::io::{self, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
                process_demo(&file_path, &output_dir, &patcher_config, &cancel_token, &mut processed, &mut skipped);
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

            process_demo(&path, &output_dir, &patcher_config, &cancel_token, &mut processed, &mut skipped);
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

fn process_demo(
    path: &std::path::Path,
    output_dir: &std::path::Path,
    patcher_config: &native::patch::PatcherConfig,
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

    if is_pov {
        streaks.retain(|s| Some(s.player_index) == local_player_idx);
    }

    if streaks.is_empty() {
        println!("  Skipped: {} — No highlights found", original_filename);
        *skipped += 1;
        return;
    }

    let jobs = native::patch::build_preview_patch_jobs(streaks, Some(output_dir));

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

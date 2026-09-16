//! Diagnostics tool to scan unique demos and inspect raw user message types and frequencies.

use clap::Parser;
use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData, NetMessage};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    /// Folder paths to scan for .dem files
    paths: Vec<PathBuf>,

    /// Limit the number of unique demos to inspect (pass 99999 for all)
    #[arg(long, default_value_t = 99999)]
    limit: usize,
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
struct FileKey {
    size: u64,
    header_hash: u64,
}

struct InspectResult {
    map_name: String,
    size_bytes: usize,
    frames: usize,
    message_counts: HashMap<String, u64>,
}

fn inspect_single_demo(path: &Path) -> Result<InspectResult, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let size_bytes = bytes.len();

    // Catch panics from the external dem crate
    let demo_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        open_demo_from_bytes(&bytes)
    }));

    let demo = match demo_res {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return Err(format!("Parse error: {:?}", e)),
        Err(_) => return Err("Parser panicked during execution".to_string()),
    };

    let map_name = demo
        .header
        .map_name
        .to_str()
        .map(|s| s.trim_end_matches('\x00'))
        .unwrap_or("unknown")
        .to_string();

    let mut message_counts = HashMap::new();
    let mut frames = 0;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            frames += 1;
            if let FrameData::NetworkMessage(box_type) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &box_type.1.messages {
                    for net_msg in msgs {
                        if let NetMessage::UserMessage(user_msg) = net_msg {
                            let name = String::from_utf8_lossy(&user_msg.name)
                                .trim_end_matches('\x00')
                                .to_string();
                            *message_counts.entry(name).or_insert(0) += 1;
                        }
                    }
                }
        }
    }

    Ok(InspectResult {
        map_name,
        size_bytes,
        frames,
        message_counts,
    })
}

fn main() {
    let args = Args::parse();
    if args.paths.is_empty() {
        eprintln!("Usage: inspect <folder1> <folder2> ...");
        std::process::exit(1);
    }

    println!("Scanning directories...");
    let mut files = vec![];
    for folder in &args.paths {
        if folder.exists() {
            scan_dir(folder, &mut files);
        }
    }

    println!("Found {} total .dem files. Deduplicating...", files.len());
    let mut groups: HashMap<FileKey, Vec<PathBuf>> = HashMap::new();
    for path in files {
        if let Ok(key) = get_file_key(&path) {
            groups.entry(key).or_default().push(path);
        }
    }

    let mut unique_files = vec![];
    for mut paths in groups.into_values() {
        if !paths.is_empty() {
            paths.sort();
            unique_files.push(paths[0].clone());
        }
    }
    unique_files.sort();

    let total_unique = unique_files.len();
    let limit = std::cmp::min(total_unique, args.limit);
    println!(
        "Deduplication complete. {} unique files identified.",
        total_unique
    );
    println!(
        "Inspecting all {} unique demos using parallel threads...",
        limit
    );

    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    println!("Using {} threads for parallel processing.", num_threads);

    let start_time = Instant::now();
    let unique_files_arc = std::sync::Arc::new(unique_files);
    let (tx, rx) = std::sync::mpsc::channel();

    let chunk_size = limit.div_ceil(num_threads);

    for thread_idx in 0..num_threads {
        let tx = tx.clone();
        let unique_files = unique_files_arc.clone();
        std::thread::spawn(move || {
            let start = thread_idx * chunk_size;
            let end = std::cmp::min(start + chunk_size, limit);
            for idx in start..end {
                let path = &unique_files[idx];
                let result = inspect_single_demo(path);
                let _ = tx.send((idx, result));
            }
        });
    }
    drop(tx);

    let mut message_counts: HashMap<String, u64> = HashMap::new();
    let mut message_demos: HashMap<String, usize> = HashMap::new();
    let mut map_counts: HashMap<String, usize> = HashMap::new();
    let mut total_frames = 0;
    let mut processed_size_bytes = 0;
    let mut processed_count = 0;

    while let Ok((idx, res)) = rx.recv() {
        processed_count += 1;
        match res {
            Ok(inspect_res) => {
                processed_size_bytes += inspect_res.size_bytes;
                total_frames += inspect_res.frames;
                *map_counts.entry(inspect_res.map_name).or_insert(0) += 1;

                let mut seen_in_this_demo = HashSet::new();
                for (name, count) in inspect_res.message_counts {
                    *message_counts.entry(name.clone()).or_insert(0) += count;
                    seen_in_this_demo.insert(name);
                }

                for name in seen_in_this_demo {
                    *message_demos.entry(name).or_insert(0) += 1;
                }
            }
            Err(e) => {
                let path = &unique_files_arc[idx];
                eprintln!(
                    "\n  [Error] Failed to inspect demo {}: {} ({})",
                    idx + 1,
                    path.display(),
                    e
                );
            }
        }

        if processed_count % 100 == 0 || processed_count == limit {
            println!(
                "  -> Progress: {}/{} demos inspected...",
                processed_count, limit
            );
        }
    }

    let elapsed = start_time.elapsed();
    println!(
        "\nAnalysis completed in {:.2} seconds.",
        elapsed.as_secs_f64()
    );
    println!(
        "Total data parsed: {:.2} MB ({:.2} GB)",
        processed_size_bytes as f64 / 1_048_576.0,
        processed_size_bytes as f64 / 1_073_741_824.0
    );
    println!("Total frames processed: {}", total_frames);

    println!("\n### Map Distributions ###");
    let mut sorted_maps: Vec<_> = map_counts.into_iter().collect();
    sorted_maps.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    for (map, count) in sorted_maps {
        println!("  - {:.<30} {} demos", map, count);
    }

    println!("\n### User Message Type Frequency & Penetration ###");
    println!(
        "| User Message Name | Total Occurrences | Demos Containing Message | Penetration % |"
    );
    println!(
        "|-------------------|-------------------|--------------------------|---------------|"
    );

    let mut sorted_messages: Vec<_> = message_counts.into_iter().collect();
    sorted_messages.sort_by_key(|&(_, count)| std::cmp::Reverse(count));

    for (name, count) in sorted_messages {
        let demos_with_msg = message_demos.get(&name).cloned().unwrap_or(0);
        let penetration = (demos_with_msg as f64 / limit as f64) * 100.0;
        println!(
            "| {:<17} | {:<17} | {:<24} | {:<12.1}% |",
            name, count, demos_with_msg, penetration
        );
    }
}

fn scan_dir(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_dir(&path, files);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dem"))
            {
                files.push(path);
            }
        }
    }
}

fn get_file_key(path: &Path) -> Result<FileKey, std::io::Error> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();

    let mut file = fs::File::open(path)?;
    let read_size = std::cmp::min(size, 65536) as usize;
    let mut buffer = vec![0; read_size];
    file.read_exact(&mut buffer)?;

    let mut hasher = DefaultHasher::new();
    buffer.hash(&mut hasher);
    let header_hash = hasher.finish();

    Ok(FileKey { size, header_hash })
}

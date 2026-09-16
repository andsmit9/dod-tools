//! What does parsing demos N-at-a-time actually cost in memory?
//!
//! Written to settle `SCAN_CONCURRENCY` for #195/#212. The scan loop in
//! `capture_manager::scan_directory_impl` parses each demo with a fixed worker
//! pool, and the obvious question -- does four workers cost four times the
//! peak? -- is not answerable by reading the code, because the answer depends
//! on how big an `Analysis` is relative to the demo that produced it.
//!
//! This mirrors that loop's Phase 2 deliberately: same
//! `scan_demo_for_highlights_with_analysis` call, same fixed pool, same
//! pre-sized result slots claimed through an atomic index. So the number it
//! prints is the real thing rather than a model of it. Peak working set comes
//! from the OS at exit, not from sampling, so it cannot miss a spike.
//!
//! It is a *floor*, though: this drops each `Analysis` as soon as it has the
//! streak count, whereas the real scan converts it to a `SerializedDemo` and
//! keeps that for the whole run. Real peak is somewhat higher.
//!
//! Measured over a 45-demo corpus (3.2GB, 72MB mean, 105MB largest):
//!
//! | workers | wall time | peak working set |
//! | ------- | --------- | ---------------- |
//! | 1       | 56.2s     | 1529 MB          |
//! | 2       | 40.8s     | 2414 MB          |
//! | 4       | 36.0s     | 4574 MB          |
//! | 8       | 49.7s     | 9157 MB          |
//!
//! Eight being slower *and* 9GB is the useful data point: the ceiling is
//! memory pressure, not CPU, so raising the pool past a couple of workers
//! buys progressively less for progressively more.
//!
//! Usage:
//!
//!     cargo run --release -p native --example scan_mem_probe -- <folder> <workers>
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Peak working set for this process, straight from the OS.
///
/// Hand-rolled binding rather than a `windows`/`sysinfo` dependency: this is a
/// probe, and making the crate it lives in carry a dependency for it would be
/// a poor trade. `K32GetProcessMemoryInfo` is in kernel32 on every supported
/// Windows, so no psapi link is needed.
fn peak_working_set_mb() -> f64 {
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    unsafe {
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
    }
    c.peak_working_set_size as f64 / 1_048_576.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(folder) = args.next() else {
        eprintln!("usage: scan_mem_probe <folder> [workers]");
        std::process::exit(2);
    };
    let workers: usize = args.next().and_then(|w| w.parse().ok()).unwrap_or(4);

    let mut list = collect_demos(std::path::Path::new(&folder));
    // Sorted so a run is comparable with the one before it -- the scan loop
    // sorts too, and which demos land in the same window changes the peak.
    list.sort();
    let total = list.len();
    if total == 0 {
        eprintln!("no .dem files under {folder}");
        std::process::exit(1);
    }
    println!("{total} demos, {workers} worker(s)");

    let slots: Mutex<Vec<Option<usize>>> = Mutex::new((0..total).map(|_| None).collect());
    let next_index = AtomicUsize::new(0);
    let started = std::time::Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..workers.min(total).max(1) {
            let (list, slots, next_index) = (&list, &slots, &next_index);
            scope.spawn(move || loop {
                let idx = next_index.fetch_add(1, Ordering::Relaxed);
                if idx >= total {
                    break;
                }
                let parsed = native::patch::scan_demo_for_highlights_with_analysis(&list[idx]).ok();
                slots.lock().unwrap()[idx] = parsed.map(|((_, streaks, ..), _)| streaks.len());
            });
        }
    });

    let parsed = slots.lock().unwrap().iter().flatten().count();
    println!(
        "parsed {parsed}/{total} in {:.1}s | peak working set {:.0} MB",
        started.elapsed().as_secs_f64(),
        peak_working_set_mb()
    );
}

fn collect_demos(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(collect_demos(&path));
            } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("dem")) {
                out.push(path);
            }
        }
    }
    out
}

//! What the game's own config files set behind the pipeline's back.
//!
//! ```text
//! cargo run --release -p native --bin check_cfgs -- <dod-folder> ["init command"...]
//! ```
//!
//! Any init commands passed after the folder are checked for overriding a value
//! a config already sets.
//!
//! Read-only. This never writes, edits or removes a config file.

use native::patch::cfg_scan;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next()
            .expect("usage: check_cfgs <dod-folder> [\"init command\"...]"),
    );
    let init_commands: Vec<String> = args.collect();

    let scan = cfg_scan::scan(&dir);
    println!("{}\n{} config file(s) executed\n", dir.display(), scan.files_read);

    let effective = scan.effective_settings();
    if effective.is_empty() {
        println!("Nothing the pipeline depends on is set by an executed config.");
    } else {
        println!("Set by configs the engine runs on its own:");
        for s in &effective {
            println!("  {:<12} {:<8} {}:{}", s.cvar, s.value, s.file_name(), s.line);
        }
    }

    // Only the cvars in play. A full config.cfg is ~200 assignments, and
    // printing every one buries the two that matter.
    let shadowed: Vec<_> = scan
        .settings
        .iter()
        .filter(|s| {
            s.auto_executed
                && cfg_scan::WATCHED_CVARS
                    .iter()
                    .any(|w| s.cvar.eq_ignore_ascii_case(w))
                && !effective.iter().any(|e| std::ptr::eq(*e, *s))
        })
        .collect();
    if !shadowed.is_empty() {
        println!("\nAlso set, but overridden later in the chain:");
        for s in shadowed {
            println!("  {:<12} {:<8} {}:{}", s.cvar, s.value, s.file_name(), s.line);
        }
    }

    println!("\n{} assignment(s) seen across those configs.", scan.settings.len());

    if !init_commands.is_empty() {
        let shadows = cfg_scan::self_overrides(&init_commands);
        let dead: std::collections::HashSet<&str> =
            shadows.iter().map(|s| s.shadowed.as_str()).collect();
        if !shadows.is_empty() {
            println!("\nInit commands beaten by a later one in the same list:");
            for s in &shadows {
                println!(
                    "  {:<20} never applies — {} sets {} (position {})",
                    s.shadowed, s.cvar, s.winner_value, s.winner_index
                );
            }
        }

        // A command that never applies overrides nothing.
        let overrides: Vec<_> = scan
            .overrides_in(&init_commands)
            .into_iter()
            .filter(|o| !dead.contains(o.command.as_str()))
            .collect();
        println!();
        if overrides.is_empty() {
            println!("None of those init commands override a config value.");
        } else {
            println!("Init commands that will override a config value:");
            for o in overrides {
                println!(
                    "  {:<20} overrides {} {} ({}:{})",
                    o.command,
                    o.cvar,
                    o.cfg_value,
                    o.file_name(),
                    o.line
                );
            }
        }
    }

    let fatal = cfg_scan::fatal_cvar_hazards(&scan);
    if !fatal.is_empty() {
        println!("\nWill quit the game the moment the HUD renders:");
        for f in &fatal {
            println!(
                "  {:<12} {:<8} DoD requires {} ({}:{})",
                f.cvar,
                f.value,
                f.required,
                f.file_name(),
                f.line
            );
        }
    }

    if !scan.unreferenced.is_empty() {
        println!("\nConfigs present that nothing execs (they set nothing today):");
        for p in &scan.unreferenced {
            println!("  {}", p.file_name().unwrap_or_default().to_string_lossy());
        }
    }
}

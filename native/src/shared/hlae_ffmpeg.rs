//! Whether HLAE can reach an FFmpeg, which is a different question from whether
//! *we* can.
//!
//! `mirv_movie_ffmpeg` makes HLAE spawn FFmpeg itself, and HLAE does not consult
//! the app's own resolution chain (User Override → Bundled → System Path). Per
//! `<HLAE>/ffmpeg/readme.advancedfx.txt` it looks in exactly two places:
//!
//!   - `<HLAE>/ffmpeg/bin/ffmpeg.exe`, or
//!   - the path named by `[Ffmpeg] Path=` in `<HLAE>/ffmpeg/ffmpeg.ini`.
//!
//! With neither present the feature fails in the least helpful way available: a
//! capture that runs to completion and produces no video. So the state is worth
//! reporting before a batch, not after.
//!
//! **Linking writes a two-line ini rather than copying the binary.** A copy
//! duplicates ~100 MB and creates a second FFmpeg to keep in step with the one
//! Render Studio uses; they drift, and then the two halves of the pipeline
//! encode with different builds.
//!
//! **Somebody else's ini is never overwritten.** HLAE is shared with Source work
//! and other projects, so silently repointing it would break someone else's
//! workflow to fix ours. Where one exists and disagrees, that is reported and
//! left alone — the same discipline `patch::cfg_scan` applies to the game's own
//! `.cfg` files.
//!
//! What is protected is a *configuration*, not a filename. An ini with no
//! `Path=` in it — empty, or comments only — states nothing, holds no data to
//! lose, and is written over. Refusing there would strand someone behind a file
//! that does nothing, in a folder they usually cannot edit without
//! administrator rights.
//!
//! A file *this app* wrote is a different matter, and is rewritten on request.
//! Treating those as untouchable too would make the first link permanent: change
//! the app's FFmpeg afterwards and HLAE stays pointed at the old one, with no
//! way back through the UI and a folder that usually needs administrator rights
//! to edit by hand. See `docs/direct_to_video_capture.md`.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

const FFMPEG_DIR: &str = "ffmpeg";
const INI_NAME: &str = "ffmpeg.ini";
const BUNDLED_RELATIVE: &str = "bin/ffmpeg.exe";

/// The GoldSrc hook DLL a real HLAE install carries beside its executable, and
/// which `PatcherConfig::build_hlae_process` passes as `-hookDllPath`.
const HOOK_DLL: &str = "AfxHookGoldSrc.dll";

/// The header `link` writes, and the marker `authored_by_us` looks for.
const AUTHORED_MARKER: &str = "Written by dod-tools";

/// What HLAE would find if it looked right now.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HlaeFfmpeg {
    /// A binary sits in HLAE's own folder. It wins over any ini, so nothing
    /// else matters and nothing needs doing.
    Bundled { path: PathBuf },
    /// An `ffmpeg.ini` names a path. `target_exists` is reported separately
    /// because a stale ini pointing at a moved FFmpeg looks configured and is
    /// not — exactly the case a "configured / not configured" boolean would
    /// hide.
    ///
    /// `ours` is whether this file carries the header `link` writes. The
    /// never-overwrite rule protects somebody *else's* configuration; a file
    /// this app wrote is a file it may correct. Without that distinction the
    /// first successful link is permanent, and changing the app's own FFmpeg
    /// afterwards leaves HLAE pointed at the old one with no way back through
    /// the UI — the folder needs administrator rights to delete from, too.
    Linked {
        ini: PathBuf,
        target: PathBuf,
        target_exists: bool,
        ours: bool,
    },
    /// HLAE's ffmpeg folder exists but holds neither. `mirv_movie_ffmpeg` will
    /// produce nothing.
    Missing { folder: PathBuf },
    /// The configured HLAE path does not resolve to an install at all, so this
    /// question cannot be answered yet. Distinct from `Missing`: nothing here
    /// is worth offering to fix.
    NoInstall,
}

impl HlaeFfmpeg {
    /// Whether HLAE would actually find something usable.
    pub fn is_usable(&self) -> bool {
        match self {
            HlaeFfmpeg::Bundled { .. } => true,
            HlaeFfmpeg::Linked { target_exists, .. } => *target_exists,
            _ => false,
        }
    }

    /// Whether offering to link would make sense. False when a binary is
    /// already bundled (an ini would be ignored anyway), when somebody else's
    /// ini is already there, and when there is no install to write into.
    pub fn can_link(&self) -> bool {
        match self {
            HlaeFfmpeg::Missing { .. } => true,
            // Ours to correct — see the `ours` field. This is also the only
            // route back when the app's own FFmpeg changes, since the folder
            // usually needs administrator rights to delete from by hand.
            HlaeFfmpeg::Linked { ours, .. } => *ours,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum LinkError {
    /// No HLAE install at the configured path.
    NoInstall,
    /// An `ffmpeg.ini` is already there. Refused rather than overwritten.
    AlreadyLinked { ini: PathBuf, target: PathBuf },
    /// A binary is already bundled, so an ini would be ignored anyway.
    AlreadyBundled { path: PathBuf },
    /// The FFmpeg offered is not a file that exists.
    NoSuchFfmpeg { path: PathBuf },
    /// It exists and it is not FFmpeg. `ffplay.exe` and `ffprobe.exe` live in
    /// the same folder and are a misclick apart in a file picker.
    NotFfmpeg { why: String },
    /// HLAE's folder is not writable by this process. HLAE ships as a zip as
    /// well as an installer, so it can live anywhere and how often this happens
    /// is not known — but a protected location is a real enough possibility to
    /// route through rather than report as a raw OS error. See `link_elevated`.
    NeedsElevation { ini: PathBuf },
    /// The elevated write was declined at the UAC prompt, or failed.
    ElevationRefused,
    Io(std::io::Error),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::NoInstall => write!(f, "no HLAE install at the configured path"),
            LinkError::AlreadyLinked { ini, target } => write!(
                f,
                "{} already points at {} — left alone, since HLAE is shared with other projects",
                ini.display(),
                target.display()
            ),
            LinkError::AlreadyBundled { path } => write!(
                f,
                "HLAE already has its own FFmpeg at {}, which takes precedence over any ini",
                path.display()
            ),
            LinkError::NoSuchFfmpeg { path } => {
                write!(f, "no FFmpeg at {}", path.display())
            }
            LinkError::NotFfmpeg { why } => write!(f, "{}", why),
            LinkError::NeedsElevation { ini } => write!(
                f,
                "{} is not writable without administrator rights",
                ini.display()
            ),
            LinkError::ElevationRefused => {
                write!(f, "the administrator prompt was declined or failed")
            }
            LinkError::Io(e) => write!(f, "{}", e),
        }
    }
}

/// HLAE's `ffmpeg` folder, given the path to `HLAE.exe`.
///
/// The configured value names the executable, not the folder — the same
/// convention `game_path` follows, and the same trap: reading it as a directory
/// resolves one level too high and silently looks somewhere else.
pub fn ffmpeg_dir(hlae_exe: &Path) -> Option<PathBuf> {
    // The executable itself has to be there. Checking only the parent would
    // accept a path whose folder happens to exist — a typo'd exe name, or the
    // folder typed in place of the exe — and then offer to write an ini into
    // somewhere that is not an HLAE install at all.
    if !hlae_exe.is_file() {
        return None;
    }
    Some(hlae_exe.parent()?.join(FFMPEG_DIR))
}

/// The hook DLL the capture pipeline passes as `-hookDllPath`, when it is not
/// beside the chosen executable.
///
/// This is how "is that really HLAE?" gets answered, and it is a better question
/// than it first looks. The obvious alternative is reading the executable's
/// embedded version metadata — HLAE's says `OriginalFilename: hlae.exe` — but
/// that only confirms what the *file* calls itself, and it needs `unsafe` FFI
/// into `version.dll` to read. This checks what the pipeline actually consumes:
/// `PatcherConfig::build_hlae_process` derives exactly this path and hands it to
/// HLAE, so if the DLL is not here, a capture cannot work no matter what the exe
/// is named.
///
/// It also catches a case metadata cannot: a genuine `HLAE.exe` whose
/// `AfxHookGoldSrc.dll` has been deleted or quarantined by antivirus. Metadata
/// would approve that install and the capture would fail anyway.
///
/// The one thing it misses is a renamed `hlae.exe` sitting in a real HLAE folder
/// — which is harmless, because everything the pipeline needs is still there.
///
/// Advisory. `None` means "nothing to say", and a result is reported rather than
/// enforced.
pub fn missing_hook_dll(hlae_exe: &Path) -> Option<PathBuf> {
    // Derived the same way `PatcherConfig::build_hlae_process` derives it, so
    // this cannot pass while the launch argument points somewhere else.
    let dll = hlae_exe.parent()?.join(HOOK_DLL);
    if dll.is_file() {
        None
    } else {
        Some(dll)
    }
}

pub fn detect(hlae_exe: &Path) -> HlaeFfmpeg {
    let Some(folder) = ffmpeg_dir(hlae_exe) else {
        return HlaeFfmpeg::NoInstall;
    };

    // A bundled binary wins: HLAE finds it without consulting the ini at all,
    // so reporting the ini here would describe something with no effect.
    let bundled = folder.join(BUNDLED_RELATIVE);
    if bundled.is_file() {
        return HlaeFfmpeg::Bundled { path: bundled };
    }

    let ini = folder.join(INI_NAME);
    if let Ok(body) = std::fs::read_to_string(&ini)
        && let Some(target) = parse_ini_path(&body) {
            let target_exists = target.is_file();
            return HlaeFfmpeg::Linked {
                ini,
                target,
                target_exists,
                ours: authored_by_us(&body),
            };
        }

    HlaeFfmpeg::Missing { folder }
}

/// Whether this `ffmpeg.ini` is one `link` wrote.
///
/// A header match is the whole test, and it is deliberately not proof: somebody
/// can edit the path under our header and we would then treat their edit as
/// ours to replace. That is why replacing is never silent — the caller reports
/// what the old target was. The alternative, treating every existing file as
/// untouchable, makes the first link permanent and leaves no way to re-point
/// HLAE after changing the app's FFmpeg.
fn authored_by_us(body: &str) -> bool {
    body.contains(AUTHORED_MARKER)
}

/// Points HLAE at `ffmpeg_exe` by writing `ffmpeg.ini`.
///
/// Refuses rather than overwrites whenever HLAE already has an answer, so this
/// can never take a working setup away from whatever else uses this install.
pub fn link(hlae_exe: &Path, ffmpeg_exe: &Path) -> Result<PathBuf, LinkError> {
    check_is_ffmpeg(ffmpeg_exe)?;
    write_link(hlae_exe, ffmpeg_exe)
}

/// Existing is not the same as being FFmpeg. `ffplay.exe` and `ffprobe.exe` sit
/// in the same folder and are one misclick away in a file picker; either would
/// give HLAE a program that cannot record.
fn check_is_ffmpeg(ffmpeg_exe: &Path) -> Result<(), LinkError> {
    if !ffmpeg_exe.is_file() {
        return Err(LinkError::NoSuchFfmpeg {
            path: ffmpeg_exe.to_path_buf(),
        });
    }
    verify_is_ffmpeg(ffmpeg_exe).map(|_| ()).map_err(|why| LinkError::NotFfmpeg { why })
}

/// The ini half, split out from the check above so the rules about *which files
/// may be written* can be tested without needing a working FFmpeg on the
/// machine running the tests. Everything public goes through `link`.
fn write_link(hlae_exe: &Path, ffmpeg_exe: &Path) -> Result<PathBuf, LinkError> {
    match detect(hlae_exe) {
        HlaeFfmpeg::NoInstall => return Err(LinkError::NoInstall),
        HlaeFfmpeg::Bundled { path } => return Err(LinkError::AlreadyBundled { path }),
        // Somebody else wrote it: left alone. Ours: replaced, since the
        // alternative is that the first link is permanent and the folder
        // usually cannot be edited by hand without administrator rights.
        HlaeFfmpeg::Linked { ini, target, ours, .. } if !ours => {
            return Err(LinkError::AlreadyLinked { ini, target })
        }
        HlaeFfmpeg::Linked { .. } | HlaeFfmpeg::Missing { .. } => {}
    }

    let folder = ffmpeg_dir(hlae_exe).ok_or(LinkError::NoInstall)?;
    let ini = folder.join(INI_NAME);
    let body = ini_body(ffmpeg_exe);

    if let Err(e) = std::fs::create_dir_all(&folder).and_then(|_| std::fs::write(&ini, &body)) {
        // HLAE can live anywhere — it ships as a zip as well as an installer —
        // so a protected location is one real possibility among several rather
        // than a known majority. Reported as its own case either way, so the
        // caller can offer the elevated route instead of showing somebody an
        // "Access is denied. (os error 5)" and leaving them there.
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            return Err(LinkError::NeedsElevation { ini });
        }
        return Err(LinkError::Io(e));
    }
    Ok(ini)
}

/// The readme's own format. The comment is for whoever opens this months from
/// now wondering where it came from.
fn ini_body(ffmpeg_exe: &Path) -> String {
    format!(
        "; Written by dod-tools so HLAE's mirv_movie_ffmpeg can find FFmpeg.\n\
         ; Delete this file to undo it. dod-tools may rewrite a file carrying\n\
         ; this header, and will never touch one that does not.\n\
         [Ffmpeg]\n\
         Path={}\n",
        ffmpeg_exe.display()
    )
}

/// The same write, through a UAC prompt.
///
/// Only for the `NeedsElevation` case. Every refusal `link` makes is re-checked
/// here first, so elevation can never be used to get around the never-overwrite
/// rule — it buys permission to write, not permission to clobber.
///
/// The paths are baked into a script file rather than passed as arguments.
/// `Start-Process -ArgumentList` re-quotes what it is given, and these are
/// user-supplied paths that routinely contain spaces and can contain quotes, so
/// argument-passing is where this would break or worse. A script with no
/// arguments has nothing to re-quote.
#[cfg(windows)]
pub fn link_elevated(hlae_exe: &Path, ffmpeg_exe: &Path) -> Result<PathBuf, LinkError> {
    use std::os::windows::process::CommandExt;

    check_is_ffmpeg(ffmpeg_exe)?;
    match detect(hlae_exe) {
        HlaeFfmpeg::NoInstall => return Err(LinkError::NoInstall),
        HlaeFfmpeg::Bundled { path } => return Err(LinkError::AlreadyBundled { path }),
        // Somebody else wrote it: left alone. Ours: replaced, since the
        // alternative is that the first link is permanent and the folder
        // usually cannot be edited by hand without administrator rights.
        HlaeFfmpeg::Linked { ini, target, ours, .. } if !ours => {
            return Err(LinkError::AlreadyLinked { ini, target })
        }
        HlaeFfmpeg::Linked { .. } | HlaeFfmpeg::Missing { .. } => {}
    }

    let ini = ffmpeg_dir(hlae_exe).ok_or(LinkError::NoInstall)?.join(INI_NAME);

    let scratch = std::env::temp_dir().join("dodtools_hlae_ffmpeg");
    std::fs::create_dir_all(&scratch).map_err(LinkError::Io)?;
    let staged = scratch.join(INI_NAME);
    std::fs::write(&staged, ini_body(ffmpeg_exe)).map_err(LinkError::Io)?;

    let script = scratch.join("link.ps1");
    std::fs::write(
        &script,
        format!(
            "$ErrorActionPreference = 'Stop'\n\
             New-Item -ItemType Directory -Force -Path {} | Out-Null\n\
             Copy-Item -LiteralPath {} -Destination {} -Force\n",
            ps_literal(ini.parent().unwrap_or(&scratch)),
            ps_literal(&staged),
            ps_literal(&ini),
        ),
    )
    .map_err(LinkError::Io)?;

    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!(
                "Start-Process -FilePath 'powershell' -Verb RunAs -Wait -WindowStyle Hidden \
                 -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',{})",
                ps_literal(&script)
            ),
        ])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status()
        .map_err(LinkError::Io)?;

    // Both checks are load-bearing, and measured rather than assumed:
    //
    //   - A declined UAC prompt makes `Start-Process` itself fail to launch,
    //     and that does surface — the outer PowerShell exits 1.
    //   - A failure *inside* the elevated script does NOT. `-Wait` waits for
    //     the process without propagating its exit code, so a script that
    //     throws still leaves the outer PowerShell exiting 0.
    //
    // So the exit code alone would report success for a copy that failed. The
    // file landing is the only thing worth trusting, and it is what decides.
    if !status.success() || !ini.is_file() {
        return Err(LinkError::ElevationRefused);
    }
    Ok(ini)
}

#[cfg(not(windows))]
pub fn link_elevated(_hlae_exe: &Path, _ffmpeg_exe: &Path) -> Result<PathBuf, LinkError> {
    Err(LinkError::ElevationRefused)
}

/// A path as a single-quoted PowerShell string literal. Inside single quotes
/// PowerShell expands nothing at all, so doubling the quote character is the
/// whole escape.
fn ps_literal(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

/// An absolute path for the FFmpeg the app itself would use.
///
/// Render Studio's config stores a bare `"ffmpeg"` to mean "whatever is on
/// PATH", which HLAE cannot act on — its ini needs a real path — so that case
/// is resolved here rather than written through as-is.
pub fn resolve_absolute(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return None;
    }

    let direct = Path::new(trimmed);
    if direct.is_file() {
        return Some(
            std::fs::canonicalize(direct)
                .ok()
                .map(|p| strip_extended_prefix(&p))
                .unwrap_or_else(|| direct.to_path_buf()),
        );
    }
    if direct.components().count() > 1 {
        // A path was given and it is not there. Searching PATH for its file
        // name would silently substitute a different binary.
        return None;
    }

    search_path(trimmed)
}

fn search_path(name: &str) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        vec![format!("{}.exe", name), name.to_string()]
    } else {
        vec![name.to_string()]
    };

    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for candidate in &candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(
                    std::fs::canonicalize(&full)
                        .ok()
                        .map(|p| strip_extended_prefix(&p))
                        .unwrap_or(full),
                );
            }
        }
    }
    None
}

/// Whether an executable is actually FFmpeg, by asking it.
///
/// "It exists" is not the same question. `ffplay.exe` and `ffprobe.exe` ship in
/// the same folder as `ffmpeg.exe` and are one misclick apart in a file picker;
/// both exist, neither will do. Writing one into HLAE's ini produces a capture
/// that spawns the wrong program and records nothing, which is the same silent
/// failure this whole module exists to prevent.
///
/// The name is not enough either — a renamed `ffmpeg.exe` is still FFmpeg, and
/// something else named `ffmpeg.exe` is still not. So it is asked: every FFmpeg
/// tool prints `<toolname> version ...` as its first line, which distinguishes
/// them from each other as well as from anything that is not FFmpeg at all.
///
/// Returns the version banner on success, so a caller can show what it found.
pub fn verify_is_ffmpeg(exe: &Path) -> Result<String, String> {
    if !exe.is_file() {
        return Err(crate::messages::not_a_file(exe.display()));
    }

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| crate::messages::could_not_run(exe.display(), e))?;

    // Bounded rather than a blocking wait: this runs on a user-chosen
    // executable, and one that never exits must not hang the caller.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            Ok(None) => {
                let _ = child.kill();
                return Err(crate::messages::did_not_respond_to_version_flag(exe.display()));
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let mut banner = String::new();
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        banner = String::from_utf8_lossy(&buf).lines().next().unwrap_or("").trim().to_string();
    }

    if banner.to_ascii_lowercase().starts_with("ffmpeg version") {
        Ok(banner)
    } else if banner.is_empty() {
        Err(crate::messages::did_not_identify_as_ffmpeg(exe.display()))
    } else {
        // Naming what it actually is beats "invalid": the usual mistake is
        // picking a sibling tool, and saying which one points straight at the
        // fix.
        Err(crate::messages::reports_itself_as(exe.display(), &banner))
    }
}

/// Whether two paths name the same executable.
///
/// Exists because "HLAE is pointed somewhere" and "HLAE is pointed at the same
/// FFmpeg Render Studio uses" are different questions, and only the second one
/// keeps both halves of the pipeline encoding with the same build. That was the
/// stated reason for writing an ini instead of copying the binary, so it is
/// worth actually checking rather than assuming it stays true.
///
/// Compared case-insensitively: Windows paths are, and a link written from a
/// differently-cased spelling of the same file is not a disagreement.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let normal = |p: &Path| {
        strip_extended_prefix(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
            .to_string_lossy()
            .to_lowercase()
    };
    normal(a) == normal(b)
}

/// Drops Windows' `\\?\` extended-length prefix.
///
/// `std::fs::canonicalize` always adds it, and the result is a path that is
/// correct, ugly, and not universally accepted — it turns off the path
/// normalisation a lot of software assumes, so the programs that choke on it do
/// so at the point of use rather than when the path is stored. This value is
/// handed to HLAE to spawn a process with, and HLAE's own readme documents a
/// plain `C:\...\ffmpeg.exe`, so there is nothing to gain by keeping it and a
/// silent launch failure to lose.
///
/// `\\?\UNC\server\share` is the network form and maps back to `\\server\share`.
fn strip_extended_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix("UNC\\") {
            Some(unc) => PathBuf::from(format!(r"\\{}", unc)),
            None => PathBuf::from(rest),
        },
        None => path.to_path_buf(),
    }
}

/// The `Path=` value from an `ffmpeg.ini`, if there is one.
///
/// Deliberately lenient about the section header and whitespace and strict
/// about nothing: this is reading somebody else's file to report on it, so a
/// shape we do not recognise should read as "no answer" rather than an error.
fn parse_ini_path(body: &str) -> Option<PathBuf> {
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("path") {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dod_hlae_ffmpeg_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// An HLAE install: the exe, and the `ffmpeg` folder its readme lives in.
    fn install(name: &str) -> PathBuf {
        let root = scratch(name);
        std::fs::create_dir_all(root.join(FFMPEG_DIR)).expect("ffmpeg dir");
        let exe = root.join("HLAE.exe");
        std::fs::write(&exe, b"").expect("exe");
        exe
    }

    fn a_real_ffmpeg(dir: &Path) -> PathBuf {
        let exe = dir.join("ffmpeg.exe");
        std::fs::write(&exe, b"").expect("ffmpeg");
        exe
    }

    #[test]
    fn the_working_install_shape_is_reported_as_missing() {
        // What is actually on this machine: an ffmpeg folder holding nothing
        // but the readme. It looks set up and is not.
        let hlae = install("missing");
        std::fs::write(
            hlae.parent().unwrap().join(FFMPEG_DIR).join("readme.advancedfx.txt"),
            b"install ffmpeg here",
        )
        .expect("readme");

        let state = detect(&hlae);
        assert!(matches!(state, HlaeFfmpeg::Missing { .. }), "{:?}", state);
        assert!(!state.is_usable());
        assert!(state.can_link(), "this is exactly the case worth offering to fix");
    }

    #[test]
    fn a_bundled_binary_needs_nothing_and_is_never_offered_an_ini() {
        let hlae = install("bundled");
        let bin = hlae.parent().unwrap().join(FFMPEG_DIR).join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join("ffmpeg.exe"), b"").expect("exe");

        let state = detect(&hlae);
        assert!(matches!(state, HlaeFfmpeg::Bundled { .. }), "{:?}", state);
        assert!(state.is_usable());
        assert!(!state.can_link());
    }

    #[test]
    fn a_bundled_binary_wins_over_an_ini() {
        // HLAE finds the binary without consulting the ini, so reporting the
        // ini would describe something that has no effect on anything.
        let hlae = install("both");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        let bin = folder.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join("ffmpeg.exe"), b"").expect("exe");
        std::fs::write(folder.join(INI_NAME), "[Ffmpeg]\nPath=C:\\elsewhere\\ffmpeg.exe\n")
            .expect("ini");

        assert!(matches!(detect(&hlae), HlaeFfmpeg::Bundled { .. }));
    }

    #[test]
    fn a_stale_ini_reads_as_linked_but_not_usable() {
        // The case a "configured / not configured" boolean would hide: HLAE has
        // an answer, and the answer is wrong.
        let hlae = install("stale");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        std::fs::write(folder.join(INI_NAME), "[Ffmpeg]\nPath=C:\\gone\\ffmpeg.exe\n")
            .expect("ini");

        match detect(&hlae) {
            HlaeFfmpeg::Linked { target_exists, .. } => assert!(!target_exists),
            other => panic!("{:?}", other),
        }
        assert!(!detect(&hlae).is_usable());
        assert!(
            !detect(&hlae).can_link(),
            "an existing ini is reported, never replaced"
        );
    }

    #[test]
    fn linking_writes_an_ini_hlae_can_read_back() {
        let hlae = install("link");
        let ffmpeg = a_real_ffmpeg(hlae.parent().unwrap());

        let ini = write_link(&hlae, &ffmpeg).expect("link");
        assert!(ini.is_file());

        match detect(&hlae) {
            HlaeFfmpeg::Linked {
                target,
                target_exists,
                ..
            } => {
                assert!(target_exists);
                assert_eq!(target, ffmpeg);
            }
            other => panic!("{:?}", other),
        }
        assert!(detect(&hlae).is_usable());
    }

    #[test]
    fn an_existing_ini_is_refused_not_overwritten() {
        // HLAE is shared with Source work. Repointing it silently would break
        // somebody else's workflow to fix ours.
        let hlae = install("refuse");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        let original = "[Ffmpeg]\nPath=D:\\someone-elses\\ffmpeg.exe\n";
        std::fs::write(folder.join(INI_NAME), original).expect("ini");
        let ffmpeg = a_real_ffmpeg(hlae.parent().unwrap());

        let err = write_link(&hlae, &ffmpeg).expect_err("must refuse");
        assert!(matches!(err, LinkError::AlreadyLinked { .. }), "{:?}", err);
        assert_eq!(
            std::fs::read_to_string(folder.join(INI_NAME)).unwrap(),
            original,
            "the existing file was modified"
        );
    }

    #[test]
    fn linking_refuses_an_ffmpeg_that_is_not_there() {
        let hlae = install("no_ffmpeg");
        let err = link(&hlae, Path::new("C:/nowhere/ffmpeg.exe")).expect_err("must refuse");
        assert!(matches!(err, LinkError::NoSuchFfmpeg { .. }), "{:?}", err);
    }

    #[test]
    fn a_path_whose_folder_exists_but_whose_exe_does_not_is_no_install() {
        // `hlae_path` names HLAE.exe, not the folder — the same convention
        // `game_path` follows, and the same one-level-too-high trap. Checking
        // only the parent would accept a typo'd exe name, or the folder typed
        // in place of the exe, and then offer to write an ini into somewhere
        // that is not an HLAE install.
        let root = scratch("folder_given");
        assert_eq!(detect(&root.join("HLAE.exe")), HlaeFfmpeg::NoInstall);
        assert_eq!(detect(&root), HlaeFfmpeg::NoInstall, "the folder itself");
        assert!(!detect(&root).can_link());
    }

    #[test]
    fn an_ini_we_cannot_make_sense_of_reads_as_no_answer() {
        for body in ["", "; just a comment\n", "[Ffmpeg]\n", "nonsense\n", "[Ffmpeg]\nPath=\n"] {
            assert_eq!(parse_ini_path(body), None, "parsed {:?}", body);
        }
    }

    #[test]
    fn the_ini_format_is_read_the_way_the_readme_writes_it() {
        assert_eq!(
            parse_ini_path("[Ffmpeg]\nPath=C:\\Users\\x\\ffmpeg\\bin\\ffmpeg.exe\n"),
            Some(PathBuf::from("C:\\Users\\x\\ffmpeg\\bin\\ffmpeg.exe"))
        );
        // Tolerating what a human might reasonably have typed instead.
        assert_eq!(
            parse_ini_path("[Ffmpeg]\n  path = \"C:\\a\\ffmpeg.exe\"  \n"),
            Some(PathBuf::from("C:\\a\\ffmpeg.exe"))
        );
    }

    #[test]
    fn a_configured_path_that_exists_resolves_to_itself() {
        let dir = scratch("resolve");
        let ffmpeg = a_real_ffmpeg(&dir);
        let got = resolve_absolute(&ffmpeg.to_string_lossy()).expect("resolve");
        assert!(got.is_file());
    }

    #[test]
    fn a_configured_path_that_is_wrong_is_not_quietly_swapped_for_another() {
        // Falling back to a PATH search here would hand HLAE a different binary
        // than the one Render Studio was told to use.
        assert_eq!(resolve_absolute("C:/nowhere/at/all/ffmpeg.exe"), None);
        assert_eq!(resolve_absolute("   "), None);
    }

    #[test]
    fn a_path_is_quoted_so_powershell_expands_nothing_in_it() {
        // These end up inside a script this code generates, and they are
        // user-supplied paths. Single quotes stop PowerShell expanding `$`,
        // backticks or anything else, and doubling is the only escape needed.
        assert_eq!(ps_literal(Path::new(r"C:\Program Files (x86)\HLAE")), r"'C:\Program Files (x86)\HLAE'");
        assert_eq!(ps_literal(Path::new(r"C:\it's\$env:PATH")), r"'C:\it''s\$env:PATH'");
    }

    #[test]
    fn elevation_still_refuses_everything_a_plain_link_refuses() {
        // Elevation buys permission to write, not permission to clobber. If it
        // skipped these checks, the never-overwrite rule would have a hole in
        // it that only opens on the machines where the button is most useful.
        let hlae = install("elevated_refuse");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        let original = "[Ffmpeg]\nPath=D:\\someone-elses\\ffmpeg.exe\n";
        std::fs::write(folder.join(INI_NAME), original).expect("ini");

        // A real FFmpeg, so the refusal under test is the ini rule and not the
        // is-it-FFmpeg check that now runs before it. Skipped where none is
        // installed rather than asserting about the environment.
        let Some(ffmpeg) = resolve_absolute("ffmpeg") else {
            eprintln!("no ffmpeg on PATH; skipping");
            return;
        };

        let err = link_elevated(&hlae, &ffmpeg).expect_err("must refuse");
        assert!(matches!(err, LinkError::AlreadyLinked { .. }), "{:?}", err);
        assert_eq!(
            std::fs::read_to_string(folder.join(INI_NAME)).unwrap(),
            original,
            "no UAC prompt should have been raised, and nothing written"
        );
    }

    #[test]
    fn a_wrong_executable_is_refused_before_any_prompt_or_write() {
        // The ordering matters: raising a UAC prompt to install a program that
        // cannot record would be worse than not offering at all, and the ini
        // must be untouched either way.
        let hlae = install("elevated_not_ffmpeg");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        let not_ffmpeg = a_real_ffmpeg(hlae.parent().unwrap()); // a zero-byte stand-in

        for err in [
            link(&hlae, &not_ffmpeg).expect_err("plain must refuse"),
            link_elevated(&hlae, &not_ffmpeg).expect_err("elevated must refuse"),
        ] {
            assert!(matches!(err, LinkError::NotFfmpeg { .. }), "{:?}", err);
        }
        assert!(!folder.join(INI_NAME).exists(), "nothing should have been written");
    }

    #[test]
    fn elevation_refuses_an_ffmpeg_that_is_not_there_before_prompting() {
        // No point raising a UAC prompt to copy a file that does not exist.
        let hlae = install("elevated_no_ffmpeg");
        let err = link_elevated(&hlae, Path::new("C:/nowhere/ffmpeg.exe")).expect_err("refuse");
        assert!(matches!(err, LinkError::NoSuchFfmpeg { .. }), "{:?}", err);
    }

    #[test]
    fn a_canonical_path_loses_the_extended_length_prefix() {
        // `canonicalize` always adds `\\?\`. It is a correct path and not a
        // universally accepted one — it turns off the normalisation a lot of
        // software assumes — and this value gets handed to HLAE to spawn a
        // process with, so keeping it risks a silent launch failure.
        assert_eq!(
            strip_extended_prefix(Path::new(r"\\?\C:\Program Files (x86)\FFmpeg\ffmpeg.exe")),
            PathBuf::from(r"C:\Program Files (x86)\FFmpeg\ffmpeg.exe")
        );
        // The network form maps back to a plain UNC path.
        assert_eq!(
            strip_extended_prefix(Path::new(r"\\?\UNC\server\share\ffmpeg.exe")),
            PathBuf::from(r"\\server\share\ffmpeg.exe")
        );
        // Anything else is left exactly as it is.
        assert_eq!(
            strip_extended_prefix(Path::new(r"C:\plain\ffmpeg.exe")),
            PathBuf::from(r"C:\plain\ffmpeg.exe")
        );
    }

    #[test]
    fn resolving_never_hands_back_an_extended_length_path() {
        // The end-to-end version of the above: whatever `canonicalize` does,
        // what reaches the ini has to be a path HLAE will accept.
        let dir = scratch("resolve_prefix");
        let ffmpeg = a_real_ffmpeg(&dir);
        let got = resolve_absolute(&ffmpeg.to_string_lossy()).expect("resolve");
        assert!(
            !got.to_string_lossy().starts_with(r"\?\"),
            "{} still carries the prefix",
            got.display()
        );
    }

    #[test]
    fn our_own_ini_is_rewritten_rather_than_treated_as_untouchable() {
        // Without this the first link is permanent: change the app's FFmpeg and
        // HLAE stays pointed at the old one, with no way back through the UI and
        // a folder that usually needs administrator rights to edit by hand.
        let hlae = install("relink");
        let first = a_real_ffmpeg(hlae.parent().unwrap());
        write_link(&hlae, &first).expect("first link");
        assert!(detect(&hlae).can_link(), "our own file must stay correctable");

        let second_dir = hlae.parent().unwrap().join("other");
        std::fs::create_dir_all(&second_dir).expect("dir");
        let second = a_real_ffmpeg(&second_dir);
        write_link(&hlae, &second).expect("relink");

        match detect(&hlae) {
            HlaeFfmpeg::Linked { target, ours, .. } => {
                assert_eq!(target, second);
                assert!(ours);
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn an_empty_or_contentless_ini_is_replaced_rather_than_protected() {
        // The rule protects a *configuration*, not a filename. A file with no
        // `Path=` in it expresses nothing, so refusing to write would strand
        // somebody behind a file that does nothing — and there is no data in it
        // to lose. A zero-length file is the case that actually turns up, from
        // an interrupted write or a hand-created placeholder.
        for body in ["", "\n\n", "; notes to self\n", "[Ffmpeg]\n"] {
            let hlae = install("contentless");
            let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
            std::fs::write(folder.join(INI_NAME), body).expect("ini");

            assert!(
                detect(&hlae).can_link(),
                "an ini containing {:?} should not be treated as somebody's setup",
                body
            );
            let ffmpeg = a_real_ffmpeg(hlae.parent().unwrap());
            write_link(&hlae, &ffmpeg).expect("should write over a file with nothing in it");
            assert_eq!(read_target(&hlae), Some(ffmpeg));
        }
    }

    fn read_target(hlae: &Path) -> Option<PathBuf> {
        match detect(hlae) {
            HlaeFfmpeg::Linked { target, .. } => Some(target),
            _ => None,
        }
    }

    #[test]
    fn a_file_without_our_header_is_still_untouchable() {
        // The distinction the rewrite rule rests on. Somebody else's file is
        // left alone however much we would like to correct it.
        let hlae = install("not_ours");
        let folder = hlae.parent().unwrap().join(FFMPEG_DIR);
        let theirs = "[Ffmpeg]\nPath=D:\\theirs\\ffmpeg.exe\n";
        std::fs::write(folder.join(INI_NAME), theirs).expect("ini");

        match detect(&hlae) {
            HlaeFfmpeg::Linked { ours, .. } => assert!(!ours),
            other => panic!("{:?}", other),
        }
        assert!(!detect(&hlae).can_link());

        let ffmpeg = a_real_ffmpeg(hlae.parent().unwrap());
        assert!(matches!(
            write_link(&hlae, &ffmpeg).expect_err("must refuse"),
            LinkError::AlreadyLinked { .. }
        ));
        assert_eq!(std::fs::read_to_string(folder.join(INI_NAME)).unwrap(), theirs);
    }

    #[test]
    fn a_file_that_is_not_a_program_is_not_ffmpeg() {
        let dir = scratch("verify_junk");
        let fake = dir.join("ffmpeg.exe");
        std::fs::write(&fake, b"not actually a program").expect("write");
        // Named exactly right, and still not FFmpeg — which is the whole reason
        // the name is not the test.
        assert!(verify_is_ffmpeg(&fake).is_err());
        assert!(verify_is_ffmpeg(&dir.join("absent.exe")).is_err());
    }

    #[test]
    fn a_real_ffmpeg_identifies_itself() {
        // Skipped rather than failed where FFmpeg is not installed: this asserts
        // about the environment, not the code, and CI need not have one.
        let Some(ffmpeg) = resolve_absolute("ffmpeg") else {
            eprintln!("no ffmpeg on PATH; skipping");
            return;
        };
        let banner = verify_is_ffmpeg(&ffmpeg).expect("the real thing must pass");
        assert!(banner.to_lowercase().starts_with("ffmpeg version"), "{}", banner);
    }

    #[test]
    fn a_sibling_ffmpeg_tool_is_rejected_by_name_of_what_it_actually_is() {
        // The mistake that prompted this: ffplay.exe sits in the same folder as
        // ffmpeg.exe and is one click away in a picker. Both exist; only one can
        // record. Skipped where the tools are not installed.
        let Some(ffmpeg) = resolve_absolute("ffmpeg") else {
            eprintln!("no ffmpeg on PATH; skipping");
            return;
        };
        let ffplay = ffmpeg.with_file_name("ffplay.exe");
        if !ffplay.is_file() {
            eprintln!("no ffplay beside ffmpeg; skipping");
            return;
        }
        let why = verify_is_ffmpeg(&ffplay).expect_err("ffplay cannot record");
        assert!(
            why.to_lowercase().contains("ffplay"),
            "the message should name what it actually found: {}",
            why
        );
    }

    #[test]
    fn an_install_carrying_the_hook_dll_has_nothing_to_say() {
        let hlae = install("hook_present");
        std::fs::write(hlae.parent().unwrap().join(HOOK_DLL), b"").expect("dll");
        assert_eq!(missing_hook_dll(&hlae), None);
    }

    #[test]
    fn an_executable_with_no_hook_dll_beside_it_is_reported() {
        // The wrong exe picked in a file dialog, or a folder that is not an
        // HLAE install at all.
        let hlae = install("hook_absent");
        let missing = missing_hook_dll(&hlae).expect("nothing beside it");
        assert!(missing.ends_with(HOOK_DLL), "{}", missing.display());
    }

    #[test]
    fn a_real_hlae_missing_its_dll_is_still_reported() {
        // What metadata could not catch: the executable is genuine and the
        // install is broken anyway, because antivirus quarantined the DLL or
        // somebody deleted it. The capture would fail either way, so the check
        // that matters is whether the file the pipeline passes is there.
        let hlae = install("hook_quarantined");
        let dll = hlae.parent().unwrap().join(HOOK_DLL);
        std::fs::write(&dll, b"").expect("dll");
        assert_eq!(missing_hook_dll(&hlae), None);

        std::fs::remove_file(&dll).expect("quarantine it");
        assert_eq!(missing_hook_dll(&hlae), Some(dll));
    }
}

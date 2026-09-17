//! Standalone companion DLL for DoD 1.3 GoldSrc capture sessions, injected
//! into `hl.exe` alongside (not instead of) HLAE's own `AfxHookGoldSrc.dll`.
//!
//! Unlike patching HLAE's own source, this doesn't need HLAE's build
//! toolchain, and doesn't touch any function HLAE itself already hooks (see
//! `engine.rs` for exactly which functions this DLL depends on). It's built
//! as a separate workspace crate specifically so it doesn't have to live
//! inside a forked copy of HLAE's own repository. How it gets a foothold in
//! `client.dll` is genuinely non-obvious -- patching that DLL's export table
//! does nothing on a "secured" build, because the engine resolves the whole
//! client interface through a single `F` export instead. See `engine.rs`'s
//! module docs and `docs/goldsrc_client_dll_internals.md`.
//!
//! Implements two fixes and one control surface, each independent of the
//! others and each safe to inject without them:
//! - `sound_fix`: force full-volume weapon-fire audio while spectating.
//! - `anim_fix`: drive the first-person viewmodel's animations -- shoot,
//!   reload, draw, idle -- while spectating a player in-eye, which the engine
//!   otherwise leaves static. Full design write-up in
//!   `docs/goldsrc_hltv_animation_fix.md`.
//! - `deathmsg`: the `dodstudio_deathmsg` command -- raise the four-line cap on
//!   the kill feed, move it, hide frags, or inject one. HLAE's own
//!   `mirv_deathmsg` covers only `cstrike` and `tfc`, so none of it works for
//!   DoD. Full design write-up in `docs/goldsrc_death_notices.md`.
//!
//! See each module's docs for the full R&D reasoning.
//!
//! Both are `dodstudio_*` **cvars**, so they behave like any other engine
//! setting: `dodstudio_hltv_animation_fix 1` from the console, `+dodstudio_hltv_animation_fix 1`
//! on the launch line, or a line in any `.cfg` the user execs. `commands.rs`
//! copies them into the runtime flags once per frame, and `dodstudio_status`
//! reports what each fix is actually doing rather than only what it is set to.
//!
//! The `GOLDSRC_HOOKS_FORCE_WEAPON_VOLUME` / `GOLDSRC_HOOKS_ANIM_FIX`
//! environment variables still set the starting value and are still the only
//! route if cvar registration ever falls back to plain commands, but the launch
//! line is now the better one: it is visible in the command that started the
//! session.

mod anim_fix;
mod commands;
mod crash;
mod deathmsg;
mod detour;
mod debug;
mod engine;
mod names;
mod patch;
mod pe;
mod scan;
mod sound_fix;

use std::sync::atomic::Ordering;
use windows_sys::Win32::Foundation::{BOOL, HINSTANCE, TRUE};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::{CreateThread, Sleep};

/// Reads a `GOLDSRC_HOOKS_*` flag, falling back to `default` when unset.
///
/// An explicit "0" always wins, so a fix that defaults on can still be turned
/// off without a rebuild.
fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => value.trim() == "1",
        Err(_) => default,
    }
}

/// Reads a `GOLDSRC_HOOKS_*` iteration number, falling back to `default`.
///
/// Out of range is clamped rather than refused: `GOLDSRC_HOOKS_ANIM_FIX=99`
/// is a usable way to say "newest" without having to know what the newest is.
/// Unparseable reads as off, which is the safe way to land.
fn env_level(name: &str, default: i32) -> i32 {
    match std::env::var(name) {
        Ok(value) => value.trim().parse::<i32>().unwrap_or(0),
        Err(_) => default,
    }
    .clamp(anim_fix::LEVEL_OFF, anim_fix::LEVEL_MAX)
}

/// The animation fix starts **off**, like the sound fix: a capture pipeline
/// should not silently alter viewmodel animations for anyone who happens to
/// have the DLL loaded. Pick an iteration per session with
/// `dodstudio_hltv_animation_fix <0-5>`, or set `GOLDSRC_HOOKS_ANIM_FIX` to
/// have it start on one -- see `anim_fix::LEVEL` for what each is.
///
/// It was on through live testing, because a session that begins by
/// forgetting to type the command produces a log with nothing in it and looks
/// like a broken hook.
const ANIM_FIX_DEFAULT: i32 = anim_fix::LEVEL_OFF;

unsafe extern "system" fn worker_thread(_lp_param: *mut std::ffi::c_void) -> u32 {
    // The sound fix stays default-off: what it currently does (extending how
    // far gunshots carry) is not the thing that turned out to be wanted, and
    // having it on would colour an animation test for no reason. The firing
    // animation does not depend on it -- anim_fix::on_weapon_fired is called
    // from the EV_PlaySound hook regardless of this flag.
    sound_fix::ENABLED.store(env_flag("GOLDSRC_HOOKS_FORCE_WEAPON_VOLUME", false), Ordering::Relaxed);
    anim_fix::LEVEL.store(env_level("GOLDSRC_HOOKS_ANIM_FIX", ANIM_FIX_DEFAULT), Ordering::Relaxed);

    unsafe { debug::new_session_separator() };
    unsafe { debug::report("goldsrc-hooks worker thread started") };

    // Both fixes default to OFF, so a session where the hooks all install
    // correctly but nothing visibly changes is the expected outcome of simply
    // not having turned them on. Log the starting state so that case is
    // obvious from the log rather than mistaken for a broken hook.
    unsafe {
        debug::report(&format!(
            "goldsrc-hooks: starting state -- gunshots fix: {}, animation fix: {} ({}) (env vars set the default; dodstudio_hltv_gunshots_fix / dodstudio_hltv_animation_fix toggle live)",
            if sound_fix::ENABLED.load(Ordering::Relaxed) { "ON" } else { "off" },
            anim_fix::level(),
            anim_fix::level_description(anim_fix::level()),
        ))
    };

    // Before anything else hooks anything: GoldSrc swallows its own
    // unhandled exceptions and exits without a dump or an event-log entry,
    // so without this a crash in an engine-thread callback is untraceable.
    crash::install();

    // Registered before install() so there's no window in which Initialize
    // could fire before the callback exists.
    engine::set_on_engine_ready(install_fixes);

    // Hooks hw.dll's LoadLibraryA and GetProcAddress imports, so we see
    // client.dll load and can substitute our own entry points as the engine
    // resolves it -- see engine.rs's module docs for the full mechanism and
    // why patching client.dll's export table does nothing.
    engine::install();

    // Nothing left to do but report if the fixes never activated. pEngfuncs
    // arrives when the engine calls client.dll's Initialize during normal
    // startup, and `install_fixes` runs from there, on the engine's thread.
    let mut waited = 0u32;
    while engine::engfuncs().is_none() {
        if waited >= 30_000 {
            unsafe { debug::report("goldsrc-hooks: timed out waiting for client.dll's Initialize to run; fixes not installed this session") };
            return 0;
        }
        unsafe { Sleep(50) };
        waited += 50;
    }

    0
}

/// Runs on the engine's own thread, once `client.dll`'s `Initialize` has
/// returned and `pEngfuncs` is live -- see `engine::set_on_engine_ready` for
/// why this must not run from the worker thread.
fn install_fixes() {
    unsafe { debug::report("goldsrc-hooks: engfuncs captured, installing fixes") };
    sound_fix::install();

    // anim_fix additionally needs engine_studio, captured when the engine
    // calls HUD_GetStudioModelInterface; install() itself only registers the
    // per-frame callback, which re-checks that availability on every call, so
    // it's safe to install even if that capture hasn't landed yet.
    anim_fix::install();

    // In-game dodstudio_hltv_gunshots_fix / dodstudio_hltv_animation_fix
    // console commands -- toggle the same ENABLED flags the env vars above
    // set as the initial default, so either mechanism works.
    commands::install();
}

/// # Safety
///
/// Only ever called by the Windows loader itself, per the standard `DllMain`
/// contract -- never call this directly.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(_hinst: HINSTANCE, reason: u32, _reserved: *mut std::ffi::c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // Do as little as possible directly in DllMain (loader-lock rules --
        // no LoadLibrary, no waiting, ideally no allocation). Hand off to a
        // worker thread immediately instead.
        unsafe {
            CreateThread(std::ptr::null(), 0, Some(worker_thread), std::ptr::null(), 0, std::ptr::null_mut());
        }
    }
    TRUE
}

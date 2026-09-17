//! The `dodtools_*` console surface: four cvars and one command.
//!
//! ## Why cvars rather than commands
//!
//! These were four `pfnAddCommand` commands, and the difference is not
//! cosmetic. A command is a function the engine calls and forgets; the state
//! lives in this DLL's own atomics, where the console cannot see it. So
//! `dodtools_hltv_animation_fix` printed nothing in the type-ahead, could not
//! be queried with a bare name the way `sensitivity` can, and — the part that
//! actually mattered — could not be set from a config file or the launch line.
//! That last gap is the entire reason `GOLDSRC_HOOKS_ANIM_FIX` and
//! `ANIM_FIX_DEFAULT` existed.
//!
//! A cvar is a named box the *engine* owns. It shows up in the type-ahead with
//! its value, answers `dodtools_hltv_animation_fix` on its own, takes
//! `+dodtools_hltv_animation_fix 1` on the launch line, and can be set from any
//! `.cfg` the user execs. `poll()` copies the values into the same atomics the
//! rest of the crate already reads, once per frame, so nothing downstream
//! changed.
//!
//! ## Not archived, deliberately
//!
//! `FCVAR_ARCHIVE` would make the engine write these into the user's
//! `config.cfg` when it quits. It is not set. The pipeline's standing rule is
//! that the game's own `.cfg` files belong to the user and are detected and
//! warned about, never written (`native/src/patch/cfg_scan.rs`, and the rule in
//! `CLAUDE.md`), and a capture-time DLL quietly adding lines to `config.cfg`
//! is exactly that. Setting from a `.cfg` or the launch line still works
//! without archiving, which is the part that retires the environment
//! variables. Flip `CVAR_FLAGS` to `FCVAR_ARCHIVE` if that trade is ever
//! wanted the other way round.
//!
//! ## The fallback
//!
//! If `pfnRegisterVariable` does not hand back a cvar whose own `name` matches
//! what was asked for, the layout assumption is wrong and every subsequent read
//! would be garbage. That case registers the old commands instead and says so
//! loudly, so a bad assumption costs the type-ahead rather than the session.
//!
//! Registration must happen after `engine::engfuncs()` is captured (i.e. after
//! `client.dll`'s real `Initialize` has run), since both `pfnRegisterVariable`
//! and `pfnAddCommand` live on that same table.

use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};

use crate::engine::{self, CvarSPartial};
use crate::names::console_name;
use crate::{anim_fix, sound_fix};

const GUNSHOTS_FIX_NAME: &str = console_name!("hltv_gunshots_fix");
const ANIMATION_FIX_NAME: &str = console_name!("hltv_animation_fix");
const ATTENUATION_NAME: &str = console_name!("hltv_gunshot_attenuation");
// Not "..._weapon_switch": it fires on stance changes too (p_mg42pr,
// p_mg42sr), and those are the reason it exists.
const HELD_MODELS_NAME: &str = console_name!("log_weapon_model");
const STATUS_NAME: &str = console_name!("status");

/// `FCVAR_ARCHIVE` is 1. Deliberately not set — see the module docs.
const CVAR_FLAGS: i32 = 0;

/// The cvars the engine handed back, read once per frame by `poll`.
static CVAR_GUNSHOTS: AtomicPtr<CvarSPartial> = AtomicPtr::new(std::ptr::null_mut());
static CVAR_ANIMATION: AtomicPtr<CvarSPartial> = AtomicPtr::new(std::ptr::null_mut());
static CVAR_ATTENUATION: AtomicPtr<CvarSPartial> = AtomicPtr::new(std::ptr::null_mut());
static CVAR_HELD_MODELS: AtomicPtr<CvarSPartial> = AtomicPtr::new(std::ptr::null_mut());

/// Set when registration succeeded, so `poll` does nothing at all on the
/// command fallback path rather than reading null pointers every frame.
static CVARS_LIVE: AtomicBool = AtomicBool::new(false);

pub(crate) fn console_print(text: &str) {
    let Some(engfuncs) = engine::engfuncs() else { return };
    let Ok(c_text) = CString::new(text) else { return };
    unsafe { (engfuncs.pfn_console_print)(c_text.as_ptr()) };
}

/// Registers one cvar and checks the engine handed back what was asked for.
///
/// The name check is the whole point: it costs one `strcmp` at startup and it
/// is the only evidence available at runtime that `CvarSPartial`'s layout and
/// the engine's agree. A wrong layout would otherwise show up as a toggle that
/// silently does nothing.
fn register(name: &str, default: &str) -> Option<*mut CvarSPartial> {
    let engfuncs = engine::engfuncs()?;
    let c_name = CString::new(name).ok()?;
    let c_default = CString::new(default).ok()?;
    let cvar = unsafe {
        (engfuncs.pfn_register_variable)(c_name.as_ptr(), c_default.as_ptr(), CVAR_FLAGS)
    };
    // The engine keeps both strings for the life of the session.
    std::mem::forget(c_name);
    std::mem::forget(c_default);

    if cvar.is_null() {
        unsafe { crate::debug::report(&format!("commands: pfnRegisterVariable returned null for {name}")) };
        return None;
    }
    match unsafe { (*cvar).name_str() } {
        Some(got) if got == name => Some(cvar),
        other => {
            unsafe {
                crate::debug::report(&format!(
                    "commands: registered {name} but the cvar came back named {:?} -- cvar_s layout is wrong, falling back to commands",
                    other.as_deref()
                ))
            };
            None
        }
    }
}

/// Reads one cvar into a bool flag, reporting only when it changes.
///
/// This runs every frame, so an unconditional log line would flood the file and
/// slow a capture — the same reason `anim_fix`'s stage trace is
/// change-triggered.
fn poll_flag(name: &str, cvar: &AtomicPtr<CvarSPartial>, flag: &AtomicBool) {
    let ptr = cvar.load(Ordering::Relaxed);
    if ptr.is_null() {
        return;
    }
    let wanted = unsafe { (*ptr).value } != 0.0;
    if flag.swap(wanted, Ordering::Relaxed) != wanted {
        let state = if wanted { "1 (on)" } else { "0 (off)" };
        unsafe { crate::debug::report(&format!("commands: {name} = {state}")) };
    }
}

/// Like `poll_flag`, but the animation fix carries an iteration number rather
/// than a flag -- see `anim_fix::LEVEL`. Out-of-range values are clamped
/// rather than refused, so `dodtools_hltv_animation_fix 99` is a usable way to
/// ask for the newest behaviour without remembering what the newest is.
fn poll_level(name: &str, cvar: &AtomicPtr<CvarSPartial>, level: &AtomicI32) {
    let ptr = cvar.load(Ordering::Relaxed);
    if ptr.is_null() {
        return;
    }
    let raw = unsafe { (*ptr).value };
    // A cvar is a float; anything unparseable reads as 0, which is "off" and
    // is the safe way to land.
    let wanted = if raw.is_finite() { raw as i32 } else { 0 };
    let wanted = wanted.clamp(anim_fix::LEVEL_OFF, anim_fix::LEVEL_MAX);
    if level.swap(wanted, Ordering::Relaxed) != wanted {
        unsafe {
            crate::debug::report(&format!(
                "commands: {name} = {wanted} ({})",
                anim_fix::level_description(wanted)
            ))
        };
    }
}

/// The last attenuation value rejected, so a bad setting is reported once
/// rather than sixty times a second.
static REJECTED_ATTENUATION: AtomicU32 = AtomicU32::new(0);

fn poll_attenuation() {
    let ptr = CVAR_ATTENUATION.load(Ordering::Relaxed);
    if ptr.is_null() {
        return;
    }
    let wanted = unsafe { (*ptr).value };
    if wanted == sound_fix::carry_attenuation() {
        return;
    }
    match sound_fix::set_carry_attenuation(wanted) {
        Ok(()) => {
            REJECTED_ATTENUATION.store(0, Ordering::Relaxed);
            unsafe { crate::debug::report(&format!("commands: {ATTENUATION_NAME} = {wanted}")) };
        }
        Err(why) => {
            // Out-of-range values sit in the cvar until the user changes them,
            // so this would repeat every frame without the guard. The previous
            // good value stays in effect.
            if REJECTED_ATTENUATION.swap(wanted.to_bits(), Ordering::Relaxed) != wanted.to_bits() {
                unsafe {
                    crate::debug::report(&format!(
                        "commands: {ATTENUATION_NAME} = {wanted} rejected -- {why}; still using {}",
                        sound_fix::carry_attenuation()
                    ))
                };
            }
        }
    }
}

/// Copies the cvars into the flags the rest of the crate reads. Registered as
/// the per-frame prologue so it lands before `anim_fix::apply()` runs.
pub fn poll() {
    if !CVARS_LIVE.load(Ordering::Relaxed) {
        return;
    }
    poll_flag(GUNSHOTS_FIX_NAME, &CVAR_GUNSHOTS, &sound_fix::ENABLED);
    poll_level(ANIMATION_FIX_NAME, &CVAR_ANIMATION, &anim_fix::LEVEL);
    poll_flag(HELD_MODELS_NAME, &CVAR_HELD_MODELS, &anim_fix::LOG_HELD_MODELS);
    poll_attenuation();
    // Re-prepends our DeathMsg handler when the engine has rebuilt the user
    // message list (it frees the whole list on disconnect). A no-op otherwise.
    crate::deathmsg::poll();
}

/// Everything a session might want to know in one reply.
///
/// A cvar answers "what is this set to" on its own, which is what the four
/// toggles used to do the long way round. What it cannot answer is whether the
/// fix is *doing* anything — the flag being on says nothing about whether the
/// preconditions are being met in the current view — so that half moves here.
fn status_text() -> String {
    let on = |flag: bool| if flag { "1 (on)" } else { "0 (off)" };
    format!(
        "{ANIMATION_FIX_NAME} = {} ({})\n  {}\n{GUNSHOTS_FIX_NAME} = {}\n  {}\n{ATTENUATION_NAME} = {}\n{HELD_MODELS_NAME} = {}\n",
        anim_fix::level(),
        anim_fix::level_description(anim_fix::level()),
        anim_fix::status(),
        on(sound_fix::ENABLED.load(Ordering::Relaxed)),
        sound_fix::status(),
        sound_fix::carry_attenuation(),
        on(anim_fix::LOG_HELD_MODELS.load(Ordering::Relaxed)),
    )
}

unsafe extern "C" fn cmd_status() {
    let report = status_text();
    console_print(&report);
    // Also to the log, so it stays a complete record of what was actually
    // enabled during a capture -- console scrollback does not survive the
    // session, and "was the fix even on for that take?" is the first question
    // worth answering when a capture looks unchanged.
    unsafe { crate::debug::report(&format!("commands: {STATUS_NAME} --\n{report}")) };
}

// ── Fallback path ────────────────────────────────────────────────────────────
// Only reached when cvar registration could not be trusted. Kept whole rather
// than degraded, because the alternative is a session with no way to turn
// either fix on.

/// Reads argv(1) (if present) as "0"/"1" and stores it into `flag`, then prints
/// the resulting state.
fn handle_toggle(name: &str, flag: &AtomicBool, status: fn() -> String) {
    let Some(engfuncs) = engine::engfuncs() else { return };

    // Cmd_Argc counts the command name itself, so a bare invocation is 1 and an
    // argument makes it 2. Bare is a query, not a no-op.
    let argc = unsafe { (engfuncs.cmd_argc)() };
    let mut assigned = false;
    if argc >= 2 {
        let arg1 = unsafe { (engfuncs.cmd_argv)(1) };
        if !arg1.is_null() {
            let value = unsafe { CStr::from_ptr(arg1 as *const c_char) }.to_string_lossy();
            match value.trim() {
                "0" => {
                    flag.store(false, Ordering::Relaxed);
                    assigned = true;
                }
                "1" => {
                    flag.store(true, Ordering::Relaxed);
                    assigned = true;
                }
                other => {
                    console_print(&format!("{name}: expected 0 or 1, got \"{other}\"\n"));
                    unsafe { crate::debug::report(&format!("commands: {name} rejected argument \"{other}\"")) };
                    return;
                }
            }
        }
    }

    let state = if flag.load(Ordering::Relaxed) { "1 (on)" } else { "0 (off)" };
    if assigned {
        console_print(&format!("{name} = {state}\n"));
    } else {
        console_print(&format!("{name} = {state}\nusage: {name} <0|1>\n{}\n", status()));
    }
    unsafe {
        crate::debug::report(&format!(
            "commands: {name} = {state} ({}, argc={argc})",
            if assigned { "set" } else { "queried, unchanged" }
        ))
    };
}

unsafe extern "C" fn cmd_gunshots_fix() {
    handle_toggle(GUNSHOTS_FIX_NAME, &sound_fix::ENABLED, sound_fix::status);
}

unsafe extern "C" fn cmd_animation_fix() {
    handle_level(ANIMATION_FIX_NAME, &anim_fix::LEVEL, anim_fix::status);
}

/// `handle_toggle` for the animation fix, which takes an iteration number
/// instead of a flag. Only reached on the fallback path, when cvar
/// registration failed -- the cvar itself is what normally carries this.
fn handle_level(name: &str, level: &AtomicI32, status: fn() -> String) {
    let Some(engfuncs) = engine::engfuncs() else { return };

    let argc = unsafe { (engfuncs.cmd_argc)() };
    let mut assigned = false;
    if argc >= 2 {
        let arg1 = unsafe { (engfuncs.cmd_argv)(1) };
        if !arg1.is_null() {
            let value = unsafe { CStr::from_ptr(arg1 as *const c_char) }.to_string_lossy();
            match value.trim().parse::<i32>() {
                Ok(n) => {
                    level.store(n.clamp(anim_fix::LEVEL_OFF, anim_fix::LEVEL_MAX), Ordering::Relaxed);
                    assigned = true;
                }
                Err(_) => {
                    let max = anim_fix::LEVEL_MAX;
                    console_print(&format!("{name}: expected 0-{max}, got \"{}\"\n", value.trim()));
                    unsafe {
                        crate::debug::report(&format!("commands: {name} rejected argument \"{}\"", value.trim()))
                    };
                    return;
                }
            }
        }
    }

    let now = anim_fix::level();
    let state = format!("{now} ({})", anim_fix::level_description(now));
    if assigned {
        console_print(&format!("{name} = {state}\n"));
    } else {
        let max = anim_fix::LEVEL_MAX;
        let mut usage = format!("{name} = {state}\nusage: {name} <0-{max}>\n");
        for n in anim_fix::LEVEL_OFF..=max {
            usage.push_str(&format!("  {n}  {}\n", anim_fix::level_description(n)));
        }
        console_print(&format!("{usage}{}\n", status()));
    }
    unsafe {
        crate::debug::report(&format!(
            "commands: {name} = {state} ({}, argc={argc})",
            if assigned { "set" } else { "queried, unchanged" }
        ))
    };
}

unsafe extern "C" fn cmd_log_held_models() {
    handle_toggle(HELD_MODELS_NAME, &anim_fix::LOG_HELD_MODELS, || {
        "logs the third-person model the spectated player holds, each time it changes".into()
    });
}

/// How far boosted gunshots carry. Separate from the on/off toggle because it
/// is the value you actually want to sweep while listening.
unsafe extern "C" fn cmd_gunshot_attenuation() {
    let Some(engfuncs) = engine::engfuncs() else { return };

    if unsafe { (engfuncs.cmd_argc)() } >= 2 {
        let arg1 = unsafe { (engfuncs.cmd_argv)(1) };
        if !arg1.is_null() {
            let raw = unsafe { CStr::from_ptr(arg1 as *const c_char) }.to_string_lossy().into_owned();
            match raw.trim().parse::<f32>() {
                Ok(value) => match sound_fix::set_carry_attenuation(value) {
                    Ok(()) => {
                        console_print(&format!("{ATTENUATION_NAME} = {value}\n"));
                        unsafe { crate::debug::report(&format!("commands: {ATTENUATION_NAME} = {value} (set)")) };
                        return;
                    }
                    Err(why) => {
                        console_print(&format!("{ATTENUATION_NAME}: {why}\n"));
                        unsafe { crate::debug::report(&format!("commands: {ATTENUATION_NAME} rejected \"{raw}\" -- {why}")) };
                        return;
                    }
                },
                Err(_) => {
                    console_print(&format!("{ATTENUATION_NAME}: expected a number, got \"{raw}\"\n"));
                    return;
                }
            }
        }
    }

    console_print(&format!(
        "{ATTENUATION_NAME} = {}\nusage: {ATTENUATION_NAME} <0.05..0.79>  (lower carries further; the game's own default is 0.8)\n",
        sound_fix::carry_attenuation()
    ));
}

/// Registers one command under several names at once.
///
/// The engine keeps each name independently, so every entry becomes a working
/// spelling of the same command -- which is how a rename keeps the old name
/// alive for a release, and how a variant spelling is added without touching
/// the handler. See `names.rs`.
pub(crate) fn add_commands(names: &[&str], function: engine::ConsoleCommandFn) {
    for name in names {
        add_command(name, function);
    }
}

fn add_command(name: &str, function: engine::ConsoleCommandFn) {
    let Some(engfuncs) = engine::engfuncs() else { return };
    let Ok(c_name) = CString::new(name) else { return };
    unsafe { (engfuncs.pfn_add_command)(c_name.as_ptr(), function) };
    // Leak intentionally: pfnAddCommand keeps this pointer for the life of the
    // engine session, the same lifetime as the DLL itself.
    std::mem::forget(c_name);
}

fn install_fallback_commands() {
    add_command(GUNSHOTS_FIX_NAME, cmd_gunshots_fix);
    add_command(ANIMATION_FIX_NAME, cmd_animation_fix);
    add_command(ATTENUATION_NAME, cmd_gunshot_attenuation);
    add_command(HELD_MODELS_NAME, cmd_log_held_models);
    unsafe {
        crate::debug::report(&format!(
            "commands: fell back to plain commands -- {GUNSHOTS_FIX_NAME}, {ANIMATION_FIX_NAME}, {ATTENUATION_NAME}, {HELD_MODELS_NAME} (no type-ahead value, no .cfg or launch-line setting)"
        ))
    };
}

/// Registers the console surface. Must be called after `engine::engfuncs()`
/// returns `Some`.
///
/// The defaults handed to the engine are whatever the environment variables
/// already put in the flags, so `GOLDSRC_HOOKS_ANIM_FIX=1` keeps working
/// exactly as before — and a value in a `.cfg` or on the launch line, applied
/// after registration, wins over it.
pub fn install() {
    if engine::engfuncs().is_none() {
        unsafe { crate::debug::report("commands::install called before engfuncs were captured -- this is a bug in install ordering") };
        return;
    }

    // `dodtools_status` is a command under either path: it takes no value, so
    // there is nothing for a cvar to hold.
    add_command(STATUS_NAME, cmd_status);

    // Always a command, never a cvar: it has subcommands and a variable number
    // of arguments, which a cvar's single value cannot carry.
    add_commands(crate::deathmsg::COMMAND_NAMES, crate::deathmsg::command);

    let bit = |flag: bool| if flag { "1" } else { "0" };
    let gunshots = register(GUNSHOTS_FIX_NAME, bit(sound_fix::ENABLED.load(Ordering::Relaxed)));
    let animation = register(ANIMATION_FIX_NAME, &anim_fix::level().to_string());
    let attenuation = register(ATTENUATION_NAME, &sound_fix::carry_attenuation().to_string());
    let held_models = register(HELD_MODELS_NAME, bit(anim_fix::LOG_HELD_MODELS.load(Ordering::Relaxed)));

    let (Some(gunshots), Some(animation), Some(attenuation), Some(held_models)) =
        (gunshots, animation, attenuation, held_models)
    else {
        install_fallback_commands();
        return;
    };

    CVAR_GUNSHOTS.store(gunshots, Ordering::Relaxed);
    CVAR_ANIMATION.store(animation, Ordering::Relaxed);
    CVAR_ATTENUATION.store(attenuation, Ordering::Relaxed);
    CVAR_HELD_MODELS.store(held_models, Ordering::Relaxed);
    CVARS_LIVE.store(true, Ordering::Release);
    engine::set_per_frame_prologue(poll);

    unsafe {
        crate::debug::report(&format!(
            "commands: registered cvars {GUNSHOTS_FIX_NAME}, {ANIMATION_FIX_NAME}, {ATTENUATION_NAME}, {HELD_MODELS_NAME} and command {STATUS_NAME}"
        ))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tripwire, not a style check. `FCVAR_ARCHIVE` would have the engine
    /// write these into the user's `config.cfg`, and the pipeline's standing
    /// rule is that the game's own `.cfg` files are detected and warned about,
    /// never written. Flipping this is a decision, so it should not be possible
    /// to make it by accident.
    #[test]
    fn cvars_are_not_archived() {
        const FCVAR_ARCHIVE: i32 = 1;
        assert_eq!(CVAR_FLAGS & FCVAR_ARCHIVE, 0);
    }

    /// The status reply is the only place the four settings are reported
    /// together, so it is worth knowing if one silently drops out of it.
    #[test]
    fn status_names_every_setting() {
        let text = status_text();
        for name in [ANIMATION_FIX_NAME, GUNSHOTS_FIX_NAME, ATTENUATION_NAME, HELD_MODELS_NAME] {
            assert!(text.contains(name), "{name} missing from the status reply:\n{text}");
        }
    }
}

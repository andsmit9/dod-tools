//! Makes DoD 1.3 weapon-fire sounds carry further while spectating (HLTV or a
//! regular demo's in-eye/free camera), **without** flattening them into
//! ambience.
//!
//! ## What the problem actually is
//!
//! Not missing sounds. Comparing an HLTV demo against a POV demo of the same
//! match half (`analysis/examples/hltv_sound_probe.rs`) shows the HLTV demo
//! carrying *more* weapon-fire events than the POV one -- 1391 vs 1248, with
//! every weapon represented. Every shot is already being requested, so there
//! is nothing to synthesise and no event to re-fire.
//!
//! What actually happens is ordinary GoldSrc distance falloff. Shots arrive at
//! `ATTN_NORM` (0.8), which goes inaudible at roughly 1250 units, and DoD's
//! maps are far larger than that -- so gunfire across the map is requested,
//! spatialised, and then attenuated to silence.
//!
//! ## What this does
//!
//! Lowers the attenuation of weapon-fire samples so they carry further, and
//! *only* lowers it -- a sample that already carries further than the
//! configured value is left alone. Volume is not touched at all, so the
//! engine's own distance calculation still sets the final level and relative
//! dynamics between near and far shots survive.
//!
//! Crucially it does not use `ATTN_NONE` (0.0). In GoldSrc that doesn't mean
//! "no falloff" so much as "not a positional sound at all" -- every shot plays
//! at full level with no direction, which sounds like the whole match is
//! happening at the camera. That was this module's first implementation and it
//! was wrong; see `CARRY_ATTENUATION` for the tunable that replaced it.
//!
//! Every DoD 1.3 weapon's firing sample is named `<weapon>_shoot.wav`
//! (confirmed against the game's own installed sound files -- garand_shoot,
//! kar_shoot, bar_shoot, mp44_shoot, luger_shoot, mg42_shoot, ... every one
//! of them, no exceptions found), which is enough on its own to distinguish
//! a gunshot from reloads, footsteps, voice lines, etc. -- no need to also
//! track weapon-fire *events* the way the animation fix does.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use crate::engine::{self, EventApiPartial};

pub static ENABLED: AtomicBool = AtomicBool::new(false);

/// GoldSrc's own attenuation constants, for reference: `ATTN_NONE` 0.0 (not a
/// positional sound), `ATTN_NORM` 0.8 (~1250 units), `ATTN_STATIC` 1.25,
/// `ATTN_IDLE` 2.0. Lower carries further; zero stops being directional.
const ATTN_NORM: f32 = 0.8;

/// How far boosted gunshots carry, as a GoldSrc attenuation value. 0.3 puts
/// the audible limit around 3300 units instead of `ATTN_NORM`'s ~1250 -- far
/// enough to hear a firefight across a DoD map, while still falling off with
/// distance and keeping its direction. Tunable live via
/// `dodtools_hltv_gunshot_attenuation`; stored as bits because there is no
/// `AtomicF32`.
static CARRY_ATTENUATION: AtomicU32 = AtomicU32::new(0x3E99_999A); // 0.3f32

pub fn carry_attenuation() -> f32 {
    f32::from_bits(CARRY_ATTENUATION.load(Ordering::Relaxed))
}

/// Rejects values outside a sane range: 0.0 would make gunshots non-positional
/// (the exact bug this replaced), and anything at or above `ATTN_NORM` would
/// make them carry no further than they already do.
pub fn set_carry_attenuation(value: f32) -> Result<(), String> {
    if !(value.is_finite() && value > 0.0 && value < ATTN_NORM) {
        return Err(format!(
            "expected a value greater than 0 and less than {ATTN_NORM} (0 would make gunshots non-positional; {ATTN_NORM} is the game's own default, so it would do nothing)"
        ));
    }
    CARRY_ATTENUATION.store(value.to_bits(), Ordering::Relaxed);
    Ok(())
}

// Counters, not per-call logging: this runs for every sound the engine plays,
// so a log line per call would flood the file and cost real time in a capture.
// They answer the question a silent take actually raises -- did the hook run
// at all, did it ever see a gunshot, and did it boost one.
static CALLS: AtomicU32 = AtomicU32::new(0);
static SHOOT_SAMPLES: AtomicU32 = AtomicU32::new(0);
static BOOSTED: AtomicU32 = AtomicU32::new(0);
static SKIPPED_NOT_SPECTATING: AtomicU32 = AtomicU32::new(0);

/// One-line summary of what the hook has actually done this session, for the
/// `dodtools_hltv_gunshots_fix` status reply.
pub fn status() -> String {
    format!(
        "sounds seen: {}, weapon-fire samples: {}, extended: {}, skipped (not spectating): {}, carry attenuation: {} (game default {ATTN_NORM})",
        CALLS.load(Ordering::Relaxed),
        SHOOT_SAMPLES.load(Ordering::Relaxed),
        BOOSTED.load(Ordering::Relaxed),
        SKIPPED_NOT_SPECTATING.load(Ordering::Relaxed),
        carry_attenuation(),
    )
}

/// Substring test without allocating -- the previous `to_string_lossy()` built
/// a `String` for every sound the engine played.
fn is_weapon_fire(sample: *const c_char) -> bool {
    if sample.is_null() {
        return false;
    }
    unsafe { CStr::from_ptr(sample) }.to_bytes().windows(6).any(|w| w == b"_shoot")
}

type EvPlaySoundFn = unsafe extern "C" fn(i32, *mut f32, i32, *const c_char, f32, f32, i32, i32);
static REAL_EV_PLAY_SOUND: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "C" fn hook_ev_play_sound(
    ent: i32,
    origin: *mut f32,
    channel: i32,
    sample: *const c_char,
    volume: f32,
    attenuation: f32,
    f_flags: i32,
    pitch: i32,
) {
    let real = REAL_EV_PLAY_SOUND.load(Ordering::Acquire);
    let real: EvPlaySoundFn = unsafe { std::mem::transmute(real) };

    if CALLS.fetch_add(1, Ordering::Relaxed) == 0 {
        unsafe { crate::debug::report("sound_fix: EV_PlaySound hook is live (first sound played through it)") };
    }

    let is_fire = is_weapon_fire(sample);
    if is_fire {
        SHOOT_SAMPLES.fetch_add(1, Ordering::Relaxed);
        // A weapon-fire sample names the entity that fired, which is the one
        // signal anim_fix needs to drive the firing animation -- see
        // `anim_fix::on_weapon_fired`. Independent of this fix's own on/off
        // state; the two are toggled separately.
        crate::anim_fix::on_weapon_fired(ent);
    }
    let spectating = engine::engfuncs().map(|e| unsafe { (e.is_spectate_only)() } != 0).unwrap_or(false);
    let should_boost = ENABLED.load(Ordering::Relaxed) && is_fire && spectating;

    // Only ever *lower* the attenuation, and leave the volume alone -- the
    // engine still derives the final level from distance, so a far shot stays
    // quieter than a near one and keeps its direction.
    let carry = carry_attenuation();
    if should_boost && attenuation > carry {
        if BOOSTED.fetch_add(1, Ordering::Relaxed) == 0 {
            let name = unsafe { CStr::from_ptr(sample) }.to_string_lossy().into_owned();
            unsafe {
                crate::debug::report(&format!(
                    "sound_fix: first extended gunshot -- \"{name}\" (volume {volume} left as-is, attenuation {attenuation} -> {carry})"
                ))
            };
        }
        unsafe { real(ent, origin, channel, sample, volume, carry, f_flags, pitch) };
    } else {
        // The most likely reason a take sounds unchanged with the fix on: the
        // gunshots are there, but IsSpectateOnly() is false so nothing boosts.
        if is_fire && ENABLED.load(Ordering::Relaxed) && !spectating && SKIPPED_NOT_SPECTATING.fetch_add(1, Ordering::Relaxed) == 0 {
            unsafe { crate::debug::report("sound_fix: saw a weapon-fire sample but IsSpectateOnly() is false, so it was left alone -- the fix only acts while spectating") };
        }
        unsafe { real(ent, origin, channel, sample, volume, attenuation, f_flags, pitch) };
    }
}

/// Installs the `EV_PlaySound` hook. Must be called after `engine::engfuncs()`
/// returns `Some` (i.e. after `client.dll` has finished loading), since it
/// needs a valid `p_event_api` pointer to patch.
pub fn install() {
    let Some(engfuncs) = engine::engfuncs() else {
        unsafe { crate::debug::report("sound_fix::install called before engfuncs were captured -- this is a bug in install ordering") };
        return;
    };

    let event_api: *mut EventApiPartial = engfuncs.p_event_api;
    if event_api.is_null() {
        unsafe { crate::debug::report("sound_fix: pEventAPI is null, cannot install") };
        return;
    }

    unsafe {
        let real = (*event_api).ev_play_sound;
        REAL_EV_PLAY_SOUND.store(real as *mut c_void, Ordering::Release);
        (*event_api).ev_play_sound = hook_ev_play_sound;
    }
}

//! Captures the engine's `cl_enginefuncs_s` table (`pEngfuncs`), the
//! `engine_studio_api_s` table (`pstudio`), and a genuine per-frame callback,
//! by intercepting how `hw.dll` resolves `client.dll`'s entry points in the
//! first place.
//!
//! ## Why this isn't the "obvious" export hook
//!
//! Two earlier designs failed, and understanding why is what makes this one
//! correct (see `docs/goldsrc_client_dll_internals.md` for the full
//! reverse-engineering write-up, and issue #204 for the investigation trail):
//!
//! 1. **Patching `client.dll`'s export table** for `Initialize` / `HUD_Frame`
//!    / `HUD_GetStudioModelInterface`. The patch was provably applied (the
//!    real Win32 `GetProcAddress` read our value back), yet nothing ever
//!    called through it. Root cause, confirmed by disassembling `hw.dll`:
//!    **the engine never asks for those names at all** on a "secured"
//!    `client.dll`. It calls `GetProcAddress(hClient, "F")` and, if that
//!    single export exists, calls it and stops -- if `F` is absent it
//!    `FreeLibrary`s and fails outright, with no per-name fallback on that
//!    path. DoD 1.3's `client.dll` does export `F`, so the classic names are
//!    dead weight in it. Xash3D's own GoldSrc-compatible loader calls this
//!    exact case "single callback export (secured client dlls)".
//!
//! 2. **Byte-signature scanning for `pEngfuncs`**, ported faithfully from
//!    HLAE's `hl_addresses.cpp`. Its anchor string `"ScreenFade"` simply does
//!    not exist anywhere in DoD's `client.dll` (verified both in-process and
//!    by offline string extraction from the file), so the scan can't start.
//!
//! ## What this module does instead
//!
//! It hooks **`hw.dll`'s Import Address Table entry for
//! `KERNEL32!GetProcAddress`** -- the exact call site the disassembly shows
//! the engine using -- alongside the `LoadLibraryA` entry it already hooked
//! to spot `client.dll` loading. This is the same IAT technique HLAE's own
//! `CAfxImportDllHook` uses, and the `LoadLibraryA` half was already proven
//! reliable in testing; the `GetProcAddress` half is a direct extension of it.
//!
//! Every lookup against the `client.dll` module is then handled in one place,
//! covering **both** engine conventions with one mechanism and no byte
//! patterns or hardcoded offsets:
//!
//! - `"F"` (secured builds, what DoD 1.3 actually uses) -- we return our own
//!   wrapper, which calls the real `F` to let it fill the caller's
//!   `cldll_func_t` table, then swaps three of its 43 slots for our
//!   trampolines before handing it back to the engine.
//! - `"Initialize"` / `"HUD_Frame"` / `"HUD_GetStudioModelInterface"`
//!   (classic non-secured builds) -- we return the trampoline directly.
//!
//! Either way the trampolines receive what we need as ordinary arguments:
//! `Initialize` hands us `pEnginefuncs`, `HUD_GetStudioModelInterface` hands
//! us `pstudio`, and `HUD_Frame` gives a real per-frame tick.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};
use windows_sys::Win32::System::Threading::{CreateThread, Sleep};

use crate::pe;

/// Partial mirror of GoldSrc's `event_api_s` (see `common/event_api.h` in the
/// Half-Life SDK). Only the fields up to and including `EV_PlaySound` are
/// declared -- we never touch anything past it, and C struct layout is
/// sequential, so the offset of `EV_PlaySound` only depends on what precedes
/// it (just `version`), not on anything declared after it here.
#[repr(C)]
pub struct EventApiPartial {
    pub version: i32,
    pub ev_play_sound: unsafe extern "C" fn(
        ent: i32,
        origin: *mut f32,
        channel: i32,
        sample: *const c_char,
        volume: f32,
        attenuation: f32,
        f_flags: i32,
        pitch: i32,
    ),
}

pub type WeaponAnimFn = unsafe extern "C" fn(sequence: i32, body: i32);
pub type GetGameDirectoryFn = unsafe extern "C" fn(sz_get_game_dir: *mut c_char);
pub type IsSpectateOnlyFn = unsafe extern "C" fn() -> i32;
pub type GetViewModelFn = unsafe extern "C" fn() -> *mut ClEntityS;
pub type GetEntityByIndexFn = unsafe extern "C" fn(index: i32) -> *mut ClEntityS;
pub type ConsoleCommandFn = unsafe extern "C" fn();
pub type AddCommandFn = unsafe extern "C" fn(cmd_name: *const c_char, function: ConsoleCommandFn);
pub type ConsolePrintFn = unsafe extern "C" fn(text: *const c_char);
pub type CmdArgcFn = unsafe extern "C" fn() -> i32;
pub type CmdArgvFn = unsafe extern "C" fn(arg: i32) -> *const c_char;
pub type RegisterVariableFn =
    unsafe extern "C" fn(name: *const c_char, value: *const c_char, flags: i32) -> *mut CvarSPartial;

/// `cvar_s` (`common/cvardef.h`). Small, stable, and unchanged since Quake.
///
/// Only `value` is ever read; `name` exists so a registration can be checked
/// against the name it asked for, which is the cheapest possible proof that
/// this layout and the engine's agree. See `commands::install`.
#[repr(C)]
pub struct CvarSPartial {
    pub name: *const c_char,
    pub string: *const c_char,
    pub flags: i32,
    pub value: f32,
    pub next: *mut CvarSPartial,
}

// Unlike the tables above, `cvar_s` mixes pointers with 32-bit scalars, so its
// size does not scale with the pointer width and the check has to name a target.
// The DLL only ever loads into 32-bit `hl.exe`; `cargo test --workspace` builds
// this crate for the host as well, hence the gate rather than a formula.
#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(
        size_of::<CvarSPartial>() == 20,
        "CvarSPartial's layout doesn't match cvardef.h (expected 20 bytes on a 32-bit build)"
    );
    // `value` is the only field read at runtime, so its offset is the one that
    // has to be right.
    assert!(std::mem::offset_of!(CvarSPartial, value) == 12);
};

impl CvarSPartial {
    /// The name the engine holds for this cvar, or `None` if the pointer is
    /// null. Borrowed from the engine's own storage, which outlives us.
    ///
    /// # Safety
    ///
    /// `self` must point at a live cvar the engine returned.
    pub unsafe fn name_str(&self) -> Option<std::borrow::Cow<'_, str>> {
        if self.name.is_null() {
            return None;
        }
        Some(unsafe { std::ffi::CStr::from_ptr(self.name) }.to_string_lossy())
    }
}

/// A 3D vector, matching `vec3_t` (`float[3]`) everywhere it's embedded in a
/// GoldSrc SDK struct below.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// Exact field-for-field mirror of `entity_state_s` (`common/entity_state.h`)
/// -- unlike `ClEngineFuncsPartial`'s uniform pointer-sized slots, this
/// struct mixes byte/short/int/float/vec3 fields with real padding between
/// them, so it's transcribed with real types rather than opaque slots:
/// `#[repr(C)]` reproduces MSVC's C struct layout rules exactly, so as long
/// as every field's type and order matches the header, the offsets of
/// `sequence` and `weaponmodel` (what we actually read) come out correct
/// automatically instead of needing hand-computed byte offsets.
#[repr(C)]
pub struct EntityStateS {
    pub entity_type: i32,
    pub number: i32,
    pub msg_time: f32,
    pub messagenum: i32,
    pub origin: Vec3,
    pub angles: Vec3,
    pub modelindex: i32,
    pub sequence: i32,
    pub frame: f32,
    pub colormap: i32,
    pub skin: i16,
    pub solid: i16,
    pub effects: i32,
    pub scale: f32,
    pub eflags: u8,
    pub rendermode: i32,
    pub renderamt: i32,
    pub rendercolor: [u8; 3], // color24 { r, g, b }
    pub renderfx: i32,
    pub movetype: i32,
    pub animtime: f32,
    pub framerate: f32,
    pub body: i32,
    pub controller: [u8; 4],
    pub blending: [u8; 4],
    pub velocity: Vec3,
    pub mins: Vec3,
    pub maxs: Vec3,
    pub aiment: i32,
    pub owner: i32,
    pub friction: f32,
    pub gravity: f32,
    pub team: i32,
    pub playerclass: i32,
    pub health: i32,
    pub spectator: i32, // qboolean
    pub weaponmodel: i32,
    pub gaitsequence: i32,
    pub basevelocity: Vec3,
    pub usehull: i32,
    pub oldbuttons: i32,
    pub onground: i32,
    pub istepleft: i32,
    pub fl_fall_velocity: f32,
    pub fov: f32,
    pub weaponanim: i32,
    pub startpos: Vec3,
    pub endpos: Vec3,
    pub impacttime: f32,
    pub starttime: f32,
    pub iuser1: i32,
    pub iuser2: i32,
    pub iuser3: i32,
    pub iuser4: i32,
    pub fuser1: f32,
    pub fuser2: f32,
    pub fuser3: f32,
    pub fuser4: f32,
    pub vuser1: Vec3,
    pub vuser2: Vec3,
    pub vuser3: Vec3,
    pub vuser4: Vec3,
}

const _: () = assert!(
    size_of::<EntityStateS>() == 340,
    "EntityStateS's layout doesn't match entity_state.h (expected 340 bytes)"
);

#[repr(C)]
struct PositionHistoryT {
    animtime: f32,
    origin: Vec3,
    angles: Vec3,
}

#[repr(C)]
struct MouthT {
    mouthopen: u8,
    sndcount: u8,
    sndavg: i32,
}

#[repr(C)]
struct LatchedVarsT {
    prevanimtime: f32,
    sequencetime: f32,
    prevseqblending: [u8; 2],
    prevorigin: Vec3,
    prevangles: Vec3,
    prevsequence: i32,
    prevframe: f32,
    prevcontroller: [u8; 4],
    prevblending: [u8; 2],
}

/// Partial mirror of `cl_entity_s` (`common/cl_entity.h`), transcribed field
/// for field up through `model` -- everything after that (`efrag`, `topnode`,
/// etc.) is never touched, so it's simply not declared.
#[repr(C)]
pub struct ClEntityS {
    pub index: i32,
    pub player: i32, // qboolean
    pub baseline: EntityStateS,
    pub prevstate: EntityStateS,
    pub curstate: EntityStateS,
    current_position: i32,
    ph: [PositionHistoryT; 64],
    mouth: MouthT,
    latched: LatchedVarsT,
    lastmove: f32,
    origin: Vec3,
    angles: Vec3,
    attachment: [Vec3; 4],
    trivial_accept: i32,
    pub model: *mut ModelSPartial,
}

/// Partial mirror of `model_s` (`common/com_model.h`) -- `name` is its first
/// field, so this is all we need to identify a model by filename substring.
#[repr(C)]
pub struct ModelSPartial {
    pub name: [u8; 64], // MAX_MODEL_NAME
}

impl ModelSPartial {
    pub fn name_str(&self) -> std::borrow::Cow<'_, str> {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..end])
    }
}

/// Exact field-for-field mirror of the studio model file header
/// (`studiohdr_t` in `engine/studio.h`) up through `seqindex` -- confirmed
/// against real DoD 1.3 `.mdl` files during the R&D phase (numseq at byte
/// offset 164, seqindex at 168).
#[repr(C)]
pub struct StudioHdrPartial {
    pub id: i32,
    pub version: i32,
    pub name: [u8; 64],
    pub length: i32,
    pub eyeposition: Vec3,
    pub min: Vec3,
    pub max: Vec3,
    pub bbmin: Vec3,
    pub bbmax: Vec3,
    pub flags: i32,
    pub numbones: i32,
    pub boneindex: i32,
    pub numbonecontrollers: i32,
    pub bonecontrollerindex: i32,
    pub numhitboxes: i32,
    pub hitboxindex: i32,
    pub numseq: i32,
    pub seqindex: i32,
}

const _: () = assert!(
    std::mem::offset_of!(StudioHdrPartial, numseq) == 164
        && std::mem::offset_of!(StudioHdrPartial, seqindex) == 168,
    "StudioHdrPartial's layout doesn't match studio.h / the confirmed .mdl offsets"
);

/// Exact field-for-field mirror of `mstudioseqdesc_t` (`engine/studio.h`) up
/// through `label` -- 176 bytes total per entry, `label` is the first 32.
#[repr(C)]
pub struct StudioSeqDescPartial {
    pub label: [u8; 32],
    /// Playback rate, and the frame count at that rate -- together they give
    /// the animation's real duration, which is how long a one-shot sequence
    /// occupies the viewmodel before it sits on its final pose.
    ///
    /// Offsets verified against the shipped models rather than assumed:
    /// `v_stick.mdl` reads back `draw` at 40fps/31 frames (0.775s), `pinpull`
    /// at 30fps/26 (0.867s) and `throw` at 40fps/2 (0.050s), which are
    /// sensible values in the right places. A wrong offset here would produce
    /// garbage durations rather than failing loudly.
    pub fps: f32,
    _flags_activity_actweight_events: [u8; 20],
    pub numframes: i32,
    _rest: [u8; 176 - 60],
}

const _: () = assert!(size_of::<StudioSeqDescPartial>() == 176);

impl StudioSeqDescPartial {
    pub fn label_str(&self) -> std::borrow::Cow<'_, str> {
        let end = self.label.iter().position(|&b| b == 0).unwrap_or(self.label.len());
        String::from_utf8_lossy(&self.label[..end])
    }
}

/// Partial mirror of `engine_studio_api_s` (`common/r_studioint.h`) up
/// through `GetModelByIndex` -- the only two functions this crate calls.
#[repr(C)]
pub struct EngineStudioApiPartial {
    _slots_before: [*mut c_void; 3], // Mem_Calloc, Cache_Check, LoadCacheFile
    _mod_for_name: *mut c_void,
    pub mod_extradata: unsafe extern "C" fn(model: *mut ModelSPartial) -> *mut c_void,
    pub get_model_by_index: unsafe extern "C" fn(index: i32) -> *mut ModelSPartial,
}

/// Partial, offset-accurate mirror of `cl_enginefuncs_s` (see
/// `advancedfx/halflife`'s `engine/cdll_int.h`, whose `k_engdstNull` macro
/// lists every field of the real struct in declaration order -- confirmed
/// against it field-by-field. Every field up through `IsSpectateOnly` is
/// present, in order, so the ones we actually use (`pfn_add_command`,
/// `pfn_console_print`, `cmd_argc`, `cmd_argv`, `pfn_weapon_anim`,
/// `pfn_get_game_directory`, `p_event_api`, `is_spectate_only`) land at the
/// correct byte offsets; everything else is kept as an opaque, untyped slot
/// purely to hold the layout together.
///
/// `pfn_register_variable` sits at index 14, which is bracketed on both sides
/// by slots proved from real call sites in DoD's own `client.dll`: index 14
/// itself carries 111 calls (exactly the 111 cvar names recovered in
/// `docs/goldsrc_client_dll_internals.md` §5) and index 17 carries 110 (the
/// 110 command names). Two independently-confirmed anchors three slots apart
/// leave no room for the count between them to be wrong.
#[repr(C)]
pub struct ClEngineFuncsPartial {
    _slots_before_register_variable: [*mut c_void; 14], // pfnSPR_Load .. pfnSetCrosshair
    pub pfn_register_variable: RegisterVariableFn,
    _slots_before_add_command: [*mut c_void; 2], // pfnGetCvarFloat, pfnGetCvarString
    pub pfn_add_command: AddCommandFn,
    _slots_before_console_print: [*mut c_void; 12], // pfnHookUserMsg .. pfnDrawConsoleStringLen
    pub pfn_console_print: ConsolePrintFn,
    _slots_before_cmd_argc: [*mut c_void; 7], // pfnCenterPrint .. Cvar_SetValue
    pub cmd_argc: CmdArgcFn,
    pub cmd_argv: CmdArgvFn,
    _slots_before_viewmodel: [*mut c_void; 12], // Con_Printf .. GetLocalPlayer
    pub get_view_model: GetViewModelFn,
    pub get_entity_by_index: GetEntityByIndexFn,
    _slots_before_weapon_anim: [*mut c_void; 12], // GetClientTime .. pfnPlaybackEvent
    pub pfn_weapon_anim: WeaponAnimFn,
    _slots_between: [*mut c_void; 4], // pfnRandomFloat, pfnRandomLong, pfnHookEvent, Con_IsVisible
    pub pfn_get_game_directory: GetGameDirectoryFn,
    _slots_after_gamedir: [*mut c_void; 12], // pfnGetCvarPointer .. pEfxAPI
    pub p_event_api: *mut EventApiPartial,
    _slots_after_event_api: [*mut c_void; 3], // pDemoAPI, pNetAPI, pVoiceTweak
    pub is_spectate_only: IsSpectateOnlyFn,
}

// Compile-time cross-check against the field count transcribed from
// APIProxy.h (pfnSPR_Load..IsSpectateOnly inclusive = 89 pointer-sized
// slots on a 32-bit build, i.e. 356 bytes). If this ever fails, a slot count
// above drifted from the reference header.
//
// Every field here is confirmed against a real call site in client.dll. A
// field was briefly added past this point for PlayerInfo_ValueForKey, whose
// slot could only be inferred; it is a non-nullable fn pointer, so merely
// forming a reference over it asserted that slot is non-null, with no call
// needed. Do not extend this struct past a field that cannot be verified.
const _: () = assert!(
    size_of::<ClEngineFuncsPartial>() == 89 * size_of::<usize>(),
    "ClEngineFuncsPartial's layout doesn't match APIProxy.h's field count (expected 89 pointer-sized slots)"
);

static ENGFUNCS: AtomicPtr<ClEngineFuncsPartial> = AtomicPtr::new(std::ptr::null_mut());
static ENGINE_STUDIO: AtomicPtr<EngineStudioApiPartial> = AtomicPtr::new(std::ptr::null_mut());

/// Returns the captured engine function table, once `client.dll` has loaded
/// and been successfully signature-scanned. `None` before that.
pub fn engfuncs() -> Option<&'static ClEngineFuncsPartial> {
    let ptr = ENGFUNCS.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        // Safety: only ever set once, from `capture_via_scan`, to a pointer
        // the engine itself owns for the life of the client.dll instance.
        Some(unsafe { &*ptr })
    }
}

/// Returns the captured studio-model engine interface, once `client.dll` has
/// loaded and been successfully signature-scanned. `None` before that (or if
/// the scan found `pEngfuncs` but not `pstudio`).
pub fn engine_studio() -> Option<&'static EngineStudioApiPartial> {
    let ptr = ENGINE_STUDIO.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        // Safety: same guarantee as `engfuncs()` above.
        Some(unsafe { &*ptr })
    }
}

/// Registers `callback` to run once per rendered frame.
///
/// Normally driven by the real `HUD_Frame` trampoline. A ~60Hz timer thread
/// is started as a safety net for the case where the `HUD_Frame` hook never
/// lands (an engine build that resolves it by some path we don't intercept),
/// and retires itself the moment a real frame callback arrives so the two
/// never both drive it.
static PER_FRAME_CALLBACK: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();
static TIMER_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the `HUD_Frame` trampoline the first time the engine actually
/// renders a frame through us; retires the fallback timer thread.
static HUD_FRAME_DRIVING: AtomicBool = AtomicBool::new(false);

/// Runs immediately before the main per-frame callback.
///
/// Exists so the cvar poll can write the flags `anim_fix::apply()` then reads
/// on the same frame, without either module having to know about the other.
/// Separate `OnceLock`s rather than a list because this is the frame path:
/// no allocation, no locking.
static PER_FRAME_PROLOGUE: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

pub fn set_per_frame_prologue(callback: fn()) {
    let _ = PER_FRAME_PROLOGUE.set(callback);
}

pub fn set_per_frame_callback(callback: fn()) {
    let _ = PER_FRAME_CALLBACK.set(callback);
    if !TIMER_THREAD_STARTED.swap(true, Ordering::AcqRel) {
        unsafe {
            CreateThread(std::ptr::null(), 0, Some(per_frame_timer_thread), std::ptr::null(), 0, std::ptr::null_mut());
        }
    }
}

fn run_per_frame_callback() {
    if let Some(prologue) = PER_FRAME_PROLOGUE.get() {
        prologue();
    }
    if let Some(callback) = PER_FRAME_CALLBACK.get() {
        callback();
    }
}

unsafe extern "system" fn per_frame_timer_thread(_lp_param: *mut c_void) -> u32 {
    loop {
        unsafe { Sleep(16) };
        if HUD_FRAME_DRIVING.load(Ordering::Acquire) {
            unsafe { crate::debug::report("per-frame: real HUD_Frame hook is driving, retiring the fallback timer thread") };
            return 0;
        }
        run_per_frame_callback();
    }
}

// ---------------------------------------------------------------------------
// client.dll's `cldll_func_t` table
// ---------------------------------------------------------------------------

/// Number of function-pointer slots the secured `F` export writes into the
/// buffer the engine hands it. Confirmed two independent ways: by
/// disassembling DoD 1.3's own `F` (`rep movsd` with `ecx = 0x2b` = 43), and
/// against Xash3D's `cldll_func_src_t`, whose 43 members line up name-for-name
/// with the 43 addresses `F` writes (each of which resolves back to the
/// identically-named export in `client.dll`).
const CLDLL_FUNC_SLOTS: usize = 43;

/// Slot indices within that table -- verified by resolving every address DoD's
/// `F` writes back to its own export name.
const SLOT_INITIALIZE: usize = 0;
const SLOT_HUD_FRAME: usize = 33;
const SLOT_GET_STUDIO_MODEL_INTERFACE: usize = 39;

const _: () = assert!(
    SLOT_INITIALIZE < CLDLL_FUNC_SLOTS
        && SLOT_HUD_FRAME < CLDLL_FUNC_SLOTS
        && SLOT_GET_STUDIO_MODEL_INTERFACE < CLDLL_FUNC_SLOTS,
    "a cldll_func_t slot index is outside the table F actually writes"
);

type InitializeFn = unsafe extern "C" fn(*mut ClEngineFuncsPartial, i32) -> i32;
type HudFrameFn = unsafe extern "C" fn(f64);
type GetStudioModelInterfaceFn =
    unsafe extern "C" fn(i32, *mut *mut c_void, *mut EngineStudioApiPartial) -> i32;
/// The secured single-callback export: fills the caller-provided buffer with
/// `CLDLL_FUNC_SLOTS` function pointers. `__cdecl`, one pointer argument --
/// confirmed from `hw.dll`'s call site (`push edx; call eax; add esp, 4`).
type ClientApiFn = unsafe extern "C" fn(*mut *mut c_void);

static REAL_F: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_INITIALIZE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_HUD_FRAME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GET_STUDIO_MODEL_INTERFACE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Runs once, on the engine's own thread, immediately after `client.dll`'s
/// real `Initialize` returns -- see `set_on_engine_ready`.
static ON_ENGINE_READY: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Registers the work that must happen once `pEngfuncs` is live.
///
/// This exists so that work runs **on the engine thread, at a deterministic
/// point**, rather than from our worker thread whenever it happens to notice.
/// It matters because that work mutates engine-owned global state --
/// `pfnAddCommand` prepends to the engine's command list, and `sound_fix`
/// overwrites a function pointer inside the live `event_api_s` -- none of
/// which is thread-safe against an engine that may be running concurrently.
/// Registering right after `Initialize` also matches where DoD's own client
/// registers its 110 console commands (from `HUD_Init`, immediately after).
///
/// Must be called before `install()`, so there is no window in which
/// `Initialize` could fire before the callback is set.
pub fn set_on_engine_ready(callback: fn()) {
    let _ = ON_ENGINE_READY.set(callback);
}

/// Captures `pEnginefuncs` -- the whole reason this crate exists -- then hands
/// straight off to `client.dll`'s real `Initialize` so the game is unaffected.
unsafe extern "C" fn tramp_initialize(engfuncs: *mut ClEngineFuncsPartial, version: i32) -> i32 {
    if !engfuncs.is_null() {
        ENGFUNCS.store(engfuncs, Ordering::Release);
        unsafe { crate::debug::report(&format!("Initialize: captured pEngfuncs at {engfuncs:p} (interface version {version})")) };
    } else {
        unsafe { crate::debug::report("Initialize: engine passed a null pEnginefuncs -- nothing captured") };
    }

    let real = REAL_INITIALIZE.load(Ordering::Acquire);
    if real.is_null() {
        // Can't happen (we only ever install the trampoline after storing the
        // real pointer), but returning "wrong interface version" is the
        // engine's own documented failure signal, so fail visibly, not weirdly.
        return 0;
    }
    let real: InitializeFn = unsafe { std::mem::transmute(real) };
    let result = unsafe { real(engfuncs, version) };

    // Only once the client is genuinely initialised, and only if we actually
    // captured the table -- installing against a null pEngfuncs would crash
    // the game rather than merely fail.
    // gEngfuncs is only populated once the real Initialize has run, so this
    // has to come after it.
    unsafe { watch_client_commands() };

    if !engfuncs.is_null()
        && let Some(callback) = ON_ENGINE_READY.get()
    {
        callback();
    }

    result
}

/// Real per-frame tick. Runs `client.dll`'s own `HUD_Frame` first so our
/// callback observes the state the engine just finished producing.
/// Seconds of playback elapsed, as raw f64 bits, summed from the per-frame
/// deltas `HUD_Frame` is passed.
///
/// Free: the engine already passes the delta every frame and it was otherwise
/// discarded. Playback time is what a log line has to be matched against
/// something seen on screen -- wall-clock time cannot be, once playback is
/// paused, seeked or fast-forwarded.
///
/// The value `HUD_Frame(double time)` receives is the frame's *duration*
/// (`host_frametime`, 6-11ms at ordinary framerates), **not** a clock. Storing
/// it directly was a bug: every log line read `[demo 0.006]` no matter how far
/// into the demo it was, and `anim_fix`'s fire-dedup window ended up comparing
/// two frame durations, which put it permanently inside its own window and
/// suppressed every firing animation. Summing the deltas gives the elapsed
/// reading both uses actually wanted.
///
/// Elapsed since the client loaded rather than since the current demo started:
/// loading a second demo in the same session does not reset it, because the
/// engine gives no clean signal to reset on. Monotonic within a session, which
/// is all either caller needs.
static DEMO_ELAPSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Seconds of playback elapsed, or 0 before the first frame.
pub fn client_time() -> f64 {
    f64::from_bits(DEMO_ELAPSED.load(Ordering::Relaxed))
}

unsafe extern "C" fn tramp_hud_frame(time: f64) {
    // Only ever called from the engine thread, so a plain load/store pair needs
    // no stronger ordering than this.
    let elapsed = f64::from_bits(DEMO_ELAPSED.load(Ordering::Relaxed));
    // Guard against a nonsense delta rather than letting it poison the running
    // total permanently: a hitch, a breakpoint or a paused demo can hand back a
    // very large frametime, and NaN would make every later comparison false.
    if time.is_finite() && time > 0.0 {
        DEMO_ELAPSED.store((elapsed + time.min(1.0)).to_bits(), Ordering::Relaxed);
    }

    let real = REAL_HUD_FRAME.load(Ordering::Acquire);
    if !real.is_null() {
        let real: HudFrameFn = unsafe { std::mem::transmute(real) };
        unsafe { real(time) };
    }

    if !HUD_FRAME_DRIVING.swap(true, Ordering::AcqRel) {
        unsafe { crate::debug::report("HUD_Frame: first real per-frame callback received") };
    }
    run_per_frame_callback();
}

// ---------------------------------------------------------------------------
// Watching what client.dll tells the engine to do
// ---------------------------------------------------------------------------

/// `pfnClientCmd`'s index in `cl_enginefunc_t`, verified against the binary --
/// DoD's client calls this slot 12 times.
const ENGFUNCS_SLOT_CLIENT_CMD: usize = 20;

type ClientCmdFn = unsafe extern "C" fn(*const c_char);
static REAL_CLIENT_CMD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Logs every console command `client.dll` issues to itself.
///
/// The motivating case is DoD's own cvar-enforcement routine, which runs from
/// `CHud::Redraw` -- every rendered frame -- and responds to `r_drawentities`
/// or `cl_lw` not being 1 by forcing the value back, printing a warning, and
/// then issuing `quit`. It assembles that `quit` byte-by-byte on the stack
/// rather than storing it as a literal, so nothing in the binary's strings
/// gives it away, and if the process exits before `qconsole.log` flushes there
/// is no trace at all. Catching it here is the only reliable way to tell "the
/// game quit on purpose" from "the game crashed". See issue #205.
unsafe extern "C" fn hook_client_cmd(text: *const c_char) {
    if !text.is_null() {
        let raw = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned();
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("quit") {
            unsafe {
                crate::debug::report(
                    "!!! client.dll issued 'quit' -- DoD is closing itself deliberately, not crashing. \
                     Its cvar enforcement does this when r_drawentities or cl_lw is not 1 (issue #205); \
                     the preceding console line names which one.",
                )
            };
        } else {
            unsafe { crate::debug::report(&format!("client.dll ClientCmd: \"{}\"", trimmed.escape_debug())) };
        }
    }

    let real = REAL_CLIENT_CMD.load(Ordering::Acquire);
    if !real.is_null() {
        let real: ClientCmdFn = unsafe { std::mem::transmute(real) };
        unsafe { real(text) };
    }
}

/// Finds `client.dll`'s own copy of `cl_enginefunc_t` by reading it straight
/// out of the real `Initialize`, which begins by memcpy-ing the engine's table
/// into a fixed global:
///
/// ```text
///   b9 87 00 00 00     mov ecx, 0x87        ; 135 dwords = sizeof(cl_enginefunc_t)
///   ...
///   bf <imm32>         mov edi, gEngfuncs   ; <- the address we want
///   50                 push eax
///   f3 a5              rep movsd
/// ```
///
/// The immediate is read from the *loaded* module, so the loader has already
/// applied relocations to it and it is the runtime address.
///
/// Safety: `real_initialize` must point at the real, mapped `Initialize`.
unsafe fn find_gengfuncs(real_initialize: *const u8) -> Option<*mut *mut c_void> {
    let window = unsafe { std::slice::from_raw_parts(real_initialize, 0x60) };
    for k in 0..window.len().saturating_sub(8) {
        if window[k] == 0xBF && window[k + 5] == 0x50 && window[k + 6] == 0xF3 && window[k + 7] == 0xA5 {
            let addr = u32::from_le_bytes([window[k + 1], window[k + 2], window[k + 3], window[k + 4]]) as usize;
            // Must land inside client.dll's own image, or the pattern matched
            // something that only looked like the prologue.
            let base = CLIENT_DLL.load(Ordering::Acquire) as usize;
            if base != 0 && addr > base && addr < base + 0x0400_0000 {
                return Some(addr as *mut *mut c_void);
            }
        }
    }
    None
}

/// Redirects `client.dll`'s own `pfnClientCmd` through `hook_client_cmd`.
///
/// This patches one function pointer inside a struct client.dll owns, rather
/// than swapping a `cldll_func_t` slot the engine also uses -- deliberately,
/// after hooking `V_CalcRefdef` that way closed the game.
unsafe fn watch_client_commands() {
    let real_initialize = REAL_INITIALIZE.load(Ordering::Acquire) as *const u8;
    if real_initialize.is_null() {
        return;
    }
    let Some(gengfuncs) = (unsafe { find_gengfuncs(real_initialize) }) else {
        unsafe { crate::debug::report("client-cmd watch: couldn't locate client.dll's gEngfuncs copy; not watching") };
        return;
    };

    let slot = unsafe { gengfuncs.add(ENGFUNCS_SLOT_CLIENT_CMD) };
    let real = unsafe { *slot };
    if real.is_null() {
        unsafe { crate::debug::report("client-cmd watch: pfnClientCmd slot is null; not watching") };
        return;
    }
    REAL_CLIENT_CMD.store(real, Ordering::Release);

    unsafe {
        let mut old: PAGE_PROTECTION_FLAGS = 0;
        if VirtualProtect(slot as *mut c_void, size_of::<*mut c_void>(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            crate::debug::report("client-cmd watch: couldn't make the slot writable; not watching");
            return;
        }
        *slot = hook_client_cmd as *mut c_void;
        VirtualProtect(slot as *mut c_void, size_of::<*mut c_void>(), old, &mut old);
        crate::debug::report(&format!(
            "client-cmd watch: gEngfuncs at {gengfuncs:p}, pfnClientCmd was {real:p} -- now logging what client.dll runs"
        ));
    }
}

/// Captures `pstudio`, which `anim_fix` needs to read a model's sequence
/// labels.
unsafe extern "C" fn tramp_get_studio_model_interface(
    version: i32,
    ppinterface: *mut *mut c_void,
    pstudio: *mut EngineStudioApiPartial,
) -> i32 {
    if !pstudio.is_null() {
        ENGINE_STUDIO.store(pstudio, Ordering::Release);
        unsafe { crate::debug::report(&format!("HUD_GetStudioModelInterface: captured pstudio at {pstudio:p} (studio interface version {version})")) };
    }

    let real = REAL_GET_STUDIO_MODEL_INTERFACE.load(Ordering::Acquire);
    if real.is_null() {
        return 0;
    }
    let real: GetStudioModelInterfaceFn = unsafe { std::mem::transmute(real) };
    unsafe { real(version, ppinterface, pstudio) }
}

/// Swaps one slot of the `cldll_func_t` table `F` just filled for our own
/// trampoline, stashing the real pointer so the trampoline can chain to it.
///
/// Safety: `table` must point to at least `CLDLL_FUNC_SLOTS` writable
/// pointer-sized slots (guaranteed by `F`'s own contract -- it just wrote
/// exactly that many).
unsafe fn swap_slot(
    table: *mut *mut c_void,
    slot: usize,
    real_store: &AtomicPtr<c_void>,
    trampoline: *mut c_void,
    name: &str,
) {
    let entry = unsafe { table.add(slot) };
    let real = unsafe { *entry };
    if real.is_null() {
        unsafe { crate::debug::report(&format!("F: slot {slot} ({name}) is null -- leaving it alone")) };
        return;
    }
    real_store.store(real, Ordering::Release);
    unsafe { *entry = trampoline };
    unsafe { crate::debug::report(&format!("F: hooked slot {slot} ({name}), real implementation at {real:p}")) };
}

/// Our stand-in for `client.dll`'s secured `F` export. Lets the real `F` fill
/// the engine's `cldll_func_t` buffer exactly as it normally would, then
/// replaces the three slots we care about before the engine ever reads them.
unsafe extern "C" fn hook_f(table: *mut *mut c_void) {
    let real = REAL_F.load(Ordering::Acquire);
    if real.is_null() {
        unsafe { crate::debug::report("F: real export pointer missing -- cannot forward, client will fail to initialise") };
        return;
    }
    let real: ClientApiFn = unsafe { std::mem::transmute(real) };
    unsafe { real(table) };

    if table.is_null() {
        unsafe { crate::debug::report("F: engine passed a null table -- nothing hooked") };
        return;
    }

    unsafe {
        swap_slot(table, SLOT_INITIALIZE, &REAL_INITIALIZE, tramp_initialize as *mut c_void, "Initialize");
        swap_slot(table, SLOT_HUD_FRAME, &REAL_HUD_FRAME, tramp_hud_frame as *mut c_void, "HUD_Frame");
        swap_slot(
            table,
            SLOT_GET_STUDIO_MODEL_INTERFACE,
            &REAL_GET_STUDIO_MODEL_INTERFACE,
            tramp_get_studio_model_interface as *mut c_void,
            "HUD_GetStudioModelInterface",
        );
    }
}

// ---------------------------------------------------------------------------
// hw.dll IAT hooks
// ---------------------------------------------------------------------------

type LoadLibraryAFn = unsafe extern "system" fn(*const u8) -> HMODULE;
type GetProcAddressFn = unsafe extern "system" fn(HMODULE, *const u8) -> *mut c_void;
static REAL_LOAD_LIBRARY_A: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GET_PROC_ADDRESS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// The `client.dll` module handle, as observed coming back out of
/// `LoadLibraryA`. Used to tell *its* `GetProcAddress` lookups apart from the
/// many others `hw.dll` makes against other modules.
static CLIENT_DLL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The engine loads the mod's client DLL by a path relative to the game exe
/// (e.g. `dod\cl_dlls\client.dll`), never by the bare filename -- so this
/// compares only the path's basename, not the whole string.
unsafe fn path_basename_eq_ignore_ascii_case(ptr: *const u8, other: &str) -> bool {
    unsafe {
        if ptr.is_null() {
            return false;
        }
        let path = std::ffi::CStr::from_ptr(ptr as *const i8).to_string_lossy();
        let basename = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        basename.eq_ignore_ascii_case(other)
    }
}

unsafe extern "system" fn hook_load_library_a(lp_lib_file_name: *const u8) -> HMODULE {
    let real = REAL_LOAD_LIBRARY_A.load(Ordering::Acquire);
    let real: LoadLibraryAFn = unsafe { std::mem::transmute(real) };
    let result = unsafe { real(lp_lib_file_name) };

    if !result.is_null() && unsafe { path_basename_eq_ignore_ascii_case(lp_lib_file_name, "client.dll") } {
        CLIENT_DLL.store(result, Ordering::Release);
        unsafe { crate::debug::report(&format!("LoadLibraryA: client.dll loaded at {result:p}, watching its GetProcAddress lookups")) };
    }

    result
}

/// Intercepts every export `hw.dll` resolves out of `client.dll`, and swaps in
/// our own entry points for the ones we need. See the module docs for why this
/// -- rather than patching `client.dll`'s export table -- is the hook point
/// that actually works.
unsafe extern "system" fn hook_get_proc_address(module: HMODULE, name: *const u8) -> *mut c_void {
    let real = REAL_GET_PROC_ADDRESS.load(Ordering::Acquire);
    let real: GetProcAddressFn = unsafe { std::mem::transmute(real) };
    let result = unsafe { real(module, name) };

    let client = CLIENT_DLL.load(Ordering::Acquire);
    if client.is_null() || !std::ptr::eq(module, client) || result.is_null() {
        return result;
    }

    // An import can be requested by ordinal instead of by name, in which case
    // `name` is not a pointer at all -- it's the ordinal in its low word, and
    // dereferencing it would fault.
    if (name as usize) >> 16 == 0 {
        return result;
    }
    let Ok(requested) = (unsafe { std::ffi::CStr::from_ptr(name as *const c_char) }).to_str() else {
        return result;
    };

    unsafe { crate::debug::report(&format!("GetProcAddress(client.dll, \"{requested}\") -> {result:p}")) };

    // "F" is the secured single-callback export, and is what DoD 1.3 actually
    // uses; the three named entries below are the classic convention, kept so
    // this works unchanged on a non-secured client.dll too.
    match requested {
        "F" => {
            REAL_F.store(result, Ordering::Release);
            unsafe { crate::debug::report("GetProcAddress: secured \"F\" export intercepted -- returning our wrapper") };
            hook_f as *mut c_void
        }
        "Initialize" => {
            REAL_INITIALIZE.store(result, Ordering::Release);
            tramp_initialize as *mut c_void
        }
        "HUD_Frame" => {
            REAL_HUD_FRAME.store(result, Ordering::Release);
            tramp_hud_frame as *mut c_void
        }
        "HUD_GetStudioModelInterface" => {
            REAL_GET_STUDIO_MODEL_INTERFACE.store(result, Ordering::Release);
            tramp_get_studio_model_interface as *mut c_void
        }
        _ => result,
    }
}

/// Waits (briefly) for `module_name` to be loaded -- we're injected before the
/// engine's own modules exist, so this can't assume it's already there.
fn wait_for_module(module_name: &str, timeout_ms: u32) -> Option<HMODULE> {
    let module_name_c = format!("{module_name}\0");
    let mut waited = 0u32;
    loop {
        let h = unsafe { GetModuleHandleA(module_name_c.as_ptr()) };
        if !h.is_null() {
            return Some(h);
        }
        if waited >= timeout_ms {
            return None;
        }
        unsafe { Sleep(25) };
        waited += 25;
    }
}

/// Overwrites one of `module`'s IAT slots so its own calls to
/// `KERNEL32!<import_name>` route through `replacement`, stashing the original
/// in `real_store`. Only that module's calls are affected -- unlike
/// inline-patching the function itself, which would redirect the whole
/// process (HLAE's `CAfxImportDllHook` makes the same trade-off).
fn hook_import(
    module: HMODULE,
    import_name: &str,
    replacement: *mut c_void,
    real_store: &AtomicPtr<c_void>,
) -> bool {
    let Some(slot) = (unsafe { pe::find_iat_slot(module as *mut u8, "KERNEL32.dll", import_name) })
    else {
        return false;
    };

    unsafe {
        real_store.store(*slot, Ordering::Release);

        let mut old_protect: PAGE_PROTECTION_FLAGS = 0;
        let ok = VirtualProtect(
            slot as *mut c_void,
            size_of::<*mut c_void>(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protect,
        );
        if ok == 0 {
            return false;
        }
        *slot = replacement;
        VirtualProtect(
            slot as *mut c_void,
            size_of::<*mut c_void>(),
            old_protect,
            &mut old_protect,
        );
    }

    true
}

/// Installs both `hw.dll` IAT hooks: `LoadLibraryA` (to learn `client.dll`'s
/// module handle) and `GetProcAddress` (to intercept how the engine resolves
/// `client.dll`'s entry points). Call once, from a background thread -- never
/// do this work directly in `DllMain` (see `lib.rs`).
pub fn install() {
    // GoldSrc's OpenGL renderer module is virtually always what's actually
    // loaded in any modern setup (HLAE's own recording pipeline requires
    // it); `sw.dll` (the old software renderer) is not handled here.
    let Some(hw) = wait_for_module("hw.dll", 15_000) else {
        unsafe { crate::debug::report("goldsrc-hooks: hw.dll never appeared -- sound/animation fixes are inactive this session.") };
        return;
    };

    // Order matters: GetProcAddress first, so there is no window in which
    // client.dll could load and be resolved before that hook is armed.
    let hooked_gpa = hook_import(hw, "GetProcAddress", hook_get_proc_address as *mut c_void, &REAL_GET_PROC_ADDRESS);
    let hooked_lla = hook_import(hw, "LoadLibraryA", hook_load_library_a as *mut c_void, &REAL_LOAD_LIBRARY_A);

    unsafe {
        crate::debug::report(&format!(
            "goldsrc-hooks: hw.dll IAT hooks -- GetProcAddress: {}, LoadLibraryA: {}",
            if hooked_gpa { "ok" } else { "FAILED" },
            if hooked_lla { "ok" } else { "FAILED" },
        ))
    };

    if !hooked_gpa || !hooked_lla {
        unsafe { crate::debug::report("goldsrc-hooks: an IAT slot wasn't found -- sound/animation fixes are inactive this session.") };
    }
}

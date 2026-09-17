//! `dodtools_deathmsg` — control over DoD 1.3's death notices (the kill feed).
//!
//! HLAE ships `mirv_deathmsg` with the same four subcommands, but only for
//! `cstrike` and `tfc`: its pattern database names them explicitly
//! (`cstrike_CHudDeathNotice_Draw`, `tfc_rgDeathNoticeList`, …) and there is no
//! `dod_` entry. So none of it works for Day of Defeat, and no amount of
//! configuration makes it. This is the DoD implementation.
//!
//! ## What the game does
//!
//! DoD's `client.dll` uses the stock Half-Life SDK death-notice code unchanged:
//!
//! ```c
//! #define MAX_DEATHNOTICES 4
//! struct DeathNoticeItem {          // 156 bytes, confirmed field-by-field
//!     char  szKiller[64];           // +0x00
//!     char  szVictim[64];           // +0x40
//!     int   iId;                    // +0x80   sprite index; 0 == empty slot
//!     int   iSuicide;               // +0x84
//!     int   iTeamKill;              // +0x88
//!     int   iNonPlayerKill;         // +0x8c
//!     float flDisplayTime;          // +0x90
//!     float *KillerColor;           // +0x94
//!     float *VictimColor;           // +0x98
//! };
//! static DeathNoticeItem rgDeathNoticeList[MAX_DEATHNOTICES + 1];
//! ```
//!
//! `InitHUDData` gives the array away as a `rep stosd` of 0xc3 dwords — 780
//! bytes, exactly five 156-byte slots.
//!
//! ## Two mechanisms, not one
//!
//! `block` and `fake` need no code patching at all, because of how the engine
//! installs user-message handlers. `pfnHookUserMsg` does **not** overwrite an
//! existing entry: it allocates a fresh record, copies the matching one over it
//! (carrying the message number and size across), sets the new handler, and
//! *prepends* it to `gClientUserMsgs`. The by-number dispatcher walks from the
//! head and stops at the first match. So the most recent hook wins, and
//! `client.dll`'s own handler stays reachable by calling it directly. See
//! [`crate::engine::HookUserMsgFn`].
//!
//! `max` and `offset` do need patching, and `max` needs the array **moved**:
//! it is five slots and there is no slack after it — live data is referenced at
//! array-end + 0. Every one of the 34 absolute references to it lives in
//! `.text`, inside four functions, and nothing in `.data` or any vtable points
//! at it, so relocating it is a closed problem. All 34 carry base relocations,
//! which is why every address here is an RVA rebased through
//! [`crate::engine::client_module_base`] rather than a literal.
//!
//! ## DoD's DeathMsg payload differs from CS's
//!
//! Three bytes: killer index, victim index, and a **weapon index** 1..=43 into
//! a table of `d_*` sprite names ([`WEAPON_SPRITES`]) — not a weapon string.
//! Out-of-range falls back to `d_world`. There is no headshot flag, so HLAE's
//! `<0|1>` argument has no DoD equivalent and `fake` takes a weapon instead.
//!
//! Analysis subject: `dod/cl_dlls/client.dll`, 977,816 bytes, byte-identical
//! across the stock, pre-Anniversary and post-Anniversary installs.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use windows_sys::Win32::System::Memory::{
    PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect,
};

use crate::engine;
use crate::names::console_name;

/// Every name this command answers to. `pfnAddCommand` takes a bare
/// `void(*)(void)` and the handler reads its own arguments, so a name costs
/// nothing but an array entry -- add one here to ship a variant spelling, or to
/// keep an old name working across a rename. The prefix itself lives in
/// `names.rs`; nothing here spells it.
pub const COMMAND_NAMES: &[&str] = &[COMMAND];

/// The name used in usage and error text, which is the first one registered.
const COMMAND: &str = console_name!("deathmsg");

// ── Addresses, as RVAs into the analysed client.dll ──────────────────────────

/// `rgDeathNoticeList`.
const ARRAY_RVA: usize = 0x17_65d8;
/// `sizeof(DeathNoticeItem)`.
const ITEM: usize = 156;
/// `MAX_DEATHNOTICES` as shipped.
const STOCK_MAX: i32 = 4;
/// `__MsgFunc_DeathMsg`, the static thunk registered with `pfnHookUserMsg`.
/// It supplies `this = gHUD.m_DeathNotice` and tail-calls the member function.
const THUNK_RVA: usize = 0x2_ad70;

/// Hard ceiling on the line count. Not arbitrary: the two loop bounds are
/// `cmp r32, imm8` and rewriting them wider would not fit the instruction.
const MAX_LINES: i32 = 127;

/// The stock y the notices start at, growing downward.
const STOCK_OFFSET: i32 = 0x14;

/// Every absolute reference to the array, as `(rva_of_the_dword, byte_offset_into_the_array)`.
///
/// Found by scanning the whole image for dwords landing inside the array, not
/// by reading the four functions — that is what makes the set provably closed.
#[rustfmt::skip]
const ARRAY_REFS: &[(usize, usize)] = &[
    (0x2_ae59, 0x000), (0x2_af27, 0x09c), (0x2_af33, 0x040), (0x2_af38, 0x000),
    (0x2_af42, 0x080), (0x2_af50, 0x090), (0x2_afac, 0x090), (0x2_afbd, 0x090),
    (0x2_afc9, 0x090), (0x2_afe4, 0x080), (0x2_b039, 0x084), (0x2_b061, 0x094),
    (0x2_b103, 0x08c), (0x2_b112, 0x098), (0x2_b22d, 0x080), (0x2_b24e, 0x09c),
    (0x2_b253, 0x000), (0x2_b2a0, 0x000), (0x2_b2b8, 0x094), (0x2_b2be, 0x000),
    (0x2_b2d4, 0x01f), (0x2_b2e7, 0x040), (0x2_b2f8, 0x040), (0x2_b302, 0x098),
    (0x2_b310, 0x05f), (0x2_b322, 0x08c), (0x2_b36f, 0x088), (0x2_b37b, 0x084),
    (0x2_b394, 0x080), (0x2_b3ba, 0x08c), (0x2_b3ce, 0x090), (0x2_b410, 0x084),
    (0x2_b448, 0x088),
];

/// `cmp eax, &rgDeathNoticeList[MAX].iId` — the scan loop's end sentinel in
/// `MsgFunc_DeathMsg`. Held apart from [`ARRAY_REFS`] because its value depends
/// on the line count, not just the base.
const SENTINEL_RVA: usize = 0x2_b23d;

/// The count constants, as `(rva_of_the_operand, width_in_bytes, what)`.
#[rustfmt::skip]
const COUNT_SITES: &[(usize, usize, CountKind)] = &[
    //  imm     width                        instruction (at the RVA in the comment)
    (0x2_ae52, 4, CountKind::MemsetDwords),  // +0x2ae51 InitHUDData: b9 mov ecx, (MAX+1)*ITEM/4
    (0x2_af60, 4, CountKind::MemmoveBytes),  // +0x2af5f Draw:        b9 mov ecx, MAX*ITEM
    (0x2_b175, 1, CountKind::Max),           // +0x2b173 Draw:        83 f8 cmp eax, MAX
    (0x2_b245, 1, CountKind::Max),           // +0x2b243 MsgFunc:     83 ff cmp edi, MAX
    (0x2_b249, 4, CountKind::MemmoveBytes),  // +0x2b248 MsgFunc:     68 push MAX*ITEM
    (0x2_b260, 4, CountKind::MaxMinusOne),   // +0x2b25f MsgFunc:     bf mov edi, MAX-1
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CountKind {
    MemsetDwords,
    MemmoveBytes,
    Max,
    MaxMinusOne,
}

impl CountKind {
    fn value_for(self, max: i32) -> u32 {
        let max = max as usize;
        match self {
            CountKind::MemsetDwords => ((max + 1) * ITEM / 4) as u32,
            CountKind::MemmoveBytes => (max * ITEM) as u32,
            CountKind::Max => max as u32,
            CountKind::MaxMinusOne => (max - 1) as u32,
        }
    }
}

/// The two immediates that set the y the first notice draws at. The second is
/// an `add eax, imm8`, which is what caps [`MAX_OFFSET`].
const OFFSET_SITES: &[(usize, usize)] = &[
    (0x2_aef4, 4), // +0x2aef0  c7 44 24 04  mov dword ptr [esp+4], 20
    (0x2_af1b, 1), // +0x2af19  83 c0        add eax, 20
];

/// Ceiling on `offset`, set by the `add eax, imm8` in `Draw`'s spectator
/// branch. 127 screen pixels down from the top is a long way for a kill feed;
/// widening that instruction would overwrite the one after it.
const MAX_OFFSET: i32 = 127;

/// DoD's weapon-sprite table, indexed by the third byte of a `DeathMsg`.
/// Index 0 and anything >= 44 fall back to `d_world`, matching the bounds check
/// in `MsgFunc_DeathMsg` (`test eax,eax; jle` / `cmp eax,0x2c; jge`).
#[rustfmt::skip]
pub const WEAPON_SPRITES: &[&str] = &[
    "",                 "d_amerknife",  "d_gerknife",   "d_colt",
    "d_luger",          "d_garand",     "d_scopedkar",  "d_thompson",
    "d_mp44",           "d_spring",     "d_kar",        "d_bar",
    "d_mp40",           "d_grenade",    "d_stick",      "d_stick",
    "d_grenade",        "d_mg42",       "d_30cal",      "d_spade",
    "d_m1carbine",      "d_mg34",       "d_greasegun",  "d_fg42",
    "d_k43",            "d_enfield",    "d_sten",       "d_bren",
    "d_webley",         "d_bazooka",    "d_pschreck",   "d_piat",
    "d_mortar",         "d_binoculars", "d_satchel",    "d_scopedfg42",
    "d_fcarbine",       "d_bayonet",    "d_scopedenfield", "d_britgrenade",
    "d_britknife",      "d_mortar",     "d_garandbutt", "d_enfbayonet",
];

// ── Runtime state ────────────────────────────────────────────────────────────

/// The line count currently patched in, or 0 while the game is untouched.
static PATCHED_MAX: AtomicI32 = AtomicI32::new(0);
/// The relocated array, allocated once and never moved, so the patch sites can
/// be rewritten for a new count without the base changing under a live demo.
static BUFFER: AtomicUsize = AtomicUsize::new(0);
/// The y currently patched in, or 0 while untouched.
static PATCHED_OFFSET: AtomicI32 = AtomicI32::new(0);
/// Whether our `DeathMsg` handler is installed and should be kept installed.
static HOOK_WANTED: AtomicBool = AtomicBool::new(false);

/// The block list. `Vec<i32>` of player indices; `allow_list` inverts the sense
/// so the listed players are the only ones that get through.
static BLOCK: Mutex<BlockList> = Mutex::new(BlockList { ids: Vec::new(), allow_list: false });

#[derive(Default)]
struct BlockList {
    ids: Vec<i32>,
    allow_list: bool,
}

impl BlockList {
    /// True when a frag between these two should not be shown.
    fn blocks(&self, killer: i32, victim: i32) -> bool {
        if self.ids.is_empty() {
            return false;
        }
        let involved = self.ids.contains(&killer) || self.ids.contains(&victim);
        // An allow-list blocks everything the listed players were *not* part of;
        // a block-list blocks exactly what they were.
        if self.allow_list { !involved } else { involved }
    }
}

// ── Patching ─────────────────────────────────────────────────────────────────

/// Writes `len` bytes over `client.dll`'s own code, flipping the page writable
/// for exactly the length of the write and putting the protection back.
///
/// Safety: `rva` must be a real code offset in the loaded module and `bytes`
/// must be the right width for the operand living there.
unsafe fn write_code(base: usize, rva: usize, bytes: &[u8]) -> bool {
    let addr = (base + rva) as *mut u8;
    let mut old: PAGE_PROTECTION_FLAGS = 0;
    let ok = unsafe { VirtualProtect(addr as *mut c_void, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) };
    if ok == 0 {
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr, bytes.len()) };
    let mut discard: PAGE_PROTECTION_FLAGS = 0;
    unsafe { VirtualProtect(addr as *mut c_void, bytes.len(), old, &mut discard) };
    true
}

unsafe fn read_u32(base: usize, rva: usize) -> u32 {
    unsafe { ((base + rva) as *const u32).read_unaligned() }
}

unsafe fn read_u8(base: usize, rva: usize) -> u8 {
    unsafe { *((base + rva) as *const u8) }
}

/// Checks that every site still holds the value the analysed build shipped.
///
/// This is the one thing standing between a wrong `client.dll` and 40 writes
/// into the middle of unrelated instructions, so it is exhaustive rather than a
/// spot check, and it refuses rather than warns.
unsafe fn verify_stock(base: usize) -> Result<(), String> {
    for &(rva, field) in ARRAY_REFS {
        let want = (base + ARRAY_RVA + field) as u32;
        let got = unsafe { read_u32(base, rva) };
        if got != want {
            return Err(format!(
                "reference at +{rva:#x} reads {got:#x}, expected {want:#x}"
            ));
        }
    }
    let want_sentinel = (base + ARRAY_RVA + STOCK_MAX as usize * ITEM + 0x80) as u32;
    let got = unsafe { read_u32(base, SENTINEL_RVA) };
    if got != want_sentinel {
        return Err(format!(
            "scan sentinel at +{SENTINEL_RVA:#x} reads {got:#x}, expected {want_sentinel:#x}"
        ));
    }
    for &(rva, width, kind) in COUNT_SITES {
        let want = kind.value_for(STOCK_MAX);
        let got = if width == 4 {
            unsafe { read_u32(base, rva) }
        } else {
            u32::from(unsafe { read_u8(base, rva) })
        };
        if got != want {
            return Err(format!("count at +{rva:#x} reads {got:#x}, expected {want:#x}"));
        }
    }
    for &(rva, width) in OFFSET_SITES {
        let got = if width == 4 {
            unsafe { read_u32(base, rva) }
        } else {
            u32::from(unsafe { read_u8(base, rva) })
        };
        if got != STOCK_OFFSET as u32 {
            return Err(format!("y offset at +{rva:#x} reads {got:#x}, expected {STOCK_OFFSET:#x}"));
        }
    }
    Ok(())
}

/// The relocated array, allocated on first use at the full [`MAX_LINES`] size.
///
/// Sized for the ceiling rather than the request so the base never moves: a
/// later `max` change rewrites only the 40 operands, and a demo already running
/// does not have the array shift under it mid-frame.
fn buffer() -> usize {
    let existing = BUFFER.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }
    let slots = (MAX_LINES + 1) as usize;
    let block = vec![0u8; slots * ITEM].into_boxed_slice();
    let addr = Box::leak(block).as_ptr() as usize;
    // Whoever lost the race leaks their allocation; both are valid, and this
    // runs at most twice in a session.
    match BUFFER.compare_exchange(0, addr, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => addr,
        Err(won) => won,
    }
}

/// Points the four death-notice functions at a buffer of `max` slots.
///
/// `max == STOCK_MAX` restores the shipped bytes exactly, array included, so
/// there is always a clean way back.
/// The `client.dll` base [`verify_stock`] has approved, or 0.
static VERIFIED_BASE: AtomicUsize = AtomicUsize::new(0);

/// Whether [`verify_stock`] still has to run for the module at `base`.
///
/// Verification asks whether every site still holds the value the analysed
/// build shipped, so it is only meaningful *before* anything is written. Once a
/// subcommand has patched, our own writes are precisely what it would flag.
/// That is not hypothetical: gating it on "have we changed the line count"
/// meant `offset 100` left `0x64` at a site the next `max` then refused over,
/// because reverting `max` to the stock 4 had reset the flag that suppressed
/// the check. The state "never touched" and the state "put back the way it was"
/// are not the same thing, and one counter cannot hold both.
///
/// Keyed on the base rather than a flag so a reloaded `client.dll` — stock code
/// again, possibly at a different address — is re-verified rather than
/// inheriting the previous module's verdict.
fn needs_verification(verified: usize, base: usize) -> bool {
    verified != base
}

/// Verifies the module at `base` once, and remembers it.
fn ensure_verified(base: usize) -> Result<(), String> {
    if !needs_verification(VERIFIED_BASE.load(Ordering::Acquire), base) {
        return Ok(());
    }
    unsafe { verify_stock(base) }.map_err(|why| {
        format!("this client.dll is not the build these offsets were derived from -- {why}")
    })?;
    // A module that verifies as stock carries none of our patches, whatever a
    // previous module made these say.
    PATCHED_MAX.store(0, Ordering::Release);
    PATCHED_OFFSET.store(0, Ordering::Release);
    VERIFIED_BASE.store(base, Ordering::Release);
    Ok(())
}

fn apply_max(max: i32) -> Result<(), String> {
    let Some(base) = engine::client_module_base() else {
        return Err("client.dll is not loaded yet".to_string());
    };
    if !(STOCK_MAX..=MAX_LINES).contains(&max) {
        return Err(format!("expected {STOCK_MAX}..={MAX_LINES}, got {max}"));
    }

    ensure_verified(base)?;

    let reverting = max == STOCK_MAX;
    let array = if reverting { base + ARRAY_RVA } else { buffer() };

    for &(rva, field) in ARRAY_REFS {
        let value = (array + field) as u32;
        if !unsafe { write_code(base, rva, &value.to_le_bytes()) } {
            return Err(format!("could not make +{rva:#x} writable"));
        }
    }
    let sentinel = (array + max as usize * ITEM + 0x80) as u32;
    if !unsafe { write_code(base, SENTINEL_RVA, &sentinel.to_le_bytes()) } {
        return Err(format!("could not make +{SENTINEL_RVA:#x} writable"));
    }
    for &(rva, width, kind) in COUNT_SITES {
        let value = kind.value_for(max);
        let ok = if width == 4 {
            unsafe { write_code(base, rva, &value.to_le_bytes()) }
        } else {
            unsafe { write_code(base, rva, &[value as u8]) }
        };
        if !ok {
            return Err(format!("could not make +{rva:#x} writable"));
        }
    }

    PATCHED_MAX.store(if reverting { 0 } else { max }, Ordering::Release);
    Ok(())
}

/// Moves the first notice's y. `STOCK_OFFSET` puts the shipped value back.
fn apply_offset(y: i32) -> Result<(), String> {
    let Some(base) = engine::client_module_base() else {
        return Err("client.dll is not loaded yet".to_string());
    };
    if !(0..=MAX_OFFSET).contains(&y) {
        return Err(format!(
            "expected 0..={MAX_OFFSET}, got {y} (the spectator branch's `add eax, imm8` sets the ceiling)"
        ));
    }
    ensure_verified(base)?;
    for &(rva, width) in OFFSET_SITES {
        let ok = if width == 4 {
            unsafe { write_code(base, rva, &(y as u32).to_le_bytes()) }
        } else {
            unsafe { write_code(base, rva, &[y as u8]) }
        };
        if !ok {
            return Err(format!("could not make +{rva:#x} writable"));
        }
    }
    PATCHED_OFFSET.store(if y == STOCK_OFFSET { 0 } else { y }, Ordering::Release);
    Ok(())
}

// ── The DeathMsg hook ────────────────────────────────────────────────────────

/// `client.dll`'s own `__MsgFunc_DeathMsg`, which we forward to.
fn original_thunk() -> Option<engine::UserMsgHookFn> {
    let base = engine::client_module_base()?;
    // Safety: THUNK_RVA is a code offset in the module, and the signature is
    // the engine's own pfnUserMsgHook.
    Some(unsafe { std::mem::transmute::<usize, engine::UserMsgHookFn>(base + THUNK_RVA) })
}

/// Our `DeathMsg` handler. Reads the three bytes, drops the message if the
/// block list says so, and otherwise hands it to the game untouched.
///
/// Deliberately does not parse or rewrite the payload on the way through: a
/// forwarded message is the *same* buffer the engine handed us, so a message
/// that is not being blocked behaves exactly as if we were not here.
unsafe extern "C" fn hooked_death_msg(name: *const c_char, size: i32, buf: *mut c_void) -> i32 {
    if size >= 3 && !buf.is_null() {
        let bytes = unsafe { std::slice::from_raw_parts(buf as *const u8, 3) };
        let (killer, victim) = (bytes[0] as i32, bytes[1] as i32);
        let blocked = BLOCK.lock().map(|list| list.blocks(killer, victim)).unwrap_or(false);
        if blocked {
            // 1 is what the engine's own dispatcher treats as handled.
            return 1;
        }
    }
    match original_thunk() {
        Some(original) => unsafe { original(name, size, buf) },
        None => 1,
    }
}

/// Installs (or re-installs) our handler.
///
/// Safe to call every frame: `pfnHookUserMsg` returns early without allocating
/// when the first record matching the name already carries this exact handler,
/// so the steady state costs one `stricmp` and leaks nothing. The engine frees
/// the whole message list on disconnect and `client.dll` re-hooks its own
/// handler on the next connect, which is precisely when we need to prepend
/// ourselves again — so the repetition is the mechanism, not waste.
fn install_hook() {
    let Some(engfuncs) = engine::engfuncs() else { return };
    let Ok(name) = CString::new("DeathMsg") else { return };
    unsafe { (engfuncs.pfn_hook_user_msg)(name.as_ptr(), hooked_death_msg) };
}

/// Called once per frame from [`crate::commands::poll`].
pub fn poll() {
    if HOOK_WANTED.load(Ordering::Relaxed) {
        install_hook();
    }
}

/// Field offsets inside a `DeathNoticeItem` that [`restamp_display_time`] needs.
const FIELD_ID: usize = 0x80;
const FIELD_DISPLAY_TIME: usize = 0x90;

const _: () = assert!(
    FIELD_DISPLAY_TIME + size_of::<f32>() <= ITEM,
    "flDisplayTime runs past the end of a DeathNoticeItem"
);

/// Where the live array is right now: the relocated buffer once `max` has
/// grown it, otherwise `client.dll`'s own.
fn current_array(base: usize) -> (usize, i32) {
    let max = PATCHED_MAX.load(Ordering::Acquire);
    if max == 0 { (base + ARRAY_RVA, STOCK_MAX) } else { (buffer(), max) }
}

/// Re-dates the notice `fake` just added, so it lives from *now*.
///
/// `MsgFunc_DeathMsg` stamps `flDisplayTime` as `gHUD.m_flTime +
/// (int)hud_deathnotice_time`, and `Draw` drops any entry whose stamp is older
/// than the frame time it is handed:
///
/// ```asm
/// +0x2b3be  fild dword ptr [0x19c33d8]      ; (int)hud_deathnotice_time
/// +0x2b3c4  fadd dword ptr [0x1a080cc]      ; + gHUD.m_flTime
/// +0x2b3cc  fstp dword ptr [esi + 0x1a76668]  ; -> flDisplayTime
///
/// +0x2af4e  fld  dword ptr [edi + 0x1a76668]
/// +0x2af54  fcomp dword ptr [esp + 0x5c]    ; vs Draw's flTime argument
/// +0x2af5d  jp   ...                        ; else memmove the entry away
/// ```
///
/// `m_flTime` is only refreshed by `CHud::Redraw`, which does not run while the
/// console is down — the same reason DoD's `cl_lw` suicide does not fire until
/// the console closes. So a notice typed at the console is stamped with
/// whatever time the HUD last saw, and the first `Draw` after the console
/// closes compares that stale stamp against a live clock and deletes it before
/// it is ever drawn. Typing several and closing the console showed nothing at
/// all; closing, reopening and typing one showed it, because that brief close
/// let `Redraw` catch `m_flTime` up.
///
/// A real notice never hits this: it arrives while the game is drawing. So the
/// fix belongs here rather than in the hook.
fn restamp_display_time(base: usize) {
    let Some(engfuncs) = engine::engfuncs() else { return };
    let Ok(cvar) = CString::new("hud_deathnotice_time") else { return };
    // Truncated, because that is what the game does to it.
    let lifetime = unsafe { (engfuncs.pfn_get_cvar_float)(cvar.as_ptr()) }.trunc();
    let expires = engine::client_time() as f32 + lifetime;

    // MsgFunc fills the first slot whose iId is 0, so the notice it just added
    // is the last occupied one.
    let (array, slots) = current_array(base);
    for slot in (0..slots as usize).rev() {
        let item = array + slot * ITEM;
        // Safety: `array` is either the leaked buffer or client.dll's own
        // `.data`, and `slot` is inside the count the code was patched for.
        if unsafe { std::ptr::read_unaligned((item + FIELD_ID) as *const i32) } != 0 {
            unsafe { std::ptr::write_unaligned((item + FIELD_DISPLAY_TIME) as *mut f32, expires) };
            return;
        }
    }
}

/// Feeds the game a death notice that never happened.
fn fake(killer: i32, victim: i32, weapon: i32) -> Result<(), String> {
    let Some(original) = original_thunk() else {
        return Err("client.dll is not loaded yet".to_string());
    };
    for (what, value) in [("killer", killer), ("victim", victim)] {
        if !(0..=255).contains(&value) {
            return Err(format!("{what} index {value} is out of range (0..=255)"));
        }
    }
    let mut payload = [killer as u8, victim as u8, weapon as u8];
    let Ok(name) = CString::new("DeathMsg") else {
        return Err("could not build the message name".to_string());
    };

    // `MsgFunc_DeathMsg` reaches `gViewPort->DeathMsg`, which reaches a thunk
    // at `client.dll+0x20520` that is exactly three instructions:
    //
    //     call [gEngfuncs + 0xcc]   ; GetLocalPlayer, slot 51
    //     mov  eax, [eax]           ; ->index
    //     ret
    //
    // There is no null check, and the path is unconditional -- every death
    // notice, real or faked, goes through it. Whatever the engine hands back
    // gets dereferenced. So check it here, because the alternative is not an
    // error message but the game vanishing: that is how this was found, at
    // `reading 0xbb8` with `eax = 0xbb8`, a pointer computed off a null base.
    let Some(engfuncs) = engine::engfuncs() else {
        return Err("the engine function table is not available yet".to_string());
    };
    let local_player = unsafe { (engfuncs.get_local_player)() } as usize;
    if !crate::crash::readable(local_player, 4) {
        return Err(format!(
            "{COMMAND} fake: no level is loaded -- GetLocalPlayer() is {local_player:#x}, which client.dll would dereference without checking and take the game down. Load a demo first.\n"
        ));
    }
    // Straight to client.dll's own handler, deliberately bypassing our hook:
    // a message you asked for by hand should not then be filtered by the block
    // list, and the engine is not involved in dispatching it either way.
    //
    // Bracketed by log lines because this is the one place the DLL hands
    // control to game code that was never written to be called from here: if
    // the game dies inside it, the missing "returned" line is what says so, and
    // says it even though the process is gone before anything else can report.
    unsafe {
        crate::debug::report(&format!(
            "deathmsg: fake -- calling client.dll __MsgFunc_DeathMsg at {:#x} with killer={killer} victim={victim} weapon={weapon} ({})",
            original as usize,
            WEAPON_SPRITES.get(weapon as usize).copied().unwrap_or("?"),
        ))
    };
    unsafe { original(name.as_ptr(), payload.len() as i32, payload.as_mut_ptr() as *mut c_void) };
    unsafe { crate::debug::report("deathmsg: fake -- returned cleanly") };
    if let Some(base) = engine::client_module_base() {
        restamp_display_time(base);
    }
    Ok(())
}

// ── Console surface ──────────────────────────────────────────────────────────

fn usage() -> String {
    format!(
        "usage:\n\
         \x20 {COMMAND} max <{STOCK_MAX}..{MAX_LINES}>      lines of kill feed shown at once (default {STOCK_MAX})\n\
         \x20 {COMMAND} offset <0..{MAX_OFFSET}>     y the feed starts at (default {STOCK_OFFSET})\n\
         \x20 {COMMAND} offset default      put the y back\n\
         \x20 {COMMAND} block <id>...       hide frags involving these players\n\
         \x20 {COMMAND} block !<id>...      hide everything EXCEPT these players\n\
         \x20 {COMMAND} block clear         stop hiding anything\n\
         \x20 {COMMAND} fake <killer> <victim> <weapon>\n\
         \x20                               weapon is a name (d_garand, garand) or 1..43\n"
    )
}

fn status() -> String {
    let max = PATCHED_MAX.load(Ordering::Acquire);
    let offset = PATCHED_OFFSET.load(Ordering::Acquire);
    let list = BLOCK.lock();
    let block = match list {
        Ok(ref l) if l.ids.is_empty() => "nothing".to_string(),
        Ok(ref l) => {
            let ids: Vec<String> = l.ids.iter().map(|i| i.to_string()).collect();
            if l.allow_list {
                format!("everything except players {}", ids.join(", "))
            } else {
                format!("frags involving players {}", ids.join(", "))
            }
        }
        Err(_) => "unknown".to_string(),
    };
    format!(
        "{COMMAND}: max = {} line(s), offset = y {}, blocking {block}\n",
        if max == 0 { STOCK_MAX } else { max },
        if offset == 0 { STOCK_OFFSET } else { offset },
    )
}

/// Resolves a weapon argument: an index 1..=43, or a sprite name with or
/// without the `d_` the table uses.
fn parse_weapon(arg: &str) -> Result<i32, String> {
    if let Ok(index) = arg.parse::<i32>() {
        if (1..WEAPON_SPRITES.len() as i32).contains(&index) {
            return Ok(index);
        }
        return Err(format!("weapon index {index} is out of range (1..={})", WEAPON_SPRITES.len() - 1));
    }
    let wanted = arg.trim().to_ascii_lowercase();
    let with_prefix = if wanted.starts_with("d_") { wanted.clone() } else { format!("d_{wanted}") };
    for (index, name) in WEAPON_SPRITES.iter().enumerate().skip(1) {
        if *name == with_prefix {
            return Ok(index as i32);
        }
    }
    Err(format!("no weapon called {arg:?} -- try a name like d_garand, or an index 1..={}", WEAPON_SPRITES.len() - 1))
}

fn args() -> Vec<String> {
    let Some(engfuncs) = engine::engfuncs() else { return Vec::new() };
    let argc = unsafe { (engfuncs.cmd_argc)() };
    (0..argc)
        .filter_map(|i| {
            let ptr = unsafe { (engfuncs.cmd_argv)(i) };
            if ptr.is_null() {
                return None;
            }
            Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
        })
        .collect()
}

/// Runs one subcommand, returning what to print.
fn dispatch(argv: &[String]) -> String {
    // argv[0] is the command name itself, so a bare invocation is a query.
    let Some(sub) = argv.get(1) else {
        return format!("{}{}", status(), usage());
    };

    match sub.to_ascii_lowercase().as_str() {
        "max" => {
            let Some(value) = argv.get(2) else {
                return format!("{}{}", status(), usage());
            };
            match value.parse::<i32>() {
                Ok(n) => match apply_max(n) {
                    Ok(()) => format!("{COMMAND}: max = {n} line(s)\n"),
                    Err(why) => format!("{COMMAND} max: {why}\n"),
                },
                Err(_) => format!("{COMMAND} max: expected a number, got {value:?}\n"),
            }
        }
        "offset" => {
            let Some(value) = argv.get(2) else {
                return format!("{}{}", status(), usage());
            };
            let wanted = if value.eq_ignore_ascii_case("default") {
                Ok(STOCK_OFFSET)
            } else {
                value.parse::<i32>().map_err(|_| ())
            };
            match wanted {
                Ok(y) => match apply_offset(y) {
                    Ok(()) => format!("{COMMAND}: offset = y {y}\n"),
                    Err(why) => format!("{COMMAND} offset: {why}\n"),
                },
                Err(()) => format!("{COMMAND} offset: expected a number or \"default\", got {value:?}\n"),
            }
        }
        "block" => {
            let rest = &argv[2..];
            if rest.is_empty() {
                return status();
            }
            if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                if let Ok(mut list) = BLOCK.lock() {
                    list.ids.clear();
                    list.allow_list = false;
                }
                return format!("{COMMAND}: blocking nothing\n");
            }
            let mut ids = Vec::new();
            let mut allow_list = false;
            for token in rest {
                let (negated, digits) = match token.strip_prefix('!') {
                    Some(d) => (true, d),
                    None => (false, token.as_str()),
                };
                match digits.parse::<i32>() {
                    Ok(id) => {
                        allow_list |= negated;
                        ids.push(id);
                    }
                    Err(_) => return format!("{COMMAND} block: {token:?} is not a player index\n"),
                }
            }
            // A mixed list has no coherent reading -- "block everyone except 3,
            // and also block 5" is two different questions -- so say so rather
            // than pick one.
            let negations = rest.iter().filter(|t| t.starts_with('!')).count();
            if negations != 0 && negations != rest.len() {
                return format!(
                    "{COMMAND} block: mix of plain and !-prefixed ids. Use all-plain to hide those \
                     players, or all-! to hide everyone else.\n"
                );
            }
            if let Ok(mut list) = BLOCK.lock() {
                list.ids = ids;
                list.allow_list = allow_list;
            }
            HOOK_WANTED.store(true, Ordering::Relaxed);
            install_hook();
            status()
        }
        "fake" => {
            let (Some(k), Some(v), Some(w)) = (argv.get(2), argv.get(3), argv.get(4)) else {
                return format!("{COMMAND} fake: need <killer> <victim> <weapon>\n{}", usage());
            };
            let (Ok(killer), Ok(victim)) = (k.parse::<i32>(), v.parse::<i32>()) else {
                return format!("{COMMAND} fake: killer and victim must be player indices\n");
            };
            match parse_weapon(w) {
                Ok(weapon) => match fake(killer, victim, weapon) {
                    Ok(()) => format!(
                        "{COMMAND}: faked {killer} -> {victim} with {}\n",
                        WEAPON_SPRITES[weapon as usize]
                    ),
                    Err(why) => format!("{COMMAND} fake: {why}\n"),
                },
                Err(why) => format!("{COMMAND} fake: {why}\n"),
            }
        }
        other => format!("{COMMAND}: no subcommand {other:?}\n{}", usage()),
    }
}

pub unsafe extern "C" fn command() {
    let argv = args();
    let reply = dispatch(&argv);
    crate::commands::console_print(&reply);
    unsafe { crate::debug::report(&format!("deathmsg: {} -> {}", argv.join(" "), reply.trim())) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restamped_fields_are_ones_the_game_itself_indexes() {
        // FIELD_ID and FIELD_DISPLAY_TIME are written straight into the array,
        // bypassing the code that normally maintains it, so they have to be the
        // offsets client.dll actually uses -- not ones taken from the SDK
        // header and assumed to have survived DoD's build. Every field the game
        // touches shows up in ARRAY_REFS, which is derived from the binary.
        for field in [FIELD_ID, FIELD_DISPLAY_TIME] {
            assert!(
                ARRAY_REFS.iter().any(|&(_, offset)| offset == field),
                "client.dll never indexes +{field:#x}; restamping it would write into a field it does not use"
            );
        }
    }

    #[test]
    fn a_module_is_verified_once_and_a_reloaded_one_again() {
        // Never seen: must verify.
        assert!(needs_verification(0, 0x1000_0000));
        // Already approved: must not re-run, or our own patched bytes read as
        // a mismatch -- which is the bug that sent `max` into "this client.dll
        // is not the build these offsets were derived from" after `offset`
        // had legitimately written a non-stock value.
        assert!(!needs_verification(0x1000_0000, 0x1000_0000));
        // client.dll reloaded elsewhere: stock code again, verify again.
        assert!(needs_verification(0x1000_0000, 0x2000_0000));
    }

    #[test]
    fn count_constants_match_the_shipped_values() {
        // The six operands as they are in the untouched client.dll. If any of
        // these drift, `verify_stock` would start refusing a correct module.
        assert_eq!(CountKind::MemsetDwords.value_for(STOCK_MAX), 0xc3);
        assert_eq!(CountKind::MemmoveBytes.value_for(STOCK_MAX), 0x270);
        assert_eq!(CountKind::Max.value_for(STOCK_MAX), 4);
        assert_eq!(CountKind::MaxMinusOne.value_for(STOCK_MAX), 3);
    }

    #[test]
    fn the_item_stride_and_slot_count_agree_with_the_memset() {
        // InitHUDData clears 0xc3 dwords; that has to be exactly MAX+1 slots,
        // which is the check that proves the 156-byte stride.
        assert_eq!(0xc3 * 4, (STOCK_MAX as usize + 1) * ITEM);
    }

    #[test]
    fn every_count_fits_its_operand_at_the_ceiling() {
        for &(_, width, kind) in COUNT_SITES {
            let value = kind.value_for(MAX_LINES);
            if width == 1 {
                assert!(value <= 0x7f, "{value} will not fit the imm8 it is written into");
            }
        }
    }

    #[test]
    fn array_references_all_land_inside_one_slot_or_the_spare() {
        // Every recorded field offset must be a real DeathNoticeItem field in
        // slot 0, except the two that deliberately point at slot 1 (the
        // memmove source). Anything else means a transcription slip.
        for &(rva, field) in ARRAY_REFS {
            assert!(field < 2 * ITEM, "reference at {rva:#x} has field offset {field:#x}");
        }
    }

    #[test]
    fn the_weapon_table_is_the_size_the_bounds_check_implies() {
        // MsgFunc_DeathMsg accepts 1..=43 (`jle` on 0, `jge` on 0x2c).
        assert_eq!(WEAPON_SPRITES.len(), 44);
        assert_eq!(WEAPON_SPRITES[0], "");
        assert_eq!(WEAPON_SPRITES[5], "d_garand");
        assert_eq!(WEAPON_SPRITES[43], "d_enfbayonet");
    }

    #[test]
    fn weapons_resolve_by_name_with_or_without_the_prefix() {
        assert_eq!(parse_weapon("d_garand"), Ok(5));
        assert_eq!(parse_weapon("garand"), Ok(5));
        assert_eq!(parse_weapon("GARAND"), Ok(5));
        assert_eq!(parse_weapon("5"), Ok(5));
        assert!(parse_weapon("0").is_err());
        assert!(parse_weapon("44").is_err());
        assert!(parse_weapon("no_such_gun").is_err());
    }

    #[test]
    fn a_block_list_hides_only_the_listed_players() {
        let list = BlockList { ids: vec![3, 7], allow_list: false };
        assert!(list.blocks(3, 9));
        assert!(list.blocks(9, 7));
        assert!(!list.blocks(1, 2));
    }

    #[test]
    fn an_allow_list_hides_everything_else() {
        let list = BlockList { ids: vec![3], allow_list: true };
        assert!(!list.blocks(3, 9), "a frag involving 3 must still show");
        assert!(list.blocks(1, 2), "a frag with nobody listed must be hidden");
    }

    #[test]
    fn an_empty_list_blocks_nothing_in_either_mode() {
        for allow_list in [false, true] {
            let list = BlockList { ids: Vec::new(), allow_list };
            assert!(!list.blocks(1, 2));
        }
    }
}

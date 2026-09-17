//! `dodtools_objectives` — where DoD 1.3 draws the objective (territory flag)
//! icons and the objective timer beside them.
//!
//! Both sit in the top-left corner, and both move down the screen while the DoD
//! spectator interface is up — which is why the same map captured POV and
//! captured from an HLTV demo does not line up. Until now, reconciling the two
//! meant `mirv_movie_separate_hud 1` and masking the alpha layer by hand.
//!
//! ## What the game does
//!
//! `CObjectiveIcons::Draw` (RVA `0x30030`, the class' own vftable slot 3) draws
//! two independent things, in this order, each gated by its own cvar:
//!
//! | element | cvar | x | y |
//! | --- | --- | --- | --- |
//! | objective timer | `cl_hud_objtimer` | fixed: sprite at 0, digits at 10/24/44/58 | `esi` |
//! | objective icons | `cl_hud_objectives` | `[esp+0x14]`, advanced per icon | `[esp+0x18]` |
//!
//! Each y is picked the same way, and the choice is the whole problem:
//!
//! ```text
//! timer  y = 2                             or round(54 * ScreenHeight / 480)
//! icons  y = round(2 * ScreenHeight / 480)  or round(54 * ScreenHeight / 480)
//!            ^ no spectator interface           ^ spectator interface up
//! ```
//!
//! At 1080p that is 2 and 5 in a POV demo, and 122 for both while spectating —
//! a ~117-pixel drop that clears DoD's spectator bar. The icons' x moves too:
//! 82 normally (the timer occupies the space to its left), or a width-scaled
//! value while `spec_pip` is on.
//!
//! The icon row's x and y are both final before the per-objective loop starts,
//! and the loop only ever advances x (`x += icon_width + 2`) — the row never
//! wraps. So one write per axis, at the point the branches converge, places the
//! whole row.
//!
//! ## Why detours rather than immediates
//!
//! The same reason the kill feed's `offset` is a detour (see
//! `docs/goldsrc_death_notices.md`): each y is *computed*, not stored as a
//! constant anyone could rewrite. `54.0`, `2.0` and `1/480` are `.rdata` floats
//! multiplied together, so patching one would change the spectated placement
//! and leave the POV placement alone — one value with two meanings again.
//! Detouring the convergence point sets the **result**, so a number typed at
//! the console means the same thing in both.
//!
//! ## What this deliberately does not reach
//!
//! While DoD's overview map is up (`_cl_minimap` non-zero, and only while
//! genuinely spectating), the loop overwrites both icon slots per objective
//! from the objective's own record — the icons are being drawn *on the map*, at
//! map coordinates, which is correct and is not a placement anyone would want
//! overridden. That path is inside the loop, downstream of this detour, so it
//! keeps winning. See `docs/goldsrc_objective_icons.md`.
//!
//! The timer has no x setting because the game does not keep one: its
//! background sprite is drawn at a literal 0 and its four digits at literal
//! 10, 24, 44 and 58, which is five sites rather than one value.
//!
//! Analysis subject: `dod/cl_dlls/client.dll`, 977,816 bytes, byte-identical
//! across the stock, pre-Anniversary and post-Anniversary installs.

use std::ffi::CStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicUsize, Ordering};

use crate::detour;
use crate::engine;
use crate::names::console_name;
use crate::scan;

/// Every name this command answers to — see [`crate::deathmsg::COMMAND_NAMES`].
pub const COMMAND_NAMES: &[&str] = &[COMMAND];

/// The name used in usage and error text, which is the first one registered.
const COMMAND: &str = console_name!("objectives");

// ── The icon row's detour ────────────────────────────────────────────────────

/// Identifies the icon row's y computation. Unique across `client.dll`'s code,
/// which [`scan::find_unique`] insists on; the wildcards cover four absolute
/// addresses and one relative call.
///
/// ```asm
/// fild  dword ptr [ScreenHeight]
/// fmul  dword ptr [1/480]
/// fld   st(0)
/// fadd  st(0), st(1)          ; 2 * ScreenHeight/480
/// fadd  dword ptr [0.5]       ; round
/// call  <ftol>
/// mov   dword ptr [esp+0x18], eax
/// mov   eax, dword ptr [<spectator interface mode>]
/// test  eax, eax
/// je    <convergence>
/// ```
const ROW_PATTERN: &str = "DB 05 ?? ?? ?? ?? D8 0D ?? ?? ?? ?? D9 C0 D8 C1 D8 05 ?? ?? ?? ?? \
                           E8 ?? ?? ?? ?? 89 44 24 18 A1 ?? ?? ?? ?? 85 C0 74 17";

/// Distance from the match to the convergence point (`+0x30248` -> `+0x30289`).
const ROW_DETOUR_AT: usize = 0x41;

/// `mov eax, [edi+0x18]; mov ecx, [edi+0x14]` — the instructions the jump
/// overwrites, reproduced verbatim at the end of the stub. Six bytes, so the
/// five-byte jump needs one `nop` of padding.
///
/// Both registers are dead here: they are being loaded, not read, and the y and
/// x the function just computed were stored to the stack one instruction
/// earlier. Nothing anywhere in the image branches into the middle of the span
/// and no dword points into it — re-checked by
/// `goldsrc-hooks/tools/verify_objicons_offsets.py`.
const ROW_STOLEN: &[u8] = &[0x8b, 0x47, 0x18, 0x8b, 0x4f, 0x14];

/// Stack slots, relative to `esp` at the convergence point. The detour is a
/// `jmp`, not a `call`, so the stub sees the same `esp` the function does.
const X_SLOT: u8 = 0x14;
const Y_SLOT: u8 = 0x18;

// ── The timer's detour ───────────────────────────────────────────────────────

/// Identifies the objective timer's y computation, upstream of the icon row's
/// in the same function. Also unique.
///
/// ```asm
/// mov  eax, dword ptr [<spectator interface mode>]
/// mov  esi, 2                 ; the POV y, a literal
/// test eax, eax
/// je   <convergence>
/// fild dword ptr [ScreenHeight]
/// fmul dword ptr [1/480]
/// fmul dword ptr [54.0]
/// fadd dword ptr [0.5]
/// call <ftol>
/// mov  esi, eax               ; the spectated y
/// ```
const TIMER_PATTERN: &str = "A1 ?? ?? ?? ?? BE 02 00 00 00 85 C0 74 1F DB 05 ?? ?? ?? ?? \
                             D8 0D ?? ?? ?? ?? D8 0D ?? ?? ?? ?? D8 05 ?? ?? ?? ?? \
                             E8 ?? ?? ?? ?? 8B F0";

/// Distance from the match to the convergence point (`+0x30119` -> `+0x30146`).
const TIMER_DETOUR_AT: usize = 0x2d;

/// `mov edx, [edi+0x24]; push 0xff` — eight bytes, so the five-byte jump needs
/// three `nop`s of padding. Three would not have been enough on their own,
/// which is why the `push` is taken along and reproduced.
///
/// The y is in `esi` here rather than on the stack, so the stub overwrites the
/// register directly and needs no scratch of its own.
const TIMER_STOLEN: &[u8] = &[0x8b, 0x57, 0x24, 0x68, 0xff, 0x00, 0x00, 0x00];

// ── Shared limits ────────────────────────────────────────────────────────────

/// Guards against a typo, not an encoding limit — the stubs write full dwords.
/// Negative is meaningful: a row the game would start at ~122 while spectating
/// can be pulled above the top of the screen deliberately.
const MIN_COORD: i32 = -4096;
const MAX_COORD: i32 = 4096;

/// Sentinel outside the accepted range. Not 0-for-unset: 0 is a coordinate a
/// user can legitimately ask for, and reporting it as "the game's" would lie.
const UNSET: i32 = i32::MIN;

// ── Runtime state ────────────────────────────────────────────────────────────

/// Read by game code, so these are addresses rather than Rust values:
/// [`AtomicU8::as_ptr`] is what makes that sound without `static mut`.
static X_ACTIVE: AtomicU8 = AtomicU8::new(0);
static X_VALUE: AtomicI32 = AtomicI32::new(0);
static Y_ACTIVE: AtomicU8 = AtomicU8::new(0);
static Y_VALUE: AtomicI32 = AtomicI32::new(0);
static TIMER_ACTIVE: AtomicU8 = AtomicU8::new(0);
static TIMER_VALUE: AtomicI32 = AtomicI32::new(0);

/// Where each stub jumps back to: the instruction after its stolen bytes.
static ROW_RESUME: AtomicUsize = AtomicUsize::new(0);
static TIMER_RESUME: AtomicUsize = AtomicUsize::new(0);

/// Installed once per process; see [`detour::Detour`] on why they are never
/// undone. Two, not one, so asking only for a timer y does not write over code
/// the icon row runs, and vice versa.
static ROW_DETOUR: Mutex<Option<detour::Detour>> = Mutex::new(None);
static TIMER_DETOUR: Mutex<Option<detour::Detour>> = Mutex::new(None);

/// What has been asked for, for the status line, or [`UNSET`].
static WANTED_X: AtomicI32 = AtomicI32::new(UNSET);
static WANTED_Y: AtomicI32 = AtomicI32::new(UNSET);
static WANTED_TIMER: AtomicI32 = AtomicI32::new(UNSET);

// ── Stubs ────────────────────────────────────────────────────────────────────

/// `cmp byte ptr [flag], 0` / `je` past / `mov eax, [value]` /
/// `mov dword ptr [esp+slot], eax`.
fn substitute_stack_slot(flag: usize, value: usize, slot: u8) -> Vec<u8> {
    let assign: Vec<u8> = [0xa1u8] // mov eax, [abs32]
        .into_iter()
        .chain((value as u32).to_le_bytes())
        .chain([0x89, 0x44, 0x24, slot]) // mov [esp+slot], eax
        .collect();
    guarded(flag, assign)
}

/// `cmp byte ptr [flag], 0` / `je` past / `mov esi, [value]`.
fn substitute_esi(flag: usize, value: usize) -> Vec<u8> {
    let assign: Vec<u8> = [0x8bu8, 0x35] // mov esi, [abs32]
        .into_iter()
        .chain((value as u32).to_le_bytes())
        .collect();
    guarded(flag, assign)
}

/// Wraps `assign` in the `cmp`/`je` that skips it while the flag is clear.
fn guarded(flag: usize, assign: Vec<u8>) -> Vec<u8> {
    let mut code = vec![0x80, 0x3d]; // cmp byte ptr [abs32],
    code.extend_from_slice(&(flag as u32).to_le_bytes());
    code.push(0x00); //   0
    code.extend_from_slice(&[0x74, assign.len() as u8]); // je over the assignment
    code.extend_from_slice(&assign);
    code
}

/// `jmp dword ptr [resume]`, which every stub ends with.
fn resume_jump(resume: usize) -> Vec<u8> {
    let mut code = vec![0xff, 0x25];
    code.extend_from_slice(&(resume as u32).to_le_bytes());
    code
}

/// The icon row's stub, hand-assembled.
///
/// ```asm
/// cmp byte ptr [X_ACTIVE], 0
/// je  .y
/// mov eax, [X_VALUE]
/// mov dword ptr [esp + 0x14], eax     ; x = ours
/// .y:
/// cmp byte ptr [Y_ACTIVE], 0
/// je  .game
/// mov eax, [Y_VALUE]
/// mov dword ptr [esp + 0x18], eax     ; y = ours
/// .game:
/// mov eax, [edi + 0x18]               ; the stolen instructions
/// mov ecx, [edi + 0x14]
/// jmp dword ptr [ROW_RESUME]
/// ```
///
/// Hand-written rather than copied, because copying the span would mean
/// relocating any relative branch inside it. The two overrides are independent
/// flags rather than one, so `offset` alone leaves the game's x alone and
/// `xoffset` alone leaves the game's y alone.
fn row_stub(x_active: usize, x_value: usize, y_active: usize, y_value: usize, resume: usize) -> Vec<u8> {
    let mut code = substitute_stack_slot(x_active, x_value, X_SLOT);
    code.extend_from_slice(&substitute_stack_slot(y_active, y_value, Y_SLOT));
    code.extend_from_slice(ROW_STOLEN);
    code.extend_from_slice(&resume_jump(resume));
    code
}

/// The timer's stub, hand-assembled.
///
/// ```asm
/// cmp byte ptr [TIMER_ACTIVE], 0
/// je  .game
/// mov esi, [TIMER_VALUE]              ; y = ours
/// .game:
/// mov edx, [edi + 0x24]               ; the stolen instructions
/// push 0xff
/// jmp dword ptr [TIMER_RESUME]
/// ```
///
/// The reproduced `push` is the first of the four `SPR_Set` arguments, so the
/// stub leaves the stack exactly as the game's own instruction would have.
fn timer_stub(active: usize, value: usize, resume: usize) -> Vec<u8> {
    let mut code = substitute_esi(active, value);
    code.extend_from_slice(TIMER_STOLEN);
    code.extend_from_slice(&resume_jump(resume));
    code
}

// ── Installing ───────────────────────────────────────────────────────────────

/// One place in `CObjectiveIcons::Draw` a stub is spliced into. Everything the
/// splice needs is here so a second site costs a `const` rather than another
/// argument on [`install_once`].
struct Site {
    /// Named in the log line and in any failure, so a session's log says which
    /// of the two could not be placed.
    what: &'static str,
    pattern: &'static str,
    detour_at: usize,
    stolen: &'static [u8],
    installed: &'static Mutex<Option<detour::Detour>>,
    resume: &'static AtomicUsize,
}

const ROW_SITE: Site = Site {
    what: "icon row's position",
    pattern: ROW_PATTERN,
    detour_at: ROW_DETOUR_AT,
    stolen: ROW_STOLEN,
    installed: &ROW_DETOUR,
    resume: &ROW_RESUME,
};

const TIMER_SITE: Site = Site {
    what: "timer's position",
    pattern: TIMER_PATTERN,
    detour_at: TIMER_DETOUR_AT,
    stolen: TIMER_STOLEN,
    installed: &TIMER_DETOUR,
    resume: &TIMER_RESUME,
};

/// Scans for the site's pattern, checks the span still holds its stolen bytes,
/// and installs. A no-op once the site is placed.
///
/// The scan proves the pattern; the byte check proves the offset from it still
/// lands where it did, so a build that moved the convergence point fails here
/// rather than having a jump written over the middle of something else. It also
/// catches someone else's detour, since an `E9` is not what we expect to find.
fn install_once(site: &Site, base: usize, build_stub: impl FnOnce(usize) -> Vec<u8>) -> Result<(), String> {
    let mut slot = site.installed.lock().map_err(|_| "the detour lock is poisoned".to_string())?;
    if slot.is_some() {
        return Ok(());
    }
    let (what, stolen) = (site.what, site.stolen);
    // Safety: `base` is a module handle the loader gave us.
    let found = unsafe { scan::find_unique(base, site.pattern) }
        .map_err(|why| format!("could not locate the {what} -- {why}"))?;
    let target = found + site.detour_at;

    // Safety: `target` is inside the module's code section.
    let present = unsafe { std::slice::from_raw_parts(target as *const u8, stolen.len()) };
    if present != stolen {
        return Err(format!(
            "expected {stolen:02x?} at +{:#x}, found {present:02x?}",
            target - base
        ));
    }

    site.resume.store(target + stolen.len(), Ordering::Release);
    let code = build_stub(site.resume.as_ptr() as usize);
    // Safety: the span was checked byte-for-byte above, and the branch-safety
    // condition was verified against a disassembly -- see `ROW_STOLEN`.
    let installed = unsafe { detour::install(target, stolen.len(), &code) }?;
    unsafe {
        crate::debug::report(&format!(
            "objicons: {what} detour installed at +{:#x} (pattern matched +{:#x}), stub at {:#x}",
            target - base,
            found - base,
            installed.stub_address()
        ))
    };
    *slot = Some(installed);
    Ok(())
}

/// Which of the three positions a subcommand is talking about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setting {
    IconX,
    IconY,
    TimerY,
}

impl Setting {
    fn state(self) -> (&'static AtomicU8, &'static AtomicI32, &'static AtomicI32) {
        match self {
            Setting::IconX => (&X_ACTIVE, &X_VALUE, &WANTED_X),
            Setting::IconY => (&Y_ACTIVE, &Y_VALUE, &WANTED_Y),
            Setting::TimerY => (&TIMER_ACTIVE, &TIMER_VALUE, &WANTED_TIMER),
        }
    }

    /// The detour that has to be in place before this setting does anything.
    fn ensure_detour(self, base: usize) -> Result<(), String> {
        match self {
            Setting::IconX | Setting::IconY => install_once(&ROW_SITE, base, |resume| {
                row_stub(
                    X_ACTIVE.as_ptr() as usize,
                    X_VALUE.as_ptr() as usize,
                    Y_ACTIVE.as_ptr() as usize,
                    Y_VALUE.as_ptr() as usize,
                    resume,
                )
            }),
            Setting::TimerY => install_once(&TIMER_SITE, base, |resume| {
                timer_stub(TIMER_ACTIVE.as_ptr() as usize, TIMER_VALUE.as_ptr() as usize, resume)
            }),
        }
    }
}

/// Places one coordinate, on every code path that reaches the draw.
fn apply(setting: Setting, value: i32) -> Result<(), String> {
    let Some(base) = engine::client_module_base() else {
        return Err("client.dll is not loaded yet".to_string());
    };
    if !(MIN_COORD..=MAX_COORD).contains(&value) {
        return Err(format!("expected {MIN_COORD}..={MAX_COORD}, got {value}"));
    }
    setting.ensure_detour(base)?;
    let (active, current, wanted) = setting.state();
    current.store(value, Ordering::Release);
    active.store(1, Ordering::Release);
    wanted.store(value, Ordering::Release);
    Ok(())
}

/// Hands one coordinate back to the game.
///
/// Nothing is unpatched: the detour stays and simply stops substituting, which
/// is strictly safer than restoring bytes under a thread that might be
/// executing them.
fn clear(setting: Setting) {
    let (active, _, wanted) = setting.state();
    active.store(0, Ordering::Release);
    wanted.store(UNSET, Ordering::Release);
}

// ── Console surface ──────────────────────────────────────────────────────────

/// Each subcommand, the setting it moves, and what to call it in a reply.
const SUBCOMMANDS: &[(&str, Setting, &str)] = &[
    ("offset", Setting::IconY, "icon y"),
    ("xoffset", Setting::IconX, "icon x"),
    ("timer", Setting::TimerY, "timer y"),
];

fn usage() -> String {
    format!(
        "usage:\n\
         \x20 {COMMAND} offset <y>         y the objective icons are drawn at\n\
         \x20 {COMMAND} xoffset <x>        x the icon row starts at\n\
         \x20 {COMMAND} timer <y>          y the objective timer beside them is drawn at\n\
         \x20 {COMMAND} <any> default      hand that one back to the game\n\
         \x20 All are absolute screen positions, and mean the same thing in a POV\n\
         \x20 demo and while spectating -- which the game's own values do not. The\n\
         \x20 timer has no x: the game draws it at a fixed one.\n"
    )
}

fn status() -> String {
    let mut text = format!("{COMMAND}:");
    for (_, setting, label) in SUBCOMMANDS {
        let (_, _, wanted) = setting.state();
        let value = match wanted.load(Ordering::Acquire) {
            UNSET => "the game's".to_string(),
            value => value.to_string(),
        };
        text.push_str(&format!(" {label} = {value},"));
    }
    text.pop();
    text.push('\n');
    text
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
    let sub = sub.to_ascii_lowercase();

    let Some(&(_, setting, label)) = SUBCOMMANDS.iter().find(|(name, _, _)| *name == sub) else {
        return format!("{COMMAND}: no subcommand {sub:?}\n{}", usage());
    };
    let Some(value) = argv.get(2) else {
        return format!("{}{}", status(), usage());
    };
    if value.eq_ignore_ascii_case("default") {
        clear(setting);
        return format!("{COMMAND}: {label} back to whatever the game computes\n");
    }
    match value.parse::<i32>() {
        Ok(n) => match apply(setting, n) {
            Ok(()) => format!("{COMMAND}: {label} = {n}\n"),
            Err(why) => format!("{COMMAND} {sub}: {why}\n"),
        },
        Err(_) => format!("{COMMAND} {sub}: expected a number or \"default\", got {value:?}\n"),
    }
}

pub unsafe extern "C" fn command() {
    let argv = args();
    let reply = dispatch(&argv);
    crate::commands::console_print(&reply);
    unsafe { crate::debug::report(&format!("objicons: {} -> {}", argv.join(" "), reply.trim())) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `je` in a stub, as `(index of the 0x74, index it lands on)`.
    ///
    /// Found by looking for the `cmp byte ptr [abs32], 0` that precedes each
    /// one rather than for a bare `0x74`, so a 0x74 inside an address does not
    /// read as a jump.
    fn guards(code: &[u8]) -> Vec<(usize, usize)> {
        code.windows(9)
            .enumerate()
            .filter(|(_, w)| w[0] == 0x80 && w[1] == 0x3d && w[6] == 0x00 && w[7] == 0x74)
            .map(|(at, w)| (at + 7, at + 9 + w[8] as usize))
            .collect()
    }

    #[test]
    fn the_row_stub_assembles_to_what_the_comment_claims() {
        let code = row_stub(0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444, 0x5555_5555);
        #[rustfmt::skip]
        assert_eq!(
            code,
            vec![
                0x80, 0x3d, 0x11, 0x11, 0x11, 0x11, 0x00, // cmp byte [X_ACTIVE], 0
                0x74, 0x09,                               // je  over the x assignment
                0xa1, 0x22, 0x22, 0x22, 0x22,             // mov eax, [X_VALUE]
                0x89, 0x44, 0x24, 0x14,                   // mov [esp+0x14], eax
                0x80, 0x3d, 0x33, 0x33, 0x33, 0x33, 0x00, // cmp byte [Y_ACTIVE], 0
                0x74, 0x09,                               // je  over the y assignment
                0xa1, 0x44, 0x44, 0x44, 0x44,             // mov eax, [Y_VALUE]
                0x89, 0x44, 0x24, 0x18,                   // mov [esp+0x18], eax
                0x8b, 0x47, 0x18, 0x8b, 0x4f, 0x14,       // the stolen instructions
                0xff, 0x25, 0x55, 0x55, 0x55, 0x55,       // jmp dword [ROW_RESUME]
            ]
        );
    }

    #[test]
    fn the_timer_stub_assembles_to_what_the_comment_claims() {
        let code = timer_stub(0x1111_1111, 0x2222_2222, 0x3333_3333);
        #[rustfmt::skip]
        assert_eq!(
            code,
            vec![
                0x80, 0x3d, 0x11, 0x11, 0x11, 0x11, 0x00, // cmp byte [TIMER_ACTIVE], 0
                0x74, 0x06,                               // je  over the assignment
                0x8b, 0x35, 0x22, 0x22, 0x22, 0x22,       // mov esi, [TIMER_VALUE]
                0x8b, 0x57, 0x24, 0x68, 0xff, 0x00, 0x00, 0x00, // the stolen instructions
                0xff, 0x25, 0x33, 0x33, 0x33, 0x33,       // jmp dword [TIMER_RESUME]
            ]
        );
    }

    #[test]
    fn every_conditional_jump_lands_on_an_instruction_boundary() {
        // Hand-assembled, so the one thing a typo would silently break is a
        // `je` displacement: too small lands mid-instruction, too large skips
        // an instruction the game needs. Derive both from the bytes themselves.
        let row = row_stub(0xaaaa_aaaa, 0xbbbb_bbbb, 0xcccc_cccc, 0xdddd_dddd, 0xeeee_eeee);
        let row_guards = guards(&row);
        assert_eq!(row_guards.len(), 2, "one guarded assignment per axis");
        // The first lands on the second guard; the second on the stolen bytes.
        assert_eq!(row_guards[0].1, row_guards[1].0 - 7);
        assert_eq!(&row[row_guards[1].1..row_guards[1].1 + ROW_STOLEN.len()], ROW_STOLEN);

        let timer = timer_stub(0xaaaa_aaaa, 0xbbbb_bbbb, 0xcccc_cccc);
        let timer_guards = guards(&timer);
        assert_eq!(timer_guards.len(), 1);
        assert_eq!(&timer[timer_guards[0].1..timer_guards[0].1 + TIMER_STOLEN.len()], TIMER_STOLEN);
    }

    #[test]
    fn each_row_assignment_writes_its_own_stack_slot() {
        // Writing the x into the y's slot would place the row correctly along
        // one axis and nowhere sensible along the other, and every other test
        // here would still pass.
        let code = row_stub(0xaaaa_aaaa, 0xbbbb_bbbb, 0xcccc_cccc, 0xdddd_dddd, 0xeeee_eeee);
        for ((_, landing), slot) in guards(&code).iter().zip([X_SLOT, Y_SLOT]) {
            // The guarded block is `mov eax,[abs32]` + `mov [esp+slot],eax`.
            let assign = landing - 9;
            assert_eq!(code[assign], 0xa1, "the guarded block must start with the load");
            assert_eq!(code[assign + 8], slot, "the assignment writes the wrong slot");
        }
    }

    #[test]
    fn each_stolen_span_is_long_enough_to_hold_the_jump_that_replaces_it() {
        for stolen in [ROW_STOLEN, TIMER_STOLEN] {
            assert!(stolen.len() >= 5, "a near jump needs five bytes: {stolen:02x?}");
        }
    }

    #[test]
    fn zero_is_a_usable_coordinate_rather_than_the_unset_sentinel() {
        // The whole point of a separate sentinel: `offset 0` is a real request
        // (it puts the row at the very top), so it must not read as "never set".
        assert_ne!(UNSET, 0);
        assert!(!(MIN_COORD..=MAX_COORD).contains(&UNSET));
    }

    #[test]
    fn no_two_settings_share_a_flag_a_value_or_a_subcommand() {
        // Sharing any of these would make one subcommand silently move another
        // element, which is exactly the "one value, two meanings" problem this
        // replaces.
        let settings = [Setting::IconX, Setting::IconY, Setting::TimerY];
        for (i, a) in settings.iter().enumerate() {
            for b in &settings[i + 1..] {
                assert_ne!(a.state().0.as_ptr(), b.state().0.as_ptr());
                assert_ne!(a.state().1.as_ptr(), b.state().1.as_ptr());
                assert_ne!(a.state().2.as_ptr(), b.state().2.as_ptr());
            }
        }
        assert_ne!(X_SLOT, Y_SLOT);
        let mut names: Vec<&str> = SUBCOMMANDS.iter().map(|(name, _, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SUBCOMMANDS.len());
        assert_eq!(SUBCOMMANDS.len(), settings.len(), "every setting needs a way to reach it");
    }

    #[test]
    fn the_status_line_distinguishes_a_set_coordinate_from_the_game_s() {
        WANTED_X.store(UNSET, Ordering::Release);
        WANTED_Y.store(42, Ordering::Release);
        WANTED_TIMER.store(UNSET, Ordering::Release);
        let text = status();
        assert!(text.contains("icon y = 42"), "{text}");
        assert!(text.contains("icon x = the game's"), "{text}");
        assert!(text.contains("timer y = the game's"), "{text}");
        WANTED_Y.store(UNSET, Ordering::Release);
    }

    #[test]
    fn every_subcommand_is_in_the_usage_text() {
        let text = usage();
        for (name, _, _) in SUBCOMMANDS {
            assert!(text.contains(name), "{name} missing from the usage text:\n{text}");
        }
    }

    #[test]
    fn an_unknown_subcommand_is_named_rather_than_guessed_at() {
        let reply = dispatch(&[COMMAND.to_string(), "y".to_string(), "10".to_string()]);
        assert!(reply.contains("no subcommand"), "{reply}");
        assert!(reply.contains("xoffset"), "the reply should show what is available");
    }
}

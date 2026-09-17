//! Redirecting a span of game code through a stub of our own.
//!
//! The other way to change what a function does is to rewrite one of its
//! immediates, which is what `deathmsg`'s `max` does. That works when the thing
//! you want to change *is* a constant in the instruction stream. It does not
//! work when the value is computed, and it inherits whatever arithmetic
//! surrounds the operand — `offset` used to set an absolute y on one code path
//! and an addend on top of a screen-scaled term on another, because those are
//! two different instructions that happen to both hold `20`.
//!
//! A detour replaces the *result* instead. HLAE does exactly this for the same
//! job (`AfxHookGoldSrc.dll+0x100100a0`): it overwrites a scanned span with a
//! jump to a stub that reproduces the original instructions, substitutes the
//! value it wants, and jumps back to just past the span.
//!
//! ## What a stub here must do
//!
//! The stub is hand-written bytes, not copied ones. Copying the original span
//! would mean relocating any relative `call`/`jmp` inside it; reproducing the
//! handful of instructions by hand is both simpler and easier to review against
//! a disassembly. So the caller is responsible for the stub ending in a jump
//! back to `target + stolen`, and for the stolen instructions being reproduced
//! in it.
//!
//! The span must also be safe to overwrite: nothing may branch *into* the
//! middle of it. Verify that against a disassembly before adding a new detour —
//! `goldsrc-hooks/tools/verify_deathmsg_offsets.py` shows the shape of that
//! check for the one detour that exists.

use std::ffi::c_void;

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree,
};

use crate::patch;

/// `E9 rel32`.
const JMP_REL32: u8 = 0xe9;
/// A near jump is five bytes; a shorter span cannot hold one.
const JMP_LEN: usize = 5;
/// `nop`, padding whatever of the span the jump does not fill.
const NOP: u8 = 0x90;

/// An installed detour. Dropping it does **not** uninstall: the game may be
/// executing the stub, and there is no safe moment to find out. Detours here
/// live for the life of the process, which is what the one caller wants.
pub struct Detour {
    stub: *mut c_void,
}

// The pointer is only ever read (and by the game's thread, executing it).
unsafe impl Send for Detour {}
unsafe impl Sync for Detour {}

impl Detour {
    /// Where the stub was placed, for logging.
    pub fn stub_address(&self) -> usize {
        self.stub as usize
    }
}

/// Points `target` at a copy of `stub_code`, overwriting `stolen` bytes.
///
/// `stub_code` must already end by jumping to `target + stolen`, and must
/// reproduce whatever the stolen bytes did. Nothing may branch into
/// `target+1 .. target+stolen`.
///
/// Safety: `target` must be executable code in a loaded module, `stolen` must
/// land on an instruction boundary, and the caller must have checked the branch
/// condition above.
pub unsafe fn install(target: usize, stolen: usize, stub_code: &[u8]) -> Result<Detour, String> {
    if stolen < JMP_LEN {
        return Err(format!(
            "{stolen} bytes is not enough for a near jump (needs {JMP_LEN})"
        ));
    }

    // Safety: a fresh RWX page we own; freed only on the error paths below.
    let stub = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            stub_code.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        )
    };
    if stub.is_null() {
        return Err("could not allocate an executable page for the stub".to_string());
    }
    // Safety: `stub` is a page of at least `stub_code.len()` bytes.
    unsafe { std::ptr::copy_nonoverlapping(stub_code.as_ptr(), stub as *mut u8, stub_code.len()) };

    // On 32-bit, `jmp rel32` reaches anywhere: the displacement is added modulo
    // 2^32, so a wrapping difference lands on the right address regardless of
    // where the allocator put the stub. No near-allocation dance is needed.
    let displacement = (stub as usize).wrapping_sub(target.wrapping_add(JMP_LEN)) as u32;
    let mut patch_bytes = vec![NOP; stolen];
    patch_bytes[0] = JMP_REL32;
    patch_bytes[1..JMP_LEN].copy_from_slice(&displacement.to_le_bytes());

    // Safety: writing to code the caller vouched for, through the same
    // protect/write/restore used everywhere else in this DLL.
    if !unsafe { patch::write_code_bytes(target, &patch_bytes) } {
        // Safety: nothing can be running the stub -- it was never reachable.
        unsafe { VirtualFree(stub, 0, MEM_RELEASE) };
        return Err(format!("could not make {target:#x} writable"));
    }
    Ok(Detour { stub })
}

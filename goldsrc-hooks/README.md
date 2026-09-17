# goldsrc-hooks

Standalone companion DLL for DoD 1.3 GoldSrc capture sessions. Injected into
`hl.exe` alongside (not instead of) HLAE's own hook DLL -- see `src/lib.rs`
for why this doesn't need HLAE's build toolchain or any DoD-specific
reverse-engineering.

Two independent fixes, each off by default and toggled by its own env var:

- **Sound fix** (`GOLDSRC_HOOKS_FORCE_WEAPON_VOLUME=1`): forces DoD weapon-fire
  sounds to play at full volume with no distance attenuation while
  spectating, instead of fading out based on camera distance.
- **Animation fix** (`GOLDSRC_HOOKS_ANIM_FIX=1`): corrects MG42/MG34/BAR/Bren
  viewmodel deploy (bipod up/down) animations while spectating in-eye.

Plus one control surface, always available and doing nothing until used:

- **Death notices** (`dodtools_deathmsg`): raises DoD's hard-coded four-line
  cap on the kill feed, moves it down the screen, hides frags involving chosen
  players, or injects one by hand. HLAE's `mirv_deathmsg` supports only
  `cstrike` and `tfc`, so none of it works for DoD -- see
  `docs/goldsrc_death_notices.md`.

All three live in one DLL since they share the same engine-interface bootstrap;
set only the env var for whichever fix you want active.

## Building

DoD 1.3 (and the whole GoldSrc engine) is **32-bit**, so this must be built
for the `i686-pc-windows-msvc` target, not the default 64-bit one:

```
rustup target add i686-pc-windows-msvc   # one-time
# --lib matters: the injector is a standalone binary that does not depend on
# the cdylib, so --bins alone silently leaves a stale DLL in place.
cargo build -p goldsrc-hooks --release --target i686-pc-windows-msvc --lib --bins
```

Produces `target/i686-pc-windows-msvc/release/dodtoolsHookGoldSrc.dll` and
`inject.exe`.

## Testing manually

1. Launch DoD 1.3 (with or without HLAE) and load an HLTV/POV demo.
2. Find `hl.exe`'s PID (Task Manager, or `Get-Process hl | Select Id`).
3. Set whichever env var(s) you want *before* launching `hl.exe` --
   `inject.exe` only delivers the DLL, it doesn't set environment variables
   for a process that's already running.
4. `inject.exe <pid> path\to\dodtoolsHookGoldSrc.dll`
5. Check `%APPDATA%\dod-tools\logs\dodtoolsHookGoldSrc.log` for its own diagnostics (never pops a
   dialog -- this is meant to run inside an unattended capture pipeline).

## Status

The animation fix and all four `dodtools_deathmsg` subcommands are live-proven
against a running game. The sound fix is confirmed by static analysis only --
see the module docs in `src/engine.rs` and `src/sound_fix.rs` for what is
established from the DoD 1.3 game files vs. what still needs a live check.

A crash inside the game leaves no dump, WER record or event-log entry, because
GoldSrc installs its own unhandled-exception filter. `src/crash.rs` logs the
faulting address as `module+RVA` so a crash is diagnosable from the log alone.

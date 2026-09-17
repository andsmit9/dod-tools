# Render Studio ↔ HLCR Parity Notes

Status (2026-08-17): **research only, nothing implemented yet.** The user's
independent Python/PySide6 rewrite of the render tool lives in the sibling
repo `../HLCR` (`ui/main_window.py`, `ui/styles.py`, `workers/render_job.py`,
`workers/scanner.py`, `core/*.py`) and is referenced once already in
`archive/tauri_parity_audit.md` (§5, the H.264 codec-default decision). The user
wants dod-studio' Render Studio tab (`desktop-studio/index.html`'s
`#render-studio-panel`, `desktop-studio/src/render_pane.js`, backed by
`native/src/hlcr/` + `desktop-studio/src-tauri`'s `render_manager.rs`) to
move closer to what HLCR does. This doc is the field-by-field diff a
research pass produced, so whoever picks this up doesn't have to re-derive
it. Not triaged into Medium/Low priority yet — do that once the user picks
which of these they actually want.

Classification borrows `archive/tauri_parity_audit.md`'s convention: **GAP** (HLCR
has it, dod-studio doesn't), **DELTA** (both have it, shaped/behaving
differently), **DOD-STUDIO-ONLY** (dod-studio has something HLCR lacks —
don't regress these while porting).

---

## Controls & fields

- **GAP — Skip Previously Rendered.** HLCR writes a `render_info.json`
  marker into each take folder on successful render (`workers/render_job.py:158-176`)
  and offers a checkbox to skip already-rendered takes on a re-scan
  (`ui/main_window.py:286-291`, `680-685`; scanner check at
  `workers/scanner.py:123-176`). dod-studio' scanner
  (`native/src/hlcr/scanner.rs`) has no equivalent — nothing marks a take as
  done, so re-scanning a folder always re-queues everything in it.
- **GAP — Global aggregate progress bar.** HLCR shows one fixed-width
  `QProgressBar` averaging every active job's % (`ui/main_window.py:247-251`,
  `914-927`) above the per-job bars. dod-studio only has per-job progress bars
  (`desktop-studio/src/render_pane.js:156-161`), no at-a-glance batch total.
- **GAP — Table checkbox multi-select + bulk actions.** HLCR's queue table
  has a checkbox column plus Select All/Deselect All/Delete Selected
  (`ui/main_window.py:265-268`, `929-1007`). dod-studio' render jobs table
  (`index.html:339-357`) has per-row actions only, no multi-select.
- **GAP — Per-row Delete Take Folder / Open Take Folder.** HLCR has a 🗑
  button per row (`send2trash` + confirm dialog,
  `ui/main_window.py:546-566`, `814-859`) and a 📂 open-folder button
  (`ui/main_window.py:497-518`). dod-studio has neither — no way to delete or
  reveal a take's source folder from the render queue.
- **GAP — Sortable columns + Reset Sort.** HLCR's table is fully sortable
  with a hidden `OrigOrder` column and a "Reset Sort" button
  (`ui/main_window.py:236`, `274-278`). dod-studio' table has no `data-sort`
  wiring (`index.html:340-351`).
- **GAP — Clear Queue button.** HLCR has a dedicated one
  (`ui/main_window.py:261-263`, `396-403`); dod-studio only supports removing
  jobs individually via per-row Cancel/Reset.
- **GAP — "Scan All Drives".** HLCR enumerates logical drives via
  `GetLogicalDrives` and scans all of them in one click
  (`ui/main_window.py:364-394`). dod-studio requires adding folders one at a
  time to the Render Folders list.
- **GAP — Take Path column.** HLCR shows the source take folder path as its
  own table column (`ui/main_window.py:202`, `447-450`); not present in
  dod-studio' table.
- **DELTA — Codec set.** HLCR: ProRes / CineForm / H.264 / DNxHR
  (`core/constants.py:1-26`). dod-studio: ProRes / DNxHR / H.264 (Software) /
  H.264 (NVENC GPU) (`native/src/hlcr/config.rs:6-16`). Neither is a subset
  of the other — HLCR has GoPro CineForm, dod-studio has NVENC hardware
  H.264. Reconcile if full parity is the goal; otherwise a deliberate,
  known gap in both directions.
- **DELTA — Max concurrent renders.** HLCR: 1 to `os.cpu_count()`
  (`ui/main_window.py:180-182`). dod-studio: hardcoded 1-8
  (`index.html:316`) — under-caps on >8-core machines.
- **DELTA — Output routing.** dod-studio has a JIT multi-drive export pool
  with a live free-space readout (`index.html:320-330`,
  `render_manager.rs`'s `get_export_pool_free_gb`); HLCR has one single
  output-folder field (`ui/main_window.py:130-149`). This is a dod-studio
  *advantage* worth keeping, not something to regress toward HLCR's simpler
  model.
- **DOD-STUDIO-ONLY — Crash-recovery autosave.** `.render_autosave.json` +
  startup recovery modal (`render_manager.rs:512-574`, `index.html:545-554`).
  HLCR has nothing equivalent. Keep.
- **DOD-STUDIO-ONLY — Per-job View Log modal.** dod-studio shows FFmpeg error
  output per failed job (`render_pane.js:57-64`, `#render-error-log-modal`
  in `index.html`); HLCR's Cancel-only recovery has no log viewer. Keep.

## Workflow

- **DELTA — Scan model.** dod-studio runs one blocking scan
  (`render_manager.rs:49-74`, `spawn_blocking`) that populates the table
  only once fully done. HLCR streams results live — each `FolderScanner`
  `QThread` emits `clip_found` per clip as it's discovered
  (`workers/scanner.py:25-31`, `ui/main_window.py:405-578`), and multiple
  scans can run concurrently. Porting this would need the Rust scan to emit
  incremental Tauri events instead of returning one final `Vec`.
- **DELTA — Session persistence.** HLCR re-validates/resets stale
  Finished/Error/Cancelled rows in place when you click Start again
  (`ui/main_window.py:604-635`), behaving like a persistent, editable
  session rather than a one-shot scan→queue→start pipeline. dod-studio has
  no equivalent re-arm step.

## Visual/styling

- HLCR (`ui/styles.py`) uses a deliberate Catppuccin Mocha palette
  (`#1e1e2e`/`#181825`/`#11111b` backgrounds, `#89b4fa` blue accent,
  `#a6e3a1`/`#f38ba8`/`#f9e2af` status greens/reds/yellows, gradient
  progress-bar fill at `styles.py:96-99`), plus a custom `RowHoverDelegate`
  for full-row table hover (`ui/main_window.py:41-87`) and consistent
  rounded corners (8px frames/tables, 6px inputs).
- dod-studio (`styles.css:45-62`) uses a flatter charcoal palette
  (`#121212`/`#1e1e1e`/`#252525`, `#2b5c8f` accent), status colors defined
  ad hoc in JS rather than as CSS variables (`render_pane.js:26-32`), **no
  row-hover rule on the render jobs table at all** (only
  `#master-demo-table` has one, `styles.css:293`), and square table corners.
- Concrete, portable takeaways: add a hover rule to the render jobs table,
  and consider tokenizing status colors into CSS variables (dovetails with
  the Theme System backlog item under R&D & Architectural Enhancements in
  `engineering_backlog.md`, since both need a real color-token system
  instead of scattered hex literals). The palette-swap-to-Catppuccin
  and rounded-corner styling are aesthetic calls for the user to make, not
  objectively missing functionality.

## Backend/logic (behavior, not just UI)

- **Clip-type / take detection.** HLCR pairs folders generically — any
  folder with "alpha"/"mask" in its name pairs with any same-frame-count
  "color"/"rgb" folder (`workers/scanner.py:113-148`), plus a `chromakey`
  type driven by real pixel analysis (`core/image_analysis.py`:
  `has_true_alpha`/`detect_chromakey_color` via PIL). dod-studio hardcodes
  literal folder names `"all"`/`"hudcolor"`/`"hudalpha"`
  (`native/src/hlcr/scanner.rs:123-155`) with no chromakey path and no
  pixel inspection — HLCR's approach is more robust to arbitrary HLAE
  folder-naming conventions and is a plausible upgrade target independent
  of the UI work.
- **Output filename collisions.** HLCR appends an incrementing `_1`, `_2`...
  suffix (`workers/render_job.py:202-208`); dod-studio appends a
  microsecond-timestamp hash (`native/src/hlcr/renderer.rs:119-126`).
  Functionally equivalent, cosmetically different filenames on disk.
- **Wake-lock scope.** HLCR's `SetThreadExecutionState` flag also keeps the
  *display* on (`ui/main_window.py:1009-1019`); dod-studio explicitly lets
  the monitor sleep (`keepawake::Builder::default().display(false)`,
  `renderer.rs:351-361`) — a deliberate, probably-better dod-studio choice.
  Don't port this one.

---

## Suggested next step

Don't implement all of this at once — it's a large surface. Once the user
picks a subset (the multi-select/delete/sort/skip-rendered table upgrades
are probably the highest-value, most self-contained slice), triage those
into `engineering_backlog.md`'s Medium/Low Priority lists individually,
the same way the Capture Studio parity gaps were triaged from
`archive/tauri_parity_audit.md`.

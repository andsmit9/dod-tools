# DoD Tools

Tooling for **Day of Defeat 1.3** (GoldSrc) demo files: parsing them for match
analytics, and driving the engine plus HLAE to batch-record highlight clips.

A creative fork of [cgdangelo/dod-tools](https://github.com/cgdangelo/dod-tools).

---

## Component status

The two halves of this repository are at very different levels of maturity.
Read this before judging the codebase by whichever part you happen to open.

| Area | Status | Notes |
| --- | --- | --- |
| `dod/` — DoD 1.3 message parsers | **Stable** | Hand-written parsers for the mod's user messages. No public library covers this layer. |
| `analysis/` — match analytics | **Stable** | Players, scoreboard, kills, rounds, chat, survival times. Recently validated across 624 demos; derived kill counts reconcile exactly with the game server's own frag counter for ~75% of players and within one kill for ~90%. |
| `dem-patch/` — demo reader/writer | **Stable** | Vendored fork of the [`dem`](https://github.com/khanghugo/dem) crate. See *Why the fork* below. |
| `hl-demo-auditor/` — duplicate finder | **Stable** | Small, self-contained. |
| `native/` — capture engine & patcher | **Active development** | Works, but rough edges. Under near-continuous change. |
| `desktop-studio/` — Tauri UI | **Active development** | Capture Studio and Render Studio are functional but **not polished**. Expect UI inconsistencies and in-flight refactors. |
| `web-analyzer/` — browser demo viewer | **Active development** | `analysis/` compiled to wasm, deployed to GitHub Pages on every push to `main`. |

If you are here to look at demo parsing or stats extraction, `dod/` and
`analysis/` are the parts to read, and they are the parts you can rely on.
The capture and render pipeline is a frag-movie workflow and is unrelated.

> [!NOTE]
> **The GUI was rewritten.** Earlier revisions shipped an `egui` desktop app
> with a WebAssembly target. That has been removed. The current frontend is
> Tauri v2 + Vite under `desktop-studio/`, and the old `dod-tools-gui` binary
> and `trunk serve` workflow no longer exist.

---

## Workspace layout

    dod/              nom-based parsers for DoD 1.3 network messages (no I/O)
    analysis/         match analytics built on top of dod/
    dem-patch/        vendored + patched `dem` crate (GoldSrc demo read/write)
    native/           capture engine, demo patcher, take management, FFmpeg
    hl-demo-auditor/  duplicate-demo detector
    benchmark/        parsing/patching performance harness
    desktop-studio/   Tauri v2 + Vite frontend
    web-analyzer/     analysis/ compiled to wasm, browser-based demo viewer

## Quick start

Analyse a demo and print match analytics:

    cargo run -p analysis --bin parse_demo -- path/to/demo.dem

Headless preview CLI (accepts a demo or a folder):

    cargo run -p native --bin preview_cli -- path/to/demo-or-folder

Desktop app:

    cd desktop-studio
    npm install
    npm run tauri dev

Tests:

    cargo test --workspace

## Localization keys

Tokens are stored **bare** — lowercase, no `#`. The `#` is a lookup-time sigil
meaning "resolve this as a token", not part of the name, which is Valve's own
convention: every shipped game file stores keys without it. `translate_key`
normalizes on both insert and lookup, so a file written either way resolves.

## Why the fork of `dem`

`dem-patch/` is `dem` v0.2.3 with the parse path hardened. Upstream resolves
delta-decoder tables with `.unwrap()` in seven files; when a demo does not carry
the expected delta-description table, that is a **panic rather than a parse
error**, which kills the whole process mid-batch. The fork falls back to the
library's built-in initial delta table where one exists and returns a graceful
parse failure otherwise. As of upstream v0.3.0 all 29 of those call sites are
still present, so the patch is still required.

## Licensing

This project is MIT (see `LICENSE`), which carries two copyright lines.
Charles D'Angelo's is from [cgdangelo/dod-tools](https://github.com/cgdangelo/dod-tools),
which this is a fork of — MIT requires that notice be retained, so it stays.
The second covers the work done here since the fork.

One directory is **not** MIT: **`dem-patch/` is LGPL-3.0**. It is a vendored
fork of upstream [`dem`](https://github.com/khanghugo/dem) and keeps upstream's
terms; `dem-patch/LICENSE` is the authority for that directory.

Because `dem-patch/` is linked into every binary this workspace builds, anyone
redistributing those binaries is redistributing LGPL-3.0 code and takes on that
licence's obligations for the library portion -- principally the requirement
that recipients be able to relink against a modified version of it. The MIT
licence at the repository root does not override that, and is not intended to
suggest otherwise.

Local modifications to `dem-patch/` are described under *Why the fork of `dem`*
above and are themselves LGPL-3.0, being changes to an LGPL work.

## Documentation

Engineering notes live in `docs/` — architecture decisions, GoldSrc and DoD
engine quirks, HLAE protocol constraints, and a running bug log.

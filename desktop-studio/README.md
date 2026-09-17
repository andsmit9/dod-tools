# desktop-studio

The active `dod-studio` desktop app — Tauri v2 backend (`src-tauri/`) + Vite/vanilla-JS
frontend (`src/`, one module per pane: `capture_pane.js`, `render_pane.js`,
`analyzer_pane.js`, `auditor_pane.js`).

Real dev instructions and workspace context live in the repo root
[`README.md`](../README.md) and [`CLAUDE.md`](../CLAUDE.md), not here — this file is
just the app's own quick pointer.

    npm install
    npm run tauri dev     # Launch the Tauri window with Vite HMR
    npm run dev            # Vite dev server only, no Tauri window
    npm run build           # Production Vite build

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

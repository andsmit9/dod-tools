# Versioning & Releases (desktop-studio)

How version numbers get assigned, where they live, and how the two update
channels (`stable`, `experimental`) relate to `main`/`dev`. Written up because
this isn't obvious from the workflow files alone and is easy to forget
between releases — refer back here instead of re-deriving it.

## Where a version number lives

Four files carry a `version` field, but they don't all mean the same thing:

| File | What it's for |
|---|---|
| `desktop-studio/src-tauri/Cargo.toml` | **The real one.** Rust bakes this into the compiled binary as `CARGO_PKG_VERSION`, and `tauri-action` reads it for bundle filenames and the published `latest.json` manifest the updater actually polls. |
| `desktop-studio/src-tauri/tauri.conf.json` | Display value only (About box, window title). Stamped to match Cargo.toml's version at release time so they never disagree. |
| `desktop-studio/package.json` | Same — display/tooling only, stamped to match. |
| Root `Cargo.toml` | The Cargo **workspace's** own version (currently `0.10.0`) — unrelated to the app's version above. This is the whole `dod-studio` workspace (native/analysis/dem-patch/etc.), not desktop-studio specifically. Don't confuse the two when checking "what version are we on."

**Important:** the release workflows stamp `tauri.conf.json`/`package.json`/`Cargo.toml` with the computed release version *during the CI run only* — that stamp is **not committed back** to the repo. The checked-in versions in these files stay whatever they were (currently `0.1.0` everywhere) until someone deliberately bumps them as part of a release PR (see "Bumping minor/major" below). So `git grep version` in this repo will not tell you what's actually been released — check GitHub Releases instead.

## Stable channel — automatic

`.github/workflows/release_stable.yml` fires on every push to `main`. Since `main` is ruleset-protected (PR-only, no direct pushes), every push here really is a `dev` → `main` release-cutover merge — there's no other kind of push that lands on `main`.

**Version is auto-computed, no manual entry required:**
1. Read `Cargo.toml`'s checked-in version, take just the `major.minor` (e.g. `0.1`).
2. Look at existing git tags matching `v<major>.<minor>.*` and find the highest patch number.
3. Next patch = highest + 1 (or `0` if no tag under that major.minor exists yet).

So a routine merge to `main` just becomes the next patch automatically — `0.1.0` → `0.1.1` → `0.1.2` → ... with zero manual steps. This is why "main is 0.1.1" even though `Cargo.toml` still says `0.1.0` on disk: `0.1.1` is a tag/release that happened, not a file edit that landed.

The workflow also accepts an optional `workflow_dispatch` `version` input — a manual override for the rare case that needs one (e.g. skipping a patch number, or re-cutting a release). Leave it blank for the normal auto-computed path.

Published as a real (non-draft, non-prerelease) GitHub Release tagged `v<version>`. The `stable` update channel polls this via GitHub's `/releases/latest` alias, which only ever resolves to a release like this one.

## Experimental channel — manual, on-demand

`.github/workflows/release_experimental.yml` is `workflow_dispatch`-only — it never fires automatically, not even on push to `dev`. You trigger it by hand whenever you want to publish a build for experimental-channel testing.

**Version entry is currently required, and free-text:**
- The `version` input asks for "the version this build is heading toward" (e.g. `0.1.2`) — your own guess at what the *next* stable version will eventually be, not a value read from anywhere.
- The workflow appends `-<GitHub run number>` automatically (e.g. `0.1.2-47`). This isn't cosmetic — it's load-bearing for three reasons documented in the workflow itself:
  1. A semver pre-release suffix (`-47`) always sorts *below* the plain version it's attached to, so an experimental build only ever offers itself as an update to someone already on an older experimental build — never to someone on the matching stable release.
  2. The run number is monotonically increasing across dispatches, unlike a git short-SHA (which sorts alphabetically, not chronologically) — so a second experimental build always looks newer than the first.
  3. The MSI/WiX bundler Tauri uses on Windows only accepts a single, purely-numeric pre-release segment. `-dev.<sha>` and `-dev.<run number>` were both tried and rejected by real builds before landing on the current bare `-<run number>` — see the workflow's own comment and [this Tauri discussion](https://github.com/tauri-apps/tauri/discussions/7600) for the failure mode.

Published as a **prerelease**, always overwriting the same fixed tag `experimental-latest` (the previous `experimental-latest` release+tag is deleted first). The `experimental` update channel polls this via the fixed URL `.../releases/download/experimental-latest/latest.json` — a direct tag reference, not `/releases/latest`, since GitHub's `/latest` alias never resolves to a prerelease.

## Bumping to a new minor or major version

Not automatic on either channel — deliberately a human decision. Edit `desktop-studio/src-tauri/Cargo.toml`'s `version` field (e.g. `0.1.0` → `0.2.0`) as part of whatever PR is landing the milestone that justifies it, same as any other code change. The next push to `main` after that lands will have no existing `v0.2.*` tags yet, so `release_stable.yml`'s patch computation naturally starts that new line at `0.2.0`.

## Open questions (not yet decided — your call)

**Should the experimental channel's version input auto-detect instead of staying manual?**
Right now it asks you to type a version guess every time. It *could* mirror stable's approach — read `dev`'s own `Cargo.toml` major.minor and auto-compute a patch guess the same way (highest existing `v<major>.<minor>.*` tag + 1), making the input optional with that as the default and a manual override still available for a deliberate minor/major signal. The one thing that computation can't know on its own is *intent* — whether the work currently on `dev` is heading toward a patch or a minor/major bump — so it would still occasionally need a manual override, just less often than every single dispatch. Worth doing if the manual entry keeps being a "wait, what number was I on" moment; not worth doing if it rarely comes up. (This would mean editing `release_experimental.yml` — flag if you want that built.)

**Should a stable release also trigger a matching experimental release, so the experimental channel doesn't fall behind?**
Depends on what "falls behind" means at the moment it happens. Right after a `dev` → `main` merge, `dev` and `main` are usually at the *same commit* (the merge that cut the stable release is the same commit dev was already on) — so re-cutting an experimental release in that exact window would just republish identical code under a different version string. The real risk isn't "the experimental channel is missing what stable has" (it never is, right after a merge) — it's "`experimental-latest` still points at an *older* prerelease build from before that merge," which could look stale to someone checking the experimental channel even though `dev` the branch has already moved on. So the more useful trigger is probably: cut a fresh experimental release whenever meaningful new work has landed on `dev` *since* the last experimental release — not automatically lockstep with every stable cutover, which would frequently be a same-commit no-op.

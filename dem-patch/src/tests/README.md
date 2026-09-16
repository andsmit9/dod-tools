# Test fixtures

This folder is where `dem-patch`'s tests (`src/lib.rs`) expect a small demo
fixture — `demotest.dem` — to live. It's tracked in git (unlike `/demos/`, which is
gitignored scratch space for real-world debugging demos), so anything committed here
should be genuinely tiny.

`demotest.dem` (731 bytes) is **not** a trimmed-down real recording — trimming one
down to a minimal, still-valid fixture turned out to be trickier than it sounds (see
[issue #16](https://github.com/ccoventry/dod-tools/issues/16)'s original discussion).
It's a demo built entirely in code instead: one directory entry holding the smallest
frame sequence `parse_directory`/`write_to_bytes` actually round-trip —
`DemoStart`, one `ConsoleCommand`, `NextSection`. A `NetworkMessage` frame needs a
fully populated `DemoInfo` (ref params, usercmd, movevars, ...) for no benefit here,
since these tests exist to exercise open/write/parse-mode plumbing, not to be a
realistic demo.

To regenerate it (only needed if the on-disk format itself changes):

    cargo test -p dem --lib test::regenerate_fixture -- --ignored

That test lives in `src/lib.rs` beside the four it unblocks (`open`,
`open_without_netmessage`, `write`, `read_a_lot`) and is itself always `#[ignore]`d —
running it again only matters if you're intentionally changing what the fixture
contains.

Files these tests produce as output (`demotest_out.dem`, `demo2test.dem`) are
gitignored — only commit the source fixture itself.

Run with `cargo test -p dem --lib` from the workspace root — `dem-patch` is pulled
in only via `[patch.crates-io]`, not a `[workspace]` member (see the repo's
`CLAUDE.md`), so `cargo test --workspace` alone still won't reach these.

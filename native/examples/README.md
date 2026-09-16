# Capture-pipeline probes

One-off diagnostic and R&D tools written while building the patcher, the decal
flush, the BSP reader and the OBS capture path. They exist so the reasoning
behind those features is reproducible rather than remembered.

These are `examples/`, so they are not part of any shipped binary and are not
autodiscovered as extra targets. `cargo build` and `cargo test` ignore them;
`cargo build --examples` compiles them.

They were previously in `native/src/bin/`, where Cargo autodiscovered all 28
files as binaries. That meant every one of them — plus a linked test harness
each — was rebuilt on every `cargo build --workspace` and `cargo test
--workspace`, roughly doubling the incremental cost of a change to
`native/src/lib.rs`, the crate everything else depends on. Moved 2026-09-10.
`native/Cargo.toml` now sets `autobins = false` and lists the seven real
binaries explicitly, so this cannot silently happen again.

## Running one

    cargo run --release -p native --example probe_decals -- path/to/demo.dem

## The probes

### Demo structure and frames

| Probe | Question it answers |
| --- | --- |
| `frame_inspector` | What frames are in this demo, in what order, at what ordinal? |
| `debug_cmds` | Which `ConsoleCommand` frames does a demo already carry? |
| `debug_scanner` | What does the highlight scanner see in this demo? |
| `check_ticks` | Do the batch queue's computed tick boundaries line up with the demo? |
| `demo_tickrate` | What frame rate was this demo actually recorded at? |
| `test_builder` | Does `build_batch_queue` produce a sane plan for this input? |
| `find_overlaps` | Which highlights are close enough to merge into one take? |

### HLTV and the director channel

| Probe | Question it answers |
| --- | --- |
| `diagnostic_hltv_director` | What `svc_director` commands does this HLTV demo carry? |
| `diagnostic_hltv_injector` | Does an injected director frame survive a round trip? |
| `inspect_reconnect` | What happens to entity state across a mid-demo reconnect? |
| `inspect_bewton`, `inspect_lenn` | Ad-hoc reads of two specific problem demos |

### Decals, geometry and visibility

| Probe | Question it answers |
| --- | --- |
| `probe_decals` | Where do decals land, and at what offsets from a surface? |
| `probe_flush_positions` | Which sweep positions does a demo actually resolve to? |
| `probe_visibility` | Is a candidate position genuinely hidden from the capture camera? |
| `probe_map_candidates` | Which maps in the library can serve a given demo? |
| `survey_decal_flush` | Corpus sweep: how many positions does each demo get? |
| `verify_decal_pipeline` | Does a flushed demo come out with frame ordinals intact? |
| `validate_bsp` | Does the BSP reader agree with coordinates the engine accepted? |

### Capture backends

| Probe | Question it answers |
| --- | --- |
| `probe_obs` | Does the obs-websocket v5 handshake and record cycle work end to end? |

`probe_obs` carries its own copy of the v5 auth handshake. The production
implementation is `native/src/obs/client.rs`, which is where the pinned
handshake tests live — don't treat the copy here as authoritative.

### Hashing

| Probe | Question it answers |
| --- | --- |
| `hash_test` | What does `fnv1a_hash` return for a given demo header? |

// patch/mod.rs
// Public surface of the patch module.
//
// Declares all sub-modules and re-exports every public item that was previously
// at the flat `native::patch::*` path. All existing call sites remain unchanged.
//
// Sub-module creation order (Phase 10 sequence):
//   Step 1 (current): types.rs, mod.rs  ← foundation
//   Step 2 (pending): highlevel.rs      ← dem-crate high-level API
//   Step 3 (pending): engine.rs         ← StreamPatcher binary I/O
//   Step 4 (pending): builder.rs        ← build_batch_queue, spawn_patch_batch
//   Step 5 (pending): scanner.rs        ← scan_demo_for_highlights, is_hltv_demo

// Engine & Memory Limits
pub const MAX_CONSOLE_CMD_LEN: usize = 64;
pub const MAX_CONSOLE_CMD_SAFE_LEN: usize = 63;
pub const MAX_DIRECTOR_STUFFTEXT_LEN: usize = 253;
pub const IO_BUFFER_CAPACITY: usize = 262_144;
pub const MAX_PAYLOAD_LIMIT_BYTES: usize = 2_097_152;

// Binary Frame & Header Sizes
pub const HLTV_HEADER_SIZE: usize = 512;
pub const DEMO_HEADER_SIZE: usize = 544;
pub const DIRECTORY_OFFSET_POS: usize = 540;
pub const FRAME_HEADER_SIZE: usize = 9;
pub const NETMSG_INFO_SIZE: usize = 464;
pub const NETWORK_HEADER_ALIGNMENT: usize = 468;
pub const DIR_ENTRY_SIZE: usize = 92;
pub const SCANNER_SECTION_BOUNDARY: u8 = 5;

// Frame Type Payload Sizes
pub const CMD_FRAME_SIZE: usize = 64;
pub const CLIENT_DATA_FRAME_SIZE: usize = 32;
pub const EVENT_FRAME_SIZE: usize = 84;

// Command Injection Logic
pub const MAX_ECHO_CHUNK_SIZE: usize = 55;
pub const CUSTOM_CMD_WARN_LIMIT: usize = 60;
pub const PRIMER_DELAY_TICKS: i32 = 500;

/// Upper bound on the size of a `NetworkMessage` frame the decal passes will
/// append injected payload to.
///
/// A frame at or above this is already close enough to the engine's own read
/// budget that adding to it risks an `svc_bad` on playback, so the passes skip
/// it and use a smaller neighbour instead — there are always plenty. This is a
/// safety margin chosen against the engine's behaviour, not a value the format
/// states anywhere, which is exactly why it wants a name rather than a `1024`
/// sitting in two files. `decal_probe` and `decal_strip` both filter on it via
/// `is_injectable_frame`.
pub const MAX_INJECTABLE_MESSAGE_LEN: u32 = 1024;

/// The engine's own ceiling on the decal ring. `r_decals` is clamped to this,
/// so a sweep of this size turns a full revolution regardless of what the cvar
/// is set to — which is what lets the pipeline stop pinning it. See
/// `decal_strip` and `docs/archive/decal_flush_bsp_surfaces.md`.
pub const MAX_RENDER_DECALS: u32 = 4096;
pub const BREADCRUMB_INTERVAL_TICKS: i32 = 5000;

pub mod types;

#[cfg(not(target_arch = "wasm32"))]
pub mod highlevel;
#[cfg(not(target_arch = "wasm32"))]
pub mod decal_strip;
#[cfg(not(target_arch = "wasm32"))]
pub mod decal_atlas;
#[cfg(not(target_arch = "wasm32"))]
pub mod bsp;
#[cfg(not(target_arch = "wasm32"))]
pub mod bsp_entities;
#[cfg(not(target_arch = "wasm32"))]
pub mod reachability;
#[cfg(not(target_arch = "wasm32"))]
pub mod cfg_scan;
pub mod map_check;
#[cfg(not(target_arch = "wasm32"))]
pub mod map_fetch;
#[cfg(not(target_arch = "wasm32"))]
pub mod decal_probe;
#[cfg(not(target_arch = "wasm32"))]
pub mod engine;
#[cfg(not(target_arch = "wasm32"))]
pub mod builder;
#[cfg(not(target_arch = "wasm32"))]
pub mod scanner;

// ── Re-export wall ────────────────────────────────────────────────────────────
// All items below were previously at the top level of patch.rs.
// Every existing `native::patch::*` call site resolves here unchanged.

pub use types::{
    MAX_PAYLOAD_SIZE,
    CustomCommand,
    CommandRelation,
    CaptureCodec,
    CaptureMode,
    ObsConfig,
    PatchOptions,
    CaptureStreak,
    CaptureBlock,
    PatchJob,
    DriveHeadroom,
    PatcherConfig,
    HighlightRules,
    HighlightStatus,
    default_goldsrc_hooks_dll_path,
};

#[cfg(not(target_arch = "wasm32"))]
pub use types::{PatchEvent, CaptureWorker};

#[cfg(not(target_arch = "wasm32"))]
pub use highlevel::patch_demo_highlights;

#[cfg(not(target_arch = "wasm32"))]
pub use decal_strip::{
    capture_fov, clean_demo_decals, on_screen_half_angle, prepare_flushed_source,
    DEFAULT_LEAD_SECONDS,
    proven_world_coordinates, ring_limit, ring_limit_from_init, ring_limit_from_game_config,
    strip_decals_outside_windows,
    CleanedSource, DecalCleanOptions, DecalCleanStats, FlushSource, VisibilityBasis,
    DECALS_PER_POSITION, MAX_OVERLAP_DECALS,
};

#[cfg(not(target_arch = "wasm32"))]
pub use decal_probe::{
    best_view_for, camera_at_time, decal_texture_histogram, probe_decal_offsets, project,
    CameraView, GridStats, Probe, ProbeOptions, ProbeRow,
    ProbeStats, Sighting,
};

#[cfg(not(target_arch = "wasm32"))]
pub use cfg_scan::{scan as scan_game_cfgs, CfgScan, CvarSetting, WATCHED_CVARS};

#[cfg(not(target_arch = "wasm32"))]
pub use decal_strip::{capture_fov_from_init, capture_fov_resolved};

#[cfg(not(target_arch = "wasm32"))]
pub use map_check::{check_demo, map_reference, MapReference, MapStatus};

#[cfg(not(target_arch = "wasm32"))]
pub use map_fetch::{fetch_map, map_url, FetchOutcome, DEFAULT_MIRROR};

#[cfg(not(target_arch = "wasm32"))]
pub use engine::StreamPatcher;

#[cfg(not(target_arch = "wasm32"))]
pub use builder::{build_batch_queue, final_init_commands, spawn_patch_batch, WorkspaceGuard, build_director_message, build_director_stufftext, build_preview_patch_jobs, playdemo_safe_stem};

#[cfg(not(target_arch = "wasm32"))]
pub use scanner::{is_hltv_demo, scan_demo_for_highlights, scan_demo_for_highlights_with_analysis};

/// Whether a frame can carry injected payload: it must be a `NetworkMessage`
/// whose contents were actually parsed (an unparsed one is opaque bytes there
/// is nothing safe to append to) and under [`MAX_INJECTABLE_MESSAGE_LEN`].
///
/// `entry_idx`/`frame_idx` index `demo.directory.entries[..].frames[..]`; an
/// out-of-range pair is simply not injectable rather than a panic, so callers
/// can hand this raw indices straight out of a frame-ordinal walk.
#[cfg(not(target_arch = "wasm32"))]
pub fn is_injectable_frame(demo: &dem::types::Demo, entry_idx: usize, frame_idx: usize) -> bool {
    use dem::types::{FrameData, MessageData};

    demo.directory
        .entries
        .get(entry_idx)
        .and_then(|entry| entry.frames.get(frame_idx))
        .is_some_and(|frame| match &frame.frame_data {
            FrameData::NetworkMessage(b) => {
                matches!(b.1.messages, MessageData::Parsed(_))
                    && b.1.message_length < MAX_INJECTABLE_MESSAGE_LEN
            }
            _ => false,
        })
}

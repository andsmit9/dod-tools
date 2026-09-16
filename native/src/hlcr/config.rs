#![cfg(not(target_arch = "wasm32"))]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum RenderCodec {
    NvencH264,
    /// Software libx264 — not in dev's original 3-variant enum. Added to
    /// preserve the Tauri rebuild's deliberate choice (see
    /// docs/archive/tauri_parity_audit.md Area 5) to keep both software and NVENC
    /// H.264 as separate, explicit options rather than picking one.
    H264Software,
    ProRes,
    DnxHr,
    /// "Skip" — leave an OBS take exactly as OBS wrote it: no FFmpeg pass at
    /// all, just a copy into the export pool under the pipeline's naming.
    /// Only meaningful for a clip with its own muxed-in audio (`ClipData.wav_file
    /// == None`); `run_render_job` rejects it for anything else.
    SourceCopy,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RenderConfig {
    pub ffmpeg_path: String,
    pub source_folder: String,
    #[serde(default)]
    pub export_directories: Vec<PathBuf>,
    pub fps: u32,
    pub target_codec: RenderCodec,
    pub max_concurrent_renders: usize,
}

impl RenderCodec {
    /// Maps the frontend's codec-select string values to the enum used by
    /// `run_render_job`. Unrecognized values fall back to ProRes, matching
    /// `desktop-studio`'s prior `codec_args_and_ext()` default.
    pub fn from_str_id(id: &str) -> Self {
        match id {
            "h264" => Self::H264Software,
            "h264_nvenc" => Self::NvencH264,
            "dnxhr" => Self::DnxHr,
            "source_copy" => Self::SourceCopy,
            _ => Self::ProRes,
        }
    }

    /// Inverse of `from_str_id` — round-trips through the same string ids
    /// (not `Debug` formatting) so autosave recovery can parse a persisted
    /// codec back into the enum without a second, drifting mapping.
    pub fn to_str_id(self) -> &'static str {
        match self {
            Self::H264Software => "h264",
            Self::NvencH264 => "h264_nvenc",
            Self::DnxHr => "dnxhr",
            Self::ProRes => "prores",
            Self::SourceCopy => "source_copy",
        }
    }

    /// Short human-readable label for UI display (e.g. the render job
    /// table's per-job Settings column).
    pub fn label(self) -> &'static str {
        match self {
            Self::H264Software => "H.264 (Software)",
            Self::NvencH264 => "H.264 (NVENC)",
            Self::DnxHr => "DNxHR",
            Self::ProRes => "ProRes",
            Self::SourceCopy => "Skip (Keep Original)",
        }
    }
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            ffmpeg_path: "ffmpeg".to_string(),
            source_folder: "".to_string(),
            export_directories: Vec::new(),
            fps: 300,
            target_codec: RenderCodec::ProRes,
            max_concurrent_renders: 2,
        }
    }
}

pub fn get_config_path() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(parent) = exe_path.parent() {
            return parent.join("hlcr_config.json");
        }
    PathBuf::from("hlcr_config.json")
}

pub fn load_config() -> RenderConfig {
    let path = get_config_path();
    if path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(config) = serde_json::from_str::<RenderConfig>(&content) {
                return config;
            }
    let default_config = RenderConfig::default();
    let _ = save_config(&default_config);
    default_config
}

pub fn save_config(config: &RenderConfig) -> Result<(), String> {
    let path = get_config_path();
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Failed to write config file to {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_str_id_round_trips_through_every_codec() {
        // to_str_id/from_str_id must stay inverses — autosave recovery
        // (render_manager.rs's recover_render_batch) round-trips a persisted
        // codec through exactly this pair.
        for codec in [RenderCodec::NvencH264, RenderCodec::H264Software, RenderCodec::ProRes, RenderCodec::DnxHr, RenderCodec::SourceCopy] {
            assert_eq!(RenderCodec::from_str_id(codec.to_str_id()), codec);
        }
    }

    #[test]
    fn test_from_str_id_unrecognized_falls_back_to_prores() {
        assert_eq!(RenderCodec::from_str_id("not_a_real_codec"), RenderCodec::ProRes);
    }
}

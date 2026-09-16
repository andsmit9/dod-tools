// patch/highlevel.rs
// High-level demo patching via the `dem` crate's in-memory API.
// Operates entirely on byte slices — no std::fs calls inside this module.
// Consumed by: cli.rs, pipeline.rs, main.rs (single-demo highlight injection path).

use dem::open_demo_from_bytes;
use dem::types::{Frame, FrameData, ConsoleCommand, ByteString};

use crate::patch::types::{PatchOptions, CommandRelation};

pub fn patch_demo_highlights(
    demo_bytes: &[u8],
    intervals: &[(f32, f32)], // (start_time, stop_time)
    options: &PatchOptions,
) -> Result<Vec<u8>, String> {
    let mut demo = open_demo_from_bytes(demo_bytes)
        .map_err(|e| format!("Could not parse demo file: {}", e))?;

    // Find the Playback entry
    let mut entry = demo.directory.entries.iter_mut().find(|e| e.type_ == 1);
    if entry.is_none() {
        entry = demo.directory.entries.iter_mut().find(|e| {
            e.description
                .to_str()
                .unwrap_or("")
                .contains("Playback")
        });
    }
    let entry = entry.ok_or_else(|| "Could not find Playback entry in demo directory".to_string())?;

    let start_of_playback = entry.frames.first().map(|f| f.time).unwrap_or(0.0);

    // 1. Inject init commands
    for cmd in &options.init_commands {
        entry.frames.push(Frame {
            time: start_of_playback,
            frame: 0,
            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                command: ByteString::from(cmd.as_str()),
            }),
        });
    }

    // Sort intervals
    let mut sorted_intervals = intervals.to_vec();
    sorted_intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let ff_speed = options.fast_forward_speed.unwrap_or(0.2);
    let initial_delay = options.initial_delay.unwrap_or(3.0);
    let pre_record_buffer = options.pre_record_buffer.unwrap_or(6.0);
    let record_start_lead = options.record_start_lead.unwrap_or(2.0);
    let record_stop_trail = options.record_stop_trail.unwrap_or(2.0);
    let post_record_buffer = options.post_record_buffer.unwrap_or(4.0);

    let ff_cmd = format!("host_framerate {}", ff_speed);
    let overlap_threshold = record_start_lead + record_stop_trail;

    // Merge close or overlapping intervals (where recording periods would overlap)
    let mut merged_intervals: Vec<(f32, f32)> = Vec::new();
    for interval in sorted_intervals {
        if let Some(last) = merged_intervals.last_mut() {
            if interval.0 <= last.1 + overlap_threshold {
                last.1 = last.1.max(interval.1);
            } else {
                merged_intervals.push(interval);
            }
        } else {
            merged_intervals.push(interval);
        }
    }

    // Fast forward initially if first highlight is a bit in
    if let Some(&(first_start, _)) = merged_intervals.first()
        && first_start > initial_delay + pre_record_buffer {
            entry.frames.push(Frame {
                time: start_of_playback + initial_delay,
                frame: 0,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from(ff_cmd.as_str()),
                }),
            });
        }

    for &(start_time, stop_time) in &merged_intervals {
        // Check if player died within 5.0 seconds before streak start
        let mut delay_deathnotice = false;
        if let Some(ref deaths) = options.player_deaths {
            for &death_time in deaths {
                if death_time >= start_time - 5.0 && death_time < start_time {
                    delay_deathnotice = true;
                    break;
                }
            }
        }

        // a. Pre-Streak Normalization: drop speed and run stop_commands
        let norm_time = (start_time - pre_record_buffer).max(start_of_playback + initial_delay + 0.1);

        entry.frames.push(Frame {
            time: norm_time,
            frame: 0,
            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                command: ByteString::from("host_framerate 0"),
            }),
        });

        if let Some(ref player_name) = options.hltv_spec_player {
            let spec_cmd = format!("spec_player \"{}\"", player_name);
            entry.frames.push(Frame {
                time: norm_time + 0.005,
                frame: 0,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from(spec_cmd.as_str()),
                }),
            });
            entry.frames.push(Frame {
                time: norm_time + 0.006,
                frame: 0,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from("spec_mode 4"),
                }),
            });
        }

        for item in &options.custom_commands {
            let mut time = match item.relation {
                CommandRelation::Before => start_time - item.offset,
                CommandRelation::After => stop_time + item.offset,
            };

            // Apply death notice delay scenario
            if delay_deathnotice
                && item.relation == CommandRelation::Before
                && item.command.to_lowercase().contains("hud_deathnotice_time")
                && time < start_time - 0.01
            {
                time = start_time - 0.01;
            }

            let final_time = time.max(start_of_playback + initial_delay + 0.1);

            entry.frames.push(Frame {
                time: final_time,
                frame: 0,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from(item.command.as_str()),
                }),
            });
        }

        // b. Record Start: mirv_recordmovie_start
        let record_start_time = (start_time - record_start_lead).max(start_of_playback + initial_delay + 0.2);
        entry.frames.push(Frame {
            time: record_start_time,
            frame: 0,
            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                command: ByteString::from("mirv_recordmovie_start"),
            }),
        });

        // c. Record Stop: mirv_recordmovie_stop
        let record_stop_time = stop_time + record_stop_trail;
        entry.frames.push(Frame {
            time: record_stop_time,
            frame: 0,
            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                command: ByteString::from("mirv_recordmovie_stop"),
            }),
        });

        // d. Post-Streak Fast-Forward: resume host_framerate <ff_speed>
        let resume_ff_time = stop_time + post_record_buffer;
        entry.frames.push(Frame {
            time: resume_ff_time,
            frame: 0,
            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                command: ByteString::from(ff_cmd.as_str()),
            }),
        });
    }

    // e. Exit On Finish: quit command after the last stop record
    if options.exit_on_finish
        && let Some(&(_, last_stop)) = merged_intervals.last() {
            let quit_time = last_stop + post_record_buffer + 0.5;
            entry.frames.push(Frame {
                time: quit_time,
                frame: 0,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from("quit"),
                }),
            });
        }

    // Sort frames by time
    entry.frames.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));

    // Re-index frame numbers sequentially
    for (idx, frame) in entry.frames.iter_mut().enumerate() {
        frame.frame = idx as i32;
    }

    // Update entry header values if necessary
    entry.frame_count = entry.frames.len() as i32;

    Ok(demo.write_to_bytes())
}

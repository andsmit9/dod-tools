//! GoldSrc demo parser and writer
//!
//! # Example
//!
//! ```no_run
//! use dem::open_demo;
//! use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
//!
//! let mut demo = open_demo("./src/tests/demotest.dem").unwrap();
//!
//! for entry in &mut demo.directory.entries {
//!     for frame in &mut entry.frames {
//!         if let FrameData::NetworkMessage(ref mut box_type) = &mut frame.frame_data {
//!             let data = &mut box_type.as_mut().1;
//!             
//!             if let MessageData::Parsed(messages) = &mut data.messages {
//!                 messages.push(NetMessage::EngineMessage(Box::new(EngineMessage::SvcBad)));
//!             };
//!         }
//!     }
//! }
//!
//! demo.write_to_file("./src/tests/demo2test.dem").unwrap();
//! ```
#![allow(mismatched_lifetime_syntaxes)]

use std::{ffi::OsStr, path::Path};
use nom::{combinator::all_consuming, multi::many0, Parser};
use types::{AuxRefCell, ByteVec, DeltaDecoderTable, Demo, NetMessage};

use nom_helper::Result;

mod byte_writer;
mod delta;
mod nom_helper;
mod utils;

pub mod bit;
pub mod demo_parser;
pub mod demo_writer;
pub mod netmsg_doer;
pub mod prelude;
pub mod types;

pub use utils::bitslice_to_string;

// /// Re-exporting hldemo to have latest changes than 0.3.0 hldemo
// pub extern crate hldemo;

/// Re-exporting bitvec to avoid clogging the main project
pub extern crate bitvec;

/// Parses all bytes in `data.msg` for each demo frame.
///
///
pub fn parse_netmsg(i: &[u8], aux: AuxRefCell) -> Result<Vec<NetMessage>> {
    let parser = move |i| NetMessage::parse(i, aux.clone());
    all_consuming(many0(parser)).parse(i)
}

/// Should be used for replacing `data.msg` of each frame.
pub fn write_netmsg(i: &Vec<NetMessage>, aux: AuxRefCell) -> ByteVec {
    let mut res: ByteVec = vec![];

    for message in i {
        res.append(&mut message.write(aux.clone()))
    }

    res
}

/// Opens a demo
///
/// # Example
/// ```no_run
/// use dem::open_demo;
///
/// let demo = open_demo("./tests/demotest.dem").unwrap();
/// ```
pub fn open_demo(demo_path: impl AsRef<Path> + AsRef<OsStr>) -> eyre::Result<Demo> {
    Demo::parse_from_file(demo_path, types::MessageDataParseMode::Parse)
}

pub fn open_demo_from_bytes(demo_bytes: &[u8]) -> eyre::Result<Demo> {
    Demo::parse_from_bytes(demo_bytes, types::MessageDataParseMode::Parse)
}

/// Writes a [`u32`] into [`types::BitVec`]
#[macro_export]
macro_rules! nbit_num {
    ($num:expr, $bit:expr) => {{
        use $crate::bit::BitWriter;

        let mut writer = BitWriter::new();
        writer.append_u32_range($num as u32, $bit);
        writer.data
    }};
}

/// Writes a string into [`types::BitVec`]
#[macro_export]
macro_rules! nbit_str {
    ($name:expr) => {{
        use $crate::bit::BitWriter;

        let mut writer = BitWriter::new();
        $name.as_bytes().iter().for_each(|s| writer.append_u8(*s));
        writer.data
    }};
}

#[cfg(test)]
mod test {
    use super::*;

    /// Regenerates `./src/tests/demotest.dem`, the small tracked fixture the
    /// four tests below need. Not run automatically -- there is nothing to
    /// regenerate *from*, this constructs a minimal demo entirely in code
    /// rather than trimming a real recording, so running it again only
    /// matters if the on-disk format itself changes.
    ///
    /// A real DoD/HLTV demo carries thousands of frames; this carries three
    /// -- DemoStart, one ConsoleCommand, NextSection -- which is the smallest
    /// shape `parse_directory`/`write_to_bytes` actually round-trip: a
    /// `NetworkMessage` frame needs a fully populated `DemoInfo` (ref params,
    /// usercmd, movevars, ...) for no benefit here, since these tests exist to
    /// exercise open/write/parse-mode plumbing, not to be a realistic demo.
    /// `frame_count`/`frame_offset`/`file_length`/`directory_offset` are left
    /// at 0 -- `Demo::write_to_bytes` recomputes all four from the actual
    /// frames and byte offsets rather than trusting these fields, which is
    /// also why a hand-built `Demo` can produce a valid file at all.
    #[test]
    #[ignore = "regenerates the tracked fixture; run manually with `cargo test -p dem --lib test::regenerate_fixture -- --ignored`"]
    fn regenerate_fixture() {
        use crate::types::{ConsoleCommand, Directory, DirectoryEntry, Frame, FrameData, Header};

        let demo = Demo {
            header: Header {
                magic: b"HLDEMO\x00\x00".to_vec(),
                demo_protocol: 5,
                network_protocol: 48,
                map_name: "dod_test".into(),
                game_directory: "dod".into(),
                map_checksum: 0,
                directory_offset: 0,
            },
            directory: Directory {
                entries: vec![DirectoryEntry {
                    type_: 1,
                    description: "fixture".into(),
                    flags: 0,
                    cd_track: 0,
                    track_time: 0.0,
                    frame_count: 0,
                    frame_offset: 0,
                    file_length: 0,
                    frames: vec![
                        Frame { time: 0.0, frame: 0, frame_data: FrameData::DemoStart },
                        Frame {
                            time: 0.0,
                            frame: 0,
                            frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                                command: "echo dem-patch test fixture".into(),
                            }),
                        },
                        Frame { time: 0.0, frame: 1, frame_data: FrameData::NextSection },
                    ],
                }],
            },
            _aux: None,
        };

        demo.write_to_file("./src/tests/demotest.dem").unwrap();
    }

    #[test]
    fn open() {
        open_demo("./src/tests/demotest.dem").unwrap();
    }

    #[test]
    fn open_without_netmessage() {
        Demo::parse_from_file(
            "./src/tests/demotest.dem",
            types::MessageDataParseMode::Parse,
        )
        .unwrap();
    }

    #[test]
    fn write() {
        let dem = open_demo("./src/tests/demotest.dem").unwrap();
        dem.write_to_file("./src/tests/demotest_out.dem").unwrap();
    }

    #[test]
    fn read_a_lot() {
        let folder = "./src/tests/";

        std::fs::read_dir(folder)
            .map(|res| {
                res.filter_map(|entry| entry.ok()).for_each(|entry| {
                    let path = entry.path();

                    if path.is_dir() {
                        return;
                    }

                    if path.extension().map(|ext| ext != "dem").unwrap_or(false) {
                        return;
                    }

                    assert!(
                        open_demo(path.as_path()).is_ok(),
                        "error opening {}",
                        path.display()
                    )
                })
            })
            .unwrap_or_else(|e| panic!("could not read the test fixture directory: {e}"));
    }
}

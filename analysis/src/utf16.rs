//! Shared decoder for the Valve localization `.txt` files, which ship in a mix
//! of UTF-8 and UTF-16 (with and without a BOM).
//!
//! This file is compiled into the crate *and* `#[path]`-included by
//! `analysis/build.rs`. A build script is its own crate and cannot `use
//! analysis::…`, so the two consumers share the source file rather than a
//! module path. Keep it dependency-free and `std`-only for that reason.
//!
//! The two halves must agree: the build script embeds these files for wasm
//! while the native path reads them off disk, and a decoding difference between
//! them would silently produce two different localization tables (#236).

use std::path::Path;

pub(crate) fn read_to_string_lossy_utf16_or_utf8(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.len() >= 2 {
        if bytes[0] == 0xFF && bytes[1] == 0xFE {
            // UTF-16 LE
            let u16_chars: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_le_bytes(*chunk))
                .collect();
            return Ok(String::from_utf16_lossy(&u16_chars));
        } else if bytes[0] == 0xFE && bytes[1] == 0xFF {
            // UTF-16 BE
            let u16_chars: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_be_bytes(*chunk))
                .collect();
            return Ok(String::from_utf16_lossy(&u16_chars));
        }
    }

    match String::from_utf8(bytes.clone()) {
        Ok(s) => Ok(s),
        Err(_) => {
            let has_nulls = bytes.iter().enumerate().any(|(i, &b)| b == 0 && i % 2 == 1);
            if has_nulls && bytes.len() % 2 == 0 {
                let u16_chars: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_le_bytes(*chunk))
                    .collect();
                Ok(String::from_utf16_lossy(&u16_chars))
            } else {
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
    }
}

// patch/bsp_entities.rs
// Read and rewrite a GoldSrc BSP's entity lump.
//
// ── Why a map file gets edited at all ────────────────────────────────────────
// `dod_lennon2` puts 276 entities on the wire from its opening snapshot, and
// the pre-Anniversary engine's `MAX_PACKET_ENTITIES` is 256, so every HLTV
// recording of that map closes `hl.exe` to desktop the moment playback starts
// (#207). The map's own entities account for essentially all of that count;
// gameplay adds only ~15-25 on top. Trimming the entity lump is therefore the
// one edit that fixes the map rather than working around it.
//
// ── Why only the entity lump ─────────────────────────────────────────────────
// The map checksum the engine stamps into a demo header is a CRC-32 over lumps
// 1..14 with the entity lump *excluded* (`bsp::map_checksum`). Editing lump 0
// alone therefore leaves the checksum untouched, so a trimmed map still loads
// every demo recorded against the original — which is the only reason shipping
// a modified BSP is defensible at all. `rewrite_entity_lump` rebuilds the file
// around a new lump 0 and nothing else, and the caller is expected to assert
// the checksum afterwards; `map_entity_trim` refuses to write if it moved.
//
// This module never writes over its input. It returns bytes; choosing a
// destination is the caller's problem, and the map in a game directory belongs
// to the user.

use std::fmt::Write as _;

const LUMP_COUNT: usize = 15;
const LUMP_ENTITIES: usize = 0;
const HEADER_SIZE: usize = 4 + LUMP_COUNT * 8;

/// One entity from the lump: an ordered list of key/value pairs.
///
/// Ordered, and not a map, because the lump is the user's file — a rewrite that
/// reorders or dedupes keys is a bigger edit than the one being asked for, and
/// duplicate keys are legal (`light` writes several `style` keys in some
/// compilers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapEntity {
    pub pairs: Vec<(String, String)>,
}

impl MapEntity {
    /// First value for `key`, or `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn classname(&self) -> &str {
        self.get("classname").unwrap_or("")
    }

    /// The submodel this entity draws, for a brush entity (`"model" "*7"`).
    ///
    /// Brush entities are the ones that matter for a packet-entity budget:
    /// each is exactly one submodel, so the number below is a one-to-one key
    /// between the lump and what a demo carries on the wire.
    pub fn brush_submodel(&self) -> Option<u32> {
        self.get("model")?.strip_prefix('*')?.parse().ok()
    }

    /// `renderamt`, defaulting the way the engine does when the key is absent.
    ///
    /// The default matters: a `func_button` with `rendermode 2` and no
    /// `renderamt` key is not faintly visible, it is *invisible*, and that is
    /// what makes it free to remove.
    pub fn render(&self) -> (i32, i32) {
        let mode = self.get("rendermode").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let amt = self.get("renderamt").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        (mode, amt)
    }
}

/// Parses the entity lump text into entities, preserving key order.
///
/// Tolerant in the same places the engine is: anything outside a `{ }` block is
/// skipped, and the lump's trailing NUL terminates it.
pub fn parse_entity_text(text: &str) -> Result<Vec<MapEntity>, String> {
    let mut out = Vec::new();
    let mut current: Option<MapEntity> = None;
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match c {
            '\0' => break,
            '{' => current = Some(MapEntity::default()),
            '}' => {
                if let Some(e) = current.take() {
                    out.push(e);
                }
            }
            '"' => {
                let key = take_quoted(text, i)?;
                let after_key = key.1;
                let vq = text[after_key..]
                    .find('"')
                    .ok_or_else(|| format!("entity lump: key {:?} has no value", key.0))?;
                let value = take_quoted(text, after_key + vq)?;
                // Advance the iterator past the value's closing quote.
                while let Some(&(j, _)) = chars.peek() {
                    if j >= value.1 {
                        break;
                    }
                    chars.next();
                }
                match current.as_mut() {
                    Some(e) => e.pairs.push((key.0, value.0)),
                    // A key/value pair outside a block is malformed; the engine
                    // ignores it and so does this, rather than failing a file
                    // the game itself loads.
                    None => continue,
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Reads a quoted string starting at the opening quote at `at`.
/// Returns the contents and the byte index just past the closing quote.
fn take_quoted(text: &str, at: usize) -> Result<(String, usize), String> {
    let rest = &text[at + 1..];
    let end = rest.find('"').ok_or_else(|| format!("entity lump: unterminated string at {at}"))?;
    Ok((rest[..end].to_string(), at + 1 + end + 1))
}

/// The entity lump of a BSP, as text (the trailing NUL is stripped).
pub fn entity_text(bytes: &[u8]) -> Result<String, String> {
    let (offset, length) = lump_span(bytes, LUMP_ENTITIES)?;
    let raw = bytes
        .get(offset..offset + length)
        .ok_or_else(|| "entity lump runs past the end of the file".to_string())?;
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    // Entity lumps are plain ASCII in practice but carry the odd high byte in
    // a mapper's name or a message string, so decode lossily rather than
    // refusing to read a map the game loads.
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

pub fn parse_entities(bytes: &[u8]) -> Result<Vec<MapEntity>, String> {
    parse_entity_text(&entity_text(bytes)?)
}

/// Renders entities back to lump text, in the format the compilers emit.
pub fn serialize_entities(entities: &[MapEntity]) -> String {
    let mut out = String::new();
    for e in entities {
        out.push_str("{\n");
        for (k, v) in &e.pairs {
            let _ = writeln!(out, "\"{k}\" \"{v}\"");
        }
        out.push_str("}\n");
    }
    out
}

/// Rebuilds a BSP around a new entity lump, leaving every other lump's bytes
/// exactly as they were.
///
/// Lumps are re-laid-out in ascending original-offset order and 4-byte aligned,
/// which is what the compilers produce and what the engine expects. Only lump 0
/// changes size, so the checksum over lumps 1..14 is untouched by construction
/// — but callers should still assert it, since "by construction" is how the
/// last three engine assumptions in this codebase were wrong.
pub fn rewrite_entity_lump(bytes: &[u8], entity_text: &str) -> Result<Vec<u8>, String> {
    if bytes.len() < HEADER_SIZE {
        return Err("file is shorter than a BSP header".to_string());
    }

    let mut spans = Vec::with_capacity(LUMP_COUNT);
    for index in 0..LUMP_COUNT {
        spans.push(lump_span(bytes, index)?);
    }

    // The new lump 0, NUL-terminated the way the engine's parser expects.
    let mut entities = entity_text.as_bytes().to_vec();
    entities.push(0);

    // Write lumps back in the order they appeared in the source file. Keeping
    // that order means a diff of the two files is confined to the entity lump
    // and the header, rather than being a wholesale reshuffle.
    let mut order: Vec<usize> = (0..LUMP_COUNT).collect();
    order.sort_by_key(|i| spans[*i].0);

    let mut out = vec![0u8; HEADER_SIZE];
    out[0..4].copy_from_slice(&(super::bsp::BSP_VERSION).to_le_bytes());

    let mut new_spans = [(0usize, 0usize); LUMP_COUNT];
    for index in order {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let offset = out.len();
        if index == LUMP_ENTITIES {
            out.extend_from_slice(&entities);
        } else {
            let (o, l) = spans[index];
            let data = bytes
                .get(o..o + l)
                .ok_or_else(|| format!("lump {index} runs past the end of the file"))?;
            out.extend_from_slice(data);
        }
        new_spans[index] = (offset, out.len() - offset);
    }

    for (index, (offset, length)) in new_spans.iter().enumerate() {
        let at = 4 + index * 8;
        out[at..at + 4].copy_from_slice(&(*offset as i32).to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&(*length as i32).to_le_bytes());
    }

    Ok(out)
}

fn lump_span(bytes: &[u8], index: usize) -> Result<(usize, usize), String> {
    let at = 4 + index * 8;
    let read = |at: usize| -> Result<i32, String> {
        bytes
            .get(at..at + 4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| format!("BSP header truncated at byte {at}"))
    };
    let offset = read(at)?;
    let length = read(at + 4)?;
    if offset < 0 || length < 0 {
        return Err(format!("lump {index} has a negative offset or length"));
    }
    Ok((offset as usize, length as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BSP with a header, an entity lump and 14 stub lumps carrying
    /// recognisable bytes, so a rewrite that drops or reorders payload shows up.
    fn synthetic_bsp(entity_text: &str) -> Vec<u8> {
        let mut entities = entity_text.as_bytes().to_vec();
        entities.push(0);

        let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(LUMP_COUNT);
        payloads.push(entities);
        for index in 1..LUMP_COUNT {
            payloads.push(vec![index as u8; index * 3]);
        }

        let mut out = vec![0u8; HEADER_SIZE];
        out[0..4].copy_from_slice(&30i32.to_le_bytes());
        let mut spans = Vec::new();
        for payload in &payloads {
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            spans.push((out.len(), payload.len()));
            out.extend_from_slice(payload);
        }
        for (index, (offset, length)) in spans.iter().enumerate() {
            let at = 4 + index * 8;
            out[at..at + 4].copy_from_slice(&(*offset as i32).to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&(*length as i32).to_le_bytes());
        }
        out
    }

    const SAMPLE: &str = "{\n\"classname\" \"worldspawn\"\n\"wad\" \"\\\\half-life\\\\dod.wad\"\n}\n\
                          {\n\"origin\" \"1 2 3\"\n\"model\" \"*7\"\n\"classname\" \"func_illusionary\"\n\"rendermode\" \"4\"\n\"renderamt\" \"255\"\n}\n\
                          {\n\"model\" \"*8\"\n\"classname\" \"func_button\"\n\"rendermode\" \"2\"\n}\n";

    #[test]
    fn parses_keys_in_order_and_finds_brush_models() {
        let ents = parse_entity_text(SAMPLE).unwrap();
        assert_eq!(ents.len(), 3);
        assert_eq!(ents[0].classname(), "worldspawn");
        assert_eq!(ents[0].brush_submodel(), None);
        assert_eq!(ents[1].brush_submodel(), Some(7));
        assert_eq!(ents[1].pairs[0].0, "origin");
        assert_eq!(ents[2].brush_submodel(), Some(8));
    }

    /// The default that decides whether an entity is worth removing: no
    /// `renderamt` key with a non-zero `rendermode` means invisible, not opaque.
    #[test]
    fn absent_renderamt_reads_as_invisible() {
        let ents = parse_entity_text(SAMPLE).unwrap();
        assert_eq!(ents[1].render(), (4, 255));
        assert_eq!(ents[2].render(), (2, 0));
        assert_eq!(ents[0].render(), (0, 0));
    }

    #[test]
    fn serialize_round_trips() {
        let ents = parse_entity_text(SAMPLE).unwrap();
        let again = parse_entity_text(&serialize_entities(&ents)).unwrap();
        assert_eq!(ents, again);
    }

    /// The whole premise of editing a shipped map: lump 0 changes, the map
    /// checksum does not, so demos recorded against the original still load.
    #[test]
    fn rewriting_entities_leaves_the_map_checksum_alone() {
        let original = synthetic_bsp(SAMPLE);
        let before = crate::patch::bsp::map_checksum(&original).unwrap();

        let mut ents = parse_entities(&original).unwrap();
        ents.retain(|e| e.classname() != "func_button");
        let trimmed = rewrite_entity_lump(&original, &serialize_entities(&ents)).unwrap();

        assert_eq!(crate::patch::bsp::map_checksum(&trimmed).unwrap(), before);
        assert_eq!(parse_entities(&trimmed).unwrap().len(), 2);
        assert!(trimmed.len() < original.len());
    }

    /// Every other lump's bytes have to survive the rebuild verbatim, or the
    /// checksum test above is passing for the wrong reason.
    #[test]
    fn rewriting_entities_preserves_every_other_lump() {
        let original = synthetic_bsp(SAMPLE);
        let trimmed = rewrite_entity_lump(&original, "{\n\"classname\" \"worldspawn\"\n}\n").unwrap();
        for index in 1..LUMP_COUNT {
            let (o1, l1) = lump_span(&original, index).unwrap();
            let (o2, l2) = lump_span(&trimmed, index).unwrap();
            assert_eq!(l1, l2, "lump {index} changed length");
            assert_eq!(&original[o1..o1 + l1], &trimmed[o2..o2 + l2], "lump {index} changed bytes");
        }
    }

    #[test]
    fn rejects_a_truncated_header() {
        assert!(rewrite_entity_lump(&[0u8; 8], "{}").is_err());
        assert!(entity_text(&[0u8; 8]).is_err());
    }
}

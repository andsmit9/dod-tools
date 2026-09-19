//! GoldSrc HLDEMO → Xash3D IDEM transcoder.
//!
//! # Why this exists
//!
//! Xash3D cannot open a DoD demo. The two engines use different demo
//! *containers*:
//!
//! | | GoldSrc (`.dem` from `hl.exe`) | Xash3D |
//! |---|---|---|
//! | magic | `"HLDEMO\0\0"` (8 bytes) | `"IDEM"` (i32) |
//! | demo protocol | 5 | 3 |
//! | path fields | `[260]` | `[64]` |
//!
//! `CL_ParseDemoHeader` bails on `hdr->id != IDEMOHEADER` before it looks at
//! anything else.
//!
//! However, Xash *does* accept one foreign value in `net_protocol`:
//! [`idem::PROTOCOL_GOLDSRC_VERSION_DEMO`] (176). With that set,
//! `CL_GetProtocolFromDemo` returns `PROTO_GOLDSRC` and the engine decodes the
//! contained network messages using GoldSrc protocol-48 rules. So the payload
//! a DoD demo already carries is understood — only the wrapper is wrong.
//!
//! This crate rewrites the wrapper.
//!
//! # What is lost
//!
//! GoldSrc has nine frame types; Xash has six commands. The mapping:
//!
//! | GoldSrc | Xash | note |
//! |---|---|---|
//! | `NetworkMessage(Start)` | `dem_usercmd` + `dem_norewind` | signon traffic |
//! | `NetworkMessage(Normal)` | `dem_usercmd` + `dem_read` | the actual gameplay stream |
//! | `DemoStart` | `dem_jumptime` | resets the section clock |
//! | `NextSection` | `dem_stop` | section terminator |
//! | `DemoBuffer` | `dem_userdata` | optional, off by default |
//! | `ConsoleCommand` | — | **dropped** — your injected director commands live here |
//! | `ClientData` | — | dropped |
//! | `Event` / `WeaponAnimation` / `Sound` | — | dropped |
//!
//! Dropping the client-side frames is why output lands around 25% of input
//! size. For a triage preview that is a feature, not a loss.
//!
//! Every `NetworkMessage` also gets a synthesized `dem_usercmd` frame ahead
//! of it: GoldSrc records the player's view angles (needed to drive the
//! camera during playback — there is no live mouse) inside each
//! `NetworkMessage`'s own header, but Xash expects them as this separate
//! frame type instead. See [`usercmd`] for the wire format.
//!
//! # No I/O
//!
//! Everything here operates on `&Demo` and returns `Vec<u8>`, so it compiles
//! to `wasm32-unknown-unknown` unchanged. The CLI in `main.rs` owns all file
//! access.

use std::collections::BTreeMap;

use dem::bit::BitSliceCast;
use dem::types::{
    AuxRefCell, ClientDataWeaponData, Delta, Demo, DirectoryEntry, EngineMessage, EntityState,
    Frame, FrameData, MessageData, NetMessage, NetworkMessage, NetworkMessageType,
    SvcClientData, SvcPacketEntities, SvcTime,
};

pub mod idem;
pub mod resources;
pub mod usercmd;
pub mod writer;

use writer::ByteWriter;

/// How a source frame was handled.
pub type Stats = BTreeMap<String, usize>;

#[derive(Debug, Clone)]
pub struct Options {
    /// Written to the IDEM header. Xash clamps this; 0.0 plays back in slow motion.
    pub host_fps: f64,
    /// Forward `DemoBuffer` frames as `dem_userdata`. These are handed to
    /// `pfnDemo_ReadBuffer` on the client library — with a non-DoD client that
    /// is at best ignored, so the default drops them and saves a lot of bytes.
    pub keep_userdata: bool,
    /// Rebase each section's frame times so the first frame sits at t=0.
    pub rebase_time: bool,
    /// Overrides the gamedir field. `None` keeps whatever the source demo had.
    pub gamedir: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            host_fps: idem::DEFAULT_HOST_FPS,
            keep_userdata: false,
            rebase_time: true,
            gamedir: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EntrySummary {
    pub entrytype: i32,
    pub description: String,
    pub frames: i32,
    pub offset: i32,
    pub length: i32,
    pub playback_time: f32,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub bytes: Vec<u8>,
    pub entries: Vec<EntrySummary>,
    pub stats: Stats,
}

#[inline]
fn bump(stats: &mut Stats, key: &str) {
    *stats.entry(key.to_string()).or_insert(0) += 1;
}

/// Resolve a frame's network payload to raw bytes.
///
/// Parse the demo with [`dem::types::MessageDataParseMode::Raw`] and this is a
/// borrow. `Parse` mode works too but costs a re-serialisation round trip
/// through `write_netmsg`, which is both slower and a fidelity risk — prefer
/// `Raw` unless you are also mutating messages.
fn raw_message(md: &MessageData) -> Option<&[u8]> {
    match md {
        MessageData::Raw(bytes) => Some(bytes.as_slice()),
        MessageData::Parsed(_) => None,
        MessageData::None => None,
    }
}

/// Transcode a parsed HLDEMO into an IDEM container.
pub fn transcode(demo: &Demo, opts: &Options) -> eyre::Result<Output> {
    transcode_entries(demo, &demo.directory.entries, opts)
}

fn transcode_entries(
    demo: &Demo,
    entries: &[DirectoryEntry],
    opts: &Options,
) -> eyre::Result<Output> {
    let mut stats = Stats::new();
    let mut w = ByteWriter::with_capacity(8 << 20);

    // ---- header ----------------------------------------------------------
    w.i32(idem::IDEMOHEADER);
    w.i32(idem::DEMO_PROTOCOL);
    w.i32(idem::PROTOCOL_GOLDSRC_VERSION_DEMO);
    w.f64(opts.host_fps);
    w.fixed_str(demo.header.map_name.as_slice(), idem::FIELD_WIDTH);
    w.fixed_str(b"transcoded by dod-tools", idem::FIELD_WIDTH);
    match &opts.gamedir {
        Some(g) => w.fixed_str(g.as_bytes(), idem::FIELD_WIDTH),
        None => w.fixed_str(demo.header.game_directory.as_slice(), idem::FIELD_WIDTH),
    }
    let dir_off_slot = w.offset();
    w.i32(0); // backpatched below

    debug_assert_eq!(w.offset(), idem::HEADER_SIZE);

    // ---- sections --------------------------------------------------------
    let mut summaries: Vec<EntrySummary> = Vec::with_capacity(entries.len());

    for entry in entries {
        let start = w.offset();
        let mut nframes: i32 = 0;

        let base = if opts.rebase_time {
            entry.frames.first().map(|f| f.time).unwrap_or(0.0)
        } else {
            0.0
        };

        for frame in &entry.frames {
            let dt = frame.time - base;

            match &frame.frame_data {
                FrameData::NetworkMessage(boxed) => {
                    let (kind, msg) = boxed.as_ref();
                    let cmd = match kind {
                        NetworkMessageType::Start => idem::DEM_NOREWIND,
                        NetworkMessageType::Normal => idem::DEM_READ,
                        // Types >= 10 are treated as network messages by the
                        // GoldSrc reader; follow suit rather than dropping data.
                        NetworkMessageType::Unknown(_) => idem::DEM_READ,
                    };

                    let payload = raw_message(&msg.messages).ok_or_else(|| {
                        eyre::eyre!(
                            "frame {} has no raw message bytes — reparse the demo with \
                             MessageDataParseMode::Raw",
                            frame.frame
                        )
                    })?;

                    if payload.len() as u32 > idem::MAX_INIT_MSG {
                        bump(&mut stats, "SKIPPED oversize netmsg");
                        continue;
                    }

                    // dem_usercmd ahead of the network message: carries the
                    // recorded view angles GoldSrc bundles inside this same
                    // frame's DemoInfo header. See `usercmd` module docs.
                    let encoded_cmd = usercmd::encode_usercmd(&msg.info.usercmd);
                    w.u8(idem::DEM_USERCMD);
                    w.f32(dt);
                    w.i32(msg.sequence_info.outgoing_sequence);
                    w.i32(msg.sequence_info.outgoing_sequence);
                    w.u16(encoded_cmd.len() as u16);
                    w.bytes(&encoded_cmd);
                    nframes += 1;
                    bump(&mut stats, "NetworkMessage -> dem_usercmd");

                    w.u8(cmd);
                    w.f32(dt);

                    // CL_ReadDemoSequence expects exactly these seven, in this
                    // order. GoldSrc's SequenceInfo is field-for-field identical.
                    let s = &msg.sequence_info;
                    w.i32(s.incoming_sequence);
                    w.i32(s.incoming_acknowledged);
                    w.i32(s.incoming_reliable_acknowledged);
                    w.i32(s.incoming_reliable_sequence);
                    w.i32(s.outgoing_sequence);
                    w.i32(s.reliable_sequence);
                    w.i32(s.last_reliable_sequence);

                    w.i32(payload.len() as i32);
                    w.bytes(payload);

                    bump(
                        &mut stats,
                        if cmd == idem::DEM_READ {
                            "NetworkMessage -> dem_read"
                        } else {
                            "NetworkMessage -> dem_norewind"
                        },
                    );
                }

                FrameData::DemoStart => {
                    w.u8(idem::DEM_JUMPTIME);
                    w.f32(dt);
                    bump(&mut stats, "DemoStart -> dem_jumptime");
                }

                FrameData::NextSection => {
                    w.u8(idem::DEM_STOP);
                    w.f32(dt);
                    bump(&mut stats, "NextSection -> dem_stop");
                }

                FrameData::DemoBuffer(b) if opts.keep_userdata => {
                    w.u8(idem::DEM_USERDATA);
                    w.f32(dt);
                    w.i32(b.buffer.len() as i32);
                    w.bytes(&b.buffer);
                    bump(&mut stats, "DemoBuffer -> dem_userdata");
                }

                other => {
                    bump(&mut stats, &format!("dropped {}", frame_kind(other)));
                    continue;
                }
            }

            nframes += 1;
        }

        let length = (w.offset() - start) as i32;
        summaries.push(EntrySummary {
            entrytype: if entry.type_ == 0 {
                idem::DEMO_STARTUP
            } else {
                idem::DEMO_NORMAL
            },
            description: String::from_utf8_lossy(
                entry
                    .description
                    .as_slice()
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or(&[]),
            )
            .into_owned(),
            frames: nframes,
            offset: start as i32,
            length,
            playback_time: entry.track_time,
        });
    }

    // ---- directory -------------------------------------------------------
    let dir_off = w.offset();
    w.i32(summaries.len() as i32);
    for s in &summaries {
        w.i32(s.entrytype);
        w.f32(s.playback_time);
        w.i32(s.frames);
        w.i32(s.offset);
        w.i32(s.length);
        w.i32(0); // flags
        w.fixed_str(s.description.as_bytes(), idem::FIELD_WIDTH);
    }
    w.patch_i32(dir_off_slot, dir_off as i32);

    Ok(Output {
        bytes: w.into_vec(),
        entries: summaries,
        stats,
    })
}

fn frame_kind(f: &FrameData) -> &'static str {
    match f {
        FrameData::NetworkMessage(_) => "NetworkMessage",
        FrameData::DemoStart => "DemoStart",
        FrameData::ConsoleCommand(_) => "ConsoleCommand",
        FrameData::ClientData(_) => "ClientData",
        FrameData::NextSection => "NextSection",
        FrameData::Event(_) => "Event",
        FrameData::WeaponAnimation(_) => "WeaponAnimation",
        FrameData::Sound(_) => "Sound",
        FrameData::DemoBuffer(_) => "DemoBuffer",
    }
}

/// Transcode only `[start - preroll, end]` of the playback section, with a
/// synthesized entity baseline spliced in so the cut doesn't land mid-chain.
///
/// The signon section is always kept whole — it carries the map name, resource
/// list and entity baselines, without which nothing loads.
///
/// # Delta-compression caveat
///
/// `svc_deltapacketentities` deltas against the client's *cumulative* running
/// entity state, not a periodically-resent snapshot: GoldSrc only sends a
/// full (non-delta) `svc_packetentities` once, in the first few frames after
/// connecting — confirmed empirically against `analysis_target_pov.dem`
/// (986 s, 90,464 delta messages, exactly 4 full ones, all inside the first
/// 0.07 s). So "walk forward to the next full update" (this function's
/// earlier approach) is a dead end for a real mid-match highlight — there
/// usually isn't one. Left uncorrected, Xash hits
/// `CL_ParseDeltaPacketEntitiesGS: (N should be M)` at signon and the
/// replayed world never advances (confirmed in a real browser, 2026-08-14).
///
/// The fix: reconstruct the state a real client would have accumulated by
/// `start - preroll`, and inject it as a synthetic full `svc_packetentities`
/// frame immediately before the retained window, so playback picks up from
/// a self-contained snapshot instead of an orphaned delta chain.
/// `parsed` — a second parse of the same source in
/// [`dem::types::MessageDataParseMode::Parse`] (`demo` stays `Raw`, for the
/// byte-exact write path elsewhere) — supplies the decoded per-entity field
/// deltas ([`replay_entities_before`]) that get folded into one map per
/// entity and re-encoded ([`encode_full_baseline`]) using the crate's own
/// `SvcPacketEntities` writer, so none of GoldSrc's bit-level delta format is
/// reimplemented here.
///
/// # Panics / preconditions
///
/// `demo` and `parsed` must be two parses of the same bytes — this walks
/// them in lockstep by directory-entry index and assumes matching frame
/// timestamps. Passing unrelated demos silently produces a baseline that
/// doesn't match the retained window's frames, not a panic — directory-entry
/// counts alone can't prove a mismatch.
pub fn cut(
    demo: &Demo,
    parsed: &Demo,
    start: f32,
    end: f32,
    preroll: f32,
    opts: &Options,
) -> eyre::Result<Output> {
    let lo = (start - preroll).max(0.0);

    let mut kept: Vec<DirectoryEntry> = Vec::with_capacity(demo.directory.entries.len());

    for (i, entry) in demo.directory.entries.iter().enumerate() {
        if entry.type_ == 0 {
            kept.push(entry.clone());
            continue;
        }

        let mut e = entry.clone();
        e.frames = entry
            .frames
            .iter()
            .filter(|f| {
                matches!(
                    f.frame_data,
                    FrameData::DemoStart | FrameData::NextSection
                ) || (f.time >= lo && f.time <= end)
            })
            .cloned()
            .collect();

        // Rebase the retained window onto the section start. The DemoStart
        // frame kept above becomes a `dem_jumptime` at t=0 that resets
        // Xash's playback clock, and `CL_DemoReadMessage` (cl_demo.c) then
        // gates every later frame on `timestamp - serverframetime >=
        // elapsed` -- so retained frames left at their original t>=lo stamps
        // make the engine show the first one (never skipped) and then wait
        // `lo` real seconds for the clock to catch up: a 300s cut freezes for
        // five minutes. `transcode_entries`' own rebasing can't fix this,
        // since it subtracts the entry's *first* frame time, which is that
        // DemoStart at 0. Section-start cuts (lo == 0) are unaffected, which
        // is why they always played fine.
        for f in e.frames.iter_mut() {
            if matches!(f.frame_data, FrameData::NextSection) {
                f.time = (end - lo).max(0.0);
            } else if f.time >= lo {
                f.time -= lo;
            }
        }

        // Splice synthesized full baselines in front of the retained window,
        // built from every entity + local-player field ever set before `lo`.
        // Usually one frame suffices for the whole reconstruction (entities
        // are cumulative, so a single from-null baseline covers them). But
        // `svc_clientdata` deltas against "the last frame this client
        // acknowledged" — a few frames behind due to round-trip latency, a
        // *sliding* reference, not a fixed one — so several leading
        // post-cut clientdata messages each reference a *different* pre-cut
        // frame in turn. See `delta_reference_chain`'s doc comment.
        if let (Some(pe), Some(aux)) = (parsed.directory.entries.get(i), parsed._aux.clone()) {
            let state = replay_state_before(pe, lo);
            if !state.entities.is_empty() || !state.client_data.is_empty() {
                let server_time = first_server_time_at_or_after(pe, lo);
                let payload = encode_synthetic_payload(&state, server_time, aux);
                if (payload.len() as u32) <= idem::MAX_INIT_MSG {
                    if let Some(anchor) = e
                        .frames
                        .iter()
                        .find(|f| matches!(f.frame_data, FrameData::NetworkMessage(_)))
                        .cloned()
                    {
                        let FrameData::NetworkMessage(anchor_boxed) = &anchor.frame_data else {
                            unreachable!("filtered to NetworkMessage above");
                        };
                        let anchor_seq = anchor_boxed.1.sequence_info.incoming_sequence;
                        let chain = delta_reference_chain(pe, lo, anchor_seq);

                        // Insert immediately before the anchor (the first
                        // retained NetworkMessage) -- not "first frame at or
                        // after lo", which no longer means anything now that
                        // the window has been rebased onto 0 above.
                        let insert_at = e
                            .frames
                            .iter()
                            .position(|f| matches!(f.frame_data, FrameData::NetworkMessage(_)))
                            .unwrap_or(0);
                        if chain.is_empty() {
                            let synthetic = synthetic_baseline_frame(&anchor, payload, None, 1);
                            e.frames.insert(insert_at, synthetic);
                        } else {
                            // Burst position, counting down: the frame furthest
                            // in the past (offset 0) gets the largest backdate,
                            // the frame right before the anchor gets 1. See
                            // `synthetic_baseline_frame`'s doc comment for why
                            // this can't just clone the anchor's own sequence
                            // numbers for every frame in the chain.
                            let chain_len = chain.len();
                            for (offset, seq) in chain.iter().enumerate() {
                                let backdate = (chain_len - offset) as i32;
                                let synthetic = synthetic_baseline_frame(
                                    &anchor,
                                    payload.clone(),
                                    Some(*seq),
                                    backdate,
                                );
                                e.frames.insert(insert_at + offset, synthetic);
                            }
                        }
                    }
                }
                // Oversize or no anchor frame: falls through with the plain
                // cut, reproducing the original corrupt-entity symptom for
                // this one section rather than failing the whole transcode.
            }
        }

        // Guarantee a terminator; Xash treats a section without dem_stop as corrupt.
        if !e
            .frames
            .iter()
            .any(|f| matches!(f.frame_data, FrameData::NextSection))
        {
            if let Some(last) = entry.frames.last() {
                e.frames.push(last.clone());
            }
        }

        e.track_time = (end - lo).max(0.0);
        kept.push(e);
    }

    transcode_entries(demo, &kept, opts)
}

/// Per-entity accumulated state: `has_custom_delta` flag plus every field
/// ever explicitly set for that entity, keyed by field name exactly as the
/// crate's own delta parser names them (never constructed by hand here, so
/// there's no risk of drifting from its naming convention).
type EntityTable = BTreeMap<u16, (bool, Delta)>;

/// Everything replayed from `[0, before)` needed to synthesize a
/// self-contained resume point: world entities (`svc_packetentities`) *and*
/// the local player's own state (`svc_clientdata`), which GoldSrc encodes as
/// a second, independent delta chain — same cumulative-against-your-own-
/// running-state semantics, same "no periodic full resync" gap, and the
/// actual cause of a real symptom: a mid-stream cut played back with only
/// the entity fix applied showed a slowly-rotating free-floating camera
/// above the player's body — classic GoldSrc observer/dead-camera fallback,
/// caused by `clientdata_t`'s `origin`/`health`/`deadflag`/`iuser1` fields
/// resolving from Xash's zeroed default instead of the real accumulated
/// values (confirmed against real DoD 1.3 `delta.lst`: `clientdata_t`
/// carries exactly these fields). Confirmed fixed 2026-08-14.
#[derive(Default)]
struct ReplayedState {
    entities: EntityTable,
    client_data: Delta,
    /// Keyed by weapon slot index (0..64, `clientdata_t`'s `weapon_data_t`
    /// delta is per-slot).
    weapon_data: BTreeMap<u8, Delta>,
}

/// Replay every `NetworkMessage` frame in `entry` with `time < before`,
/// folding entity and local-player fields into running tables.
///
/// Field *values* fold with "later wins" — every field GoldSrc sends is the
/// absolute current value, so a single running table per entity is exact.
/// Entity *presence* does not fold that simply, and getting it wrong put
/// player models in the wrong places on a real mid-stream cut (a stationary
/// player being shot at had no model at all; see below). A delta packet is
/// relative to the frame its `delta_sequence` byte names — the last one the
/// client acknowledged, a few packets back, a *sliding* reference (same
/// mechanic `delta_reference_chain` deals with) — and its `remove` bit
/// means "in that referenced frame, not in this one". Entities in the
/// referenced frame that the packet doesn't mention are copied forward
/// unchanged by the client (`CL_ParseDeltaPacketEntitiesGS`, "any remaining
/// entities in the old frame are copied over"). So an entity that drops out
/// of the PVS for a frame or two and comes back *unchanged* — routine at PVS
/// edges — is removed once and then never re-sent: the client just copies it
/// from the older reference frame. Treating `remove` as "delete from the
/// running state" (the previous version of this function) lost every such
/// entity for good. A moving player heals within a frame (its origin gets
/// re-sent); a camping or prone one stays missing from the synthesized
/// baseline, and so from every frame after it.
///
/// This keeps the per-frame presence set for the last `CL_UPDATE_BACKUP`
/// frames keyed by absolute `incoming_sequence`, resolves each delta's
/// reference exactly the way the client does (`(incoming_sequence -
/// delta_sequence) & 0xFF`, `cl_frame.c` `CL_ValidateDeltaPacket`), and
/// builds each new frame's set as `referenced − removed + listed`. The
/// state at the cut is the running values restricted to the *last* frame's
/// set. `svc_clientdata` has no full/delta distinction at all — every
/// occurrence merges the same way.
fn replay_state_before(entry: &DirectoryEntry, before: f32) -> ReplayedState {
    use std::collections::BTreeSet;

    let mut state = ReplayedState::default();
    let mut values: EntityTable = EntityTable::new();
    let mut presence: BTreeMap<i32, BTreeSet<u16>> = BTreeMap::new();
    let mut last_seq: Option<i32> = None;

    for f in &entry.frames {
        if f.time >= before {
            continue;
        }
        let FrameData::NetworkMessage(boxed) = &f.frame_data else {
            continue;
        };
        let msg_seq = boxed.1.sequence_info.incoming_sequence;
        let MessageData::Parsed(messages) = &boxed.as_ref().1.messages else {
            continue;
        };

        let mut frame_set: Option<BTreeSet<u16>> = None;

        for m in messages {
            let NetMessage::EngineMessage(engine) = m else {
                continue;
            };
            match engine.as_ref() {
                EngineMessage::SvcPacketEntities(full) => {
                    let mut set = BTreeSet::new();
                    for ent in &full.entity_states {
                        merge_entity(&mut values, ent.entity_index, ent.has_custom_delta, &ent.delta);
                        set.insert(ent.entity_index);
                    }
                    frame_set = Some(set);
                }
                EngineMessage::SvcDeltaPacketEntities(delta_msg) => {
                    let byte = delta_msg.delta_sequence.to_u8() as i32;
                    let lag = ((msg_seq & 0xFF) - byte).rem_euclid(0x100);
                    let referenced = msg_seq - lag;
                    // Unknown reference (e.g. a signon-time frame that lives in
                    // another directory entry): best effort from the newest
                    // frame we do have, which is what the previous fold
                    // effectively assumed everywhere.
                    let mut set = presence
                        .get(&referenced)
                        .or_else(|| last_seq.and_then(|s| presence.get(&s)))
                        .cloned()
                        .unwrap_or_default();
                    for ent in &delta_msg.entity_states {
                        if ent.remove_entity {
                            set.remove(&ent.entity_index);
                            continue;
                        }
                        let hcd = ent.has_custom_delta.unwrap_or(false);
                        if let Some(delta) = &ent.delta {
                            merge_entity(&mut values, ent.entity_index, hcd, delta);
                        } else {
                            values.entry(ent.entity_index).or_default();
                        }
                        set.insert(ent.entity_index);
                    }
                    frame_set = Some(set);
                }
                EngineMessage::SvcClientData(cd) => {
                    for (k, v) in &cd.client_data {
                        state.client_data.insert(k.clone(), v.clone());
                    }
                    if let Some(weapons) = &cd.weapon_data {
                        for w in weapons {
                            let fields = state.weapon_data.entry(w.weapon_index.to_u8()).or_default();
                            for (k, v) in &w.weapon_data {
                                fields.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(set) = frame_set {
            presence.insert(msg_seq, set);
            last_seq = Some(msg_seq);
            // The client's own window is CL_UPDATE_BACKUP frames; keep a bit
            // more so an out-of-order reference still resolves.
            while presence.len() > 4 * CL_UPDATE_BACKUP as usize {
                let oldest = *presence.keys().next().unwrap();
                presence.remove(&oldest);
            }
        }
    }

    if let Some(set) = last_seq.and_then(|s| presence.get(&s)) {
        state.entities = values
            .into_iter()
            .filter(|(idx, _)| set.contains(idx))
            .collect();
    } else {
        state.entities = values;
    }

    state
}

/// GoldSrc's client-data ring buffer size for a multiplayer session
/// (`MULTIPLAYER_BACKUP`, `netchan.h`) — `CL_UPDATE_BACKUP` is set to this at
/// connect time whenever `maxclients > 1` (`cl_game.c:999`), true for any
/// real match recording. `CL_UPDATE_MASK` is this minus one.
const CL_UPDATE_BACKUP: i32 = 64;

/// Walk the real post-cut `svc_clientdata` stream and determine every
/// distinct *pre-cut* ring-buffer slot its *leading* messages need populated
/// before their own delta references resolve to frames that are actually
/// part of the retained window.
///
/// `svc_clientdata` is not cumulative like entities — `CL_ParseClientData`
/// (`cl_parse.c:1102-1119`) looks up one specific prior frame by an explicit
/// sequence-number byte (`SvcClientData.delta_update_mask`, despite the
/// name, is exactly that byte — `cl.frames[delta_sequence & CL_UPDATE_MASK]`).
/// That byte only carries the *low bits* of the referenced frame's absolute
/// sequence number, so which actual frame it names depends on the message's
/// *own* sequence number too (the referenced frame is always within the last
/// `CL_UPDATE_BACKUP` frames of it). Critically, GoldSrc deltas clientdata
/// against "the last frame this client has acknowledged" — a few frames
/// behind due to round-trip latency, a *sliding* reference — not a fixed
/// point. Confirmed empirically against a real recording (`analysis_target_
/// pov.dem`, 300s cut): the first several post-cut messages each reference a
/// *different* pre-cut frame (e.g. bytes 5, 6, 7, 8, 8, ...) before the
/// window ages past the cut boundary and later messages start referencing
/// real retained frames that populate themselves normally during playback.
/// A single synthetic frame (the original, incomplete version of this fix)
/// only satisfied the very first of these.
///
/// `svc_deltapacketentities` has exactly the same problem, with its own
/// `delta_sequence` byte, chained independently (`CL_ParseDeltaPacketEntitiesGS`
/// deltas against `cl.frames[delta_sequence & CL_UPDATE_MASK].entities`).
/// Chaining only clientdata left the leading entity deltas referencing
/// whatever *else* happened to be in those ring slots -- in practice the
/// kept LOADING section's own connect-time frames (sequences 0..~30, a
/// handful of entities from the original recording's t=0), so the first few
/// post-cut updates applied against a 7-entity frame instead of the ~30
/// entity reconstruction and the engine warned `(7 should be 30)` until the
/// window aged past the cut. One synthetic-frame burst covering the *union*
/// of both chains fixes both, since every synthetic frame carries the full
/// entity + clientdata payload anyway.
///
/// Returns the distinct bytes needed, in first-seen order, stopping once
/// *both* kinds of reference have resolved to a frame at/after `anchor_seq`
/// (i.e. the chain has caught up to the retained stream and everything from
/// here self-heals).
fn delta_reference_chain(entry: &DirectoryEntry, at_or_after: f32, anchor_seq: i32) -> Vec<i32> {
    let mut needed: Vec<i32> = Vec::new();
    let mut cd_done = false;
    let mut pe_done = false;

    // Returns true once this reference kind has caught up to the retained
    // window; pushes the byte onto `needed` otherwise.
    let consider = |needed: &mut Vec<i32>, msg_seq: i32, byte: u8| -> bool {
        let lag = ((msg_seq & 0xFF) - byte as i32).rem_euclid(0x100);
        let referenced = msg_seq - lag;
        if referenced >= anchor_seq {
            return true;
        }
        if !needed.contains(&referenced) {
            needed.push(referenced);
        }
        false
    };

    for f in &entry.frames {
        if f.time < at_or_after {
            continue;
        }
        let FrameData::NetworkMessage(boxed) = &f.frame_data else {
            continue;
        };
        let msg_seq = boxed.1.sequence_info.incoming_sequence;
        let MessageData::Parsed(messages) = &boxed.as_ref().1.messages else {
            continue;
        };

        for m in messages {
            let NetMessage::EngineMessage(engine) = m else {
                continue;
            };
            match engine.as_ref() {
                EngineMessage::SvcClientData(cd) if !cd_done => {
                    let byte = if cd.has_delta_update_mask {
                        cd.delta_update_mask.as_ref().map(|mask| mask.to_u8())
                    } else {
                        None
                    };
                    match byte {
                        // From-null full update -- always self-sufficient.
                        None => cd_done = true,
                        Some(b) => cd_done = consider(&mut needed, msg_seq, b),
                    }
                }
                EngineMessage::SvcDeltaPacketEntities(pe) if !pe_done => {
                    pe_done = consider(&mut needed, msg_seq, pe.delta_sequence.to_u8());
                }
                // A full entity update is self-sufficient.
                EngineMessage::SvcPacketEntities(_) if !pe_done => pe_done = true,
                _ => {}
            }
        }
        if cd_done && pe_done {
            break;
        }
    }

    // Ascending, so the burst's sequence numbers are monotonic like a real one.
    needed.sort_unstable();
    needed
}

/// The `svc_time` carried by the first retained frame (the anchor) -- the
/// server clock the synthetic frames spliced in ahead of it should claim.
fn first_server_time_at_or_after(entry: &DirectoryEntry, at_or_after: f32) -> Option<f32> {
    for f in &entry.frames {
        if f.time < at_or_after {
            continue;
        }
        let FrameData::NetworkMessage(boxed) = &f.frame_data else {
            continue;
        };
        let MessageData::Parsed(messages) = &boxed.as_ref().1.messages else {
            continue;
        };
        for m in messages {
            if let NetMessage::EngineMessage(engine) = m {
                if let EngineMessage::SvcTime(t) = engine.as_ref() {
                    return Some(t.time);
                }
            }
        }
        // First retained frame had no svc_time -- don't go hunting further
        // into the stream for one that belongs to a later frame.
        return None;
    }
    None
}

fn merge_entity(table: &mut EntityTable, index: u16, has_custom_delta: bool, delta: &Delta) {
    let (hcd, fields) = table.entry(index).or_default();
    *hcd = has_custom_delta;
    for (k, v) in delta {
        fields.insert(k.clone(), v.clone());
    }
}

/// Encode a [`ReplayedState`] as a full `svc_clientdata` payload followed by
/// a full `svc_packetentities` payload — two independent messages
/// concatenated, exactly like a real frame carrying several `svc_` messages
/// back to back. Entities use absolute (not incremental) indices throughout
/// for simplicity — slightly bigger on the wire, never wrong. `aux` supplies
/// the delta decoder tables the source demo's own `svc_deltadescription`
/// messages established; `Demo::_aux` (despite its "do not use this" doc
/// comment — there is no other way to get a decoder table matching this
/// specific demo) still holds them after a full parse, since they're set
/// once near signon and read-only from then on.
///
/// **Order is load-bearing: clientdata first.** The engine decides which
/// ring-buffer slot the *current* frame's entities go into only when it
/// parses `svc_clientdata` (`cl_parse.c` `CL_ParseClientData`:
/// `cl.parsecountmod = cls.netchan.incoming_sequence & CL_UPDATE_MASK`), and
/// `CL_ParsePacketEntitiesGS` writes into `cl.frames[cl.parsecountmod]`
/// whatever that currently is. Entities-then-clientdata therefore put the
/// reconstructed entity set into the *previous* frame's slot, then moved the
/// pointer to this frame's slot -- which still held whatever the kept
/// LOADING section had left there (a 7-entity connect-time frame, in the
/// case that exposed this: `CL_ParseDeltaPacketEntitiesGS: (7 should be
/// 30)` on every leading post-cut delta). Real GoldSrc packets are ordered
/// `svc_time, svc_clientdata, svc_packetentities` for exactly this reason.
fn encode_synthetic_payload(
    state: &ReplayedState,
    server_time: Option<f32>,
    aux: AuxRefCell,
) -> Vec<u8> {
    let mut out = Vec::new();

    // svc_time first, like every real packet. Not decoration: the engine
    // decodes GoldSrc "timewindow" delta fields -- `animtime` and friends --
    // relative to the packet's time (`CL_DeltaEntityGS` passes `cl.mtime[0]`
    // into `Delta_ReadGSFields`), and stamps every entity's `msg_time` with
    // it for position interpolation. Without one, the synthetic frames
    // inherit whatever the kept LOADING section last set, hundreds of
    // seconds stale, and every entity's animtime comes out garbage until its
    // animation next changes. Seen on a real cut as models playing broken
    // animations.
    if let Some(t) = server_time {
        let time_msg = NetMessage::EngineMessage(Box::new(EngineMessage::SvcTime(SvcTime { time: t })));
        out.extend(time_msg.write(aux.clone()));
    }

    if !state.client_data.is_empty() {
        let weapon_data = if state.weapon_data.is_empty() {
            None
        } else {
            Some(
                state
                    .weapon_data
                    .iter()
                    .map(|(&index, delta)| ClientDataWeaponData {
                        weapon_index: dem::nbit_num!(index as u32, 6),
                        weapon_data: delta.clone(),
                    })
                    .collect(),
            )
        };

        let cd_msg = NetMessage::EngineMessage(Box::new(EngineMessage::SvcClientData(
            SvcClientData {
                has_delta_update_mask: false,
                delta_update_mask: None,
                client_data: state.client_data.clone(),
                weapon_data,
            },
        )));

        out.extend(cd_msg.write(aux.clone()));
    }

    let entity_states: Vec<EntityState> = state
        .entities
        .iter()
        .map(|(&entity_index, (has_custom_delta, delta))| EntityState {
            entity_index,
            increment_entity_number: false,
            is_absolute_entity_index: Some(true),
            absolute_entity_index: Some(dem::nbit_num!(entity_index as u32, 11)),
            entity_index_difference: None,
            has_custom_delta: *has_custom_delta,
            has_baseline_index: false,
            baseline_index: None,
            delta: delta.clone(),
        })
        .collect();

    let entities_msg = NetMessage::EngineMessage(Box::new(EngineMessage::SvcPacketEntities(
        SvcPacketEntities {
            entity_count: dem::nbit_num!(entity_states.len() as u32, 16),
            entity_states,
        },
    )));

    out.extend(entities_msg.write(aux));

    out
}

/// Build a synthetic `NetworkMessage` frame carrying `payload`, reusing
/// `anchor`'s `DemoInfo`/`SequenceInfo` (view angles, netchan sequence
/// numbers) since the synthesized baseline logically belongs to the same
/// instant as the first real retained frame.
///
/// `client_data_sequence`, when `Some`, overrides `sequence_info`'s
/// `incoming_sequence` with the `delta_sequence` the real stream's next
/// `svc_clientdata` message expects (see [`expected_client_data_sequence`]).
/// The client looks that value up as `cl.frames[delta_sequence &
/// CL_UPDATE_MASK]` — an exact numeric match on `incoming_sequence`, not
/// the anchor's own sequence number, is what makes that lookup land on this
/// synthetic frame's clientdata instead of an empty/stale ring-buffer slot.
///
/// `sequence_backdate` moves `outgoing_sequence` and `incoming_acknowledged`
/// *back* by that many units from the anchor's own values (preserving the
/// anchor's real round-trip gap between them). This is load-bearing when more
/// than one synthetic frame is spliced in (see `delta_reference_chain`): cloning
/// the anchor's `SequenceInfo` verbatim for every frame in the burst gives
/// them all the *same* `outgoing_sequence`. The transcoder writes that value
/// into each frame's paired `dem_usercmd` as both sequence and cmdnumber
/// (`xash3d-fwgs/engine/client/cl_demo.c`'s `CL_ReadDemoUserCmd`), which the
/// client uses to index `cl.commands[(outgoing_sequence + 1) & CL_UPDATE_
/// MASK]` for movement prediction (`cl_pmove.c`'s `CL_PredictMovement` walks
/// forward while `incoming_acknowledged + i < outgoing_sequence + stoppoint`)
/// — several identical `outgoing_sequence` values in a row collide into one
/// ring-buffer slot and freeze that walk's target for the whole burst, which
/// reads as a floating/rotating camera until the real stream's genuinely
/// advancing sequence numbers resume. A real few-packet round trip always has
/// strictly increasing sequence numbers frame to frame; backdating each
/// synthetic frame by its distance from the anchor reproduces that shape
/// instead of repeating one snapshot. Pass `1` for a lone from-null frame (no
/// burst) so it lands immediately behind the anchor rather than on top of it.
fn synthetic_baseline_frame(
    anchor: &Frame,
    payload: Vec<u8>,
    client_data_sequence: Option<i32>,
    sequence_backdate: i32,
) -> Frame {
    let FrameData::NetworkMessage(boxed) = &anchor.frame_data else {
        unreachable!("caller only passes NetworkMessage frames as anchor");
    };
    let (_, anchor_msg) = boxed.as_ref();

    let mut sequence_info = anchor_msg.sequence_info.clone();
    sequence_info.outgoing_sequence -= sequence_backdate;
    sequence_info.incoming_acknowledged -= sequence_backdate;
    if let Some(seq) = client_data_sequence {
        sequence_info.incoming_sequence = seq;
    }

    Frame {
        time: anchor.time,
        frame: anchor.frame,
        frame_data: FrameData::NetworkMessage(Box::new((
            NetworkMessageType::Normal,
            NetworkMessage {
                info: anchor_msg.info.clone(),
                sequence_info,
                message_length: payload.len() as u32,
                messages: MessageData::Raw(payload),
            },
        ))),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Re-check produced bytes the way `CL_ParseDemoHeader`, `CL_PlayDemo_f` and
/// `CL_DemoReadMessage` would.
///
/// Passing here means the file is structurally acceptable to Xash. It does not
/// prove the *contents* replay correctly — that still needs a real engine.
pub fn validate(data: &[u8]) -> Vec<String> {
    let mut errs = Vec::new();

    macro_rules! rd_i32 {
        ($at:expr) => {{
            let a = $at;
            if a + 4 > data.len() {
                errs.push(format!("truncated read at {}", a));
                return errs;
            }
            i32::from_le_bytes([data[a], data[a + 1], data[a + 2], data[a + 3]])
        }};
    }

    if data.len() < idem::HEADER_SIZE {
        errs.push("shorter than IDEM header".into());
        return errs;
    }

    if rd_i32!(0) != idem::IDEMOHEADER {
        errs.push("id != IDEMOHEADER".into());
    }
    if rd_i32!(4) != idem::DEMO_PROTOCOL {
        errs.push(format!("dem_protocol {} != 3", rd_i32!(4)));
    }
    let np = rd_i32!(8);
    if np != idem::PROTOCOL_GOLDSRC_VERSION_DEMO {
        errs.push(format!("net_protocol {} would be rejected", np));
    }

    let dir_off = rd_i32!(212);
    if dir_off <= 0 || dir_off as usize >= data.len() {
        errs.push(format!("directory_offset {} out of range", dir_off));
        return errs;
    }

    let n = rd_i32!(dir_off as usize);
    if !(1..=1024).contains(&n) {
        errs.push(format!("bogus numentries {}", n));
        return errs;
    }

    for i in 0..n as usize {
        let p = dir_off as usize + 4 + i * idem::ENTRY_SIZE;
        if p + idem::ENTRY_SIZE > data.len() {
            errs.push(format!("entry {} truncated", i));
            return errs;
        }
        let off = rd_i32!(p + 12) as usize;
        let len = rd_i32!(p + 16) as usize;
        if off + len > data.len() {
            errs.push(format!("entry {} extends past EOF", i));
            continue;
        }

        let (mut q, end) = (off, off + len);
        let mut saw_stop = false;
        while q < end {
            let cmd = data[q];
            q += 1;
            if cmd > idem::DEM_STOP {
                errs.push(format!("entry {}: cmd {} > dem_stop", i, cmd));
                break;
            }
            q += 4; // dt
            match cmd {
                idem::DEM_NOREWIND | idem::DEM_READ => {
                    q += 28;
                    let mlen = rd_i32!(q);
                    q += 4;
                    if mlen < 0 || mlen as u32 > idem::MAX_INIT_MSG {
                        errs.push(format!("entry {}: msglen {} rejected", i, mlen));
                        break;
                    }
                    q += mlen as usize;
                }
                idem::DEM_USERDATA => {
                    let sz = rd_i32!(q);
                    q += 4 + sz.max(0) as usize;
                }
                idem::DEM_USERCMD => {
                    q += 8;
                    let nb = u16::from_le_bytes([data[q], data[q + 1]]) as usize;
                    q += 2 + nb;
                }
                idem::DEM_STOP => saw_stop = true,
                _ => {}
            }
        }
        if q != end {
            errs.push(format!(
                "entry {}: frame walk ended at {}, expected {} (drift {})",
                i,
                q,
                end,
                q as i64 - end as i64
            ));
        }
        if !saw_stop {
            errs.push(format!("entry {}: no terminating dem_stop", i));
        }
    }

    errs
}

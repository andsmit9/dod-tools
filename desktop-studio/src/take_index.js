// take_index.js
//
// Stable identity for highlights, so status survives things that rebuild the
// demo objects underneath it (a re-scan, a project save/load round trip).
//
// Highlights have no id of their own — nothing in CaptureStreak is unique and
// durable — so this derives a content-addressed one. Kill frames are used
// rather than start_tick/end_tick because editing a highlight's Kill Range
// moves the ticks but never mutates the kills array, so the uid stays put
// across range edits.

/**
 * Content-addressed id for one highlight, stable across re-scans, project
 * save/load, and Kill Range edits. Cached onto the streak after first use.
 *
 * Falls back to tick bounds when a streak has no kills (only really happens
 * for synthetic/test data) — still stable, just less precise.
 */
export function streakUid(demoPath, streak) {
  if (streak.uid) return streak.uid;

  const kills = streak.kills || [];
  const firstKill = kills.length ? kills[0][0] : streak.start_tick;
  const lastKill = kills.length ? kills[kills.length - 1][0] : streak.end_tick;
  const uid = `${demoPath}#${streak.player_index}#${firstKill}#${lastKill}`;

  // Non-enumerable so it never leaks into the capture payload sent over IPC,
  // where the Rust side would reject an unknown field.
  Object.defineProperty(streak, 'uid', { value: uid, enumerable: false, writable: true });
  return uid;
}

/**
 * Copies the user-owned, non-derivable state from a previously-loaded version
 * of a demo onto a freshly-scanned one, matching highlights by uid.
 *
 * A re-scan re-parses the demo and produces brand new streak objects, so
 * without this every status, selection, and note on that demo is silently
 * destroyed. Only fields the user set are carried over — everything derived
 * from the demo file itself comes from the fresh scan.
 */
export function preserveHighlightState(previousDemo, freshDemo) {
  if (!previousDemo?.streaks?.length || !freshDemo?.streaks?.length) return freshDemo;

  const previousByUid = new Map(
    previousDemo.streaks.map(s => [streakUid(previousDemo.path, s), s])
  );

  freshDemo.streaks.forEach(fresh => {
    const previous = previousByUid.get(streakUid(freshDemo.path, fresh));
    if (!previous) return;
    if (previous.status !== undefined) fresh.status = previous.status;
    if (previous.selected !== undefined) fresh.selected = previous.selected;
    if (previous.notes !== undefined) fresh.notes = previous.notes;
    // Kill Range edits are user edits too, not scan output.
    if (previous.start_index !== undefined) fresh.start_index = previous.start_index;
    if (previous.end_index !== undefined) fresh.end_index = previous.end_index;
  });

  return freshDemo;
}

// ── Durable take index ──────────────────────────────────────────────────────
//
// take_key -> uid[]. Recorded when a capture batch verifies a block on disk
// (capture_pane.js) so a render finishing later — possibly after a restart,
// re-scan, or project reload that produced entirely new streak objects — can
// still resolve which highlights to flip to Rendered. Plain object (not a
// Map) so it round-trips through JSON.stringify/parse with the rest of the
// project file with no extra conversion step.

/**
 * Records that the highlights identified by `uids` produced the take at
 * `takeKey`. Additive: a take key can cover more than one highlight (an
 * overlap merge recording several highlights into one block), and recording
 * the same key again (e.g. a re-capture) just unions the uid sets rather
 * than dropping the earlier ones.
 */
export function recordTake(takeIndex, takeKey, uids) {
  if (!takeIndex || !takeKey || !uids || uids.length === 0) return;
  const existing = takeIndex[takeKey] || [];
  takeIndex[takeKey] = Array.from(new Set([...existing, ...uids]));
}

/**
 * Highlight uids the given take key maps to, or an empty array if this take
 * was never recorded (e.g. captured before Phase 3, or from a project saved
 * on an older version that had no take index at all).
 */
export function resolveTake(takeIndex, takeKey) {
  if (!takeIndex || !takeKey) return [];
  return takeIndex[takeKey] || [];
}

/**
 * Drops index entries for uids no longer present in the current highlight
 * set (a demo removed from the queue, say), so the index doesn't grow
 * forever across a project's lifetime. Entries left with zero surviving
 * uids are dropped entirely rather than kept as an empty array.
 */
export function pruneTakeIndex(takeIndex, knownUids) {
  const known = new Set(knownUids);
  const pruned = {};
  for (const [key, uids] of Object.entries(takeIndex || {})) {
    const kept = uids.filter(uid => known.has(uid));
    if (kept.length > 0) pruned[key] = kept;
  }
  return pruned;
}

// ── "Tracked" predicate (Workspace mode's Clear Untracked protection) ──────

/** True if a streak's Kill Range has been narrowed from the full range. */
export function isRangeModified(streak) {
  const kills = streak.kills || [];
  const maxKillIdx = Math.max(kills.length - 1, 0);
  return (streak.start_index || 0) > 0 || (streak.end_index ?? maxKillIdx) < maxKillIdx;
}

/**
 * True if this highlight carries anything the user did on purpose — real
 * pipeline-earned status (Pending/Captured/Rendered), a note, or a narrowed
 * kill range. This is deliberately a wide net (per user, 2026-08-19): any
 * one of the three is enough to protect the row from Clear Untracked in
 * Workspace mode.
 *
 * `Pending` used to deliberately NOT count (revised 2026-08-19, was
 * originally "status !== None"): back then `streak.status` started as
 * `undefined` and the status dropdown *displayed* undefined as "Pending"
 * purely for convenience, without ever writing to the field — so an
 * explicit "Pending" selection looked identical to an untouched row, and
 * counting it as tracked could silently flip a row's protection with no
 * visible change. That premise is gone now that an unset status displays
 * (and counts, master_pane.js's countByStatus) as "None" instead —
 * "Pending" only ever appears once the user deliberately sets it, e.g. to
 * flag a highlight for a later capture pass without selecting it yet — so
 * it is exactly the kind of on-purpose signal this predicate exists to
 * protect, and the 2026-08-19 restriction no longer applies.
 */
function isHighlightTracked(streak) {
  if (!streak) return false;
  if (streak.status === 'Pending' || streak.status === 'Captured' || streak.status === 'Rendered') return true;
  if (streak.notes && streak.notes.trim()) return true;
  return isRangeModified(streak);
}

/** True if any of a demo's highlights are tracked (see isHighlightTracked). */
export function isDemoTracked(demo) {
  return (demo?.streaks || []).some(isHighlightTracked);
}

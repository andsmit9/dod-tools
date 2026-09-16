// cfg_warnings.js
// Tells the user what their own config files set, and what the app will
// override in them.
//
// Two different problems, both invisible without this:
//
//   1. A config sets something the pipeline reads and the app never hears about
//      it. That is how a capture ran at `mirv_fov 105` from movie.cfg while the
//      flush sized its on-screen test for the default 90.
//   2. An init command sets the same cvar a config does. The init command runs
//      later, so it wins — which is the point of typing it, but it also means a
//      line the user set deliberately, possibly years ago, quietly stops
//      applying. Including the commands the app adds for itself: the capture
//      fps overrides a `mirv_movie_fps` in movie.cfg, and the decal pin
//      overrides an `r_decals` there.
//
// ADVISORY ONLY. Nothing here, or anywhere in this app, writes to a config file.

import { scanGameConfigs } from './ipc_bridge.js';
import { STRINGS } from './strings.js';

const EMPTY = {
  unseen: [],
  overrides: [],
  shadowed: [],
  custom: [],
  bannedInit: [],
  bannedScheduled: [],
  decalDefaultRing: null,
  decalFlushIsNoop: false,
  noopInit: [],
  noopScheduled: [],
  fatalCvars: [],
};

let report = EMPTY;

/**
 * How many banned commands (see `cfg_scan::BANNED_COMMANDS`) the most
 * recently fetched report found, across Initial and Scheduled Commands
 * combined. `refreshLaunchGuard` (capture_pane.js) reads this to block Start
 * Capture Batch — reflects whatever `refreshCfgWarnings` last resolved, so
 * it can go briefly stale between an edit and the next scan finishing, same
 * as every other advisory here.
 */
export function bannedCommandCount() {
  return (report?.bannedInit?.length ?? 0) + (report?.bannedScheduled?.length ?? 0);
}

/**
 * Re-scan and redraw. Safe to call repeatedly — on start-up, when the hl.exe
 * path changes, and whenever the init command list is edited.
 *
 * `context` carries the settings that decide what the app appends for itself,
 * so the overrides it reports are the ones a capture would really apply.
 *
 * `customCommands` are `{command, relation, offset_seconds}` rather than bare
 * strings, because the order the engine reaches them decides which one is
 * actually displacing a config value and which is just changing it again.
 */
export async function refreshCfgWarnings(
  gamePath,
  initCommands = [],
  customCommands = [],
  context = {}
) {
  report = gamePath
    ? await scanGameConfigs(gamePath, initCommands, customCommands, context)
    : EMPTY;
  render();
}

/** The value half of `cvar value`, for rows that name the cvar separately. */
function valueOf(command) {
  return String(command).trim().split(/\s+/)[1] ?? '';
}

/** The cvar half of `cvar value` — `b.command` is the full typed line
 *  (arguments and all), but BANNED_REASONS is keyed by the bare cvar. */
function cvarOf(command) {
  return String(command).trim().split(/\s+/)[0] ?? '';
}

function section(title, advice, rows, accent) {
  const style = accent ? ` style="color:${accent}"` : '';
  return `
      <strong${style}>${title}</strong>
      <ul style="margin:6px 0 6px 18px; padding:0;">${rows}</ul>
      <div style="opacity:.8">${advice}</div>`;
}

/**
 * Wraps each section from `section()` and separates them with a divider —
 * except the last, which gets none. Done here rather than via a CSS
 * `:last-child` rule because each wrapper needs its own inline
 * `border-bottom`, and an inline style always beats a stylesheet selector
 * regardless of specificity, so a CSS-only "remove it on the last one"
 * rule can never actually take effect.
 */
function joinSections(parts) {
  return parts
    .map((html, i) => {
      const style =
        i === parts.length - 1
          ? ''
          : ' style="margin-bottom:16px; padding-bottom:16px; border-bottom:1px solid rgba(255,255,255,.12);"';
      return `<div${style}>${html}</div>`;
    })
    .join('');
}

/**
 * Fills one of the three per-field banners and shows/hides it independently
 * of the other two — each field's warnings sit directly under that field
 * (same reasoning as `.path-warning` in Path Routing) rather than all piling
 * into one banner under Initial Commands regardless of which field they're
 * actually about.
 */
function renderInto(elId, sectionsHtml) {
  const el = document.querySelector(elId);
  if (!el) return;

  if (!sectionsHtml) {
    el.hidden = true;
    el.innerHTML = '';
    return;
  }

  el.hidden = false;
  el.style.cssText =
    'margin: 0 0 8px; padding: 10px 12px; border: 1px solid #b58900; ' +
    'border-radius: 4px; background: #2a2410; color: #e8dcb0; font-size: 12px;';
  el.innerHTML = sectionsHtml;
}

function render() {
  const unseen = report?.unseen ?? [];
  const overrides = report?.overrides ?? [];
  const shadowed = report?.shadowed ?? [];
  const custom = report?.custom ?? [];
  const bannedInit = report?.bannedInit ?? [];
  const bannedScheduled = report?.bannedScheduled ?? [];
  const decalDefaultRing = report?.decalDefaultRing ?? null;
  const decalFlushIsNoop = report?.decalFlushIsNoop ?? false;
  const noopInit = report?.noopInit ?? [];
  const noopScheduled = report?.noopScheduled ?? [];
  const fatalCvars = report?.fatalCvars ?? [];
  // Banned commands are already flagged, more specifically, in the banned
  // section above — MID_DEMO_HAZARDS is a superset of BANNED_COMMANDS on the
  // Rust side, so without this a banned command would otherwise also show up
  // here as a mere "breaks the decal flush" hazard, understating a command
  // that's actually going to block Start Capture Batch outright.
  const bannedScheduledCvars = new Set(bannedScheduled.map((b) => cvarOf(b.command)));
  const hazards = custom.filter((c) => c.kind === 'hazard' && !bannedScheduledCvars.has(cvarOf(c.command)));
  const customOverrides = custom.filter((c) => c.kind !== 'hazard');

  // ── Initial Commands ─────────────────────────────────────────────────────
  // Game Config (unseen) belongs here, not its own block: the fix it advises
  // is "state this in Initial Commands", so that is where seeing it is useful.
  const initParts = [];
  // First of all, and ahead of even the block-on-Start-Capture-Batch case
  // below: a config already sitting on disk will quit the game outright the
  // moment the HUD renders, whether or not this batch ever starts. Nothing
  // here can fix a file the app does not write to, so it is not blocking --
  // but it is the most severe fact this banner can report.
  if (fatalCvars.length > 0) {
    const rows = fatalCvars
      .map((f) => `<li><code>${STRINGS.CFG.fatalRow(f.cvar, f.value, f.required, f.file, f.line)}</code></li>`)
      .join('');
    initParts.push(section(STRINGS.CFG.FATAL_TITLE, STRINGS.CFG.FATAL_ADVICE, rows, '#f44336'));
  }
  // Next — this one blocks Start Capture Batch, everything else below it is
  // merely advisory.
  if (bannedInit.length > 0) {
    const rows = bannedInit
      .map((b) => `<li><code>${STRINGS.CFG.bannedRowDetailed(b.command, STRINGS.CFG.BANNED_REASONS[cvarOf(b.command)])}</code></li>`)
      .join('');
    initParts.push(section(STRINGS.CFG.BANNED_TITLE, STRINGS.CFG.BANNED_ADVICE, rows, '#f44336'));
  }
  // Next, because something the user typed is being thrown away rather than
  // winning.
  if (shadowed.length > 0) {
    const rows = shadowed
      .map((s) => {
        const text = s.winnerFromApp
          ? STRINGS.CFG.shadowedByApp(
              s.cvar,
              s.shadowedValue,
              s.winnerValue,
              STRINGS.CFG.SETTING_FOR_CVAR[s.cvar.toLowerCase()] || STRINGS.CFG.UNKNOWN_SETTING
            )
          : STRINGS.CFG.shadowedByYou(s.cvar, s.shadowedValue, s.winnerValue);
        return `<li><code>${text}</code></li>`;
      })
      .join('');
    initParts.push(section(STRINGS.CFG.SHADOWED_TITLE, STRINGS.CFG.SHADOWED_ADVICE, rows, '#ff8a5c'));
  }
  if (unseen.length > 0) {
    const rows = unseen
      .map(
        (f) =>
          `<li><code>${f.cvar} ${f.value}</code> — ${STRINGS.CFG.location(f.file, f.line)}</li>`
      )
      .join('');
    initParts.push(section(STRINGS.CFG.BANNER_TITLE, STRINGS.CFG.ADVICE, rows));
  }
  if (overrides.length > 0) {
    const rows = overrides
      .map((o) => {
        const note = o.fromApp
          ? ` <span style="opacity:.7">(${STRINGS.CFG.FROM_APP_NOTE})</span>`
          : '';
        return `<li><code>${STRINGS.CFG.override(o.cvar, o.initValue, o.cfgValue, o.file, o.line)}</code>${note}</li>`;
      })
      .join('');
    initParts.push(section(STRINGS.CFG.OVERRIDE_TITLE, STRINGS.CFG.OVERRIDE_ADVICE, rows, '#ff8a5c'));
  }
  // Real, active problem — not a neutral FYI like decalDefaultRing below —
  // so it gets a warning accent even though there is only ever one row.
  if (decalFlushIsNoop) {
    const rows = `<li><code>${STRINGS.CFG.DECAL_NOOP_ROW}</code></li>`;
    initParts.push(section(STRINGS.CFG.DECAL_NOOP_TITLE, STRINGS.CFG.DECAL_NOOP_ADVICE, rows, '#f44336'));
  }
  // Last, and no accent: nothing is wrong with taking the default, so this
  // is an FYI rather than something to fix, unlike everything above it.
  if (decalDefaultRing != null) {
    const rows = `<li><code>${STRINGS.CFG.decalDefaultRow(decalDefaultRing)}</code></li>`;
    initParts.push(section(STRINGS.CFG.DECAL_DEFAULT_TITLE, STRINGS.CFG.DECAL_DEFAULT_ADVICE, rows));
  }
  // Weakest signal of all — not wrong, not silently changing behavior, just
  // wasted keystrokes — so it goes last and gets no accent, same as the
  // default-ring FYI above.
  if (noopInit.length > 0) {
    const rows = noopInit
      .map((n) => `<li><code>${STRINGS.CFG.noopRow(n.command, STRINGS.CFG.NOOP_REASONS[n.cvar], n.source)}</code></li>`)
      .join('');
    initParts.push(section(STRINGS.CFG.NOOP_TITLE, STRINGS.CFG.NOOP_ADVICE, rows));
  }
  renderInto('#init-commands-warning-banner', joinSections(initParts));

  // ── Scheduled Commands ───────────────────────────────────────────────────
  const schedParts = [];
  // First of all — this one blocks Start Capture Batch.
  if (bannedScheduled.length > 0) {
    const rows = bannedScheduled
      .map((b) => `<li><code>${STRINGS.CFG.bannedRowDetailed(b.command, STRINGS.CFG.BANNED_REASONS[cvarOf(b.command)])}</code></li>`)
      .join('');
    schedParts.push(section(STRINGS.CFG.BANNED_TITLE, STRINGS.CFG.BANNED_ADVICE, rows, '#f44336'));
  }
  // Next, because this one does not merely surprise: it breaks the
  // flush and leaves a capture that completes and looks plausible.
  if (hazards.length > 0) {
    const rows = hazards
      .map((h) => `<li><code>${STRINGS.CFG.hazardRow(h.command)}</code></li>`)
      .join('');
    schedParts.push(section(STRINGS.CFG.HAZARD_TITLE, STRINGS.CFG.HAZARD_ADVICE, rows, '#ff6b6b'));
  }
  if (customOverrides.length > 0) {
    const rows = customOverrides
      .map((c) => {
        const text =
          c.kind === 'overridesInit'
            ? STRINGS.CFG.customOverridesInit(c.cvar, valueOf(c.command), c.replacedValue)
            : STRINGS.CFG.customOverridesConfig(
                c.cvar,
                valueOf(c.command),
                c.replacedValue,
                c.source
              );
        return `<li><code>${text}</code></li>`;
      })
      .join('');
    schedParts.push(section(STRINGS.CFG.CUSTOM_TITLE, STRINGS.CFG.CUSTOM_ADVICE, rows, '#ff8a5c'));
  }
  if (noopScheduled.length > 0) {
    const rows = noopScheduled
      .map((n) => `<li><code>${STRINGS.CFG.noopRow(n.command, STRINGS.CFG.NOOP_REASONS[n.cvar], n.source)}</code></li>`)
      .join('');
    schedParts.push(section(STRINGS.CFG.NOOP_TITLE, STRINGS.CFG.NOOP_ADVICE, rows));
  }
  renderInto('#scheduled-commands-warning-banner', joinSections(schedParts));
}

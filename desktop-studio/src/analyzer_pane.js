// analyzer_pane.js
// Standalone "Demo Analyzer" tab — a JS port of the egui report_ui views
// (Summary / Scoreboard / Player Details / Team Details / Timeline / Rounds /
// Chat Log) from the `dev` branch. Reads the full analysis::{DemoInfo,
// AnalyzerState} payload from `analyze_demo_full` rather than the flattened
// generic JSON used by the compact inline telemetry summary.

import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import { analyzeDemoFull, browseDirectory, defaultBrowseDir, countDemoFiles, scanDemoFolders, getWeaponDisplayNames } from './ipc_bridge.js';
import { STRINGS } from './strings.js';

function setAnalyzerFileIndicator(text) {
  const titleEl = document.querySelector('#analyzer-current-file');
  const footerEl = document.querySelector('#footer-analyzer-current-file');
  if (titleEl) titleEl.textContent = text;
  if (footerEl) footerEl.textContent = text || STRINGS.ANALYZER.NO_DEMO_LOADED;
}

let report = null;
let analyzerLoadInProgress = false;
let activeSubTab = 'summary';
let highlightedPlayerId = null; // shared selection: Scoreboard row <-> Player Details dropdown
let selectedPlayerId = null;
let chatFilters = {
  showMm1: true, showMm2: true, team: 'All', search: '',
  status: 'All', // 'All' | 'Alive' | 'Dead'
  showJoins: true, showTeams: true, showGameplay: true, showOtherSys: true,
};
// Kill Streaks weapon-category filter — reset whenever the selected player changes.
let disabledWeapons = new Set();

const WEAPON_CATEGORIES = [
  [STRINGS.ANALYZER.WEAPON_CATEGORY_GRENADES, ['Mk2Grenade', 'StickGrenade', 'MillsBomb']],
  [STRINGS.ANALYZER.WEAPON_CATEGORY_MELEE, ['Kabar', 'GermanKnife', 'BritishKnife', 'Spade', 'K98Bayonet', 'EnfieldBayonet', 'ButtStock']],
  [STRINGS.ANALYZER.WEAPON_CATEGORY_ALLIED, ['M1911', 'Garand', 'Springfield', 'Thompson', 'Bar', 'M1Carbine', 'Browning30Cal', 'GreaseGun', 'Bazooka', 'LeeEnfield', 'ScopedLeeEnfield', 'Sten', 'Bren', 'Webley', 'Piat', 'M1A1Carbine', 'Mortar']],
  [STRINGS.ANALYZER.AXIS_LABEL, ['Luger', 'ScopedK98', 'Stg44', 'K98', 'Mp40', 'Mg42', 'Mg34', 'Fg42', 'ScopedFg42', 'K43', 'Panzerschreck']],
];

// ── Explorer sidebar state — a real native folder tree (drives -> subfolders,
// lazily loaded, expand/collapse) plus a 3-tier Quick Links box (Pinned /
// Recent / Local), both driving one shared `currentDir`. The demos table
// below is scoped to ONLY that single folder's contents (non-recursive) —
// mirrors dev's SidePanel::left explorer + `desktop_files`, see
// docs/archive/tauri_parity_audit.md Area 3 for the corrected design this replaced
// an earlier (wrong-shape) recursive multi-folder aggregate with. Dev's
// "Group by Match"/"Group by Player-Recorder" view modes are NOT ported:
// their grouping keys (server_ip/player_roster_hash/recorder_id) were only
// ever assigned `None` in dev's own source, so those two view modes never
// actually grouped anything there either — restoring the one real, working
// view (Flat List) is the faithful port here, not a scope cut.
let getPinnedFolders = () => [];
let pinFolder = async () => {};
let unpinFolder = async () => {};
let getDemoFolderHistory = () => [];
// Gates the Explorer tree's per-subfolder "(N)" demo-count badge — dev's
// `settings.scan_folders_for_demos`, defaults false. Quick Links counts are
// NOT gated by this (dev never gated those either), only the tree is.
let getScanFoldersForDemos = () => false;
let setScanFoldersForDemos = async () => {};
let recordDemoFolderVisit = async () => {};
let forgetDemoFolderVisit = async () => {};
let getAnalyzerExplorerWidth = () => 260;
let setAnalyzerExplorerWidth = async () => {};

let currentDir = null;
const dirCache = new Map(); // path -> DirListing from browse_directory
const openTreeNodes = new Set();
let thisPcOpen = true;
let driveRoots = []; // DirEntryLite[]
let localFolders = []; // DemoFolderHit[] from scan_demo_folders
let localFoldersScanning = false;
let localFoldersScanTriggered = false;
const quickLinkCountCache = new Map(); // path -> demo_count

let currentFolderDemos = []; // DemoFileEntry[] for currentDir only
let browserSelectedDemo = null;
// Arrow-key cursor, separate from browserSelectedDemo (the actually-loaded
// demo) — mirrors master_pane.js's keyboard-selected/table-row-selected
// split so arrowing through the list previews a lightweight outline instead
// of moving the same heavy "selected" fill Enter/click commits. #99.
let browserCursorPath = null;
// coldemoplayer-style focus scoping: click into a list and arrow keys
// apply only to it. Defaults to 'demos' so arrow-nav keeps working
// out-of-the-box exactly like before this existed. Only the Demo Browser
// still uses a cursor/commit split (analyze_demo_full is a real parse worth
// gating) — the Explorer Tree is instant-select, since browse_directory is
// cheap. #99.
let focusedList = 'demos';
let browserError = null;
let demoFilterQuery = '';
let demoFilterType = 'All';
let demoFilterMap = '';
let demoFilterDateStart = '';
let demoFilterDateEnd = '';
let demoSortColumn = null; // 'name' | 'type' | 'map' | 'date'
let demoSortAscending = true;

const TEAM_COLORS = {
  Allies: '#4caf50',
  British: '#daa520',
  Axis: '#e05252',
  Spectators: '#cccc33',
  Unassigned: '#aaaaaa',
};

// ── Small formatting helpers ─────────────────────────────────────────────────

function teamColor(team) { return TEAM_COLORS[team] || '#ffffff'; }

function teamLabel(team, alliesAreBritish) {
  if (team === 'Allies' || team === 'British') return alliesAreBritish ? STRINGS.ANALYZER.BRITISH_LABEL : STRINGS.ANALYZER.ALLIES_LABEL;
  if (team === 'Axis') return STRINGS.ANALYZER.AXIS_LABEL;
  if (team === 'Spectators') return STRINGS.ANALYZER.SPECTATORS_LABEL;
  return STRINGS.ANALYZER.UNASSIGNED_LABEL;
}

function durSecs(d) { return d ? (d.secs || 0) + (d.nanos || 0) / 1e9 : 0; }

function formatDuration(totalSecs) {
  totalSecs = Math.max(0, Math.floor(totalSecs || 0));
  const h = Math.floor(totalSecs / 3600);
  const m = Math.floor((totalSecs % 3600) / 60);
  const s = totalSecs % 60;
  return STRINGS.ANALYZER.durationLong(h, m, s);
}

function formatMMSS(totalSecs) {
  totalSecs = Math.max(0, Math.floor(totalSecs || 0));
  const m = Math.floor(totalSecs / 60);
  const s = totalSecs % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

function formatGameTime(totalSecs) {
  totalSecs = Math.max(0, totalSecs || 0);
  const m = Math.floor(totalSecs / 60);
  const s = Math.floor(totalSecs % 60);
  const cc = Math.floor((totalSecs - Math.floor(totalSecs)) * 100);
  return `${m}:${String(s).padStart(2, '0')}:${String(cc).padStart(2, '0')}`;
}

// Populated once from get_weapon_display_names (see loadWeaponDisplayNames,
// kicked off by initAnalyzerPane) — the same names
// native/src/patch/scanner.rs bakes into a kill streak's timeline text, so
// this pane can no longer show a different name for the same weapon (#34).
let weaponDisplayNames = null;

function loadWeaponDisplayNames() {
  getWeaponDisplayNames().then((names) => { weaponDisplayNames = names; });
}

// Falls back to a regex-derived approximation (e.g. "ScopedK98" -> "Scoped
// K98") only until the backend map above has loaded, or for a variant the
// loc files don't cover — never leaves the cell blank.
function weaponName(w) {
  if (!w) return STRINGS.ANALYZER.WEAPON_UNKNOWN;
  const resolved = weaponDisplayNames && weaponDisplayNames[w];
  if (resolved) return resolved;
  return String(w).replace(/([a-z0-9])([A-Z])/g, '$1 $2');
}

function esc(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// SteamID64 -> classic STEAM_0:X:YYYY. Falls back to the raw id for
// non-numeric PlayerGlobalId values (e.g. "PLAYER_<fid>").
function steamIdDisplay(id) {
  if (!id || !/^\d{15,20}$/.test(id)) return id || STRINGS.ANALYZER.EMPTY_DASH;
  try {
    const big = BigInt(id);
    const base = 76561197960265728n;
    if (big < base) return id;
    const accountId = big - base;
    return `STEAM_0:${accountId % 2n}:${accountId / 2n}`;
  } catch {
    return id;
  }
}

function isConnected(player) {
  return player.connection && typeof player.connection === 'object' && 'Connected' in player.connection;
}

function playerClientId(player) {
  return isConnected(player) ? player.connection.Connected.client_id : null;
}

function getTeamScore(matchFn) {
  const timeline = (report.state.team_scores && report.state.team_scores.timeline) || [];
  for (let i = timeline.length - 1; i >= 0; i--) {
    if (matchFn(timeline[i][1])) return timeline[i][2];
  }
  return 0;
}

function mortalityLifespans(mortality) {
  if (!mortality || mortality.length === 0) return [];
  const spans = [];
  let aliveAt = null;
  for (const change of mortality) {
    const [time, state] = change;
    const secs = durSecs(time.viewdemo_offset);
    if (state === 'Alive') aliveAt = secs;
    else if (state === 'Dead' && aliveAt !== null) {
      if (secs >= aliveAt) spans.push(secs - aliveAt);
      aliveAt = null;
    }
  }
  return spans;
}

function groupConsecutiveWeapons(names) {
  const parts = [];
  let i = 0;
  while (i < names.length) {
    let j = i;
    while (j + 1 < names.length && names[j + 1] === names[i]) j++;
    const count = j - i + 1;
    parts.push(count > 1 ? `${names[i]} x${count}` : names[i]);
    i = j + 1;
  }
  return parts.join(', ');
}

// ── Explorer sidebar + single-folder demo list ───────────────────────────────

function demoTypeOf(entry) {
  return entry.demo_type || STRINGS.ANALYZER.TYPE_POV;
}

// Guarantees the drive letter and final folder name stay visible, eliding
// the middle when the full path is too long to fit the sidebar — e.g.
// "C:\...\DoD Demos". Falls back to a forward-slash root-relative path when
// short enough (dev's own `display_path`, see tree quick-links in
// docs/archive/tauri_parity_audit.md Area 3). Full path is always available via the
// row's `title` attribute regardless of how the visible label is shortened.
function shortenPath(fullPath, maxChars = 34) {
  if (!fullPath) return '';
  const forward = fullPath.replace(/\\/g, '/');
  if (forward.length <= maxChars) return forward;

  const driveMatch = fullPath.match(/^([A-Za-z]:\\|\\\\|\/)/);
  const drive = driveMatch ? driveMatch[0].replace(/[\\/]+$/, '') : '';
  const segments = fullPath.split(/[\\/]/).filter(Boolean);
  let finalComponent = segments[segments.length - 1] || fullPath;

  let collapsed = `${drive}\\...\\${finalComponent}`;
  if (collapsed.length > maxChars) {
    const keep = Math.max(6, maxChars - drive.length - 8);
    finalComponent = `…${finalComponent.slice(-keep)}`;
    collapsed = `${drive}\\...\\${finalComponent}`;
  }
  return collapsed;
}

// `pinned_folders`/`demo_folder_history` are shared with Capture Studio's
// own "+ Add Demo Files" flow, which legitimately pushes individual `.dem`
// FILE paths into the same list (main.js's scanPaths) for its own rescan
// purposes. Quick Links/the tree only ever navigate to folders, so filter
// those file entries out here rather than showing them as bogus "pinned
// folders" that error out with "Not a directory" when clicked.
function isFolderPath(p) { return !/\.dem$/i.test(p); }

function pathSeparator(p) { return p.includes('\\') ? '\\' : '/'; }

function parentDirOf(path) {
  const sep = pathSeparator(path);
  const idx = path.lastIndexOf(sep);
  return idx > 0 ? path.slice(0, idx) : null;
}

// Full ancestor chain from the top of the tree down to (and including)
// `path` itself — used to auto-expand the Explorer Tree down to a selection
// and to detect ancestor/descendant relationships for the collapse-jumps-up
// mechanic below.
function ancestorChain(path) {
  const sep = pathSeparator(path);
  const trimmed = path.endsWith(sep) && path.length > sep.length ? path.slice(0, -1) : path;
  const segments = trimmed.split(sep).filter((s) => s.length > 0);
  const chain = [];
  if (sep === '\\' && /^[A-Za-z]:$/.test(segments[0] || '')) {
    let acc = `${segments[0]}\\`;
    chain.push(acc);
    for (let i = 1; i < segments.length; i++) {
      acc += segments[i];
      chain.push(acc);
      acc += '\\';
    }
  } else {
    let acc = '/';
    for (const seg of segments) {
      acc = acc === '/' ? `/${seg}` : `${acc}/${seg}`;
      chain.push(acc);
    }
  }
  return chain;
}

function isAncestorOf(candidate, descendant) {
  if (!descendant || candidate === descendant) return false;
  return ancestorChain(descendant).includes(candidate);
}

async function countDemoFilesCached(path) {
  if (quickLinkCountCache.has(path)) return quickLinkCountCache.get(path);
  const n = await countDemoFiles(path);
  quickLinkCountCache.set(path, n);
  return n;
}

function quickLinkRowHtml(folder, count, isPinned) {
  const label = shortenPath(folder);
  return `<div class="quicklink-row ${folder === currentDir ? 'selected' : ''}" data-path="${esc(folder)}" title="${esc(folder)}">
    <button class="quicklink-pin-btn ${isPinned ? 'pinned' : ''}" data-path="${esc(folder)}" title="${isPinned ? STRINGS.ANALYZER.UNPIN_FOLDER_TITLE : STRINGS.ANALYZER.PIN_FOLDER_TITLE}">📌</button>
    <span class="quicklink-label">${esc(label)} (${count})</span>
  </div>`;
}

// Dev's Windows-11-style Quick Access pattern: Pinned (explicit bookmarks),
// Recent (auto-tracked history), Local (bounded background scan) — each
// tier hidden entirely when empty, excludes anything already promoted to a
// higher tier. See docs/archive/tauri_parity_audit.md Area 3.
async function renderQuickLinksSection() {
  const container = document.querySelector('#analyzer-quick-links');
  if (!container) return;

  const pinned = (getPinnedFolders() || []).filter(isFolderPath);
  const recent = (getDemoFolderHistory() || []).filter(isFolderPath).filter((f) => !pinned.includes(f));
  const local = localFolders.map((f) => f.path).filter((f) => !pinned.includes(f));

  const allPaths = [...new Set([...pinned, ...recent, ...local])];
  await Promise.all(allPaths.map((p) => countDemoFilesCached(p)));

  const tier = (label, paths) => paths.length === 0 ? '' : `
    <div class="quicklink-tier-label">${esc(label)}</div>
    ${paths.map((p) => quickLinkRowHtml(p, quickLinkCountCache.get(p) || 0, pinned.includes(p))).join('')}`;

  if (pinned.length === 0 && recent.length === 0 && local.length === 0) {
    container.innerHTML = localFoldersScanning
      ? `<div class="quicklink-empty"><span class="spinner"></span> ${STRINGS.ANALYZER.SCANNING_WORKSPACE}</div>`
      : `<div class="quicklink-empty">${STRINGS.ANALYZER.NO_DEMO_FOLDERS_FOUND}</div>`;
  } else {
    container.innerHTML = tier(STRINGS.ANALYZER.TIER_PINNED, pinned) + tier(STRINGS.ANALYZER.TIER_RECENT, recent) + tier(STRINGS.ANALYZER.TIER_LOCAL, local);
  }

  container.querySelectorAll('.quicklink-row').forEach((row) => {
    row.addEventListener('click', (e) => {
      if (e.target.closest('.quicklink-pin-btn')) return;
      setCurrentDir(row.dataset.path);
    });
  });
  container.querySelectorAll('.quicklink-pin-btn').forEach((btn) => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const path = btn.dataset.path;
      const isPinned = (getPinnedFolders() || []).includes(path);
      if (isPinned) await unpinFolder(path); else await pinFolder(path);
      renderQuickLinksSection();
    });
  });
}

function treeRowHtml(entry) {
  const { path, name, demo_count } = entry;
  const isOpen = openTreeNodes.has(path);
  const isSelected = path === currentDir;
  const showCount = getScanFoldersForDemos() && demo_count > 0;
  const icon = showCount ? '📂' : '📁';
  const label = showCount ? `${name} (${demo_count})` : name;
  const arrow = isOpen ? '⏷' : '⏵';

  let childrenHtml = '';
  if (isOpen) {
    const listing = dirCache.get(path);
    if (listing) {
      childrenHtml = listing.subdirs.length > 0
        ? `<div class="tree-children">${listing.subdirs.map(treeRowHtml).join('')}</div>`
        : '';
    } else {
      childrenHtml = `<div class="tree-children"><div class="tree-loading">${STRINGS.ANALYZER.LOADING_LABEL}</div></div>`;
    }
  }

  return `<div class="tree-node">
    <div class="tree-row ${isSelected ? 'selected' : ''}">
      <button class="tree-toggle" data-path="${esc(path)}">${arrow}</button>
      <span class="tree-label" data-path="${esc(path)}" title="${esc(path)}">${icon} ${esc(label)}</span>
    </div>
    ${childrenHtml}
  </div>`;
}

// Flattens the currently-*visible*, navigable tree rows (open nodes only, in
// the same order treeRowHtml renders them) so Up/Down can move the selection
// through it without querying the DOM. This PC itself is excluded — it isn't
// a real folder setCurrentDir can navigate to. #99.
function flattenVisibleTreeRows() {
  const rows = [];
  if (thisPcOpen) {
    const walk = (entries) => {
      for (const entry of entries) {
        rows.push({ path: entry.path });
        if (openTreeNodes.has(entry.path)) {
          const listing = dirCache.get(entry.path);
          if (listing) walk(listing.subdirs);
        }
      }
    };
    walk(driveRoots);
  }
  return rows;
}

// A drive root (e.g. "C:\") has no real parent folder to navigate to. #99.
// parentDirOf slices off everything after the last separator, which for a
// folder directly under a drive root yields a bare "C:" — one character
// short of how driveRoots/dirCache actually key the root ("C:\\", see
// native_roots() in dir_browser.rs) — so normalize that one case back to it.
function treeParentOf(path) {
  if (/^[A-Za-z]:\\?$/.test(path)) return null;
  const parent = parentDirOf(path);
  return parent && /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
}

// Native Explorer Tree: drives -> subfolders, lazily loaded and cached per
// node, genuinely expand/collapse (default closed). Mirrors dev's
// `tree.rs::render_native_dir_node` — see docs/archive/tauri_parity_audit.md Area 3.
async function renderExplorerTree() {
  const container = document.querySelector('#analyzer-tree');
  if (!container) return;
  if (driveRoots.length === 0) {
    try {
      const listing = await browseDirectory(null);
      driveRoots = listing.subdirs;
    } catch {
      driveRoots = [];
    }
  }

  const thisPcArrow = thisPcOpen ? '⏷' : '⏵';
  container.innerHTML = `<div class="tree-node">
    <div class="tree-row">
      <button class="tree-toggle" id="tree-this-pc-toggle">${thisPcArrow}</button>
      <span class="tree-label">${STRINGS.ANALYZER.THIS_PC_LABEL}</span>
    </div>
    ${thisPcOpen ? `<div class="tree-children">${driveRoots.map(treeRowHtml).join('')}</div>` : ''}
  </div>`;

  const thisPcToggle = container.querySelector('#tree-this-pc-toggle');
  thisPcToggle?.addEventListener('click', () => {
    thisPcOpen = !thisPcOpen;
    renderExplorerTree();
  });

  container.querySelectorAll('.tree-toggle:not(#tree-this-pc-toggle)').forEach((btn) => {
    btn.addEventListener('click', () => {
      const path = btn.dataset.path;
      if (openTreeNodes.has(path)) closeTreeNode(path); else openTreeNode(path);
    });
  });
  container.querySelectorAll('.tree-label').forEach((el) => {
    el.addEventListener('click', () => setCurrentDir(el.dataset.path));
  });
}

// Shared by the toggle-button click handler above and the keyboard Right
// key (treeRight) so both open a node identically. #99.
async function openTreeNode(path) {
  if (openTreeNodes.has(path)) return;
  openTreeNodes.add(path);
  renderExplorerTree();
  if (!dirCache.has(path)) {
    try {
      dirCache.set(path, await browseDirectory(path));
    } catch {
      dirCache.set(path, { subdirs: [], demos: [] });
    }
    renderExplorerTree();
  }
}

// Shared by the toggle-button click handler and the keyboard Left key
// (treeLeft). Key mechanic: collapsing a node you're currently inside
// navigates you up to it, rather than leaving you on a hidden selection —
// matches dev/Windows Explorer both (tree.rs:373-383). #99.
async function closeTreeNode(path) {
  if (!openTreeNodes.has(path)) return;
  openTreeNodes.delete(path);
  if (isAncestorOf(path, currentDir)) {
    await setCurrentDir(path);
    return;
  }
  renderExplorerTree();
}

function scrollTreeRowIntoView(path) {
  const container = document.querySelector('#analyzer-tree');
  if (!container || !path) return;
  container.querySelector(`.tree-label[data-path="${CSS.escape(path)}"]`)?.closest('.tree-row')?.scrollIntoView({ block: 'nearest' });
}

// Instant select, no separate cursor: browse_directory is a cheap
// non-recursive read_dir (no demo parsing), so there's no cost to gate
// behind a commit step, unlike the Demo Browser's analyze_demo_full. #99.
function moveTreeSelection(dir) {
  const rows = flattenVisibleTreeRows();
  if (rows.length === 0) return;
  const idx = rows.findIndex((r) => r.path === currentDir);
  const newIdx = idx === -1 ? (dir > 0 ? 0 : rows.length - 1) : Math.min(rows.length - 1, Math.max(0, idx + dir));
  const target = rows[newIdx].path;
  if (target === currentDir) return;
  setCurrentDir(target).then(() => scrollTreeRowIntoView(target));
}

// Right: expands the selected folder if closed, or steps the selection into
// its first child if already open — matches Windows Explorer's tree view. #99.
function treeRight() {
  if (!currentDir) return;
  if (!openTreeNodes.has(currentDir)) {
    openTreeNode(currentDir);
    return;
  }
  const listing = dirCache.get(currentDir);
  if (listing && listing.subdirs.length > 0) {
    const target = listing.subdirs[0].path;
    setCurrentDir(target).then(() => scrollTreeRowIntoView(target));
  }
}

// Left: collapses the selected folder if open, else steps the selection up
// to its parent. #99.
function treeLeft() {
  if (!currentDir) return;
  if (openTreeNodes.has(currentDir)) {
    closeTreeNode(currentDir);
    return;
  }
  const parent = treeParentOf(currentDir);
  if (parent) setCurrentDir(parent).then(() => scrollTreeRowIntoView(parent));
}

async function forgetInvalidFolder(path) {
  if ((getPinnedFolders() || []).includes(path)) await unpinFolder(path);
  await forgetDemoFolderVisit(path);
}

// Sets the single folder the Demos panel and Explorer Tree are scoped to.
// Key mechanic: whenever a node's ancestor chain contains `currentDir`, that
// node force-opens on the next render — so simply changing `currentDir`
// (from a tree click *or* a Quick Links click) is what makes the tree
// auto-expand down to the newly selected folder (tree.rs:358-368).
async function setCurrentDir(path) {
  // currentDir (and the tree's ancestor-open state) update unconditionally,
  // even when the folder below turns out to be unreadable — an inaccessible
  // folder (permission denied, etc.) still exists and is still a real place
  // in the tree, it just has no demos to list. Previously this only ran on
  // success, so navigating onto a blocked folder left currentDir pointing at
  // whatever came before it — every Up/Down/Left/Right computed "next from
  // currentDir" landed back on the same blocked folder forever, arrow-key
  // navigation couldn't move past it in either direction. #99.
  currentDir = path;
  browserSelectedDemo = null;
  browserCursorPath = null;

  const ancestors = ancestorChain(path);
  ancestors.slice(0, -1).forEach((a) => openTreeNodes.add(a));
  await Promise.all(ancestors.slice(0, -1).map(async (a) => {
    if (!dirCache.has(a)) {
      try { dirCache.set(a, await browseDirectory(a)); } catch { /* leaf render shows a loading placeholder */ }
    }
  }));

  let listing;
  try {
    listing = await browseDirectory(path);
  } catch (err) {
    browserError = String(err);
    currentFolderDemos = [];
    await forgetInvalidFolder(path);
    renderQuickLinksSection();
    await renderExplorerTree();
    renderDemoTable();
    return;
  }

  dirCache.set(path, listing);
  browserError = null;
  currentFolderDemos = listing.demos;

  renderQuickLinksSection();
  await renderExplorerTree();
  renderDemoTable();

  if (listing.demos.length > 0) {
    await recordDemoFolderVisit(path);
    renderQuickLinksSection();
  }
}

async function triggerLocalFoldersScan(force = false) {
  if (localFoldersScanTriggered && !force) return;
  localFoldersScanTriggered = true;
  localFoldersScanning = true;
  renderQuickLinksSection();
  try {
    const root = currentDir || (await defaultBrowseDir());
    localFolders = await scanDemoFolders(root);
  } catch {
    localFolders = [];
  }
  localFoldersScanning = false;
  renderQuickLinksSection();
}

function demoDateISO(entry) {
  if (!entry.modified_unix_secs) return '';
  return new Date(entry.modified_unix_secs * 1000).toISOString().slice(0, 10);
}

function demoDateDisplay(entry) {
  if (!entry.modified_unix_secs) return STRINGS.ANALYZER.EMPTY_DASH;
  const d = new Date(entry.modified_unix_secs * 1000);
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
}

function passesDemoFilter(entry) {
  if (demoFilterQuery) {
    const q = demoFilterQuery.toLowerCase();
    const hay = `${entry.name} ${entry.map_name || ''} ${entry.path}`.toLowerCase();
    if (!hay.includes(q)) return false;
  }
  if (demoFilterType !== 'All' && demoTypeOf(entry) !== demoFilterType) return false;
  if (demoFilterMap && !(entry.map_name || '').toLowerCase().includes(demoFilterMap.toLowerCase())) return false;
  const iso = demoDateISO(entry);
  if (demoFilterDateStart.length === 10 && (iso.length < 10 || iso < demoFilterDateStart)) return false;
  if (demoFilterDateEnd.length === 10 && (iso.length < 10 || iso > demoFilterDateEnd)) return false;
  return true;
}

function sortedFilteredDemos() {
  let list = currentFolderDemos.filter(passesDemoFilter);
  if (demoSortColumn) {
    list = list.slice().sort((a, b) => {
      let cmp;
      switch (demoSortColumn) {
        case 'name': cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase()); break;
        case 'type': cmp = demoTypeOf(a).localeCompare(demoTypeOf(b)); break;
        case 'map': cmp = (a.map_name || '').toLowerCase().localeCompare((b.map_name || '').toLowerCase()); break;
        case 'date': cmp = a.modified_unix_secs - b.modified_unix_secs; break;
        default: cmp = 0;
      }
      return demoSortAscending ? cmp : -cmp;
    });
  }
  return list;
}

function updateSortHeaderIndicators() {
  document.querySelectorAll('#analyzer-demo-table th[data-sort]').forEach((th) => {
    const base = STRINGS.ANALYZER.DEMO_TABLE_HEADERS[th.dataset.sort] || th.dataset.label;
    th.textContent = demoSortColumn === th.dataset.sort ? `${base} ${demoSortAscending ? '▲' : '▼'}` : base;
  });
}

function renderDemoTable() {
  const tbody = document.querySelector('#analyzer-demo-tbody');
  if (!tbody) return;
  updateSortHeaderIndicators();

  if (browserError) {
    tbody.innerHTML = `<tr><td colspan="4" class="table-empty" style="color:#f44336;">${esc(browserError)}</td></tr>`;
    return;
  }
  if (!currentDir) {
    tbody.innerHTML = `<tr><td colspan="4" class="table-empty">${STRINGS.ANALYZER.PICK_FOLDER_FROM_SIDEBAR}</td></tr>`;
    return;
  }
  if (currentFolderDemos.length === 0) {
    tbody.innerHTML = `<tr><td colspan="4" class="table-empty">${STRINGS.ANALYZER.NO_DEMOS_IN_FOLDER}</td></tr>`;
    return;
  }
  const list = sortedFilteredDemos();
  if (list.length === 0) {
    tbody.innerHTML = `<tr><td colspan="4" class="table-empty">${STRINGS.ANALYZER.NO_DEMOS_MATCH_FILTERS}</td></tr>`;
    return;
  }

  tbody.innerHTML = list.map((entry) => {
    const isSelected = entry.path === browserSelectedDemo;
    const isCursor = entry.path === browserCursorPath;
    const classes = ['analyzer-demo-row', isSelected ? 'selected' : '', isCursor ? 'keyboard-selected' : ''].filter(Boolean).join(' ');
    return `<tr class="${classes}" data-path="${esc(entry.path)}" title="${esc(entry.path)}">
      <td>${esc(entry.name)}</td>
      <td>${esc(demoTypeOf(entry))}</td>
      <td>${esc(entry.map_name || STRINGS.ANALYZER.EMPTY_DASH)}</td>
      <td>${esc(demoDateDisplay(entry))}</td>
    </tr>`;
  }).join('');

  tbody.querySelectorAll('tr[data-path]').forEach((tr) => {
    tr.addEventListener('click', () => selectDemo(tr.dataset.path));
  });
}

function selectDemo(path) {
  browserSelectedDemo = path;
  renderDemoTable();
  loadAnalyzerDemo(path);
}

function handleDemoTableKeydown(e) {
  const list = sortedFilteredDemos();
  if (list.length === 0) return;

  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const dir = e.key === 'ArrowDown' ? 1 : -1;
    // No cursor yet — start from wherever the actual selection already is,
    // same fallback as master_pane.js, so arrowing away from a loaded demo
    // moves one row from it instead of always restarting at the top.
    const currentPath = browserCursorPath ?? browserSelectedDemo;
    const idx = list.findIndex((d) => d.path === currentPath);
    const newIdx = idx === -1 ? (dir > 0 ? 0 : list.length - 1) : Math.min(list.length - 1, Math.max(0, idx + dir));
    browserCursorPath = list[newIdx].path;
    renderDemoTable();
    const row = document.querySelector(`#analyzer-demo-tbody tr[data-path="${CSS.escape(browserCursorPath)}"]`);
    row?.scrollIntoView({ block: 'nearest' });
  } else if (e.key === 'Enter') {
    const target = browserCursorPath ?? browserSelectedDemo;
    if (target) selectDemo(target);
  } else if (e.key === 'Escape' && browserCursorPath) {
    // Clears the arrow-nav cursor only — never what loaded the
    // currently-displayed demo (only Enter/click do that via selectDemo),
    // so this can't lose or change anything shown. #28.
    e.preventDefault();
    browserCursorPath = null;
    renderDemoTable();
  }
}

function handleTreeKeydown(e) {
  if (e.key === 'ArrowDown') { e.preventDefault(); moveTreeSelection(1); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); moveTreeSelection(-1); }
  else if (e.key === 'ArrowRight') { e.preventDefault(); treeRight(); }
  else if (e.key === 'ArrowLeft') { e.preventDefault(); treeLeft(); }
}

// Single global handler, dispatched by focusedList (coldemoplayer-style:
// whichever of the Explorer Tree / Demos table was last clicked). #99.
function initAnalyzerBrowserKeyboardNav() {
  document.addEventListener('keydown', (e) => {
    const pane = document.querySelector('#pane-demo-analyzer');
    if (!pane || pane.style.display === 'none') return;
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;

    if (focusedList === 'tree') handleTreeKeydown(e);
    else handleDemoTableKeydown(e);
  });
}

function initAnalyzerBrowser() {
  const refreshBtn = document.querySelector('#analyzer-tree-refresh-btn');
  if (refreshBtn) {
    refreshBtn.addEventListener('click', () => {
      dirCache.clear();
      driveRoots = [];
      quickLinkCountCache.clear();
      triggerLocalFoldersScan(true);
      renderExplorerTree();
      if (currentDir) setCurrentDir(currentDir);
    });
  }

  // Focus-scoping: click into either list and arrow keys apply to it. #99.
  document.querySelector('#analyzer-tree')?.addEventListener('click', () => { focusedList = 'tree'; });
  document.querySelector('#analyzer-demo-table')?.addEventListener('click', () => { focusedList = 'demos'; });

  const scanFoldersCb = document.querySelector('#analyzer-scan-folders-for-demos');
  if (scanFoldersCb) {
    scanFoldersCb.checked = getScanFoldersForDemos();
    scanFoldersCb.addEventListener('change', async (e) => {
      await setScanFoldersForDemos(e.target.checked);
      renderExplorerTree();
    });
  }

  const addPinBtn = document.querySelector('#analyzer-add-pin-btn');
  if (addPinBtn) {
    addPinBtn.addEventListener('click', async () => {
      try {
        const selected = await open({ directory: true, multiple: false, title: STRINGS.ANALYZER.ADD_PINNED_FOLDER_DIALOG_TITLE });
        if (selected) {
          const folder = Array.isArray(selected) ? selected[0] : selected;
          await pinFolder(folder);
          renderQuickLinksSection();
        }
      } catch (err) {
        console.error('Error picking folder to pin:', err);
      }
    });
  }

  document.querySelectorAll('#analyzer-demo-table th[data-sort]').forEach((th) => {
    th.addEventListener('click', () => {
      const col = th.dataset.sort;
      if (demoSortColumn === col) demoSortAscending = !demoSortAscending;
      else { demoSortColumn = col; demoSortAscending = true; }
      renderDemoTable();
    });
  });

  const searchEl = document.querySelector('#analyzer-filter-search');
  const typeEl = document.querySelector('#analyzer-filter-type');
  const mapEl = document.querySelector('#analyzer-filter-map');
  const dateStartEl = document.querySelector('#analyzer-filter-date-start');
  const dateEndEl = document.querySelector('#analyzer-filter-date-end');
  const resetBtn = document.querySelector('#analyzer-filter-reset');

  searchEl?.addEventListener('input', (e) => { demoFilterQuery = e.target.value; renderDemoTable(); });
  typeEl?.addEventListener('change', (e) => { demoFilterType = e.target.value; renderDemoTable(); });
  mapEl?.addEventListener('input', (e) => { demoFilterMap = e.target.value; renderDemoTable(); });
  dateStartEl?.addEventListener('input', (e) => { demoFilterDateStart = e.target.value; renderDemoTable(); });
  dateEndEl?.addEventListener('input', (e) => { demoFilterDateEnd = e.target.value; renderDemoTable(); });
  resetBtn?.addEventListener('click', () => {
    demoFilterQuery = ''; demoFilterType = 'All'; demoFilterMap = ''; demoFilterDateStart = ''; demoFilterDateEnd = '';
    if (searchEl) searchEl.value = '';
    if (typeEl) typeEl.value = 'All';
    if (mapEl) mapEl.value = '';
    if (dateStartEl) dateStartEl.value = '';
    if (dateEndEl) dateEndEl.value = '';
    renderDemoTable();
  });

  initAnalyzerBrowserKeyboardNav();

  renderQuickLinksSection();
  renderExplorerTree();
  triggerLocalFoldersScan();

  (async () => {
    const startDir = (getPinnedFolders() || []).filter(isFolderPath)[0]
      || (getDemoFolderHistory() || []).filter(isFolderPath)[0]
      || (await defaultBrowseDir());
    if (startDir) setCurrentDir(startDir);
  })();
}

// ── Init / entry points ──────────────────────────────────────────────────────

// Registered once here (not per-load) to avoid the double-registration bug
// noted for render_status in ipc_bridge.js — analyzer_progress is throttled
// to ~30fps backend-side (Rust `analyze_demo_full`), so no further
// throttling is needed on the receiving end.
listen('analyzer_progress', (event) => {
  if (!analyzerLoadInProgress) return;
  const { processed, total } = event.payload || {};
  if (!total) return;
  const pct = Math.min(100, Math.round((processed / total) * 100));
  const container = document.querySelector('#analyzer-tab-content');
  setAnalyzerFileIndicator(STRINGS.ANALYZER.analyzingPct(pct));
  if (container) container.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.analyzingDemoPct(pct)}</p>`;
});

// Explorer sidebar drag-to-resize — width persists to settings.json via
// getAnalyzerExplorerWidth/setAnalyzerExplorerWidth (main.js), the same
// pattern every other layout/preference field in this app uses.
function initExplorerResize() {
  const handle = document.querySelector('#analyzer-explorer-resize-handle');
  const sidebar = document.querySelector('#analyzer-explorer-sidebar');
  if (!handle || !sidebar) return;

  // 260px matches the sidebar's original static width (pre-resize) — known
  // to render every row cleanly. Anything narrower wraps the Explorer
  // Settings checkbox label onto two lines and truncates folder names to
  // 3-4 characters.
  const MIN_WIDTH = 260;
  // Cap tracks the cap's actual purpose — never let the Demos/Analyzer main
  // column get squeezed below a usable width — rather than a window-relative
  // percentage, which produced a confusingly different max % depending on
  // whether the window was maximized, restored, or moved to another monitor.
  const MIN_MAIN_COLUMN_WIDTH = 500;
  sidebar.style.width = `${getAnalyzerExplorerWidth()}px`;

  let startX = 0;
  let startWidth = 0;

  function onPointerMove(e) {
    const maxWidth = Math.max(MIN_WIDTH, window.innerWidth - MIN_MAIN_COLUMN_WIDTH);
    const newWidth = Math.min(maxWidth, Math.max(MIN_WIDTH, startWidth + (e.clientX - startX)));
    sidebar.style.width = `${newWidth}px`;
  }

  function onPointerUp() {
    handle.classList.remove('resizing');
    document.removeEventListener('mousemove', onPointerMove);
    document.removeEventListener('mouseup', onPointerUp);
    setAnalyzerExplorerWidth(sidebar.getBoundingClientRect().width).catch(() => {});
  }

  handle.addEventListener('mousedown', (e) => {
    e.preventDefault();
    startX = e.clientX;
    startWidth = sidebar.getBoundingClientRect().width;
    handle.classList.add('resizing');
    document.addEventListener('mousemove', onPointerMove);
    document.addEventListener('mouseup', onPointerUp);
  });
}

export function initAnalyzerPane({
  getPinnedFolders: getPinnedFoldersCb,
  pinFolder: pinFolderCb,
  unpinFolder: unpinFolderCb,
  getDemoFolderHistory: getDemoFolderHistoryCb,
  recordDemoFolderVisit: recordDemoFolderVisitCb,
  forgetDemoFolderVisit: forgetDemoFolderVisitCb,
  getScanFoldersForDemos: getScanFoldersForDemosCb,
  setScanFoldersForDemos: setScanFoldersForDemosCb,
  getAnalyzerExplorerWidth: getAnalyzerExplorerWidthCb,
  setAnalyzerExplorerWidth: setAnalyzerExplorerWidthCb,
} = {}) {
  if (getPinnedFoldersCb) getPinnedFolders = getPinnedFoldersCb;
  if (pinFolderCb) pinFolder = pinFolderCb;
  if (unpinFolderCb) unpinFolder = unpinFolderCb;
  if (getDemoFolderHistoryCb) getDemoFolderHistory = getDemoFolderHistoryCb;
  if (recordDemoFolderVisitCb) recordDemoFolderVisit = recordDemoFolderVisitCb;
  if (forgetDemoFolderVisitCb) forgetDemoFolderVisit = forgetDemoFolderVisitCb;
  if (getScanFoldersForDemosCb) getScanFoldersForDemos = getScanFoldersForDemosCb;
  if (setScanFoldersForDemosCb) setScanFoldersForDemos = setScanFoldersForDemosCb;
  if (getAnalyzerExplorerWidthCb) getAnalyzerExplorerWidth = getAnalyzerExplorerWidthCb;
  if (setAnalyzerExplorerWidthCb) setAnalyzerExplorerWidth = setAnalyzerExplorerWidthCb;

  loadWeaponDisplayNames();
  initExplorerResize();

  const browseBtn = document.querySelector('#analyzer-browse-btn');
  if (browseBtn) {
    browseBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.DEMO_FILES_FILTER_NAME, extensions: ['dem'] }],
          title: STRINGS.ANALYZER.SELECT_DEMO_DIALOG_TITLE,
        });
        if (selected) {
          const path = Array.isArray(selected) ? selected[0] : selected;
          loadAnalyzerDemo(path);
        }
      } catch (err) {
        console.error('Error opening demo file dialog:', err);
      }
    });
  }

  document.querySelectorAll('.analyzer-subtab-btn').forEach((btn) => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.analyzer-subtab-btn').forEach((b) => b.classList.remove('active'));
      btn.classList.add('active');
      activeSubTab = btn.dataset.subtab;
      renderActiveTab();
    });
  });

  initAnalyzerBrowser();
}

// Entry point for cross-pane jumps (e.g. Workspace's "View Match Telemetry"
// button) — points the Explorer sidebar at the demo's own folder first, so
// the Demos table reliably highlights it instead of only coincidentally
// matching whatever folder the sidebar was already browsing.
export async function openAnalyzerDemo(path) {
  const parentDir = parentDirOf(path);
  if (parentDir && parentDir !== currentDir) {
    await setCurrentDir(parentDir);
  }
  await loadAnalyzerDemo(path);
}

async function loadAnalyzerDemo(path) {
  const container = document.querySelector('#analyzer-tab-content');
  if (container) container.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.ANALYZING_DEMO_ELLIPSIS}</p>`;
  setAnalyzerFileIndicator(STRINGS.ANALYZER.ANALYZING_ELLIPSIS);
  analyzerLoadInProgress = true;
  try {
    report = await analyzeDemoFull(path);
    highlightedPlayerId = null;
    selectedPlayerId = null;
    setAnalyzerFileIndicator(report.file_name);
    browserSelectedDemo = path;
    renderDemoTable();
    renderActiveTab();
  } catch (err) {
    if (container) {
      container.innerHTML = `<p class="analyzer-empty" style="color:#f44336;">${STRINGS.ANALYZER.analyzeFailed(esc(String(err)))}</p>`;
    }
    setAnalyzerFileIndicator('');
  } finally {
    analyzerLoadInProgress = false;
  }
}

function renderActiveTab() {
  const container = document.querySelector('#analyzer-tab-content');
  if (!container) return;
  if (!report) {
    container.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.EMPTY_PICK_DEMO_JS_FALLBACK}</p>`;
    return;
  }
  switch (activeSubTab) {
    case 'summary': renderSummaryTab(container); break;
    case 'scoreboard': renderScoreboardTab(container); break;
    case 'player-details': renderPlayerDetailsTab(container); break;
    case 'team-details': renderTeamDetailsTab(container); break;
    case 'timeline': renderTimelineTab(container); break;
    case 'rounds': renderRoundsTab(container); break;
    case 'chat': renderChatTab(container); break;
  }
}

// ── 1. Summary ────────────────────────────────────────────────────────────────

function renderSummaryTab(container) {
  const r = report;
  const di = r.demo_info;
  const st = r.state;

  const section = (title, rows) => `
    <div class="analyzer-section">
      <h4 class="analyzer-section-title">${esc(title)}</h4>
      <table class="analyzer-kv-table"><tbody>
        ${rows.map(([label, val]) => `<tr><td class="kv-label">${esc(label)}</td><td class="kv-value">${val}</td></tr>`).join('')}
      </tbody></table>
    </div>`;

  const createdDate = new Date(r.file_created_unix_secs * 1000);
  const createdStr = isFinite(createdDate.getTime()) && r.file_created_unix_secs > 0 ? createdDate.toLocaleString() : STRINGS.ANALYZER.EMPTY_DASH;

  const gameModMap = { dod: STRINGS.ANALYZER.GAME_MOD_DOD, cstrike: STRINGS.ANALYZER.GAME_MOD_CS, valve: STRINGS.ANALYZER.GAME_MOD_HL };
  const gameMod = gameModMap[di.game_directory] || di.game_directory;

  const recordedBy = (() => {
    if (di.demo_type === 'HLTV') return st.hltv_name || STRINGS.ANALYZER.RECORDED_BY_HLTV_DEFAULT;
    const p = (st.players || []).find((p) => playerClientId(p) === st.pov_player_index);
    return p ? p.name : STRINGS.ANALYZER.RECORDED_BY_UNKNOWN;
  })();

  const matchType = (() => {
    if (!st.clan_match_detected) return STRINGS.ANALYZER.MATCH_TYPE_PUBLIC;
    const hasCompletedRound = (st.rounds || []).some((r) => !!r.Completed);
    if (!st.match_start_witnessed && !hasCompletedRound) return STRINGS.ANALYZER.MATCH_TYPE_PREGAME;
    if (st.started_late || st.ended_early) return STRINGS.ANALYZER.MATCH_TYPE_INCOMPLETE;
    return STRINGS.ANALYZER.MATCH_TYPE_FULL;
  })();

  const demoDuration = formatDuration(durSecs(st.current_time && st.current_time.viewdemo_offset));

  const matchDuration = (() => {
    const rounds = st.rounds || [];
    if (rounds.length === 0) return STRINGS.ANALYZER.EMPTY_DASH;
    const first = rounds[0];
    const last = rounds[rounds.length - 1];
    const startTime = first.Active ? first.Active.start_time : first.Completed.start_time;
    const endTime = last.Completed ? last.Completed.end_time : st.current_time;
    return formatDuration(Math.max(0, durSecs(endTime.viewdemo_offset) - durSecs(startTime.viewdemo_offset)));
  })();

  container.innerHTML = `
    <div class="analyzer-summary-grid">
      ${section(STRINGS.ANALYZER.FILE_INFO_SECTION, [
        [STRINGS.ANALYZER.FILE_NAME_LABEL, esc(r.file_name)],
        [STRINGS.ANALYZER.FILE_PATH_LABEL, esc(r.file_dir)],
        [STRINGS.ANALYZER.FILE_SIZE_LABEL, STRINGS.ANALYZER.megabytesLabel(r.file_size_mb.toFixed(2))],
        [STRINGS.ANALYZER.FILE_CREATED_LABEL, esc(createdStr)],
      ])}
      ${section(STRINGS.ANALYZER.GAME_DETAILS_SECTION, [
        [STRINGS.ANALYZER.GAME_MOD_LABEL, esc(gameMod)],
        [STRINGS.ANALYZER.MAP_NAME_LABEL, esc(di.map_name)],
        [STRINGS.ANALYZER.MAP_CHECKSUM_LABEL, String(di.map_checksum)],
      ])}
      ${section(STRINGS.ANALYZER.SERVER_INFO_SECTION, [
        [STRINGS.ANALYZER.SERVER_NAME_LABEL, esc(st.server_name || STRINGS.ANALYZER.EMPTY_DASH)],
        [STRINGS.ANALYZER.SERVER_ADDRESS_LABEL, esc(st.server_address || STRINGS.ANALYZER.EMPTY_DASH)],
      ])}
      ${section(STRINGS.ANALYZER.DEMO_MATCH_DETAILS_SECTION, [
        [STRINGS.ANALYZER.RECORDED_BY_LABEL, esc(recordedBy)],
        [STRINGS.ANALYZER.DEMO_TYPE_LABEL, esc(di.demo_type)],
        [STRINGS.ANALYZER.MATCH_TYPE_LABEL, esc(matchType)],
        [STRINGS.ANALYZER.DEMO_DURATION_LABEL, demoDuration],
        [STRINGS.ANALYZER.MATCH_DURATION_LABEL, matchDuration],
      ])}
      ${section(STRINGS.ANALYZER.TECH_SPECS_SECTION, [
        [STRINGS.ANALYZER.DEMO_PROTOCOL_LABEL, String(di.demo_protocol)],
        [STRINGS.ANALYZER.NETWORK_PROTOCOL_LABEL, String(di.network_protocol)],
      ])}
    </div>`;
}

// ── 2. Scoreboard ────────────────────────────────────────────────────────────

function buildScoreboardGroups() {
  const players = report.state.players || [];
  const groups = { allies: [], axis: [], spec: [], unassigned: [] };
  players.forEach((p) => {
    if (p.team === 'Allies' || p.team === 'British') groups.allies.push(p);
    else if (p.team === 'Axis') groups.axis.push(p);
    else if (p.team === 'Spectators') groups.spec.push(p);
    else groups.unassigned.push(p);
  });
  // Score DESC, Kills DESC, Deaths ASC, Name ASC, id ASC — matches dev's ScoreboardCache sort key.
  const sortFn = (a, b) => {
    if (b.stats[0] !== a.stats[0]) return b.stats[0] - a.stats[0];
    if (b.stats[1] !== a.stats[1]) return b.stats[1] - a.stats[1];
    if (a.stats[2] !== b.stats[2]) return a.stats[2] - b.stats[2];
    const na = a.name.toLowerCase(), nb = b.name.toLowerCase();
    if (na !== nb) return na < nb ? -1 : 1;
    return (a.id || '') < (b.id || '') ? -1 : 1;
  };
  Object.values(groups).forEach((g) => g.sort(sortFn));
  const totals = (arr) => arr.reduce((acc, p) => [acc[0] + p.stats[0], acc[1] + p.stats[1], acc[2] + p.stats[2]], [0, 0, 0]);
  return {
    groups,
    totals: { allies: totals(groups.allies), axis: totals(groups.axis), spec: totals(groups.spec), unassigned: totals(groups.unassigned) },
  };
}

function renderScoreboardTab(container) {
  const st = report.state;
  const { groups, totals } = buildScoreboardGroups();
  const alliesLabel = teamLabel('Allies', st.allies_are_british);
  const alliesColor = teamColor(st.allies_are_british ? 'British' : 'Allies');

  const alliesScore = getTeamScore((t) => t === 'Allies' || t === 'British');
  const axisScore = getTeamScore((t) => t === 'Axis');
  const cmp = STRINGS.ANALYZER.compareGlyph(alliesScore, axisScore);

  let banner = '';
  if (st.started_late || st.ended_early) {
    const fmt = (d) => (d ? formatMMSS(durSecs(d)) : STRINGS.ANALYZER.CLOCK_UNKNOWN);
    let msg;
    if (st.started_late && st.ended_early) msg = STRINGS.ANALYZER.partialRecordingBoth(fmt(st.first_time_left));
    else if (st.started_late) msg = STRINGS.ANALYZER.partialRecordingStartedLate(fmt(st.first_time_left));
    else msg = STRINGS.ANALYZER.partialRecordingEndedEarly(fmt(st.last_time_left));
    banner = `<div class="analyzer-warning-banner">${esc(msg)}</div>`;
  }

  const groupBlock = (label, color, players, score, tot, key) => {
    if (players.length === 0 && (key === 'spec' || key === 'unassigned')) return '';
    const rows = players.map((p) => renderScoreboardRow(p, color)).join('');
    return `
      <tr class="scoreboard-group-header" style="color:${color};">
        <td colspan="2">${esc(STRINGS.ANALYZER.groupLabelWithCount(label, players.length))}</td>
        <td style="text-align:right;">${score}</td>
        <td style="text-align:right;">${tot[1]}</td>
        <td style="text-align:right;">${tot[2]}</td>
      </tr>
      ${rows}
      <tr class="scoreboard-spacer-row"><td colspan="5"></td></tr>`;
  };

  container.innerHTML = `
    <h3 class="analyzer-heading">${esc(STRINGS.ANALYZER.scoreboardHeading(alliesLabel, alliesScore, cmp, axisScore))}</h3>
    ${banner}
    <div class="table-wrapper">
      <table class="analyzer-table">
        <thead><tr><th>${STRINGS.ANALYZER.COL_NAME}</th><th>${STRINGS.ANALYZER.COL_CLASS}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_SCORE}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_KILLS}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_DEATHS}</th></tr></thead>
        <tbody>
          ${groupBlock(alliesLabel, alliesColor, groups.allies, alliesScore, totals.allies, 'allies')}
          ${groupBlock(STRINGS.ANALYZER.AXIS_LABEL, teamColor('Axis'), groups.axis, axisScore, totals.axis, 'axis')}
          ${groupBlock(STRINGS.ANALYZER.SPECTATORS_LABEL, teamColor('Spectators'), groups.spec, totals.spec[0], totals.spec, 'spec')}
          ${groupBlock(STRINGS.ANALYZER.UNASSIGNED_LABEL, teamColor('Unassigned'), groups.unassigned, totals.unassigned[0], totals.unassigned, 'unassigned')}
        </tbody>
      </table>
    </div>`;

  container.querySelectorAll('tr.scoreboard-player-row').forEach((tr) => {
    tr.addEventListener('click', () => {
      const id = tr.dataset.playerId;
      highlightedPlayerId = highlightedPlayerId === id ? null : id;
      // Keep the Player Details dropdown in sync so selection works both
      // ways (dropdown -> scoreboard already did via renderPlayerDetailsTab).
      if (highlightedPlayerId) selectedPlayerId = highlightedPlayerId;
      renderScoreboardTab(container);
    });
  });
}

function renderScoreboardRow(p, color) {
  const isSelected = highlightedPlayerId === p.id;
  const isPov = report.state.pov_player_index !== null && report.state.pov_player_index !== undefined && playerClientId(p) === report.state.pov_player_index;
  const povBadge = isPov ? ' 🎥' : '';
  const reconnBadge = p.has_reconnected ? ` <span title="${STRINGS.ANALYZER.RECONNECTED_TITLE}" style="color:#ffb74d;">🔄</span>` : '';
  const preDemoBadge = p.has_pre_demo_activity ? ` <span title="${STRINGS.ANALYZER.PRE_DEMO_ACTIVITY_TITLE}" style="color:#ffb74d;">*</span>` : '';
  const rowColor = isSelected ? '#ffffff' : color;
  return `
    <tr class="scoreboard-player-row" data-player-id="${esc(p.id)}" style="color:${rowColor};cursor:pointer;${isSelected ? 'background:rgba(255,255,255,0.08);' : ''}">
      <td>${esc(p.name)}${povBadge}${reconnBadge}${preDemoBadge}</td>
      <td>${esc(p.class || STRINGS.ANALYZER.UNKNOWN_CLASS)}</td>
      <td style="text-align:right;">${p.stats[0]}</td>
      <td style="text-align:right;">${p.stats[1]}</td>
      <td style="text-align:right;">${p.stats[2]}</td>
    </tr>`;
}

// ── 3. Player Details ────────────────────────────────────────────────────────

function renderPlayerDetailsTab(container) {
  const players = (report.state.players || []).slice().sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  if (players.length === 0) {
    container.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.NO_PLAYERS_FOUND}</p>`;
    return;
  }
  const previousSelectedId = selectedPlayerId;
  let selectedId = selectedPlayerId;
  if (!selectedId || !players.find((p) => p.id === selectedId)) {
    selectedId = highlightedPlayerId && players.find((p) => p.id === highlightedPlayerId) ? highlightedPlayerId : players[0].id;
  }
  selectedPlayerId = selectedId;
  // Any change in the effective selected player (dropdown, scoreboard click,
  // or a kill-streak victim jump) clears the per-player weapon filter state.
  if (selectedId !== previousSelectedId) disabledWeapons = new Set();

  const options = players.map((p) => `<option value="${esc(p.id)}" ${p.id === selectedId ? 'selected' : ''}>${esc(p.name)}</option>`).join('');

  container.innerHTML = `
    <div class="analyzer-toolbar">
      <label>${STRINGS.ANALYZER.PLAYER_LABEL}</label>
      <select id="player-details-select">${options}</select>
    </div>
    <div id="player-details-body"></div>`;

  container.querySelector('#player-details-select').addEventListener('change', (e) => {
    selectedPlayerId = e.target.value;
    highlightedPlayerId = selectedPlayerId;
    disabledWeapons = new Set();
    renderPlayerDetailsBody(container, players.find((p) => p.id === selectedPlayerId));
  });

  renderPlayerDetailsBody(container, players.find((p) => p.id === selectedId));
}

function renderPlayerDetailsBody(tabContainer, p) {
  const body = tabContainer.querySelector('#player-details-body');
  if (!body) return;
  if (!p) { body.innerHTML = ''; return; }

  const color = teamColor(p.team);
  const kd = p.stats[2] > 0 ? p.stats[1] / p.stats[2] : p.stats[1];
  const lifespans = mortalityLifespans(p.mortality);
  const avgLife = lifespans.length ? lifespans.reduce((a, b) => a + b, 0) / lifespans.length : 0;
  const minLife = lifespans.length ? Math.min(...lifespans) : 0;
  const maxLife = lifespans.length ? Math.max(...lifespans) : 0;

  const connected = isConnected(p);
  const clientId = playerClientId(p);
  const steamId = steamIdDisplay(p.id);

  const weaponRows = Object.entries(p.weapon_breakdown || {}).sort((a, b) => b[1][0] - a[1][0] || a[0].localeCompare(b[0]));
  const totalKills = weaponRows.reduce((s, [, v]) => s + v[0], 0) || 1;

  body.innerHTML = `
    <div class="analyzer-hero-card" style="border-color:${color}66;">
      <div class="flex-between">
        <div>
          <div class="analyzer-hero-name">${esc(p.name)}</div>
          <div class="analyzer-hero-sub" style="color:${color};">${esc((teamLabel(p.team, report.state.allies_are_british) || STRINGS.ANALYZER.UNASSIGNED_LABEL).toUpperCase())}${p.class ? ` &nbsp;|&nbsp; ${esc(p.class.toUpperCase())}` : ''}</div>
        </div>
        <div class="analyzer-hero-links">
          ${/^\d{15,20}$/.test(p.id) ? `<a href="https://www.legit-proof.com/search?q=${esc(steamId)}" target="_blank" rel="noopener" title="${STRINGS.ANALYZER.LEGIT_PROOF_LINK_TITLE}">${STRINGS.ANALYZER.LEGIT_PROOF_TEXT}</a> / <a href="https://steamcommunity.com/profiles/${esc(p.id)}" target="_blank" rel="noopener">${STRINGS.ANALYZER.STEAM_PROFILE_TEXT}</a>` : `<span class="text-muted">${STRINGS.ANALYZER.NO_STEAM_ID}</span>`}
        </div>
      </div>
      <div class="analyzer-hero-status">
        <span>${STRINGS.ANALYZER.STEAM_ID_LABEL}<code>${esc(steamId)}</code></span>
        <span style="color:${connected ? '#4caf50' : '#888'};">${connected ? STRINGS.ANALYZER.connectedSlot(clientId) : STRINGS.ANALYZER.DISCONNECTED}</span>
        ${p.has_reconnected ? `<span style="color:#ffb74d;">${STRINGS.ANALYZER.RECONNECTED_MID_DEMO}</span>` : ''}
        ${p.has_pre_demo_activity ? `<span style="color:#ffb74d;">${STRINGS.ANALYZER.PRE_EXISTING_STATS}</span>` : ''}
      </div>
    </div>

    <div class="analyzer-stat-cards">
      <div class="analyzer-stat-card"><div class="stat-title">${STRINGS.ANALYZER.MATCH_SCORE_TITLE}</div><div class="stat-value">${p.stats[0]}</div></div>
      <div class="analyzer-stat-card"><div class="stat-title">${STRINGS.ANALYZER.KILLS_TITLE}</div><div class="stat-value">${p.stats[1]}</div></div>
      <div class="analyzer-stat-card"><div class="stat-title">${STRINGS.ANALYZER.DEATHS_TITLE}</div><div class="stat-value">${p.stats[2]}</div><div class="stat-badge" style="color:${kd >= 1 ? '#22c55e' : '#ef4444'};">${kd.toFixed(2)} ${STRINGS.ANALYZER.KD_BADGE_LABEL}</div></div>
      <div class="analyzer-stat-card"><div class="stat-title">${STRINGS.ANALYZER.AVG_LIFESPAN_TITLE}</div><div class="stat-value">${STRINGS.ANALYZER.secondsSuffix(avgLife.toFixed(1))}</div><div class="stat-badge text-muted">${STRINGS.ANALYZER.minMaxBadge(minLife.toFixed(0), maxLife.toFixed(0))}</div></div>
    </div>

    <div class="analyzer-stacked-sections">
      <div>
        <h4 class="analyzer-section-title">${STRINGS.ANALYZER.WEAPON_BREAKDOWN_TITLE}</h4>
        <div class="table-wrapper" style="max-height:320px;">
          <table class="analyzer-table">
            <thead><tr><th>${STRINGS.ANALYZER.COL_WEAPON}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_KILLS}</th><th>${STRINGS.ANALYZER.COL_PCT_TOTAL}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_TEAM_KILLS}</th></tr></thead>
            <tbody>
              ${weaponRows.map(([w, [k, tk]]) => `
                <tr>
                  <td>${esc(weaponName(w))}</td>
                  <td style="text-align:right;">${k}</td>
                  <td><div class="analyzer-progress"><div class="analyzer-progress-fill" style="width:${(k / totalKills * 100).toFixed(1)}%;"></div></div><span class="analyzer-progress-label">${(k / totalKills * 100).toFixed(1)}%</span></td>
                  <td style="text-align:right;">${tk}</td>
                </tr>`).join('') || `<tr><td colspan="4" class="table-empty">${STRINGS.ANALYZER.NO_WEAPON_DATA}</td></tr>`}
            </tbody>
          </table>
        </div>
      </div>
      <div>
        <h4 class="analyzer-section-title">${STRINGS.ANALYZER.KILL_STREAKS_TITLE}</h4>
        <div id="killstreak-weapon-filters" class="analyzer-weapon-filters"></div>
        <div id="killstreak-table-wrap"></div>
      </div>
    </div>`;

  renderKillStreaksSection(p);
}

function renderKillStreaksSection(p) {
  const filtersEl = document.querySelector('#killstreak-weapon-filters');
  const tableEl = document.querySelector('#killstreak-table-wrap');
  if (!filtersEl || !tableEl) return;

  const allWeapons = new Set();
  (p.kill_streaks || []).forEach((s) => (s.kills || []).forEach((k) => allWeapons.add(k[1])));

  renderWeaponFilterCategories(filtersEl, allWeapons, p);
  renderKillStreaksTable(tableEl, p);
}

function renderWeaponFilterCategories(el, allWeapons, p) {
  const categorized = new Set(WEAPON_CATEGORIES.flatMap(([, ws]) => ws));
  const other = [...allWeapons].filter((w) => !categorized.has(w)).sort((a, b) => weaponName(a).localeCompare(weaponName(b)));
  const groups = [...WEAPON_CATEGORIES, ...(other.length ? [[STRINGS.ANALYZER.WEAPON_CATEGORY_OTHER, other]] : [])]
    .map(([label, ws]) => [label, ws.filter((w) => allWeapons.has(w))])
    .filter(([, ws]) => ws.length > 0);

  if (groups.length === 0) { el.innerHTML = ''; return; }

  el.innerHTML = groups.map(([label, ws]) => {
    const allEnabled = ws.every((w) => !disabledWeapons.has(w));
    return `
      <div class="weapon-filter-group">
        <button type="button" class="weapon-filter-toggle-all" title="${allEnabled ? STRINGS.ANALYZER.HIDE_ALL_TITLE : STRINGS.ANALYZER.SHOW_ALL_TITLE}" data-weapons="${esc(ws.join(','))}">[${esc(label)}]</button>
        ${ws.map((w) => `<label><input type="checkbox" class="weapon-filter-cb" data-weapon="${esc(w)}" ${!disabledWeapons.has(w) ? 'checked' : ''} /> ${esc(weaponName(w))}</label>`).join('')}
      </div>`;
  }).join('');

  el.querySelectorAll('.weapon-filter-toggle-all').forEach((btn) => {
    btn.addEventListener('click', () => {
      const ws = btn.dataset.weapons.split(',');
      const allEnabled = ws.every((w) => !disabledWeapons.has(w));
      ws.forEach((w) => (allEnabled ? disabledWeapons.add(w) : disabledWeapons.delete(w)));
      renderKillStreaksSection(p);
    });
  });
  el.querySelectorAll('.weapon-filter-cb').forEach((cb) => {
    cb.addEventListener('change', () => {
      const w = cb.dataset.weapon;
      if (cb.checked) disabledWeapons.delete(w); else disabledWeapons.add(w);
      renderKillStreaksSection(p);
    });
  });
}

function renderKillStreaksTable(container, p) {
  const streaks = (p.kill_streaks || [])
    .map((s) => ({ ...s, kills: (s.kills || []).filter((k) => !disabledWeapons.has(k[1])) }))
    .filter((s) => s.kills.length > 0);
  if (streaks.length === 0) {
    container.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.NO_KILL_STREAKS}</p>`;
    return;
  }

  const rows = streaks.map((s, idx) => {
    const kills = s.kills;
    const startSecs = durSecs(kills[0][0].viewdemo_offset);
    const endSecs = durSecs(kills[kills.length - 1][0].viewdemo_offset);
    const durationSecs = Math.max(0, endSecs - startSecs);
    const weaponSummary = groupConsecutiveWeapons(kills.map((k) => weaponName(k[1])));

    const sub = kills.map((k, i) => {
      const victim = (report.state.players || []).find((pl) => pl.id === k[2]);
      const victimName = victim ? victim.name : k[2];
      const victimColor = victim ? teamColor(victim.team) : '#7ec8e3';
      const delta = i === 0 ? STRINGS.ANALYZER.EMPTY_DASH : `+${formatMMSS(durSecs(k[0].viewdemo_offset) - durSecs(kills[i - 1][0].viewdemo_offset))}`;
      return `<tr class="killstreak-subrow"><td></td><td class="text-muted">${esc(delta)}</td><td colspan="2" style="font-size:0.85em;">${esc(weaponName(k[1]))} &#9876; <span class="killstreak-victim" data-victim-id="${esc(k[2])}" style="color:${victimColor};cursor:pointer;">${esc(victimName)}</span></td></tr>`;
    }).join('');

    return `
      <tr class="killstreak-row">
        <td>${idx + 1}</td>
        <td>${kills.length}</td>
        <td>${formatGameTime(startSecs)}</td>
        <td>${STRINGS.ANALYZER.secondsSuffix(durationSecs.toFixed(1))}</td>
      </tr>
      <tr><td></td><td colspan="3" style="font-size:0.85em;color:#aaa;">${esc(weaponSummary)}</td></tr>
      ${sub}`;
  }).join('');

  container.innerHTML = `
    <div class="table-wrapper" style="max-height:320px;">
      <table class="analyzer-table">
        <thead><tr><th>${STRINGS.ANALYZER.COL_WAVE}</th><th>${STRINGS.ANALYZER.COL_KILLS}</th><th>${STRINGS.ANALYZER.COL_TIME}</th><th>${STRINGS.ANALYZER.COL_DURATION}</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </div>`;

  container.querySelectorAll('.killstreak-victim').forEach((el) => {
    el.addEventListener('click', () => {
      const vid = el.dataset.victimId;
      if (!(report.state.players || []).find((pl) => pl.id === vid)) return;
      selectedPlayerId = vid;
      highlightedPlayerId = vid;
      renderPlayerDetailsTab(document.querySelector('#analyzer-tab-content'));
    });
  });
}

// ── 4. Team Details ──────────────────────────────────────────────────────────

function renderTeamDetailsTab(container) {
  const st = report.state;
  const players = st.players || [];
  const alliesLabel = teamLabel('Allies', st.allies_are_british);
  const alliesColor = teamColor(st.allies_are_british ? 'British' : 'Allies');
  const axisColor = teamColor('Axis');

  const alliesPlayers = players.filter((p) => p.team === 'Allies' || p.team === 'British');
  const axisPlayers = players.filter((p) => p.team === 'Axis');

  const sumStat = (arr, idx) => arr.reduce((s, p) => s + p.stats[idx], 0);
  const alliesScore = getTeamScore((t) => t === 'Allies' || t === 'British');
  const axisScore = getTeamScore((t) => t === 'Axis');
  const alliesKills = sumStat(alliesPlayers, 1), axisKills = sumStat(axisPlayers, 1);
  const alliesDeaths = sumStat(alliesPlayers, 2), axisDeaths = sumStat(axisPlayers, 2);
  const kdOrKills = (k, d) => (d > 0 ? (k / d).toFixed(2) : String(k));

  const overviewRow = (label, allies, axis) => `<tr><td class="kv-label">${esc(label)}</td><td style="text-align:right;">${allies}</td><td style="text-align:right;">${axis}</td></tr>`;

  container.innerHTML = `
    <h3 class="analyzer-heading">${STRINGS.ANALYZER.TEAM_DETAILS_HEADING}</h3>
    <h4 class="analyzer-section-title">${STRINGS.ANALYZER.MATCH_OVERVIEW_TITLE}</h4>
    <table class="analyzer-kv-table" style="max-width:520px;">
      <thead><tr><th></th><th style="text-align:right;color:${alliesColor};">${esc(alliesLabel)}</th><th style="text-align:right;color:${axisColor};">${STRINGS.ANALYZER.AXIS_LABEL}</th></tr></thead>
      <tbody>
        ${overviewRow(STRINGS.ANALYZER.ROUND_SCORE_LABEL, alliesScore, axisScore)}
        ${overviewRow(STRINGS.ANALYZER.TOTAL_KILLS_LABEL, alliesKills, axisKills)}
        ${overviewRow(STRINGS.ANALYZER.TOTAL_DEATHS_LABEL, alliesDeaths, axisDeaths)}
        ${overviewRow(STRINGS.ANALYZER.TEAM_KD_LABEL, kdOrKills(alliesKills, alliesDeaths), kdOrKills(axisKills, axisDeaths))}
        ${overviewRow(STRINGS.ANALYZER.ACTIVE_PLAYERS_LABEL, alliesPlayers.length, axisPlayers.length)}
      </tbody>
    </table>

    <h4 class="analyzer-section-title" style="margin-top:16px;">${STRINGS.ANALYZER.TEAM_WEAPON_PERFORMANCE_TITLE}</h4>
    ${renderTeamWeaponSections(players, st.allies_are_british)}`;
}

// Dev groups weapon breakdowns by the player's raw `team` value (Allies vs.
// British kept separate, not merged) so a mid-match side-label switch shows
// as two sections instead of silently combining into one.
function renderTeamWeaponSections(players, alliesAreBritish) {
  const alliesPlayers = players.filter((p) => p.team === 'Allies');
  const britishPlayers = players.filter((p) => p.team === 'British');
  const axisPlayers = players.filter((p) => p.team === 'Axis');

  const alliesColor = teamColor('Allies');
  const britishColor = teamColor('British');
  const axisColor = teamColor('Axis');

  const sections = [];
  // Matches dev exactly: show the Allies/US section unless there's British
  // data present with no pure-Allies data at all.
  if (alliesPlayers.length > 0 || britishPlayers.length === 0) {
    const label = alliesAreBritish ? STRINGS.ANALYZER.ALLIES_US_LABEL : STRINGS.ANALYZER.ALLIES_LABEL;
    sections.push(`<details open class="analyzer-collapsible"><summary style="color:${alliesColor};">${esc(label)}</summary>${weaponBreakdownTable(alliesPlayers)}</details>`);
  }
  if (britishPlayers.length > 0) {
    sections.push(`<details open class="analyzer-collapsible"><summary style="color:${britishColor};">${STRINGS.ANALYZER.BRITISH_LABEL}</summary>${weaponBreakdownTable(britishPlayers)}</details>`);
  }
  sections.push(`<details open class="analyzer-collapsible"><summary style="color:${axisColor};">${STRINGS.ANALYZER.AXIS_LABEL}</summary>${weaponBreakdownTable(axisPlayers)}</details>`);
  return sections.join('');
}

function weaponBreakdownTable(arr) {
  const agg = {};
  arr.forEach((p) => Object.entries(p.weapon_breakdown || {}).forEach(([w, [k, tk]]) => {
    if (!agg[w]) agg[w] = [0, 0];
    agg[w][0] += k; agg[w][1] += tk;
  }));
  const rows = Object.entries(agg).sort((a, b) => b[1][0] - a[1][0] || a[0].localeCompare(b[0]));
  if (rows.length === 0) return `<p class="analyzer-empty">${STRINGS.ANALYZER.NO_WEAPON_DATA}</p>`;
  const totalKills = rows.reduce((s, [, v]) => s + v[0], 0) || 1;
  const totalTk = rows.reduce((s, [, v]) => s + v[1], 0) || 1;
  return `
    <div class="table-wrapper">
      <table class="analyzer-table">
        <thead><tr><th>${STRINGS.ANALYZER.COL_WEAPON}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_KILLS}</th><th>${STRINGS.ANALYZER.COL_PCT_TOTAL}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_TEAM_KILLS}</th><th>${STRINGS.ANALYZER.COL_PCT_TOTAL}</th></tr></thead>
        <tbody>
          ${rows.map(([w, [k, tk]]) => `
            <tr>
              <td>${esc(weaponName(w))}</td>
              <td style="text-align:right;">${k}</td>
              <td><div class="analyzer-progress"><div class="analyzer-progress-fill" style="width:${(k / totalKills * 100).toFixed(1)}%;"></div></div><span class="analyzer-progress-label">${(k / totalKills * 100).toFixed(1)}%</span></td>
              <td style="text-align:right;">${tk}</td>
              <td><div class="analyzer-progress"><div class="analyzer-progress-fill" style="width:${(tk / totalTk * 100).toFixed(1)}%;background:#e57373;"></div></div><span class="analyzer-progress-label">${(tk / totalTk * 100).toFixed(1)}%</span></td>
            </tr>`).join('')}
        </tbody>
      </table>
    </div>`;
}

// ── 5. Timeline ───────────────────────────────────────────────────────────────

function renderTimelineTab(container) {
  const st = report.state;
  const alliesLabel = teamLabel('Allies', st.allies_are_british);
  const alliesColor = teamColor(st.allies_are_british ? 'British' : 'Allies');
  const axisColor = teamColor('Axis');

  container.innerHTML = `
    <h3 class="analyzer-heading">${STRINGS.ANALYZER.TEAM_SCORE_TIMELINE_TITLE}</h3>
    <div class="analyzer-timeline-legend">
      <span><span class="legend-swatch" style="background:${alliesColor};"></span>${esc(alliesLabel)}</span>
      <span><span class="legend-swatch" style="background:${axisColor};"></span>${STRINGS.ANALYZER.AXIS_LABEL}</span>
    </div>
    <div style="position:relative;">
      <canvas id="analyzer-timeline-canvas" style="width:100%;height:340px;background:#121212;border:1px solid #333;border-radius:2px;"></canvas>
      <div id="analyzer-timeline-tooltip" class="analyzer-timeline-tooltip" style="display:none;"></div>
    </div>`;

  const timeline = (st.team_scores && st.team_scores.timeline) || [];
  const alliesSeries = timeline.filter(([, t]) => t === 'Allies' || t === 'British').map(([time, , score]) => [durSecs(time.viewdemo_offset), score]);
  const axisSeries = timeline.filter(([, t]) => t === 'Axis').map(([time, , score]) => [durSecs(time.viewdemo_offset), score]);

  const canvas = container.querySelector('#analyzer-timeline-canvas');
  const points = drawTimelineChart(canvas, alliesSeries, axisSeries, alliesColor, axisColor, alliesLabel, STRINGS.ANALYZER.AXIS_LABEL);
  initTimelineTooltip(canvas, container.querySelector('#analyzer-timeline-tooltip'), points);
}

function drawTimelineChart(canvas, seriesA, seriesB, colorA, colorB, labelA, labelB) {
  if (!canvas) return [];
  const width = canvas.clientWidth || 600;
  const height = canvas.clientHeight || 340;
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = '#121212';
  ctx.fillRect(0, 0, width, height);

  let allPoints = [...seriesA, ...seriesB];
  if (allPoints.length === 0) {
    ctx.fillStyle = '#666';
    ctx.font = '12px sans-serif';
    ctx.textAlign = 'center';
    ctx.fillText(STRINGS.ANALYZER.NO_TEAM_SCORE_EVENTS, width / 2, height / 2);
    return [];
  }

  // Rebase so the earliest plotted point reads as 0:00. For a clan match this
  // is already the moment the real match went live -- clan-match detection
  // clears team_scores (and kill streaks, rounds, etc.) at that point, so
  // nothing from warmup ever reaches this data. Without the rebase, that
  // point's own absolute demo-clock offset (which can be many minutes in)
  // left most of the chart blank instead of using the space for real data.
  const rebaseBy = Math.min(...allPoints.map((p) => p[0]));
  seriesA = seriesA.map(([x, y]) => [x - rebaseBy, y]);
  seriesB = seriesB.map(([x, y]) => [x - rebaseBy, y]);
  allPoints = [...seriesA, ...seriesB];

  const padding = 36;
  const maxX = Math.max(...allPoints.map((p) => p[0]), 1);
  const maxY = Math.max(...allPoints.map((p) => p[1]), 1);
  const minY = Math.min(0, ...allPoints.map((p) => p[1]));

  const xAt = (x) => padding + (x / maxX) * (width - padding * 2);
  const yAt = (y) => height - padding - ((y - minY) / (maxY - minY || 1)) * (height - padding * 2);

  ctx.strokeStyle = '#333';
  ctx.beginPath();
  ctx.moveTo(padding, height - padding);
  ctx.lineTo(width - padding, height - padding);
  ctx.moveTo(padding, padding);
  ctx.lineTo(padding, height - padding);
  ctx.stroke();

  ctx.fillStyle = '#888';
  ctx.font = '10px monospace';
  ctx.textAlign = 'left';
  ctx.fillText(STRINGS.ANALYZER.TIMELINE_START_LABEL, padding - 4, height - padding + 14);
  ctx.textAlign = 'right';
  ctx.fillText(formatMMSS(maxX), width - padding, height - padding + 14);
  ctx.fillText(String(maxY), padding - 6, padding + 4);

  const hitPoints = [];
  const drawSeries = (series, color, label) => {
    if (series.length === 0) return;
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.beginPath();
    series.forEach(([x, y], i) => {
      const px = xAt(x), py = yAt(y);
      if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py);
    });
    ctx.stroke();
    ctx.fillStyle = color;
    series.forEach(([x, y]) => {
      const px = xAt(x), py = yAt(y);
      ctx.beginPath();
      ctx.arc(px, py, 2.5, 0, Math.PI * 2);
      ctx.fill();
      hitPoints.push({ px, py, x, y, rawX: x + rebaseBy, color, label });
    });
  };
  drawSeries(seriesA, colorA, labelA);
  drawSeries(seriesB, colorB, labelB);
  return hitPoints;
}

// Dev's egui_plot had a built-in hover tooltip (label_formatter); the canvas
// reimplementation needs its own nearest-point hit test.
function initTimelineTooltip(canvas, tooltip, points) {
  if (!canvas || !tooltip || points.length === 0) return;
  const HIT_RADIUS_PX = 14;
  canvas.addEventListener('mousemove', (e) => {
    const rect = canvas.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    let nearest = null;
    let nearestDist = HIT_RADIUS_PX;
    for (const p of points) {
      const d = Math.hypot(p.px - mx, p.py - my);
      if (d < nearestDist) { nearest = p; nearestDist = d; }
    }
    if (nearest) {
      tooltip.style.display = 'block';
      tooltip.style.left = `${Math.min(nearest.px + 12, canvas.clientWidth - 170)}px`;
      tooltip.style.top = `${Math.max(nearest.py - 46, 0)}px`;
      tooltip.innerHTML = `<strong>${esc(STRINGS.ANALYZER.TIMELINE_TOOLTIP_ELAPSED_LABEL)}</strong> ${esc(formatMMSS(nearest.x))}<br>`
        + `<strong>${esc(STRINGS.ANALYZER.TIMELINE_TOOLTIP_TIMESTAMP_LABEL)}</strong> ${esc(formatGameTime(nearest.rawX))}<br>`
        + `<span style="color:${nearest.color};"><strong>${esc(nearest.label)}:</strong> ${nearest.y}</span>`;
    } else {
      tooltip.style.display = 'none';
    }
  });
  canvas.addEventListener('mouseleave', () => { tooltip.style.display = 'none'; });
}

// ── 6. Rounds ─────────────────────────────────────────────────────────────────

function renderRoundsTab(container) {
  const rounds = report.state.rounds || [];
  const completed = rounds.filter((r) => r.Completed).map((r) => r.Completed);
  let matchDurationSecs = 0;

  const rows = completed.map((r, i) => {
    const startSecs = durSecs(r.start_time.viewdemo_offset);
    const endSecs = durSecs(r.end_time.viewdemo_offset);
    const roundDurSecs = Math.max(0, endSecs - startSecs);
    matchDurationSecs += roundDurSecs;

    let winnerCell = '', killsCell = '', color = '#ffffff';
    if (r.winner_stats) {
      const [team, kills] = r.winner_stats;
      color = teamColor(team === 'Allies' && report.state.allies_are_british ? 'British' : team);
      winnerCell = esc(teamLabel(team, report.state.allies_are_british));
      killsCell = String(kills);
    }
    return `
      <tr>
        <td><div style="width:10px;height:10px;border-radius:2px;background:${color};"></div></td>
        <td>${i + 1}</td>
        <td>${formatDuration(startSecs)}</td>
        <td>${formatDuration(roundDurSecs)}</td>
        <td>${winnerCell}</td>
        <td style="text-align:right;">${killsCell}</td>
      </tr>`;
  }).join('');

  container.innerHTML = `
    <h3 class="analyzer-heading">${STRINGS.ANALYZER.ROUNDS_TITLE}</h3>
    <div class="table-wrapper">
      <table class="analyzer-table">
        <thead><tr><th></th><th>${STRINGS.ANALYZER.COL_ROUND_NUM}</th><th>${STRINGS.ANALYZER.COL_START_TIME}</th><th>${STRINGS.ANALYZER.COL_DURATION}</th><th>${STRINGS.ANALYZER.COL_WINNER}</th><th style="text-align:right;">${STRINGS.ANALYZER.COL_KILLS_BY_WINNER}</th></tr></thead>
        <tbody>
          ${rows || `<tr><td colspan="6" class="table-empty">${STRINGS.ANALYZER.NO_COMPLETED_ROUNDS}</td></tr>`}
          <tr class="analyzer-total-row"><td></td><td></td><td></td><td>${formatDuration(matchDurationSecs)}</td><td></td><td></td></tr>
        </tbody>
      </table>
    </div>`;
}

// ── 7. Chat Log ───────────────────────────────────────────────────────────────

// Categorizes a System chat message the same way dev's chat.rs does, from
// its raw system_token string — used to drive the 4 system-log checkboxes.
function systemMessageCategory(m) {
  if (!m.system_token) return 'other';
  const t = m.system_token.toLowerCase();
  if (t.includes('connect') || t.includes('join_game') || t.includes('joined_game') || t.includes('kick') || t.includes('disconnect')) return 'join_leave';
  if (t.includes('joined_team') || t.includes('team')) return 'team_change';
  if (t.includes('score') || t.includes('capture') || t.includes('cap') || t.includes('reinforce')) return 'gameplay';
  return 'other';
}

const SYSTEM_KEYWORD_STYLES = [
  { patterns: ['allies', 'allied'], color: '#228b22' },
  { patterns: ['axis'], color: '#b22222' },
  { patterns: ['spectators', 'spectator', 'spec'], color: '#dddd00' },
];

// Colors team-name keywords inline within an already-translated system
// message, matching dev's earliest-match scan in render_system_message.
function colorSystemMessage(text) {
  const baseColor = '#b4dcdc';
  let remainder = text;
  let out = '';
  while (remainder.length > 0) {
    const lower = remainder.toLowerCase();
    let earliest = null;
    for (const style of SYSTEM_KEYWORD_STYLES) {
      for (const pattern of style.patterns) {
        const idx = lower.indexOf(pattern);
        if (idx !== -1 && (!earliest || idx < earliest.idx)) {
          earliest = { idx, len: pattern.length, color: style.color };
        }
      }
    }
    if (!earliest) {
      out += `<span style="color:${baseColor};font-style:italic;">${esc(remainder)}</span>`;
      break;
    }
    if (earliest.idx > 0) {
      out += `<span style="color:${baseColor};font-style:italic;">${esc(remainder.slice(0, earliest.idx))}</span>`;
    }
    out += `<span style="color:${earliest.color};font-style:italic;">${esc(remainder.slice(earliest.idx, earliest.idx + earliest.len))}</span>`;
    remainder = remainder.slice(earliest.idx + earliest.len);
  }
  return out;
}

function renderChatTab(container) {
  container.innerHTML = `
    <h3 class="analyzer-heading">${STRINGS.ANALYZER.CHAT_HEADING}</h3>
    <div class="analyzer-toolbar" style="flex-wrap:wrap;gap:10px;">
      <button type="button" id="chat-select-all">${STRINGS.ANALYZER.SELECT_ALL_BUTTON}</button>
      <button type="button" id="chat-clear-all">${STRINGS.ANALYZER.CLEAR_ALL_BUTTON}</button>
    </div>
    <div class="analyzer-toolbar" style="flex-wrap:wrap;gap:10px;">
      <label><input type="checkbox" id="chat-filter-mm1" ${chatFilters.showMm1 ? 'checked' : ''}/> ${STRINGS.ANALYZER.ALL_CHAT_LABEL}</label>
      <label><input type="checkbox" id="chat-filter-mm2" ${chatFilters.showMm2 ? 'checked' : ''}/> ${STRINGS.ANALYZER.TEAM_CHAT_LABEL}</label>
      <span class="text-muted">|</span>
      <label><input type="radio" name="chat-status" id="chat-status-all" ${chatFilters.status === 'All' ? 'checked' : ''}/> ${STRINGS.ANALYZER.STATUS_ALL}</label>
      <label><input type="radio" name="chat-status" id="chat-status-alive" ${chatFilters.status === 'Alive' ? 'checked' : ''}/> ${STRINGS.ANALYZER.STATUS_ALIVE}</label>
      <label><input type="radio" name="chat-status" id="chat-status-dead" ${chatFilters.status === 'Dead' ? 'checked' : ''}/> ${STRINGS.ANALYZER.STATUS_DEAD}</label>
    </div>
    <div class="analyzer-toolbar" style="flex-wrap:wrap;gap:10px;">
      <label>${STRINGS.ANALYZER.TEAM_LABEL}
        <select id="chat-filter-team">
          ${[
            ['All', STRINGS.ANALYZER.TYPE_ALL],
            ['Allies', STRINGS.ANALYZER.ALLIES_LABEL],
            ['British', STRINGS.ANALYZER.BRITISH_LABEL],
            ['Axis', STRINGS.ANALYZER.AXIS_LABEL],
            ['Spectators', STRINGS.ANALYZER.SPECTATORS_LABEL],
          ].map(([value, label]) => `<option value="${value}" ${chatFilters.team === value ? 'selected' : ''}>${label}</option>`).join('')}
        </select>
      </label>
    </div>
    <div class="analyzer-toolbar" style="flex-wrap:wrap;gap:10px;">
      <span class="text-muted">${STRINGS.ANALYZER.SYSTEM_LOGS_LABEL}</span>
      <label><input type="checkbox" id="chat-filter-joins" ${chatFilters.showJoins ? 'checked' : ''}/> ${STRINGS.ANALYZER.JOINS_LEAVES_LABEL}</label>
      <label><input type="checkbox" id="chat-filter-teams" ${chatFilters.showTeams ? 'checked' : ''}/> ${STRINGS.ANALYZER.TEAM_CHANGES_LABEL}</label>
      <label><input type="checkbox" id="chat-filter-gameplay" ${chatFilters.showGameplay ? 'checked' : ''}/> ${STRINGS.ANALYZER.GAMEPLAY_LABEL}</label>
      <label><input type="checkbox" id="chat-filter-othersys" ${chatFilters.showOtherSys ? 'checked' : ''}/> ${STRINGS.ANALYZER.OTHER_SYSTEM_LABEL}</label>
      <input type="text" id="chat-filter-search" placeholder="${STRINGS.ANALYZER.SEARCH_SENDER_TEXT_PLACEHOLDER}" value="${esc(chatFilters.search)}" style="flex:1;min-width:150px;" />
    </div>
    <div id="chat-log-list" class="analyzer-chat-log"></div>`;

  container.querySelector('#chat-select-all').addEventListener('click', () => {
    Object.assign(chatFilters, { showMm1: true, showMm2: true, status: 'All', team: 'All', showJoins: true, showTeams: true, showGameplay: true, showOtherSys: true });
    renderChatTab(container);
  });
  container.querySelector('#chat-clear-all').addEventListener('click', () => {
    Object.assign(chatFilters, { showMm1: false, showMm2: false, showJoins: false, showTeams: false, showGameplay: false, showOtherSys: false });
    renderChatTab(container);
  });

  container.querySelector('#chat-filter-mm1').addEventListener('change', (e) => { chatFilters.showMm1 = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-filter-mm2').addEventListener('change', (e) => { chatFilters.showMm2 = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-status-all').addEventListener('change', () => { chatFilters.status = 'All'; renderChatLogList(); });
  container.querySelector('#chat-status-alive').addEventListener('change', () => { chatFilters.status = 'Alive'; renderChatLogList(); });
  container.querySelector('#chat-status-dead').addEventListener('change', () => { chatFilters.status = 'Dead'; renderChatLogList(); });
  container.querySelector('#chat-filter-team').addEventListener('change', (e) => { chatFilters.team = e.target.value; renderChatLogList(); });
  container.querySelector('#chat-filter-joins').addEventListener('change', (e) => { chatFilters.showJoins = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-filter-teams').addEventListener('change', (e) => { chatFilters.showTeams = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-filter-gameplay').addEventListener('change', (e) => { chatFilters.showGameplay = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-filter-othersys').addEventListener('change', (e) => { chatFilters.showOtherSys = e.target.checked; renderChatLogList(); });
  container.querySelector('#chat-filter-search').addEventListener('input', (e) => { chatFilters.search = e.target.value; renderChatLogList(); });

  renderChatLogList();
}

function renderChatLogList() {
  const listEl = document.querySelector('#chat-log-list');
  if (!listEl) return;
  const messages = report.state.chat_messages || [];
  const search = chatFilters.search.toLowerCase();

  const filtered = messages.filter((m) => {
    if (m.chat_type === 'System') {
      const cat = systemMessageCategory(m);
      if (cat === 'join_leave' && !chatFilters.showJoins) return false;
      if (cat === 'team_change' && !chatFilters.showTeams) return false;
      if (cat === 'gameplay' && !chatFilters.showGameplay) return false;
      if (cat === 'other' && !chatFilters.showOtherSys) return false;
    } else {
      if (m.chat_type === 'Mm1' && !chatFilters.showMm1) return false;
      if (m.chat_type === 'Mm2' && !chatFilters.showMm2) return false;
      if (chatFilters.status === 'Alive' && m.sender_dead) return false;
      if (chatFilters.status === 'Dead' && !m.sender_dead) return false;
      if (chatFilters.team !== 'All') {
        const t = m.sender_team;
        const matchesTeam = t === chatFilters.team
          || (chatFilters.team === 'Allies' && t === 'British')
          || (chatFilters.team === 'British' && t === 'Allies');
        if (!matchesTeam) return false;
      }
    }
    if (search) {
      const hay = `${m.sender_name || ''} ${m.text || ''}`.toLowerCase();
      if (!hay.includes(search)) return false;
    }
    return true;
  });

  if (filtered.length === 0) {
    listEl.innerHTML = `<p class="analyzer-empty">${STRINGS.ANALYZER.NO_MESSAGES_MATCH_FILTERS}</p>`;
    return;
  }

  listEl.innerHTML = filtered.map((m) => {
    const time = formatGameTime(durSecs(m.time.viewdemo_offset));
    const deadBadge = m.sender_dead ? `<span style="color:#dc3232;">${STRINGS.ANALYZER.CHAT_DEAD_BADGE}</span> ` : '';
    let body;
    if (m.chat_type === 'System') {
      body = `<span style="color:#c89650;">${STRINGS.ANALYZER.CHAT_SYSTEM_TAG}</span> ${colorSystemMessage(m.text)}`;
    } else {
      const teamBadge = m.chat_type === 'Mm2' ? `<span style="color:${teamColor(m.sender_team)};">${STRINGS.ANALYZER.CHAT_TEAM_BADGE}</span> ` : '';
      body = `${teamBadge}<span style="color:${teamColor(m.sender_team)};font-weight:600;">${esc(m.sender_name || STRINGS.ANALYZER.CHAT_SENDER_UNKNOWN)}:</span> ${esc(m.text)}`;
    }
    return `<div class="analyzer-chat-row"><span class="chat-timestamp">[${time}]</span> ${deadBadge}${body}</div>`;
  }).join('');
}

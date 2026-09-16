// cfg_warnings.js — the fatal-cvar banner added in #209.
//
// This is the most severe thing the config scanner can report (a config
// already on disk that will quit the game outright), and it was the one part
// of #209 with no automated coverage at all — the PR said as much, and the
// only check was "open it in tauri dev and look". These tests pin the parts
// that would silently regress: that the banner appears at all, that it names
// the cvar/value/file/line the backend reported, and that it stays hidden
// when there is nothing to report.
import { test, expect } from '@playwright/test';

const GAME_PATH = 'C:/games/dod/hl.exe';

/** The backend's CfgReport shape (serde camelCase), with everything empty. */
const EMPTY_REPORT = {
  settings: [],
  unreferenced: [],
  overrides: [],
  bannedInit: [],
  bannedScheduled: [],
  unseen: [],
  midDemoHazards: [],
  selfOverrides: [],
  decalFlushIsNoop: false,
  noopInit: [],
  noopScheduled: [],
  fatalCvars: [],
};

async function loadHarness(page, report) {
  await page.addInitScript((r) => {
    window.__mockInvokeHandlers = window.__mockInvokeHandlers || {};
    window.__mockInvokeHandlers.scan_game_configs = () => r;
  }, report);
  await page.goto('/tests/e2e/cfg-warnings.html');
  await page.waitForFunction(() => window.__harnessReady === true);
  await page.evaluate((p) => window.__refresh(p), GAME_PATH);
}

test.describe('fatal cvar banner', () => {
  test('a fatal cvar renders a banner naming the cvar, value, file and line', async ({ page }) => {
    await loadHarness(page, {
      ...EMPTY_REPORT,
      fatalCvars: [{ cvar: 'cl_lw', value: '0', required: '1', file: 'movie.cfg', line: 12 }],
    });

    const banner = page.locator('#init-commands-warning-banner');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('These config values will quit the game:');

    const row = banner.locator('li code');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('cl_lw');
    await expect(row).toContainText('0');
    await expect(row).toContainText('movie.cfg');
    await expect(row).toContainText('12');
  });

  test('the banner stays hidden when nothing is reported', async ({ page }) => {
    await loadHarness(page, EMPTY_REPORT);

    await expect(page.locator('#init-commands-warning-banner')).toBeHidden();
    await expect(page.locator('#scheduled-commands-warning-banner')).toBeHidden();
  });

  test('several fatal cvars each get their own row', async ({ page }) => {
    await loadHarness(page, {
      ...EMPTY_REPORT,
      fatalCvars: [
        { cvar: 'cl_lw', value: '0', required: '1', file: 'movie.cfg', line: 3 },
        { cvar: 'r_drawentities', value: '0', required: '1', file: 'config.cfg', line: 41 },
      ],
    });

    const rows = page.locator('#init-commands-warning-banner li code');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText('cl_lw');
    await expect(rows.nth(1)).toContainText('r_drawentities');
  });

  test('the fatal section renders above the banned-command section', async ({ page }) => {
    // Ordering is deliberate: a config already on disk quits the game whether
    // or not this batch ever starts, so it outranks the merely
    // blocks-Start-Capture-Batch case. A future edit reordering the sections
    // would be a real regression in what the user reads first.
    await loadHarness(page, {
      ...EMPTY_REPORT,
      fatalCvars: [{ cvar: 'cl_lw', value: '0', required: '1', file: 'movie.cfg', line: 3 }],
      bannedInit: [{ command: 'host_framerate 0', reason: '' }],
    });

    const text = await page.locator('#init-commands-warning-banner').innerText();
    const fatalAt = text.indexOf('quit the game');
    const bannedAt = text.indexOf('host_framerate');
    expect(fatalAt).toBeGreaterThanOrEqual(0);
    expect(bannedAt).toBeGreaterThanOrEqual(0);
    expect(fatalAt).toBeLessThan(bannedAt);
  });
});

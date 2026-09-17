// error_reporter.js
//
// Global safety net for anything that goes truly uncaught — not a
// try/catch wrapper (nothing here changes how errors propagate), just two
// browser-standard listeners that fire for whatever nothing else handled:
// window 'error' (thrown synchronously, uncaught) and 'unhandledrejection'
// (a rejected Promise nobody awaited/caught — which is what both of this
// session's real crashes turned out to be, since every click handler here
// is async).
//
// Built after two real bugs (a stack overflow and a null-deref) produced
// zero visible symptoms without opening devtools — no toast, no log line,
// just a silently-abandoned action. This makes that class of failure
// visible three ways at once: a tagged console.error (on top of the
// browser's own default uncaught-error logging, not replacing it), a line
// in crash_log.md, and a capped toast so a rapid-fire bug (like a
// recursion loop throwing repeatedly right up to the stack limit) doesn't
// spam the screen.

import { logFrontendEvent } from './ipc_bridge.js';
import { showToast } from './toast.js';
import { STRINGS } from './strings.js';

const MAX_TOASTS_PER_SESSION = 3;
let toastCount = 0;

function describeError(err) {
  if (err instanceof Error) {
    return { message: err.message, stack: err.stack || '(no stack)' };
  }
  return { message: String(err), stack: '(not an Error object)' };
}

function reportUncaught(source, err) {
  const { message, stack } = describeError(err);

  // Tagged and separate from the browser's own default uncaught-error
  // logging (which still happens on its own) — easy to grep/spot on
  // purpose, since the whole point is not blending into the raw trace.
  console.error(`[dod-studio] Uncaught ${source}:`, err);

  logFrontendEvent(`[error] Uncaught ${source}: ${message}\n${stack}`);

  toastCount += 1;
  if (toastCount <= MAX_TOASTS_PER_SESSION) {
    showToast(STRINGS.ERROR_REPORTER.somethingWentWrong(message), 'error', 6000);
  }
}

export function initErrorReporter() {
  window.addEventListener('error', (event) => {
    reportUncaught('error', event.error || event.message);
  });
  window.addEventListener('unhandledrejection', (event) => {
    reportUncaught('promise rejection', event.reason);
  });
}

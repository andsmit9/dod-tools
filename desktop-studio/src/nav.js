// nav.js — shared top-nav tab switching. Extracted from main.js so panes that
// need to jump to a different tab programmatically (e.g. detail_pane.js's
// "View Match Telemetry" button jumping to the Demo Analyzer tab) can call it
// without creating a circular import with main.js.

// In-workflow Studio phase — dev's SelectTab::Highlights/Configuration live
// inside the single continuous Studio destination, not as separate top-level
// nav tabs ("Batch Capture Config" was DRIFT, see docs/archive/tauri_parity_audit.md
// Area 1; Render Studio's own top-level tab was folded in the same way for
// #81). 'highlights' shows the Master Queue's detail view + advanced
// diagnostics; 'render' shows the Render Studio job panel; 'configuration'
// shows Path Routing/Timing/Destinations/Custom Commands/Render Output.
let activeCaptureDetailSubtab = 'highlights';

function applyCaptureDetailSubtabDisplay() {
  const workspacePane = document.querySelector('#pane-workspace');
  const detailPane = document.querySelector('#detail-pane');
  const advancedPanel = document.querySelector('#advanced-diagnostics-details');
  const exportPanel = document.querySelector('#export-config-panel');
  const renderPanel = document.querySelector('#render-studio-panel');
  const footerCaptureStudio = document.querySelector('#footer-capture-studio');
  const footerRenderStudio = document.querySelector('#footer-render-studio');
  const footerConfiguration = document.querySelector('#footer-configuration');
  const showHighlights = activeCaptureDetailSubtab === 'highlights';
  const showRender = activeCaptureDetailSubtab === 'render';
  const showConfiguration = !showHighlights && !showRender;

  // The left master-demo-queue list is only relevant while picking which
  // demo's highlights to look at — Render works off its own scanned take
  // list, and Configuration is settings, not something you cross-reference
  // against the queue. Both get the same full-width treatment Render
  // already had (a deliberate call, not a layout regression): only
  // Highlights keeps the master-queue pane visible alongside it.
  if (workspacePane) workspacePane.style.display = showHighlights ? 'flex' : 'none';
  if (detailPane) detailPane.style.display = showHighlights ? 'block' : 'none';
  if (advancedPanel) advancedPanel.style.display = showHighlights ? 'block' : 'none';
  if (exportPanel) exportPanel.style.display = showConfiguration ? 'block' : 'none';
  if (renderPanel) renderPanel.style.display = showRender ? 'block' : 'none';
  if (footerCaptureStudio) footerCaptureStudio.style.display = showHighlights ? 'flex' : 'none';
  if (footerRenderStudio) footerRenderStudio.style.display = showRender ? 'flex' : 'none';
  if (footerConfiguration) footerConfiguration.style.display = showConfiguration ? 'flex' : 'none';

  document.querySelectorAll('.capture-detail-subtab-btn').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.captureSubtab === activeCaptureDetailSubtab);
  });
}

export function setCaptureDetailSubtab(subtab) {
  activeCaptureDetailSubtab = subtab;
  applyCaptureDetailSubtabDisplay();
}

export function switchNavTab(navKey) {
  const workspacePane = document.querySelector('#pane-workspace');
  const detailsPane = document.querySelector('#pane-details-config');
  const captureSubtabsBar = document.querySelector('#capture-detail-subtabs');
  const detailPane = document.querySelector('#detail-pane');
  const advancedPanel = document.querySelector('#advanced-diagnostics-details');
  const exportPanel = document.querySelector('#export-config-panel');
  const renderPanel = document.querySelector('#render-studio-panel');
  const auditorPane = document.querySelector('#pane-demo-auditor');
  const analyzerPane = document.querySelector('#pane-demo-analyzer');

  [workspacePane, detailsPane, captureSubtabsBar, detailPane, advancedPanel, exportPanel, renderPanel, auditorPane, analyzerPane]
    .forEach((el) => { if (el) el.style.display = 'none'; });

  const footerCaptureStudio = document.querySelector('#footer-capture-studio');
  const footerRenderStudio = document.querySelector('#footer-render-studio');
  const footerConfiguration = document.querySelector('#footer-configuration');
  const footerDemoAuditor = document.querySelector('#footer-demo-auditor');
  const footerDemoAnalyzer = document.querySelector('#footer-demo-analyzer');

  [footerCaptureStudio, footerRenderStudio, footerConfiguration, footerDemoAuditor, footerDemoAnalyzer]
    .forEach((el) => { if (el) el.style.display = 'none'; });


  if (navKey === 'workspace') {
    if (detailsPane) detailsPane.style.display = 'flex';
    if (captureSubtabsBar) captureSubtabsBar.style.display = 'flex';
    // Owns workspacePane/detailPane/advancedPanel/exportPanel/renderPanel
    // and the footer swap — Highlights/Render/Configuration are now all
    // subtabs of this one navKey, not separate top-level destinations.
    applyCaptureDetailSubtabDisplay();
  } else if (navKey === 'demo-auditor') {
    if (auditorPane) auditorPane.style.display = 'flex';
    if (footerDemoAuditor) footerDemoAuditor.style.display = 'flex';
  } else if (navKey === 'demo-analyzer') {
    if (analyzerPane) analyzerPane.style.display = 'flex';
    if (footerDemoAnalyzer) footerDemoAnalyzer.style.display = 'flex';
  }

  document.querySelectorAll('.nav-tab-btn').forEach((b) => {
    b.classList.toggle('active', b.dataset.nav === navKey);
  });
}

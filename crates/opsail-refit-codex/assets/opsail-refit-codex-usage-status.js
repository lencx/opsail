(() => {
  const runtime = window.__OPSAIL_REFIT_CODEX_STATE__;
  let diagnostics = null;
  try { diagnostics = runtime?.diagnostics?.() ?? null; } catch {}
  return {
    installed: Boolean(runtime && runtime.mode === "usage"),
    revision: runtime?.revision ?? null,
    expectedRevision: __OPSAIL_REFIT_CODEX_STATUS_REVISION_JSON__,
    diagnostics,
    hostCount: document.querySelectorAll("#opsail-refit-codex-usage").length,
    styleCount: document.querySelectorAll("#opsail-refit-codex-usage-style").length,
    detailsCount: document.querySelectorAll("#opsail-refit-codex-usage-details").length,
  };
})()

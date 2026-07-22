(() => {
  window.__OPSAIL_REFIT_CODEX_DISABLED__ = true;
  window.__OPSAIL_REFIT_CODEX_EARLY_GENERATION__ = `disabled:${Date.now()}`;
  try { window.__OPSAIL_REFIT_CODEX_EARLY_STATE__?.cleanup?.(); } catch {}
  try { window.__OPSAIL_REFIT_CODEX_STATE__?.cleanup?.(); } catch {}
  document.getElementById("opsail-refit-codex-usage")?.remove();
  document.getElementById("opsail-refit-codex-usage-details")?.remove();
  document.getElementById("opsail-refit-codex-usage-style")?.remove();
  document.documentElement?.classList.remove("opsail-refit-codex-usage-enabled");
  delete window.__OPSAIL_REFIT_CODEX_STATE__;
  delete window.__OPSAIL_REFIT_CODEX_EARLY_STATE__;
  return {
    clean: !document.getElementById("opsail-refit-codex-usage")
      && !document.getElementById("opsail-refit-codex-usage-details")
      && !document.getElementById("opsail-refit-codex-usage-style")
      && !window.__OPSAIL_REFIT_CODEX_STATE__,
  };
})()

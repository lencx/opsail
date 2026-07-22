(() => {
  const operation = __OPSAIL_REFIT_CODEX_OPERATION_JSON__;

  if (operation === "probe") {
    __OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__
    return createOpsailRefitCodexDomAdapter().probeRenderer();
  }

  if (operation === "early") {
    __OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__

    const STATE_KEY = "__OPSAIL_REFIT_CODEX_EARLY_STATE__";
    const GENERATION_KEY = "__OPSAIL_REFIT_CODEX_EARLY_GENERATION__";
    const generation = __OPSAIL_REFIT_CODEX_EARLY_REVISION_JSON__;
    const installToken = {};
    const codexDom = createOpsailRefitCodexDomAdapter();
    try { window[STATE_KEY]?.cleanup?.(); } catch {}
    window[GENERATION_KEY] = generation;
    let observer = null;
    let timeout = null;
    const cleanup = () => {
      if (window[STATE_KEY]?.installToken !== installToken) return false;
      observer?.disconnect();
      observer = null;
      if (timeout !== null) clearTimeout(timeout);
      timeout = null;
      delete window[STATE_KEY];
      return true;
    };
    const install = () => {
      if (window[GENERATION_KEY] !== generation) { cleanup(); return true; }
      if (!document.documentElement) return false;
      const probe = codexDom.probeRenderer();
      if (!probe.appProtocol) { cleanup(); return true; }
      if (!probe.bridge || !probe.shell || !probe.sidebar) return false;
      cleanup();
      __OPSAIL_REFIT_CODEX_CURRENT_PAYLOAD__;
      return true;
    };
    window[STATE_KEY] = { cleanup, installToken };
    if (!install()) {
      if (typeof MutationObserver === "function" && document.documentElement) {
        observer = new MutationObserver(install);
        observer.observe(document.documentElement, { childList: true, subtree: true });
      }
      timeout = setTimeout(cleanup, 30000);
    }
    return undefined;
  }

  if (operation === "status") {
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
  }

  if (operation === "disable") {
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
  }

  throw new Error("unsupported opsail renderer operation");
})()

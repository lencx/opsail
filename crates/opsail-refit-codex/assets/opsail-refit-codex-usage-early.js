(() => {
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
})()

(() => {
  __OPSAIL_REFIT_CODEX_MODEL_SOURCE__

  const STATE_KEY = "__OPSAIL_REFIT_CODEX_STATE__";
  const DISABLED_KEY = "__OPSAIL_REFIT_CODEX_DISABLED__";
  const STYLE_ID = "opsail-refit-codex-usage-style";
  const USAGE_ID = "opsail-refit-codex-usage";
  const DETAILS_ID = "opsail-refit-codex-usage-details";
  const ROOT_CLASS = "opsail-refit-codex-usage-enabled";
  const HOST_ID = "local";
  const SIDEBAR_SELECTOR = "aside.app-shell-left-panel, aside[data-testid='app-shell-floating-left-panel']";
  const AVATAR_SELECTOR = "img, [data-testid*='avatar' i], [class*='avatar' i]";
  const ACTION_SELECTOR = "button, [role='button']";
  const VERSION = __OPSAIL_REFIT_CODEX_VERSION_JSON__;
  const REVISION = __OPSAIL_REFIT_CODEX_REVISION_JSON__;
  const SESSION_MODE = __OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__;
  const MANAGER_TOKEN = __OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__;
  const CSS_TEXT = __OPSAIL_REFIT_CODEX_CSS_JSON__;
  const LOCALE_BUNDLE = __OPSAIL_REFIT_CODEX_LOCALES_JSON__;
  const installToken = {};
  const usageModel = createOpsailRefitCodexUsageModel(LOCALE_BUNDLE);
  const copy = usageModel.selectLocale(
    document.documentElement?.lang,
    typeof navigator === "object" ? navigator.language : "",
  );
  const systemLocale = typeof navigator === "object" && navigator.language
    ? navigator.language
    : document.documentElement?.lang;

  try { window[STATE_KEY]?.cleanup?.(); } catch {}
  document.getElementById(USAGE_ID)?.remove();
  document.getElementById(DETAILS_ID)?.remove();
  document.getElementById(STYLE_ID)?.remove();
  document.documentElement?.classList.remove(ROOT_CLASS);
  window[DISABLED_KEY] = false;

  const state = {
    disposed: false,
    status: "loading",
    snapshot: null,
    host: null,
    details: null,
    parts: null,
    sidebar: null,
    row: null,
    hasWindows: false,
    detailsOpen: false,
    closeTimer: null,
    refreshTimer: null,
  };
  const metrics = {
    ensureCalls: 0,
    layoutCalls: 0,
    usageRequests: 0,
    usageUpdates: 0,
  };
  const listeners = [];
  let mutationObserver = null;
  let observedMutationSidebar;
  let resizeObserver = null;
  let observedSidebar = null;
  let observedRow = null;
  const scheduler = {
    ensureTimeout: null,
    frame: null,
    timeout: null,
    tooltipFrame: null,
    tooltipFrameKind: null,
  };

  const addListener = (target, type, listener, options) => {
    if (!target?.addEventListener) return;
    target.addEventListener(type, listener, options);
    listeners.push({ target, type, listener, options });
  };

  const removeListeners = () => {
    for (const item of listeners.splice(0)) {
      try { item.target.removeEventListener(item.type, item.listener, item.options); } catch {}
    }
  };

  const elementRect = (element) => {
    try {
      const rect = element?.getBoundingClientRect?.();
      if (!rect) return null;
      const values = [rect.left, rect.top, rect.right, rect.bottom, rect.width, rect.height].map(Number);
      if (!values.every(Number.isFinite)) return null;
      return {
        left: values[0],
        top: values[1],
        right: values[2],
        bottom: values[3],
        width: values[4],
        height: values[5],
        centerX: values[0] + values[4] / 2,
        centerY: values[1] + values[5] / 2,
      };
    } catch {
      return null;
    }
  };

  const queryRects = (root, selector) => {
    try {
      return [...(root?.querySelectorAll?.(selector) || [])]
        .map((element) => ({ element, rect: elementRect(element) }))
        .filter((entry) => entry.rect);
    } catch {
      return [];
    }
  };

  const nearestCommonAncestor = (left, right) => {
    if (!left || !right) return null;
    const ancestors = new Set();
    for (let current = left; current; current = current.parentElement) ancestors.add(current);
    for (let current = right; current; current = current.parentElement) {
      if (ancestors.has(current)) return current;
    }
    return null;
  };

  const directChildContaining = (ancestor, descendant) => {
    if (!ancestor || !descendant) return null;
    let current = descendant;
    while (current?.parentElement && current.parentElement !== ancestor) current = current.parentElement;
    return current?.parentElement === ancestor ? current : null;
  };

  const measureNativeLayout = (sidebar) => {
    const sidebarRect = elementRect(sidebar);
    if (!sidebarRect) return null;
    const footerTop = sidebarRect.bottom - Math.min(112, sidebarRect.height * 0.3);
    const avatars = queryRects(sidebar, AVATAR_SELECTOR)
      .filter(({ rect }) => rect.width >= 16 && rect.width <= 48
        && rect.height >= 16 && rect.height <= 48
        && Math.abs(rect.width - rect.height) <= 8
        && rect.centerY >= footerTop
        && rect.centerX <= sidebarRect.left + sidebarRect.width * 0.55)
      .sort((left, right) => right.rect.centerY - left.rect.centerY);
    const actions = queryRects(sidebar, ACTION_SELECTOR)
      .filter(({ rect }) => rect.width >= 20 && rect.width <= 48
        && rect.height >= 20 && rect.height <= 48
        && rect.centerY >= footerTop
        && rect.centerX >= sidebarRect.left + sidebarRect.width * 0.5)
      .sort((left, right) => right.rect.centerY - left.rect.centerY
        || right.rect.centerX - left.rect.centerX);
    const avatar = avatars[0] || null;
    const trailingAction = actions[0] || null;
    const accountControlElement = avatar?.element?.closest?.("button, [role='button'], a") || null;
    const accountControlRect = elementRect(accountControlElement);
    const accountControl = accountControlElement && accountControlRect
      ? { element: accountControlElement, rect: accountControlRect }
      : null;
    const row = nearestCommonAncestor(accountControlElement, trailingAction?.element);
    const accountSlot = directChildContaining(row, accountControlElement);
    const trailingSlot = directChildContaining(row, trailingAction?.element);
    const rowRect = elementRect(row);
    const inline = Boolean(
      row && row !== sidebar && accountSlot && trailingSlot && accountSlot !== trailingSlot
      && rowRect && rowRect.width >= 120 && rowRect.height >= 28 && rowRect.height <= 72
      && Math.abs((avatar?.rect.centerY || 0) - (trailingAction?.rect.centerY || 0)) <= 16,
    );
    return {
      sidebarRect,
      avatar,
      accountControl,
      trailingAction,
      row: inline ? row : null,
      accountSlot: inline ? accountSlot : null,
      trailingSlot: inline ? trailingSlot : null,
    };
  };

  const setText = (node, value) => {
    if (node && node.textContent !== value) node.textContent = value;
  };

  const createElement = (tagName, className = "") => {
    const element = document.createElement(tagName);
    if (className) element.className = className;
    return element;
  };

  const createRow = (index) => {
    const row = createElement("div", "opsail-refit-codex-usage-row");
    row.dataset.opsailRefitCodexWindowIndex = String(index);
    const line = createElement("div", "opsail-refit-codex-usage-line");
    const label = createElement("span");
    const value = createElement("b");
    line.append(label, value);
    const meta = createElement("div", "opsail-refit-codex-usage-meta");
    const track = createElement("div", "opsail-refit-codex-usage-track");
    track.setAttribute("role", "progressbar");
    const fill = createElement("i");
    fill.setAttribute("aria-hidden", "true");
    track.append(fill);
    row.append(line, meta, track);
    return { row, label, value, meta, track };
  };

  const scheduleTooltipPosition = () => {
    if (!state.detailsOpen || scheduler.tooltipFrame !== null) return;
    const position = () => {
      scheduler.tooltipFrame = null;
      scheduler.tooltipFrameKind = null;
      if (!state.detailsOpen || !state.host || !state.details || !state.sidebar) return;
      const anchor = elementRect(state.host);
      const sidebar = elementRect(state.sidebar);
      const tooltip = elementRect(state.details);
      const viewport = {
        left: 0,
        top: 0,
        right: Number(window.innerWidth) || 0,
        bottom: Number(window.innerHeight) || 0,
        width: Number(window.innerWidth) || 0,
        height: Number(window.innerHeight) || 0,
      };
      let placement = usageModel.computeTooltipPlacement({ anchor, sidebar, viewport, tooltip });
      if (!placement) {
        closeDetails();
        return;
      }
      state.details.style.setProperty(
        "--opsail-refit-usage-details-width",
        `${Math.round(placement.width)}px`,
      );
      state.details.style.setProperty(
        "--opsail-refit-usage-details-max-height",
        `${Math.round(placement.maximumHeight)}px`,
      );
      placement = usageModel.computeTooltipPlacement({
        anchor,
        sidebar,
        viewport,
        tooltip: elementRect(state.details),
      });
      if (!placement) {
        closeDetails();
        return;
      }
      state.details.style.setProperty("--opsail-refit-usage-details-left", `${Math.round(placement.left)}px`);
      state.details.style.setProperty("--opsail-refit-usage-details-top", `${Math.round(placement.top)}px`);
      state.details.style.setProperty("--opsail-refit-usage-details-width", `${Math.round(placement.width)}px`);
      state.details.style.setProperty(
        "--opsail-refit-usage-details-max-height",
        `${Math.round(placement.maximumHeight)}px`,
      );
    };
    if (typeof requestAnimationFrame === "function") {
      scheduler.tooltipFrameKind = "animation";
      scheduler.tooltipFrame = requestAnimationFrame(position);
    } else {
      scheduler.tooltipFrameKind = "timeout";
      scheduler.tooltipFrame = setTimeout(position, 0);
    }
  };

  const openDetails = () => {
    if (!state.hasWindows || state.host?.hidden || !state.details) return;
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    state.closeTimer = null;
    state.detailsOpen = true;
    state.details.dataset.opsailRefitCodexOpen = "true";
    state.details.setAttribute("aria-hidden", "false");
    scheduleTooltipPosition();
  };

  const closeDetails = () => {
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    state.closeTimer = null;
    state.detailsOpen = false;
    if (state.details) {
      state.details.dataset.opsailRefitCodexOpen = "false";
      state.details.setAttribute("aria-hidden", "true");
    }
  };

  const scheduleCloseDetails = () => {
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    state.closeTimer = setTimeout(closeDetails, 80);
  };

  const createUi = () => {
    const host = createElement("section", "opsail-refit-codex-usage-host");
    host.id = USAGE_ID;
    host.tabIndex = 0;
    host.hidden = true;
    host.setAttribute("aria-live", "polite");
    host.setAttribute("aria-describedby", DETAILS_ID);
    const summary = createElement("div", "opsail-refit-codex-usage-summary");
    host.append(summary);

    const details = createElement("aside");
    details.id = DETAILS_ID;
    details.dataset.opsailRefitCodexOpen = "false";
    details.setAttribute("role", "tooltip");
    details.setAttribute("aria-hidden", "true");
    details.setAttribute("aria-label", copy.usageTitle);
    const stale = createElement("div", "opsail-refit-codex-usage-stale");
    stale.hidden = true;
    const rows = [createRow(0), createRow(1)];
    details.append(stale, ...rows.map((row) => row.row));
    (document.body || document.documentElement).append(details);

    addListener(host, "pointerenter", openDetails);
    addListener(host, "pointerleave", scheduleCloseDetails);
    addListener(host, "focusin", openDetails);
    addListener(host, "focusout", (event) => {
      if (event.relatedTarget && (host.contains(event.relatedTarget) || details.contains(event.relatedTarget))) return;
      closeDetails();
    });
    addListener(details, "pointerenter", openDetails);
    addListener(details, "pointerleave", scheduleCloseDetails);

    state.host = host;
    state.details = details;
    state.parts = { summary, stale, rows };
  };

  const ensureStyle = () => {
    let style = document.getElementById(STYLE_ID);
    if (!style) {
      style = document.createElement("style");
      style.id = STYLE_ID;
      (document.head || document.documentElement).append(style);
    }
    if (style.textContent !== CSS_TEXT) style.textContent = CSS_TEXT;
    style.dataset.opsailRefitCodexRevision = REVISION;
  };

  const hideHost = () => {
    if (state.host) state.host.hidden = true;
    closeDetails();
  };

  const layout = () => {
    metrics.layoutCalls += 1;
    const host = state.host;
    const sidebar = state.sidebar;
    const previousRow = state.row;
    const wasInline = host?.dataset.opsailRefitCodexLayout === "inline"
      && previousRow?.isConnected
      && host.parentElement === previousRow;
    state.row = null;
    if (!host || !sidebar || !state.hasWindows) {
      hideHost();
      observeGeometry();
      return;
    }

    if (!wasInline && host.parentElement !== sidebar) sidebar.append(host);
    host.dataset.opsailRefitCodexLayout = "measuring";
    host.hidden = false;
    host.style.visibility = "hidden";
    host.style.removeProperty("--opsail-refit-usage-left");
    host.style.removeProperty("--opsail-refit-usage-top");

    const measured = measureNativeLayout(sidebar);
    const hostRect = elementRect(host);
    const capsuleWidth = Math.max(
      hostRect?.width || 0,
      Number(state.parts?.summary?.scrollWidth) || 0,
    );
    let mounted = false;

    if (measured?.row && measured.accountSlot && measured.trailingSlot && capsuleWidth > 0) {
      const accountRect = elementRect(measured.accountSlot);
      const trailingRect = elementRect(measured.trailingSlot);
      if (accountRect && trailingRect && usageModel.canFitCapsule({
        leftBoundary: accountRect.right,
        rightBoundary: trailingRect.left,
        capsuleWidth,
      })) {
        measured.row.insertBefore(host, measured.trailingSlot);
        host.dataset.opsailRefitCodexLayout = "inline";
        const inlineHostRect = elementRect(host);
        const inlineAccountRect = elementRect(measured.accountSlot);
        const inlineTrailingRect = elementRect(measured.trailingSlot);
        mounted = Boolean(
          inlineHostRect && inlineAccountRect && inlineTrailingRect
          && inlineHostRect.left >= inlineAccountRect.right
          && inlineHostRect.right <= inlineTrailingRect.left
          && inlineHostRect.left >= measured.sidebarRect.left
          && inlineHostRect.right <= measured.sidebarRect.right
          && inlineHostRect.top >= Math.max(0, measured.sidebarRect.top)
          && inlineHostRect.bottom <= Math.min(
            Number(window.innerHeight) || measured.sidebarRect.bottom,
            measured.sidebarRect.bottom,
          ),
        );
        if (mounted) state.row = measured.row;
      }
    }

    if (!mounted && measured?.accountControl && measured.trailingAction && capsuleWidth > 0) {
      const leftBoundary = measured.accountControl.rect.right;
      const rightBoundary = measured.trailingAction.rect.left;
      const hostHeight = hostRect?.height || 0;
      const minimumTop = Math.max(0, measured.sidebarRect.top);
      const maximumBottom = Math.min(
        Number(window.innerHeight) || measured.sidebarRect.bottom,
        measured.sidebarRect.bottom,
      );
      if (hostHeight > 0
        && maximumBottom - minimumTop >= hostHeight
        && usageModel.canFitCapsule({ leftBoundary, rightBoundary, capsuleWidth })) {
        (document.body || document.documentElement).append(host);
        host.dataset.opsailRefitCodexLayout = "fallback";
        const left = Math.max(
          measured.sidebarRect.left,
          Math.min(
            measured.sidebarRect.right - capsuleWidth,
            leftBoundary + (rightBoundary - leftBoundary - capsuleWidth) / 2,
          ),
        );
        const preferredTop = (
          measured.accountControl.rect.centerY + measured.trailingAction.rect.centerY
        ) / 2 - hostHeight / 2;
        const top = Math.max(
          minimumTop,
          Math.min(maximumBottom - hostHeight, preferredTop),
        );
        host.style.setProperty("--opsail-refit-usage-left", `${Math.round(left)}px`);
        host.style.setProperty("--opsail-refit-usage-top", `${Math.round(top)}px`);
        const fallbackRect = elementRect(host);
        mounted = Boolean(
          fallbackRect
          && fallbackRect.left >= measured.sidebarRect.left
          && fallbackRect.right <= measured.sidebarRect.right
          && fallbackRect.left >= leftBoundary
          && fallbackRect.right <= rightBoundary
          && fallbackRect.top >= minimumTop
          && fallbackRect.bottom <= maximumBottom,
        );
        state.row = null;
      }
    }

    host.style.visibility = "";
    host.hidden = !mounted;
    if (!mounted) closeDetails();
    if (mounted) scheduleTooltipPosition();
    observeGeometry();
  };

  const flushLayout = () => {
    if (scheduler.frame !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(scheduler.frame);
    }
    if (scheduler.timeout !== null) clearTimeout(scheduler.timeout);
    scheduler.frame = null;
    scheduler.timeout = null;
    layout();
  };

  const scheduleLayout = () => {
    if (state.disposed || scheduler.frame !== null || scheduler.timeout !== null) return;
    if (typeof requestAnimationFrame === "function") {
      scheduler.frame = requestAnimationFrame(flushLayout);
      scheduler.timeout = setTimeout(flushLayout, 96);
    } else {
      scheduler.timeout = setTimeout(flushLayout, 64);
    }
  };

  const observeGeometry = () => {
    if (typeof ResizeObserver !== "function") return;
    const nextSidebar = state.sidebar?.isConnected ? state.sidebar : null;
    const nextRow = state.row?.isConnected && state.row !== nextSidebar ? state.row : null;
    if (!resizeObserver) resizeObserver = new ResizeObserver(scheduleLayout);
    if (observedSidebar === nextSidebar && observedRow === nextRow) return;
    resizeObserver.disconnect();
    observedSidebar = nextSidebar;
    observedRow = nextRow;
    if (observedSidebar) resizeObserver.observe(observedSidebar);
    if (observedRow) resizeObserver.observe(observedRow);
  };

  const render = () => {
    if (!state.parts || !state.host || !state.details) return;
    const windows = usageModel.presentWindows(state.snapshot, copy, systemLocale);
    const stale = state.status === "stale" && windows.length > 0;
    state.hasWindows = windows.length > 0;
    state.host.dataset.opsailRefitCodexStale = String(stale);
    state.host.dataset.opsailRefitCodexState = state.status;
    state.parts.stale.hidden = !stale;
    setText(state.parts.stale, stale ? copy.stale : "");

    if (windows.length === 0) {
      setText(state.parts.summary, "");
      for (const row of state.parts.rows) row.row.hidden = true;
      hideHost();
      return;
    }

    const summary = usageModel.summaryFor(windows, copy);
    setText(state.parts.summary, summary);
    state.host.setAttribute("aria-label", usageModel.formatMessage(copy.ariaSummary, { summary }));
    for (let index = 0; index < state.parts.rows.length; index += 1) {
      const row = state.parts.rows[index];
      const windowValue = windows[index];
      if (!windowValue) {
        row.row.hidden = true;
        continue;
      }
      row.row.hidden = false;
      setText(row.label, windowValue.label);
      setText(row.value, usageModel.formatMessage(copy.remaining, windowValue));
      setText(row.meta, [
        usageModel.formatMessage(copy.used, windowValue),
        windowValue.reset ? usageModel.formatMessage(copy.reset, { time: windowValue.reset }) : null,
      ].filter(Boolean).join("\n"));
      row.track.style.setProperty("--opsail-refit-usage-remaining", `${windowValue.remaining}%`);
      row.track.setAttribute("aria-label", usageModel.formatMessage(copy.ariaProgress, windowValue));
      row.track.setAttribute("aria-valuemin", "0");
      row.track.setAttribute("aria-valuemax", "100");
      row.track.setAttribute("aria-valuenow", String(windowValue.remaining));
    }
    scheduleLayout();
  };

  const markReadFailure = () => {
    state.status = state.snapshot ? "stale" : "unavailable";
    render();
  };

  const bridgeSend = (requestId) => {
    const bridge = window.electronBridge;
    if (!bridge || typeof bridge.sendMessageFromView !== "function") {
      throw new Error("opsail-refit-codex-bridge-unavailable");
    }
    metrics.usageRequests += 1;
    return bridge.sendMessageFromView({
      type: "mcp-request",
      hostId: HOST_ID,
      request: { id: requestId, method: "account/rateLimits/read" },
    });
  };

  const coordinator = usageModel.createReadCoordinator({
    send: bridgeSend,
    onFailure: markReadFailure,
  });

  const handleMessage = (event) => {
    try {
      const payload = event?.data;
      if (!payload || payload.hostId !== HOST_ID) return;
      if (payload.type === "mcp-response") {
        const message = payload.message;
        if (!coordinator.finish(String(message?.id || ""))) return;
        const snapshot = usageModel.normalizeSnapshot(message?.result?.rateLimits);
        if (message?.error || !snapshot) {
          markReadFailure();
          return;
        }
        state.snapshot = snapshot;
        state.status = "ready";
        metrics.usageUpdates += 1;
        render();
        return;
      }
      if (payload.type !== "mcp-notification" || payload.method !== "account/rateLimits/updated") return;
      const snapshot = usageModel.normalizeSnapshot(payload.params?.rateLimits);
      if (snapshot) {
        state.snapshot = usageModel.mergeSnapshot(state.snapshot, snapshot);
        state.status = "ready";
        metrics.usageUpdates += 1;
        render();
      }
      coordinator.scheduleCalibration();
    } catch {
      markReadFailure();
    }
  };

  const ensure = () => {
    if (state.disposed || window[DISABLED_KEY]) return;
    metrics.ensureCalls += 1;
    ensureStyle();
    document.documentElement?.classList.add(ROOT_CLASS);
    if (!state.host || !state.details) createUi();
    const sidebar = document.querySelector(SIDEBAR_SELECTOR);
    if (!sidebar) {
      const sidebarChanged = state.sidebar !== null;
      state.sidebar = null;
      state.row = null;
      if (sidebarChanged) {
        observedMutationSidebar = undefined;
        observeMutations();
      }
      observeGeometry();
      hideHost();
      return;
    }
    if (state.sidebar !== sidebar) {
      state.sidebar = sidebar;
      state.row = null;
      observedMutationSidebar = undefined;
      observeMutations();
      observeGeometry();
    }
    render();
  };

  const scheduleEnsure = () => {
    if (state.disposed || scheduler.ensureTimeout !== null) return;
    scheduler.ensureTimeout = setTimeout(() => {
      scheduler.ensureTimeout = null;
      ensure();
    }, 96);
  };

  const observeMutations = () => {
    if (!mutationObserver || !document.documentElement) return;
    const nextSidebar = state.sidebar?.isConnected ? state.sidebar : null;
    if (observedMutationSidebar === nextSidebar) return;
    mutationObserver.disconnect();
    observedMutationSidebar = nextSidebar;
    if (!nextSidebar) {
      mutationObserver.observe(document.documentElement, { childList: true, subtree: true });
      return;
    }
    mutationObserver.observe(nextSidebar, { childList: true, subtree: true });
    for (let ancestor = nextSidebar.parentElement; ancestor; ancestor = ancestor.parentElement) {
      mutationObserver.observe(ancestor, { childList: true });
    }
  };

  const nodeMayAffectLayout = (node) => {
    if (!node || typeof node !== "object") return false;
    try {
      return Boolean(
        node.matches?.(AVATAR_SELECTOR)
        || node.matches?.(ACTION_SELECTOR)
        || node.querySelector?.(AVATAR_SELECTOR)
        || node.querySelector?.(ACTION_SELECTOR),
      );
    } catch {
      return true;
    }
  };

  const handleMutations = (records) => {
    if (!state.sidebar?.isConnected) {
      observedMutationSidebar = undefined;
      observeMutations();
      scheduleEnsure();
      return;
    }
    if (state.hasWindows && !state.host?.isConnected) {
      scheduleEnsure();
      return;
    }
    for (const record of records || []) {
      const changedNodes = [...(record.addedNodes || []), ...(record.removedNodes || [])];
      if (changedNodes.some((node) => node === state.sidebar || node?.contains?.(state.sidebar))) {
        observedMutationSidebar = undefined;
        observeMutations();
        scheduleLayout();
        return;
      }
      const inSidebar = record.target === state.sidebar || state.sidebar?.contains?.(record.target);
      if (!inSidebar) continue;
      for (const node of changedNodes) {
        if (nodeMayAffectLayout(node)) {
          scheduleLayout();
          return;
        }
      }
    }
  };

  const cleanup = () => {
    if (window[STATE_KEY]?.installToken !== installToken) return false;
    window[DISABLED_KEY] = true;
    state.disposed = true;
    coordinator.dispose();
    mutationObserver?.disconnect();
    observedMutationSidebar = undefined;
    resizeObserver?.disconnect();
    observedSidebar = null;
    observedRow = null;
    removeListeners();
    if (state.refreshTimer !== null) clearInterval(state.refreshTimer);
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    if (scheduler.ensureTimeout !== null) clearTimeout(scheduler.ensureTimeout);
    if (scheduler.timeout !== null) clearTimeout(scheduler.timeout);
    if (scheduler.frame !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(scheduler.frame);
    }
    if (scheduler.tooltipFrame !== null) {
      if (scheduler.tooltipFrameKind === "animation"
        && typeof cancelAnimationFrame === "function") {
        cancelAnimationFrame(scheduler.tooltipFrame);
      } else if (scheduler.tooltipFrameKind === "timeout") {
        clearTimeout(scheduler.tooltipFrame);
      }
    }
    scheduler.tooltipFrame = null;
    scheduler.tooltipFrameKind = null;
    state.host?.remove();
    state.details?.remove();
    document.getElementById(STYLE_ID)?.remove();
    document.documentElement?.classList.remove(ROOT_CLASS);
    delete window[STATE_KEY];
    return true;
  };

  addListener(window, "message", handleMessage);
  addListener(window, "focus", () => coordinator.focus());
  addListener(window, "resize", () => {
    scheduleLayout();
    scheduleTooltipPosition();
  });
  addListener(window, "scroll", scheduleTooltipPosition, { capture: true, passive: true });
  if (typeof MutationObserver === "function" && document.documentElement) {
    mutationObserver = new MutationObserver(handleMutations);
    observeMutations();
  }
  state.refreshTimer = setInterval(
    () => coordinator.visibleTick(document.visibilityState !== "hidden"),
    usageModel.VISIBLE_REFRESH_MS,
  );
  window[STATE_KEY] = {
    cleanup,
    ensure,
    installToken,
    mode: "usage",
    sessionMode: SESSION_MODE,
    managerToken: MANAGER_TOKEN,
    revision: REVISION,
    version: VERSION,
    diagnostics: () => ({
      installed: true,
      mode: "usage",
      sessionMode: SESSION_MODE,
      managerToken: MANAGER_TOKEN,
      revision: REVISION,
      hostCount: document.querySelectorAll(`#${USAGE_ID}`).length,
      styleCount: document.querySelectorAll(`#${STYLE_ID}`).length,
      detailsCount: document.querySelectorAll(`#${DETAILS_ID}`).length,
      listenerCount: listeners.length,
      mutationObserver: Boolean(mutationObserver),
      resizeObserver: Boolean(resizeObserver),
      refreshTimer: state.refreshTimer !== null,
      bridgeAvailable: typeof window.electronBridge?.sendMessageFromView === "function",
      dataState: state.status,
      visible: Boolean(state.hasWindows && !state.host?.hidden),
      stale: state.status === "stale",
    }),
    metrics,
  };
  ensure();
  coordinator.request();
  return window[STATE_KEY].diagnostics();
})();

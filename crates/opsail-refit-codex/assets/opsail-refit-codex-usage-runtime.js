(() => {
  __OPSAIL_REFIT_CODEX_MODEL_SOURCE__
  __OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__

  const STATE_KEY = "__OPSAIL_REFIT_CODEX_STATE__";
  const DISABLED_KEY = "__OPSAIL_REFIT_CODEX_DISABLED__";
  const STYLE_ID = "opsail-refit-codex-usage-style";
  const USAGE_ID = "opsail-refit-codex-usage";
  const DETAILS_ID = "opsail-refit-codex-usage-details";
  const NOTICE_ID = "opsail-refit-codex-launch-notice";
  const ROOT_CLASS = "opsail-refit-codex-usage-enabled";
  const HOST_ID = "local";
  const CONFIG_READ_METHOD = "config/read";
  const CONFIG_READ_STALE_MS = 15_000;
  const CONFIG_READ_GATE_MS = 1_000;
  const VERSION = __OPSAIL_REFIT_CODEX_VERSION_JSON__;
  const REVISION = __OPSAIL_REFIT_CODEX_REVISION_JSON__;
  const SESSION_MODE = __OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__;
  const MANAGER_TOKEN = __OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__;
  const CSS_TEXT = __OPSAIL_REFIT_CODEX_CSS_JSON__;
  const LOCALE_BUNDLE = __OPSAIL_REFIT_CODEX_LOCALES_JSON__;
  const installToken = {};
  const usageModel = createOpsailRefitCodexUsageModel(LOCALE_BUNDLE);
  const codexDom = createOpsailRefitCodexDomAdapter();
  let configuredLocale = null;
  const resolveCopy = () => usageModel.selectLocale(
    configuredLocale,
    ...codexDom.languageCandidates(),
  );
  let copy = resolveCopy();
  const syncLocale = () => {
    const nextCopy = resolveCopy();
    if (nextCopy === copy) return false;
    copy = nextCopy;
    return true;
  };

  try { window[STATE_KEY]?.cleanup?.(); } catch {}
  document.getElementById(USAGE_ID)?.remove();
  document.getElementById(DETAILS_ID)?.remove();
  document.getElementById(STYLE_ID)?.remove();
  document.getElementById(NOTICE_ID)?.remove();
  document.documentElement?.classList.remove(ROOT_CLASS);
  window[DISABLED_KEY] = false;

  const state = {
    disposed: false,
    status: "loading",
    snapshot: null,
    startupCalibrationAttempted: false,
    resetCredits: [],
    resetCreditState: "not-observed",
    presentResetCreditCount: 0,
    host: null,
    details: null,
    parts: null,
    sidebar: null,
    row: null,
    hasWindows: false,
    detailsOpen: false,
    closeTimer: null,
    resetCountdownTimer: null,
    refreshTimer: null,
    notice: null,
    noticeTimer: null,
    localeRefreshOnAccountRecovery: false,
  };
  const metrics = {
    ensureCalls: 0,
    layoutCalls: 0,
    localeRequests: 0,
    usageRequests: 0,
    usageUpdates: 0,
  };
  const listeners = [];
  let mutationObserver = null;
  let observedMutationAnchor;
  let observedMutationTargets = [];
  let accountRecoveryPath = [];
  let resizeObserver = null;
  let observedSidebar = null;
  let observedRow = null;
  let documentReadyListener = null;
  let configRequestId = null;
  let configRequestStartedAt = 0;
  let lastConfigReadAt = 0;
  let configRequestSequence = 0;
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

  const setText = (node, value) => {
    if (node && node.textContent !== value) node.textContent = value;
  };

  const createElement = (tagName, className = "") => {
    const element = document.createElement(tagName);
    if (className) element.className = className;
    return element;
  };

  const normalizeConfiguredLocale = (value) => {
    if (typeof value !== "string") return null;
    const locale = value.trim();
    if (locale.length === 0 || locale.length > 64) return null;
    return /^[A-Za-z0-9]+(?:[-_][A-Za-z0-9]+)*$/.test(locale) ? locale : null;
  };

  const requestConfiguredLocale = (force = false) => {
    if (state.disposed || window[DISABLED_KEY]) return false;
    const now = Date.now();
    if (configRequestId && now - configRequestStartedAt < CONFIG_READ_STALE_MS) {
      return false;
    }
    if (configRequestId) {
      configRequestId = null;
      configRequestStartedAt = 0;
    }
    if (!force && now - lastConfigReadAt < CONFIG_READ_GATE_MS) return false;
    const bridge = window.electronBridge;
    if (!bridge || typeof bridge.sendMessageFromView !== "function") return false;
    const requestId = `opsail-refit-codex-config:${now}:${++configRequestSequence}`;
    configRequestId = requestId;
    configRequestStartedAt = now;
    lastConfigReadAt = now;
    try {
      bridge.sendMessageFromView({
        type: "mcp-request",
        hostId: HOST_ID,
        request: {
          id: requestId,
          method: CONFIG_READ_METHOD,
          params: { includeLayers: false, cwd: null },
        },
      });
      metrics.localeRequests += 1;
      return true;
    } catch {
      if (configRequestId === requestId) {
        configRequestId = null;
        configRequestStartedAt = 0;
      }
      return false;
    }
  };

  const refreshConfiguredLocaleAfterRecovery = () => {
    if (!state.localeRefreshOnAccountRecovery || !state.row?.isConnected) return;
    if (requestConfiguredLocale(true)) state.localeRefreshOnAccountRecovery = false;
  };

  const removeLaunchNotice = () => {
    if (state.noticeTimer !== null) clearTimeout(state.noticeTimer);
    state.noticeTimer = null;
    state.notice?.remove();
    document.getElementById(NOTICE_ID)?.remove();
    state.notice = null;
  };

  const showLaunchNotice = () => {
    if (state.disposed || window[DISABLED_KEY]) return false;
    syncLocale();
    removeLaunchNotice();
    const notice = createElement("section", "opsail-refit-codex-launch-notice");
    notice.id = NOTICE_ID;
    notice.setAttribute("role", "status");
    notice.setAttribute("aria-live", "polite");
    notice.setAttribute("aria-atomic", "true");
    const title = createElement("strong", "opsail-refit-codex-launch-notice-title");
    const message = createElement("span", "opsail-refit-codex-launch-notice-message");
    setText(title, copy.launchNoticeTitle);
    setText(message, copy.launchNoticeMessage);
    notice.append(title, message);
    (document.body || document.documentElement).append(notice);
    state.notice = notice;
    state.noticeTimer = setTimeout(removeLaunchNotice, 2800);
    return true;
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
      const anchor = codexDom.elementRect(state.host);
      const sidebar = codexDom.elementRect(state.sidebar);
      const tooltip = codexDom.elementRect(state.details);
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
        tooltip: codexDom.elementRect(state.details),
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
    render();
    scheduleTooltipPosition();
  };

  const closeDetails = () => {
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    state.closeTimer = null;
    state.detailsOpen = false;
    clearResetCreditCountdown();
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
    const resetCreditSection = createElement(
      "section",
      "opsail-refit-codex-reset-credits",
    );
    resetCreditSection.hidden = true;
    const resetCreditTitle = createElement(
      "div",
      "opsail-refit-codex-reset-credits-title",
    );
    const resetCreditTable = createElement(
      "table",
      "opsail-refit-codex-reset-credits-table",
    );
    const resetCreditBody = createElement("tbody");
    resetCreditTable.append(resetCreditBody);
    resetCreditSection.append(resetCreditTitle, resetCreditTable);
    const timeFormatNote = createElement("p", "opsail-refit-codex-time-format-note");
    details.append(
      stale,
      ...rows.map((row) => row.row),
      resetCreditSection,
      timeFormatNote,
    );
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
    state.parts = {
      summary,
      stale,
      rows,
      resetCreditSection,
      resetCreditTitle,
      resetCreditTable,
      resetCreditBody,
      timeFormatNote,
    };
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

  const rememberAccountRow = (row) => {
    if (!row?.isConnected) return;
    const path = [];
    for (let current = row.parentElement; current; current = current.parentElement) {
      path.push(current);
    }
    accountRecoveryPath = path;
  };

  const layout = () => {
    metrics.layoutCalls += 1;
    const host = state.host;
    const sidebar = state.sidebar;
    const previousRow = state.row;
    if (previousRow && !previousRow.isConnected) {
      state.localeRefreshOnAccountRecovery = true;
    }
    const wasInline = host?.dataset.opsailRefitCodexLayout === "inline"
      && previousRow?.isConnected
      && host.parentElement === previousRow;
    state.row = null;
    if (!host || !sidebar || !state.hasWindows) {
      if (host && sidebar) {
        const measured = codexDom.measureNativeLayout(sidebar, previousRow);
        state.row = measured?.row?.isConnected ? measured.row : null;
        rememberAccountRow(state.row);
        refreshConfiguredLocaleAfterRecovery();
      }
      hideHost();
      observeMutations();
      observeGeometry();
      return;
    }

    const measurementRoot = document.body || document.documentElement;
    if (!wasInline && host.parentElement !== measurementRoot) measurementRoot.append(host);
    host.dataset.opsailRefitCodexLayout = wasInline ? "inline" : "measuring";
    host.hidden = false;
    host.style.visibility = "hidden";
    host.style.removeProperty("--opsail-refit-usage-left");
    host.style.removeProperty("--opsail-refit-usage-top");
    host.style.removeProperty("--opsail-refit-usage-inline-max-width");

    const measured = codexDom.measureNativeLayout(sidebar, previousRow);
    state.row = measured?.row?.isConnected ? measured.row : null;
    rememberAccountRow(state.row);
    refreshConfiguredLocaleAfterRecovery();
    const hostRect = codexDom.elementRect(host);
    const capsuleWidth = Math.max(
      hostRect?.width || 0,
      Number(state.parts?.summary?.scrollWidth) || 0,
    );
    let mounted = false;

    if (measured?.row && measured.accountSlot && measured.trailingSlot && capsuleWidth > 0) {
      const maximumInlineWidth = Math.max(
        usageModel.MIN_INLINE_CAPSULE_WIDTH,
        Math.min(112, measured.sidebarRect.width * 0.42),
      );
      host.style.setProperty(
        "--opsail-refit-usage-inline-max-width",
        `${Math.floor(maximumInlineWidth)}px`,
      );
      if (host.parentElement !== measured.row
        || host.nextElementSibling !== measured.trailingSlot) {
        measured.row.insertBefore(host, measured.trailingSlot);
      }
      host.dataset.opsailRefitCodexLayout = "inline";
      mounted = usageModel.isSafeInlineCapsuleLayout({
        accountSlot: codexDom.elementRect(measured.accountSlot),
        avatar: codexDom.elementRect(measured.avatar?.element),
        host: codexDom.elementRect(host),
        trailingSlot: codexDom.elementRect(measured.trailingSlot),
        sidebar: measured.sidebarRect,
        viewportBottom: Number(window.innerHeight) || measured.sidebarRect.bottom,
      });
    }

    if (!mounted && !measured?.row
      && measured?.accountControl && measured.trailingAction && capsuleWidth > 0) {
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
        const fallbackRect = codexDom.elementRect(host);
        mounted = Boolean(
          fallbackRect
          && fallbackRect.left >= measured.sidebarRect.left
          && fallbackRect.right <= measured.sidebarRect.right
          && fallbackRect.left >= leftBoundary
          && fallbackRect.right <= rightBoundary
          && fallbackRect.top >= minimumTop
          && fallbackRect.bottom <= maximumBottom,
        );
      }
    }

    host.style.visibility = "";
    host.hidden = !mounted;
    if (!mounted) closeDetails();
    if (mounted) scheduleTooltipPosition();
    observeMutations();
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

  const clearResetCreditCountdown = () => {
    if (state.resetCountdownTimer !== null) clearTimeout(state.resetCountdownTimer);
    state.resetCountdownTimer = null;
  };

  const scheduleDetailCountdown = (windows, resetCredits) => {
    clearResetCreditCountdown();
    if (state.disposed
      || !state.detailsOpen
      || document.visibilityState === "hidden"
    ) return;
    const updates = [
      ...(Array.isArray(windows) ? windows : [])
        .map((windowValue) => windowValue.reset?.nextUpdateMs),
      ...(Array.isArray(resetCredits) ? resetCredits : [])
        .map((credit) => credit.nextUpdateMs),
    ].filter(Number.isFinite);
    if (updates.length === 0) return;
    const delay = Math.max(1000, Math.min(...updates));
    if (!Number.isFinite(delay)) return;
    state.resetCountdownTimer = setTimeout(() => {
      state.resetCountdownTimer = null;
      if (document.visibilityState !== "hidden") render();
    }, delay);
  };

  const refreshResetCreditObservation = (presentCount) => {
    if (state.resetCreditState === "not-observed") {
      state.presentResetCreditCount = 0;
      return;
    }
    const count = Number.isInteger(presentCount) && presentCount >= 0
      ? presentCount
      : state.resetCredits.filter((credit) => (
        Number.isFinite(credit?.expiresAt) && credit.expiresAt * 1000 > Date.now()
      )).length;
    state.presentResetCreditCount = count;
    state.resetCreditState = count > 0 ? "available" : "empty";
  };

  const renderResetCredits = () => {
    if (!state.parts) return [];
    const resetCredits = usageModel.presentResetCredits(
      state.resetCredits,
      copy,
      copy.locale,
    );
    refreshResetCreditObservation(resetCredits.length);
    state.parts.resetCreditSection.hidden = resetCredits.length === 0;
    state.parts.resetCreditSection.setAttribute("aria-label", copy.resetCreditsTitle);
    state.parts.resetCreditTable.setAttribute("aria-label", copy.resetCreditsTitle);
    setText(state.parts.resetCreditTitle, copy.resetCreditsTitle);
    const resetCreditRows = resetCredits.map((credit) => ({
      ariaCountdown: usageModel.formatMessage(copy.resetCreditCountdown, credit),
      ariaExpiry: usageModel.formatMessage(copy.resetCreditExpires, {
        ...credit,
        dateTime: credit.full,
      }),
      credit,
    }));
    const resetCreditsKey = `${copy.locale}:${resetCreditRows
      .map(({ ariaCountdown, ariaExpiry, credit }) => (
        `${credit.expiresAt}:${credit.dateTime}:${credit.countdown}:${ariaExpiry}:${ariaCountdown}`
      ))
      .join("|")}`;
    if (state.parts.resetCreditBody.dataset.opsailRefitCodexCredits !== resetCreditsKey) {
      while (state.parts.resetCreditBody.children.length > 0) {
        state.parts.resetCreditBody.children[0].remove();
      }
      for (const { ariaCountdown, ariaExpiry, credit } of resetCreditRows) {
        const item = createElement("tr", "opsail-refit-codex-reset-credits-row");
        const expiryCell = createElement(
          "td",
          "opsail-refit-codex-reset-credits-expiry",
        );
        const countdownCell = createElement(
          "td",
          "opsail-refit-codex-reset-credits-countdown",
        );
        setText(expiryCell, credit.dateTime);
        setText(countdownCell, credit.countdown);
        item.append(expiryCell, countdownCell);
        item.setAttribute("aria-label", usageModel.formatMessage(copy.resetCreditAria, {
          countdown: ariaCountdown,
          expiry: ariaExpiry,
        }));
        state.parts.resetCreditBody.append(item);
      }
      state.parts.resetCreditBody.dataset.opsailRefitCodexCredits = resetCreditsKey;
    }
    return resetCredits;
  };

  const render = () => {
    syncLocale();
    if (!state.parts || !state.host || !state.details) return;
    const windows = usageModel.presentWindows(state.snapshot, copy, copy.locale);
    const resetCredits = renderResetCredits();
    const stale = state.status === "stale" && windows.length > 0;
    state.hasWindows = windows.length > 0;
    state.details.setAttribute("aria-label", copy.usageTitle);
    state.host.dataset.opsailRefitCodexStale = String(stale);
    state.host.dataset.opsailRefitCodexState = state.status;
    state.parts.stale.hidden = !stale;
    setText(state.parts.stale, stale ? copy.stale : "");
    setText(state.parts.timeFormatNote, copy.timeFormatNote);
    if (windows.length === 0) {
      clearResetCreditCountdown();
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
      const resetCountdownLine = windowValue.reset?.countdown
        ? usageModel.formatMessage(copy.windowResetCountdown, windowValue.reset)
        : null;
      const resetLine = windowValue.reset
        ? usageModel.formatMessage(copy.windowReset, windowValue.reset)
        : null;
      setText(row.meta, [
        usageModel.formatMessage(copy.used, windowValue),
        resetCountdownLine,
        resetLine,
      ].filter(Boolean).join("\n"));
      if (windowValue.reset) {
        row.meta.setAttribute("aria-label", usageModel.formatMessage(copy.ariaMeta, {
          ...windowValue,
          time: windowValue.reset.full,
        }));
      } else {
        row.meta.removeAttribute?.("aria-label");
      }
      row.track.style.setProperty("--opsail-refit-usage-remaining", `${windowValue.remaining}%`);
      row.track.setAttribute("aria-label", usageModel.formatMessage(copy.ariaProgress, windowValue));
      row.track.setAttribute("aria-valuemin", "0");
      row.track.setAttribute("aria-valuemax", "100");
      row.track.setAttribute("aria-valuenow", String(windowValue.remaining));
    }
    scheduleDetailCountdown(windows, resetCredits);
    scheduleLayout();
  };

  const hasPresentableSnapshot = () => usageModel.hasPresentableWindows(state.snapshot);

  const scheduleStartupCalibration = () => {
    if (hasPresentableSnapshot() || state.startupCalibrationAttempted) return false;
    state.startupCalibrationAttempted = true;
    coordinator.scheduleCalibration();
    return true;
  };

  const markReadFailure = () => {
    const hasSnapshot = hasPresentableSnapshot();
    if (!hasSnapshot) scheduleStartupCalibration();
    state.status = hasSnapshot ? "stale" : "unavailable";
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

  const mergeResetCredits = (value) => {
    const resetCredits = usageModel.normalizeResetCreditsUpdate(value);
    if (resetCredits === null) return false;
    state.resetCredits = resetCredits;
    state.resetCreditState = resetCredits.length > 0 ? "available" : "empty";
    return true;
  };

  const mergeUsagePayload = (result, {
    mergeWindows = false,
  } = {}) => {
    const snapshot = usageModel.normalizeSnapshot(result?.rateLimits);
    const resetCreditsAccepted = mergeResetCredits(result?.rateLimitResetCredits);
    if (snapshot) {
      state.snapshot = mergeWindows
        ? usageModel.mergeSnapshot(state.snapshot, snapshot)
        : snapshot;
    }
    const hasSnapshot = hasPresentableSnapshot();
    if (hasSnapshot) state.startupCalibrationAttempted = true;
    else if (snapshot && !mergeWindows) scheduleStartupCalibration();
    const accepted = Boolean(snapshot) || resetCreditsAccepted;
    if (accepted) {
      state.status = hasSnapshot ? "ready" : "unavailable";
      metrics.usageUpdates += 1;
      render();
    }
    return accepted;
  };

  const handleMessage = (event) => {
    try {
      const payload = event?.data;
      if (!payload || payload.hostId !== HOST_ID) return;
      if (payload.type === "mcp-response") {
        const message = payload.message;
        const messageId = String(message?.id || "");
        if (messageId === configRequestId) {
          configRequestId = null;
          configRequestStartedAt = 0;
          if (!message?.error) {
            const nextLocale = normalizeConfiguredLocale(
              message?.result?.config?.desktop?.localeOverride,
            );
            if (configuredLocale !== nextLocale) {
              configuredLocale = nextLocale;
              render();
            }
          }
          refreshConfiguredLocaleAfterRecovery();
          return;
        }
        if (!coordinator.finish(messageId)) return;
        if (message?.error) {
          markReadFailure();
          return;
        }
        if (!mergeUsagePayload(message?.result)) {
          markReadFailure();
        }
        return;
      }
      if (payload.type !== "mcp-notification" || payload.method !== "account/rateLimits/updated") return;
      mergeUsagePayload(payload.params, { mergeWindows: true });
      coordinator.scheduleCalibration();
    } catch {
      markReadFailure();
    }
  };

  const waitForDocument = () => {
    if ((document.documentElement && document.readyState === "complete")
      || documentReadyListener !== null) return;
    documentReadyListener = () => {
      if (!document.documentElement) return;
      window.removeEventListener("load", documentReadyListener);
      documentReadyListener = null;
      ensure();
      requestConfiguredLocale(true);
      if (!hasPresentableSnapshot()) coordinator.request();
    };
    window.addEventListener("load", documentReadyListener);
  };

  const ensure = () => {
    if (state.disposed || window[DISABLED_KEY]) return;
    if (!document.documentElement) {
      waitForDocument();
      return;
    }
    metrics.ensureCalls += 1;
    ensureStyle();
    document.documentElement?.classList.add(ROOT_CLASS);
    if (!state.host || !state.details) createUi();
    if (!mutationObserver && typeof MutationObserver === "function") {
      mutationObserver = new MutationObserver(handleMutations);
      observeMutations();
    }
    const sidebar = codexDom.findSidebar(document);
    if (!sidebar) {
      const sidebarChanged = state.sidebar !== null;
      if (state.row || accountRecoveryPath.length > 0) {
        state.localeRefreshOnAccountRecovery = true;
      }
      state.sidebar = null;
      state.row = null;
      if (sidebarChanged) {
        observedMutationAnchor = undefined;
        observedMutationTargets = [];
        observeMutations();
      }
      observeGeometry();
      hideHost();
      return;
    }
    if (state.sidebar !== sidebar) {
      state.sidebar = sidebar;
      state.row = null;
      observedMutationAnchor = undefined;
      observedMutationTargets = [];
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
    const nextRow = state.row?.isConnected ? state.row : null;
    const sidebarMissing = !nextSidebar;
    const recoveryAnchor = (sidebarMissing ? document.body : null)
      || accountRecoveryPath.find((element) => (
        element?.isConnected
        && (element === nextSidebar || nextSidebar?.contains?.(element))
      ))
      || nextSidebar
      || accountRecoveryPath.find((element) => element?.isConnected)
      || document.body
      || document.documentElement;
    const nextAnchor = nextRow || recoveryAnchor;
    const nextTargets = [];
    if (nextRow) nextTargets.push({ subtree: true, target: nextRow });
    const bootstrapping = !nextRow && accountRecoveryPath.length === 0;
    if ((sidebarMissing || bootstrapping) && recoveryAnchor) {
      nextTargets.push({ subtree: true, target: recoveryAnchor });
    }
    for (
      let current = sidebarMissing || bootstrapping
        ? recoveryAnchor?.parentElement
        : nextRow?.parentElement || recoveryAnchor;
      current && current !== document.documentElement;
      current = current.parentElement
    ) {
      if (!nextTargets.some((entry) => entry.target === current)) {
        nextTargets.push({ subtree: false, target: current });
      }
    }
    nextTargets.push({ subtree: false, target: document.documentElement });
    const unchanged = observedMutationAnchor === nextAnchor
      && observedMutationTargets.length === nextTargets.length
      && observedMutationTargets.every((entry, index) => (
        entry.target === nextTargets[index].target
        && entry.subtree === nextTargets[index].subtree
      ));
    if (unchanged) return;
    mutationObserver.disconnect();
    observedMutationAnchor = nextAnchor;
    observedMutationTargets = nextTargets;
    for (const { subtree, target } of nextTargets) {
      if (target === document.documentElement) {
        mutationObserver.observe(target, {
          attributes: true,
          attributeFilter: ["lang"],
          childList: true,
        });
      } else {
        mutationObserver.observe(target, { childList: true, subtree });
      }
    }
  };

  const handleMutations = (records) => {
    if ([...(records || [])].some((record) => record.type === "attributes"
      && record.target === document.documentElement
      && record.attributeName === "lang")) {
      render();
      return;
    }
    const mutationNodes = [...(records || [])].flatMap((record) => [
      ...(record.addedNodes || []),
      ...(record.removedNodes || []),
    ]);
    if (!state.sidebar?.isConnected) {
      if (state.row || accountRecoveryPath.length > 0) {
        state.localeRefreshOnAccountRecovery = true;
      }
      state.sidebar = null;
      state.row = null;
      observeMutations();
      if (mutationNodes.some(codexDom.nodeMayContainSidebar)) {
        scheduleEnsure();
      }
      return;
    }
    if (state.row && !state.row.isConnected) {
      state.localeRefreshOnAccountRecovery = true;
      state.row = null;
      observeMutations();
      scheduleEnsure();
      return;
    }
    if (!state.row?.isConnected && accountRecoveryPath.length === 0) {
      if (mutationNodes.some(codexDom.nodeMayAffectLayout)) scheduleEnsure();
      return;
    }
    if (state.hasWindows && !state.host?.isConnected) {
      scheduleEnsure();
      return;
    }
    for (const record of records || []) {
      const changedNodes = [...(record.addedNodes || []), ...(record.removedNodes || [])];
      if (!state.row?.isConnected) {
        const touchesRecoveryAnchor = record.target === observedMutationAnchor
          || changedNodes.some((node) => (
            node === observedMutationAnchor
            || node?.contains?.(observedMutationAnchor)
            || node === state.sidebar
            || node?.contains?.(state.sidebar)
          ));
        if (touchesRecoveryAnchor) {
          scheduleEnsure();
          return;
        }
        continue;
      }
      const inAccountRow = record.target === state.row || state.row.contains?.(record.target);
      if (!inAccountRow) continue;
      for (const node of changedNodes) {
        if (codexDom.nodeMayAffectLayout(node)) {
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
    configRequestId = null;
    configRequestStartedAt = 0;
    if (documentReadyListener !== null) {
      window.removeEventListener("load", documentReadyListener);
      documentReadyListener = null;
    }
    coordinator.dispose();
    mutationObserver?.disconnect();
    observedMutationAnchor = undefined;
    observedMutationTargets = [];
    accountRecoveryPath = [];
    resizeObserver?.disconnect();
    observedSidebar = null;
    observedRow = null;
    removeListeners();
    if (state.refreshTimer !== null) clearInterval(state.refreshTimer);
    if (state.closeTimer !== null) clearTimeout(state.closeTimer);
    clearResetCreditCountdown();
    removeLaunchNotice();
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
  addListener(window, "focus", () => {
    requestConfiguredLocale();
    coordinator.focus();
    render();
  });
  addListener(window, "resize", () => {
    scheduleLayout();
    scheduleTooltipPosition();
  });
  addListener(window, "scroll", scheduleTooltipPosition, { capture: true, passive: true });
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
    showLaunchNotice,
    revision: REVISION,
    version: VERSION,
    diagnostics: () => {
      refreshResetCreditObservation();
      return {
        installed: true,
        mode: "usage",
        sessionMode: SESSION_MODE,
        managerToken: MANAGER_TOKEN,
        revision: REVISION,
        domAdapterVersion: codexDom.VERSION,
        language: copy.locale,
        hostCount: document.querySelectorAll(`#${USAGE_ID}`).length,
        styleCount: document.querySelectorAll(`#${STYLE_ID}`).length,
        detailsCount: document.querySelectorAll(`#${DETAILS_ID}`).length,
        listenerCount: listeners.length,
        mutationObserver: Boolean(mutationObserver),
        resizeObserver: Boolean(resizeObserver),
        refreshTimer: state.refreshTimer !== null,
        resetCountdownTimer: state.resetCountdownTimer !== null,
        bridgeAvailable: typeof window.electronBridge?.sendMessageFromView === "function",
        dataState: state.status,
        visible: Boolean(state.hasWindows && state.host?.isConnected && !state.host.hidden),
        stale: state.status === "stale",
        resetCreditCount: state.presentResetCreditCount,
        resetCreditState: state.resetCreditState,
      };
    },
    metrics,
  };
  waitForDocument();
  ensure();
  requestConfiguredLocale(true);
  coordinator.request();
  return window[STATE_KEY].diagnostics();
})();

const createOpsailRefitCodexUsageModel = (localeBundle) => {
  const REQUEST_TIMEOUT_MS = 15 * 1000;
  const FOCUS_REFRESH_MIN_MS = 60 * 1000;
  const NOTIFICATION_CALIBRATION_MS = 1200;
  const VISIBLE_REFRESH_MS = 15 * 60 * 1000;
  const REQUEST_ID_PREFIX = "opsail-refit-codex-rate-limits";
  const MIN_INLINE_CAPSULE_WIDTH = 36;
  const MIN_ACCOUNT_SLOT_WIDTH = 32;

  const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));
  const normalizedLanguage = (value) => String(value || "").trim().replaceAll("_", "-").toLowerCase();
  const formatMessage = (template, values = {}) => String(template || "").replace(
    /\{([A-Za-z][A-Za-z0-9]*)\}/g,
    (_match, key) => Object.hasOwn(values, key) ? String(values[key]) : "",
  );

  const selectLocale = (...languageCandidates) => {
    const locales = localeBundle?.locales && typeof localeBundle.locales === "object"
      ? localeBundle.locales
      : {};
    const entries = Object.entries(locales);
    for (const candidate of languageCandidates) {
      const normalized = normalizedLanguage(candidate);
      if (!normalized) continue;
      const exact = entries.find(([key, value]) =>
        normalizedLanguage(key) === normalized || normalizedLanguage(value?.locale) === normalized);
      if (exact) return exact[1];
      const base = normalized.split("-")[0];
      const baseMatch = entries.find(([key, value]) =>
        normalizedLanguage(key).split("-")[0] === base
        || normalizedLanguage(value?.locale).split("-")[0] === base);
      if (baseMatch) return baseMatch[1];
    }
    return locales[localeBundle?.defaultLocale] || entries[0]?.[1] || {};
  };

  const normalizeWindow = (value) => {
    if (!value || typeof value !== "object") return null;
    const presence = {
      usedPercent: Object.hasOwn(value, "usedPercent"),
      windowDurationMins: Object.hasOwn(value, "windowDurationMins"),
      resetsAt: Object.hasOwn(value, "resetsAt"),
    };
    const usedPercent = value.usedPercent;
    const duration = value.windowDurationMins;
    const resetsAt = value.resetsAt;
    return {
      usedPercent: presence.usedPercent
        && typeof usedPercent === "number"
        && Number.isFinite(usedPercent)
        ? clamp(usedPercent, 0, 100)
        : null,
      windowDurationMins: presence.windowDurationMins
        && typeof duration === "number"
        && Number.isFinite(duration)
        && duration > 0
        ? duration
        : null,
      resetsAt: presence.resetsAt
        && typeof resetsAt === "number"
        && Number.isFinite(resetsAt)
        && resetsAt > 0
        ? resetsAt
        : null,
      presence,
    };
  };

  const normalizeSnapshot = (value) => {
    if (!value || typeof value !== "object") return null;
    const presence = {
      primary: Object.hasOwn(value, "primary"),
      secondary: Object.hasOwn(value, "secondary"),
    };
    if (!presence.primary && !presence.secondary) return null;
    return {
      primary: presence.primary ? normalizeWindow(value.primary) : null,
      secondary: presence.secondary ? normalizeWindow(value.secondary) : null,
      presence,
    };
  };

  const mergeSnapshot = (current, incoming) => {
    if (!incoming) return current || null;
    if (!current) return incoming;
    const mergeWindow = (currentWindow, incomingWindow, incomingPresent) => {
      if (!incomingPresent) return currentWindow;
      if (!incomingWindow) return null;
      const currentPresence = currentWindow?.presence || {};
      return {
        usedPercent: incomingWindow.presence.usedPercent
          ? incomingWindow.usedPercent
          : currentWindow?.usedPercent ?? null,
        windowDurationMins: incomingWindow.presence.windowDurationMins
          ? incomingWindow.windowDurationMins
          : currentWindow?.windowDurationMins ?? null,
        resetsAt: incomingWindow.presence.resetsAt
          ? incomingWindow.resetsAt
          : currentWindow?.resetsAt ?? null,
        presence: {
          usedPercent: Boolean(
            currentPresence.usedPercent || incomingWindow.presence.usedPercent
          ),
          windowDurationMins: Boolean(
            currentPresence.windowDurationMins || incomingWindow.presence.windowDurationMins
          ),
          resetsAt: Boolean(currentPresence.resetsAt || incomingWindow.presence.resetsAt),
        },
      };
    };
    return {
      primary: mergeWindow(current.primary, incoming.primary, incoming.presence.primary),
      secondary: mergeWindow(current.secondary, incoming.secondary, incoming.presence.secondary),
      presence: {
        primary: Boolean(current.presence.primary || incoming.presence.primary),
        secondary: Boolean(current.presence.secondary || incoming.presence.secondary),
      },
    };
  };

  const labelForDuration = (duration, copy, collection = "windowLabels") => {
    const labels = copy?.[collection] || copy?.windowLabels || {};
    if (duration !== null && Math.abs(duration - 300) <= 15) return labels.fiveHours;
    if (duration !== null && Math.abs(duration - 10080) <= 504) return labels.weekly;
    if (duration !== null && Math.abs(duration - 1440) <= 72) return labels.daily;
    if (duration !== null && Math.abs(duration - 43200) <= 2160) return labels.monthly;
    if (duration !== null && duration >= 60 && Number.isInteger(duration / 60)) {
      return formatMessage(labels.hours, { value: duration / 60 });
    }
    if (duration !== null) return formatMessage(labels.minutes, { value: Math.round(duration) });
    return labels.generic;
  };

  const formatRelativeReset = (milliseconds, copy) => {
    const units = copy?.relativeUnits;
    if (!units || ![units.day, units.hour, units.minute, units.separator]
      .every((value) => typeof value === "string")) return null;
    const totalMinutes = Math.max(1, Math.ceil(milliseconds / (60 * 1000)));
    const days = Math.floor(totalMinutes / (24 * 60));
    const hours = Math.floor((totalMinutes % (24 * 60)) / 60);
    const minutes = totalMinutes % 60;
    const parts = [];
    if (days > 0) {
      parts.push(`${days}${units.day}`);
      if (hours > 0) parts.push(`${hours}${units.hour}`);
    } else if (hours > 0) {
      parts.push(`${hours}${units.hour}`);
      if (minutes > 0) parts.push(`${minutes}${units.minute}`);
    } else {
      parts.push(`${minutes || 1}${units.minute}`);
    }
    return parts.join(units.separator);
  };

  const formatReset = (resetsAt, copy, displayLocale, nowMs = Date.now()) => {
    if (resetsAt === null) return null;
    const value = new Date(resetsAt * 1000);
    if (!Number.isFinite(value.getTime())) return null;
    const locale = normalizedLanguage(displayLocale) ? displayLocale : undefined;
    try {
      const full = new Intl.DateTimeFormat(locale, {
        dateStyle: "full",
        timeStyle: "long",
      }).format(value);
      const date = new Intl.DateTimeFormat(locale, {
        month: "short",
        day: "numeric",
      }).format(value);
      const weekday = new Intl.DateTimeFormat(locale, {
        weekday: "short",
      }).format(value);
      const time = new Intl.DateTimeFormat(locale, {
        hour: "numeric",
        minute: "2-digit",
      }).format(value);
      const remainingMilliseconds = value.getTime()
        - (Number.isFinite(nowMs) ? nowMs : Date.now());
      return {
        date,
        full,
        relative: remainingMilliseconds > 0
          ? formatRelativeReset(remainingMilliseconds, copy)
          : null,
        soon: remainingMilliseconds <= 0,
        time,
        weekday,
      };
    } catch {
      try {
        const full = value.toLocaleString(locale);
        return {
          date: full,
          full,
          relative: null,
          soon: false,
          time: "",
          weekday: "",
        };
      } catch {
        return null;
      }
    }
  };

  const presentWindows = (snapshot, copy, displayLocale, nowMs = Date.now()) => [
    snapshot?.primary,
    snapshot?.secondary,
  ]
    .filter((windowValue) => windowValue
      && typeof windowValue.usedPercent === "number"
      && Number.isFinite(windowValue.usedPercent))
    .sort((left, right) =>
      (left.windowDurationMins ?? Number.MAX_SAFE_INTEGER)
      - (right.windowDurationMins ?? Number.MAX_SAFE_INTEGER))
    .map((windowValue) => {
      const used = Math.round(windowValue.usedPercent);
      const remaining = Math.round(100 - windowValue.usedPercent);
      return {
        label: labelForDuration(windowValue.windowDurationMins, copy),
        summaryLabel: labelForDuration(
          windowValue.windowDurationMins,
          copy,
          "summaryWindowLabels",
        ),
        used,
        remaining,
        reset: formatReset(windowValue.resetsAt, copy, displayLocale, nowMs),
      };
    });

  const summaryFor = (windows, copy) => windows
    .map((windowValue) => formatMessage(copy?.summaryItem, {
      ...windowValue,
      label: windowValue.summaryLabel || windowValue.label,
    }))
    .join(" / ");

  const finiteRect = (rect) => {
    if (!rect || ![rect.left, rect.top, rect.right, rect.bottom].every(Number.isFinite)) return null;
    const width = Number.isFinite(rect.width) ? rect.width : rect.right - rect.left;
    const height = Number.isFinite(rect.height) ? rect.height : rect.bottom - rect.top;
    if (width < 0 || height < 0) return null;
    return { ...rect, width, height };
  };

  const computeTooltipPlacement = ({ anchor, sidebar, viewport, tooltip, gap = 8 }) => {
    const anchorRect = finiteRect(anchor);
    const sidebarRect = finiteRect(sidebar);
    const viewportRect = finiteRect(viewport);
    if (!anchorRect || !sidebarRect || !viewportRect) return null;
    const horizontalInset = 8;
    const maximumWidth = Math.max(1, Math.min(
      240,
      sidebarRect.width - horizontalInset * 2,
      viewportRect.width - horizontalInset * 2,
    ));
    const requestedWidth = Number.isFinite(tooltip?.width) && tooltip.width > 0
      ? tooltip.width
      : maximumWidth;
    const width = Math.max(1, Math.min(requestedWidth, maximumWidth));
    const maximumHeight = Math.max(1, Math.min(
      sidebarRect.height - horizontalInset * 2,
      viewportRect.height - horizontalInset * 2,
    ));
    const requestedHeight = Number.isFinite(tooltip?.height) && tooltip.height > 0
      ? tooltip.height
      : 1;
    const height = Math.min(requestedHeight, maximumHeight);
    const minimumLeft = Math.max(sidebarRect.left + horizontalInset, viewportRect.left + horizontalInset);
    const maximumLeft = Math.min(
      sidebarRect.right - width - horizontalInset,
      viewportRect.right - width - horizontalInset,
    );
    const left = maximumLeft >= minimumLeft
      ? clamp(anchorRect.left, minimumLeft, maximumLeft)
      : clamp(anchorRect.left, viewportRect.left, Math.max(viewportRect.left, viewportRect.right - width));
    const minimumTop = Math.max(sidebarRect.top + horizontalInset, viewportRect.top + horizontalInset);
    const maximumTop = Math.max(minimumTop, Math.min(
      sidebarRect.bottom - height - horizontalInset,
      viewportRect.bottom - height - horizontalInset,
    ));
    const above = anchorRect.top - height - gap;
    const below = anchorRect.bottom + gap;
    const preferredTop = above >= minimumTop ? above : below;
    return {
      left: clamp(left, viewportRect.left, viewportRect.right - width),
      top: clamp(preferredTop, minimumTop, maximumTop),
      width,
      maximumHeight,
    };
  };

  const canFitCapsule = ({ leftBoundary, rightBoundary, capsuleWidth, gap = 8 }) => [
    leftBoundary,
    rightBoundary,
    capsuleWidth,
    gap,
  ].every(Number.isFinite) && rightBoundary - leftBoundary >= capsuleWidth + gap * 2;

  const isSafeInlineCapsuleLayout = ({
    accountSlot,
    avatar,
    host,
    trailingSlot,
    sidebar,
    viewportBottom,
    minimumHostWidth = MIN_INLINE_CAPSULE_WIDTH,
    minimumAccountWidth = MIN_ACCOUNT_SLOT_WIDTH,
  }) => {
    const accountRect = finiteRect(accountSlot);
    const avatarRect = finiteRect(avatar);
    const hostRect = finiteRect(host);
    const trailingRect = finiteRect(trailingSlot);
    const sidebarRect = finiteRect(sidebar);
    if (!accountRect || !avatarRect || !hostRect || !trailingRect || !sidebarRect) return false;
    if (![viewportBottom, minimumHostWidth, minimumAccountWidth].every(Number.isFinite)) {
      return false;
    }
    const maximumBottom = Math.min(viewportBottom, sidebarRect.bottom);
    return accountRect.width >= minimumAccountWidth
      && hostRect.width >= minimumHostWidth
      && accountRect.left >= sidebarRect.left
      && accountRect.right <= hostRect.left
      && avatarRect.left >= accountRect.left
      && avatarRect.right <= accountRect.right
      && avatarRect.top >= accountRect.top
      && avatarRect.bottom <= accountRect.bottom
      && hostRect.right <= trailingRect.left
      && trailingRect.right <= sidebarRect.right
      && hostRect.left >= sidebarRect.left
      && hostRect.right <= sidebarRect.right
      && hostRect.top >= Math.max(0, sidebarRect.top)
      && hostRect.bottom <= maximumBottom;
  };

  const createReadCoordinator = ({
    now = () => Date.now(),
    setTimer = setTimeout,
    clearTimer = clearTimeout,
    send,
    onFailure = () => {},
  }) => {
    let disposed = false;
    let sequence = 0;
    let inFlight = null;
    let requestTimeout = null;
    let calibrationTimer = null;
    let calibrationPending = false;
    let calibrationReady = false;
    let lastRequestedAt = Number.NEGATIVE_INFINITY;

    const clearRequest = (requestId) => {
      if (inFlight !== requestId) return false;
      if (requestTimeout !== null) clearTimer(requestTimeout);
      requestTimeout = null;
      inFlight = null;
      return true;
    };

    const request = () => {
      if (disposed || inFlight !== null) return null;
      const requestedAt = now();
      const requestId = `${REQUEST_ID_PREFIX}:${requestedAt}:${++sequence}`;
      inFlight = requestId;
      lastRequestedAt = requestedAt;
      requestTimeout = setTimer(() => {
        if (!clearRequest(requestId)) return;
        onFailure(requestId);
        if (calibrationPending && calibrationReady) requestCalibration();
      }, REQUEST_TIMEOUT_MS);
      try {
        Promise.resolve(send(requestId)).catch(() => {
          if (!clearRequest(requestId)) return;
          onFailure(requestId);
          if (calibrationPending && calibrationReady) requestCalibration();
        });
      } catch {
        if (clearRequest(requestId)) onFailure(requestId);
      }
      return requestId;
    };

    const requestCalibration = () => {
      if (disposed || !calibrationPending || !calibrationReady || inFlight !== null) return null;
      calibrationPending = false;
      calibrationReady = false;
      return request();
    };

    const finish = (requestId) => {
      if (!clearRequest(requestId)) return false;
      requestCalibration();
      return true;
    };

    const scheduleCalibration = () => {
      calibrationPending = true;
      calibrationReady = false;
      if (calibrationTimer !== null) clearTimer(calibrationTimer);
      calibrationTimer = setTimer(() => {
        calibrationTimer = null;
        calibrationReady = true;
        requestCalibration();
      }, NOTIFICATION_CALIBRATION_MS);
    };

    const focus = () => now() - lastRequestedAt >= FOCUS_REFRESH_MIN_MS ? request() : null;
    const visibleTick = (visible) => visible ? request() : null;
    const dispose = () => {
      disposed = true;
      if (requestTimeout !== null) clearTimer(requestTimeout);
      if (calibrationTimer !== null) clearTimer(calibrationTimer);
      requestTimeout = null;
      calibrationTimer = null;
      inFlight = null;
      calibrationPending = false;
    };

    return {
      dispose,
      finish,
      focus,
      request,
      scheduleCalibration,
      visibleTick,
      inspect: () => ({ disposed, inFlight, lastRequestedAt, calibrationPending }),
    };
  };

  return {
    FOCUS_REFRESH_MIN_MS,
    MIN_INLINE_CAPSULE_WIDTH,
    NOTIFICATION_CALIBRATION_MS,
    REQUEST_ID_PREFIX,
    REQUEST_TIMEOUT_MS,
    VISIBLE_REFRESH_MS,
    canFitCapsule,
    computeTooltipPlacement,
    createReadCoordinator,
    formatMessage,
    isSafeInlineCapsuleLayout,
    mergeSnapshot,
    normalizeSnapshot,
    presentWindows,
    selectLocale,
    summaryFor,
  };
};

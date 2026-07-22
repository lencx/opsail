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
  const formatLocaleTypography = (value, locale) => {
    const text = String(value || "");
    if (!normalizedLanguage(locale).startsWith("zh")) return text;
    return text
      .replace(/(\p{Script=Han})([A-Za-z0-9])/gu, "$1 $2")
      .replace(/([A-Za-z0-9])(\p{Script=Han})/gu, "$1 $2")
      .replace(/[ \t]+/g, " ")
      .trim();
  };
  const formatMessage = (template, values = {}) => String(template || "").replace(
    /\{([A-Za-z][A-Za-z0-9]*)\}/g,
    (_match, key) => Object.hasOwn(values, key) ? String(values[key]) : "",
  );

  const localeCache = new Map();
  const selectLocale = (...languageCandidates) => {
    const locales = localeBundle?.locales && typeof localeBundle.locales === "object"
      ? localeBundle.locales
      : {};
    const entries = Object.entries(locales);
    const defaultKey = localeBundle?.defaultLocale && locales[localeBundle.defaultLocale]
      ? localeBundle.defaultLocale
      : entries[0]?.[0];
    const defaultCopy = locales[defaultKey] || {};
    const supported = new Map(
      (Array.isArray(localeBundle?.supportedLocales)
        ? localeBundle.supportedLocales
        : Object.keys(locales))
        .map((locale) => [normalizedLanguage(locale), locale]),
    );
    const mergedCopy = (entryKey, displayLocale) => {
      const cacheKey = `${entryKey || defaultKey || ""}:${displayLocale || ""}`;
      if (localeCache.has(cacheKey)) return localeCache.get(cacheKey);
      const override = locales[entryKey] || {};
      const copy = {
        ...defaultCopy,
        ...override,
        locale: displayLocale || override.locale || defaultCopy.locale || defaultKey,
        resetCreditCountdownUnits: {
          ...(defaultCopy.resetCreditCountdownUnits || {}),
          ...(override.resetCreditCountdownUnits || {}),
        },
        windowLabels: {
          ...(defaultCopy.windowLabels || {}),
          ...(override.windowLabels || {}),
        },
        summaryWindowLabels: {
          ...(defaultCopy.summaryWindowLabels || {}),
          ...(override.windowLabels || {}),
          ...(override.summaryWindowLabels || {}),
        },
      };
      localeCache.set(cacheKey, copy);
      return copy;
    };
    for (const candidate of languageCandidates) {
      const normalized = normalizedLanguage(candidate);
      if (!normalized) continue;
      const exact = entries.find(([key, value]) =>
        normalizedLanguage(key) === normalized || normalizedLanguage(value?.locale) === normalized);
      const displayLocale = supported.get(normalized) || String(candidate).replaceAll("_", "-");
      if (exact) return mergedCopy(exact[0], displayLocale);
      const base = normalized.split("-")[0];
      const baseMatch = entries.find(([key, value]) =>
        normalizedLanguage(key).split("-")[0] === base
        || normalizedLanguage(value?.locale).split("-")[0] === base);
      if (baseMatch) return mergedCopy(baseMatch[0], displayLocale);
      if (supported.has(normalized)) return mergedCopy(defaultKey, displayLocale);
    }
    return mergedCopy(defaultKey, defaultCopy.locale || defaultKey);
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

  const formatReset = (resetsAt, _copy, displayLocale) => {
    if (resetsAt === null) return null;
    const value = new Date(resetsAt * 1000);
    if (!Number.isFinite(value.getTime())) return null;
    const locale = normalizedLanguage(displayLocale) ? displayLocale : undefined;
    try {
      const full = formatLocaleTypography(new Intl.DateTimeFormat(locale, {
        dateStyle: "full",
        timeStyle: "long",
      }).format(value), locale);
      const display = formatLocaleTypography(new Intl.DateTimeFormat(locale, {
        weekday: "short",
        month: "short",
        day: "numeric",
        hour: "numeric",
        minute: "2-digit",
      }).format(value), locale);
      return { display, full };
    } catch {
      try {
        const full = formatLocaleTypography(value.toLocaleString(locale), locale);
        return { display: full, full };
      } catch {
        return null;
      }
    }
  };

  const presentWindows = (snapshot, copy, displayLocale) => [
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
        reset: formatReset(windowValue.resetsAt, copy, displayLocale),
      };
    });

  const normalizeResetCredits = (value) => {
    if (!value || typeof value !== "object" || !Array.isArray(value.credits)) return [];
    return value.credits
      .filter((credit) => credit
        && typeof credit === "object"
        && credit.status === "available"
        && typeof credit.expiresAt === "number"
        && Number.isFinite(credit.expiresAt)
        && credit.expiresAt > 0)
      .map((credit) => ({ expiresAt: credit.expiresAt }))
      .sort((left, right) => left.expiresAt - right.expiresAt);
  };

  const formatResetCreditCountdown = (remainingMilliseconds, copy) => {
    const units = copy?.resetCreditCountdownUnits;
    if (!units || ![units.day, units.hour, units.minute, units.separator]
      .every((value) => typeof value === "string")) return null;
    if (!Number.isFinite(remainingMilliseconds) || remainingMilliseconds <= 0) return null;
    const minuteMilliseconds = 60 * 1000;
    const hourMilliseconds = 60 * minuteMilliseconds;
    if (remainingMilliseconds < hourMilliseconds) {
      const totalMinutes = Math.floor(remainingMilliseconds / minuteMilliseconds);
      return {
        countdown: `${totalMinutes}${units.minute}`,
        nextUpdateMs: totalMinutes === 0
          ? Math.max(1000, remainingMilliseconds)
          : Math.max(1000, remainingMilliseconds - totalMinutes * minuteMilliseconds + 1),
      };
    }
    const totalHours = Math.floor(remainingMilliseconds / hourMilliseconds);
    const days = Math.floor(totalHours / 24);
    const hours = totalHours % 24;
    const parts = [];
    if (days > 0) parts.push(`${days}${units.day}`);
    if (hours > 0 || days === 0) parts.push(`${hours}${units.hour}`);
    return {
      countdown: parts.join(units.separator),
      nextUpdateMs: Math.max(
        1000,
        remainingMilliseconds - totalHours * hourMilliseconds + 1,
      ),
    };
  };

  const formatLocalDateTime = (value) => {
    const part = (number, width = 2) => String(number).padStart(width, "0");
    return `${part(value.getFullYear(), 4)}-${part(value.getMonth() + 1)}-${part(value.getDate())}`
      + ` ${part(value.getHours())}:${part(value.getMinutes())}:${part(value.getSeconds())}`;
  };

  const presentResetCredits = (credits, copy, displayLocale, nowMs = Date.now()) => {
    const locale = normalizedLanguage(displayLocale) ? displayLocale : undefined;
    return (Array.isArray(credits) ? credits : []).flatMap((credit) => {
      const expiresAt = credit?.expiresAt;
      if (typeof expiresAt !== "number" || !Number.isFinite(expiresAt) || expiresAt <= 0) return [];
      const value = new Date(expiresAt * 1000);
      if (!Number.isFinite(value.getTime())) return [];
      const countdown = formatResetCreditCountdown(
        value.getTime() - (Number.isFinite(nowMs) ? nowMs : Date.now()),
        copy,
      );
      if (!countdown) return [];
      const dateTime = formatLocalDateTime(value);
      let full = dateTime;
      try {
        full = formatLocaleTypography(new Intl.DateTimeFormat(locale, {
          dateStyle: "full",
          timeStyle: "long",
        }).format(value), locale);
      } catch {
        try {
          full = formatLocaleTypography(value.toLocaleString(locale), locale);
        } catch {}
      }
      return [{ expiresAt, dateTime, full, ...countdown }];
    });
  };

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
    const horizontalInset = 16;
    const verticalInset = 8;
    const maximumWidth = Math.max(1, Math.min(
      240,
      viewportRect.width - horizontalInset * 2,
    ));
    const requestedWidth = Number.isFinite(tooltip?.width) && tooltip.width > 0
      ? tooltip.width
      : maximumWidth;
    const width = Math.max(1, Math.min(requestedWidth, maximumWidth));
    const maximumHeight = Math.max(1, Math.min(
      viewportRect.height - verticalInset * 2,
    ));
    const requestedHeight = Number.isFinite(tooltip?.height) && tooltip.height > 0
      ? tooltip.height
      : 1;
    const height = Math.min(requestedHeight, maximumHeight);
    const viewportMinimumLeft = viewportRect.left + horizontalInset;
    const viewportMaximumLeft = Math.max(
      viewportMinimumLeft,
      viewportRect.right - width - horizontalInset,
    );
    const sidebarMinimumLeft = Math.max(
      sidebarRect.left + horizontalInset,
      viewportMinimumLeft,
    );
    const sidebarMaximumLeft = Math.min(
      sidebarRect.right - width - horizontalInset,
      viewportMaximumLeft,
    );
    const attachedLeft = Math.max(sidebarMinimumLeft, anchorRect.right - width);
    const left = sidebarMaximumLeft >= sidebarMinimumLeft
      ? clamp(attachedLeft, sidebarMinimumLeft, sidebarMaximumLeft)
      : clamp(attachedLeft, viewportMinimumLeft, viewportMaximumLeft);
    const minimumTop = viewportRect.top + verticalInset;
    const maximumTop = Math.max(
      minimumTop,
      viewportRect.bottom - height - verticalInset,
    );
    const above = anchorRect.top - height - gap;
    const below = anchorRect.bottom + gap;
    const preferredTop = above >= minimumTop ? above : below;
    return {
      left,
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
    normalizeResetCredits,
    normalizeSnapshot,
    presentResetCredits,
    presentWindows,
    selectLocale,
    summaryFor,
  };
};

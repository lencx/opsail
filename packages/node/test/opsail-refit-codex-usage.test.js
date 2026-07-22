import assert from "node:assert/strict";
import fs from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const assetRoot = new URL(
  "../../../crates/opsail-refit-codex/assets/",
  import.meta.url,
);

async function loadModel() {
  const [source, en, zhCN] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-model.js", assetRoot), "utf8"),
    fs.readFile(new URL("locales/en.json", assetRoot), "utf8").then(JSON.parse),
    fs.readFile(new URL("locales/zh-CN.json", assetRoot), "utf8").then(JSON.parse),
  ]);
  const localeBundle = {
    defaultLocale: "en",
    locales: { en, "zh-CN": zhCN },
  };
  return vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexUsageModel(${JSON.stringify(localeBundle)});`,
    { Date, Intl, Promise, clearTimeout, setTimeout },
  );
}

async function assembleRuntimeSource({
  sessionMode = "once",
  managerToken = "opsail-refit-codex:test",
} = {}) {
  const [model, domAdapter, runtime, css, en, zhCN] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-model.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-dom-adapter.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-runtime.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage.css", assetRoot), "utf8"),
    fs.readFile(new URL("locales/en.json", assetRoot), "utf8").then(JSON.parse),
    fs.readFile(new URL("locales/zh-CN.json", assetRoot), "utf8").then(JSON.parse),
  ]);
  const replacements = [
    ["__OPSAIL_REFIT_CODEX_MODEL_SOURCE__", model],
    ["__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__", domAdapter],
    ["__OPSAIL_REFIT_CODEX_VERSION_JSON__", JSON.stringify("test")],
    ["__OPSAIL_REFIT_CODEX_REVISION_JSON__", JSON.stringify("test-revision")],
    ["__OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__", JSON.stringify(sessionMode)],
    ["__OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__", JSON.stringify(managerToken)],
    ["__OPSAIL_REFIT_CODEX_CSS_JSON__", JSON.stringify(css)],
    ["__OPSAIL_REFIT_CODEX_LOCALES_JSON__", JSON.stringify({
      defaultLocale: "en",
      locales: { en, "zh-CN": zhCN },
    })],
  ];
  const source = replacements.reduce(
    (value, [marker, replacement]) => value.split(marker).join(replacement),
    runtime,
  );
  return { replacements, source };
}

function fakeClock() {
  let now = 0;
  let sequence = 0;
  const timers = new Map();
  return {
    now: () => now,
    setTimer(callback, delay) {
      const id = ++sequence;
      timers.set(id, { callback, due: now + delay });
      return id;
    },
    clearTimer(id) {
      timers.delete(id);
    },
    advance(milliseconds) {
      const target = now + milliseconds;
      while (true) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.due <= target)
          .sort((left, right) => left[1].due - right[1].due || left[0] - right[0])[0];
        if (!next) break;
        timers.delete(next[0]);
        now = next[1].due;
        next[1].callback();
      }
      now = target;
    },
  };
}

function createRuntimeHarness({
  bridgeAvailable = true,
  nativeAccountRow = false,
  sidebarAvailable = true,
} = {}) {
  const eventRegistry = { count: 0 };

  class FakeEventTarget {
    constructor() {
      this.listeners = new Map();
    }

    addEventListener(type, listener) {
      if (!this.listeners.has(type)) this.listeners.set(type, new Set());
      const listeners = this.listeners.get(type);
      if (!listeners.has(listener)) {
        listeners.add(listener);
        eventRegistry.count += 1;
      }
    }

    removeEventListener(type, listener) {
      const listeners = this.listeners.get(type);
      if (listeners?.delete(listener)) eventRegistry.count -= 1;
    }

    dispatch(type, event) {
      for (const listener of [...(this.listeners.get(type) || [])]) listener(event);
    }
  }

  class FakeElement extends FakeEventTarget {
    constructor(tagName, ownerDocument) {
      super();
      this.tagName = tagName.toUpperCase();
      this.ownerDocument = ownerDocument;
      this.parentElement = null;
      this.children = [];
      this.id = "";
      this.className = "";
      this.dataset = {};
      this.attributes = new Map();
      this.hidden = false;
      this.tabIndex = -1;
      this.textContent = "";
      this.style = {
        values: new Map(),
        setProperty: (name, value) => this.style.values.set(name, value),
        removeProperty: (name) => this.style.values.delete(name),
      };
      const classes = new Set();
      this.classList = {
        add: (...values) => values.forEach((value) => classes.add(value)),
        remove: (...values) => values.forEach((value) => classes.delete(value)),
        contains: (value) => classes.has(value),
      };
      this.rect = null;
      this.rectProvider = null;
    }

    get isConnected() {
      for (let current = this; current; current = current.parentElement) {
        if (current === this.ownerDocument.documentElement) return true;
      }
      return false;
    }

    get scrollWidth() {
      return Math.max(24, this.textContent.length * 6 + 16);
    }

    get nextElementSibling() {
      if (!this.parentElement) return null;
      const index = this.parentElement.children.indexOf(this);
      return index >= 0 ? this.parentElement.children[index + 1] || null : null;
    }

    append(...nodes) {
      for (const node of nodes) {
        node.remove();
        node.parentElement = this;
        this.children.push(node);
      }
    }

    insertBefore(node, reference) {
      node.remove();
      const index = this.children.indexOf(reference);
      node.parentElement = this;
      if (index < 0) this.children.push(node);
      else this.children.splice(index, 0, node);
    }

    remove() {
      if (!this.parentElement) return;
      const index = this.parentElement.children.indexOf(this);
      if (index >= 0) this.parentElement.children.splice(index, 1);
      this.parentElement = null;
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
    }

    contains(node) {
      for (let current = node; current; current = current.parentElement) {
        if (current === this) return true;
      }
      return false;
    }

    closest() {
      return null;
    }

    matches() {
      return false;
    }

    querySelectorAll(selector) {
      const id = selector.startsWith("#") ? selector.slice(1) : null;
      const matches = [];
      const visit = (element) => {
        for (const child of element.children) {
          if (id && child.id === id) matches.push(child);
          visit(child);
        }
      };
      visit(this);
      return matches;
    }

    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
    }

    getBoundingClientRect() {
      if (this.rectProvider) return { ...this.rectProvider() };
      if (this.rect) return { ...this.rect };
      const width = this.id === "opsail-refit-codex-usage"
        ? Math.max(40, this.children[0]?.scrollWidth || 40)
        : 24;
      const height = this.id === "opsail-refit-codex-usage-details" ? 120 : 20;
      return {
        left: 100,
        top: 740,
        right: 100 + width,
        bottom: 740 + height,
        width,
        height,
      };
    }
  }

  class FakeDocument {
    constructor() {
      this.documentElement = new FakeElement("html", this);
      this.documentElement.lang = "en";
      this.head = new FakeElement("head", this);
      this.body = new FakeElement("body", this);
      this.sidebar = new FakeElement("aside", this);
      this.sidebar.rect = {
        left: 0,
        top: 0,
        right: 240,
        bottom: 800,
        width: 240,
        height: 800,
      };
      this.documentElement.append(this.head, this.body);
      if (sidebarAvailable) this.body.append(this.sidebar);
      if (nativeAccountRow) {
        const row = new FakeElement("div", this);
        const accountSlot = new FakeElement("div", this);
        const accountControl = new FakeElement("button", this);
        const avatar = new FakeElement("img", this);
        const trailingAction = new FakeElement("button", this);
        row.rect = {
          left: 0, top: 754, right: 240, bottom: 800, width: 240, height: 46,
        };
        const accountRect = () => {
          const inline = row.children.some((child) => child.id === "opsail-refit-codex-usage");
          return inline
            ? { left: 8, top: 762, right: 100, bottom: 792, width: 92, height: 30 }
            : { left: 8, top: 762, right: 192, bottom: 792, width: 184, height: 30 };
        };
        accountSlot.rectProvider = accountRect;
        accountControl.rectProvider = accountRect;
        avatar.rect = {
          left: 16, top: 768, right: 34, bottom: 786, width: 18, height: 18,
        };
        trailingAction.rect = {
          left: 192, top: 761, right: 224, bottom: 793, width: 32, height: 32,
        };
        avatar.closest = () => accountControl;
        accountControl.append(avatar);
        accountSlot.append(accountControl);
        row.append(accountSlot, trailingAction);
        this.sidebar.append(row);
        const querySelectorAll = this.sidebar.querySelectorAll.bind(this.sidebar);
        this.sidebar.querySelectorAll = (selector) => {
          if (selector === "img, [data-testid*='avatar' i], [class*='avatar' i]") {
            return [avatar];
          }
          if (selector === "button, [role='button']") return [accountControl, trailingAction];
          return querySelectorAll(selector);
        };
        this.nativeLayout = { accountControl, accountSlot, avatar, row, trailingAction };
      }
      this.visibilityState = "visible";
    }

    createElement(tagName) {
      return new FakeElement(tagName, this);
    }

    getElementById(id) {
      return this.querySelector(`#${id}`);
    }

    querySelector(selector) {
      if (selector.startsWith("aside.app-shell-left-panel")) {
        return sidebarAvailable ? this.sidebar : null;
      }
      if (this.documentElement.id === selector.slice(1)) return this.documentElement;
      return this.documentElement.querySelector(selector);
    }

    querySelectorAll(selector) {
      const values = this.documentElement.querySelectorAll(selector);
      if (selector.startsWith("#") && this.documentElement.id === selector.slice(1)) {
        values.unshift(this.documentElement);
      }
      return values;
    }
  }

  let timerSequence = 0;
  const timeouts = new Map();
  const intervals = new Map();
  const animationFrames = new Map();
  const setHarnessTimeout = (callback) => {
    const id = ++timerSequence;
    timeouts.set(id, callback);
    return id;
  };
  const setHarnessInterval = (callback) => {
    const id = ++timerSequence;
    intervals.set(id, callback);
    return id;
  };
  const requestHarnessAnimationFrame = (callback) => {
    const id = ++timerSequence;
    animationFrames.set(id, callback);
    return id;
  };

  const activeMutationObservers = new Set();
  const activeResizeObservers = new Set();
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
    }

    observe() {
      activeMutationObservers.add(this);
    }

    disconnect() {
      activeMutationObservers.delete(this);
    }
  }
  class FakeResizeObserver extends FakeMutationObserver {
    observe() {
      activeResizeObservers.add(this);
    }

    disconnect() {
      activeResizeObservers.delete(this);
    }
  }

  const document = new FakeDocument();
  if (document.nativeLayout) {
    const originalCreateElement = document.createElement.bind(document);
    document.createElement = (tagName) => {
      const element = originalCreateElement(tagName);
      if (tagName === "section") {
        element.rectProvider = () => element.parentElement === document.nativeLayout.row
          ? { left: 108, top: 763, right: 184, bottom: 791, width: 76, height: 28 }
          : { left: 100, top: 740, right: 176, bottom: 768, width: 76, height: 28 };
      }
      return element;
    };
  }
  const window = new FakeEventTarget();
  const sent = [];
  Object.assign(window, {
    document,
    electronBridge: bridgeAvailable
      ? { sendMessageFromView: (message) => sent.push(message) }
      : undefined,
    innerHeight: 800,
    innerWidth: 1200,
  });
  const context = vm.createContext({
    Date,
    Intl,
    MutationObserver: FakeMutationObserver,
    Promise,
    ResizeObserver: FakeResizeObserver,
    clearInterval: (id) => intervals.delete(id),
    clearTimeout: (id) => timeouts.delete(id),
    cancelAnimationFrame: (id) => animationFrames.delete(id),
    document,
    navigator: { language: "en-US" },
    requestAnimationFrame: requestHarnessAnimationFrame,
    setInterval: setHarnessInterval,
    setTimeout: setHarnessTimeout,
    window,
  });

  const flushAnimationFrames = () => {
    while (animationFrames.size > 0) {
      const frames = [...animationFrames.entries()];
      animationFrames.clear();
      for (const [, callback] of frames) callback();
    }
  };

  return {
    activeCounts: () => ({
      animationFrames: animationFrames.size,
      eventListeners: eventRegistry.count,
      intervals: intervals.size,
      mutationObservers: activeMutationObservers.size,
      resizeObservers: activeResizeObservers.size,
      timeouts: timeouts.size,
    }),
    context,
    document,
    nativeLayout: document.nativeLayout || null,
    respondWithWeekly({ resetsAt } = {}) {
      const requestId = sent.at(-1)?.request?.id;
      assert.ok(requestId);
      const secondary = { usedPercent: 72, windowDurationMins: 10080 };
      if (Number.isFinite(resetsAt)) secondary.resetsAt = resetsAt;
      window.dispatch("message", {
        data: {
          hostId: "local",
          type: "mcp-response",
          message: {
            id: requestId,
            result: {
              rateLimits: {
                secondary,
              },
            },
          },
        },
      });
      flushAnimationFrames();
    },
    sent,
    triggerResize() {
      for (const observer of activeResizeObservers) observer.callback([]);
      flushAnimationFrames();
    },
    triggerLanguage(language) {
      document.documentElement.lang = language;
      for (const observer of [...activeMutationObservers]) {
        observer.callback([{
          type: "attributes",
          target: document.documentElement,
          attributeName: "lang",
          addedNodes: [],
          removedNodes: [],
        }]);
      }
      flushAnimationFrames();
    },
    window,
  };
}

test("weekly-only usage renders remaining quota without a synthetic short window", async () => {
  const model = await loadModel();
  const copy = model.selectLocale("en-US");
  const snapshot = model.normalizeSnapshot({
    secondary: { usedPercent: 72, windowDurationMins: 10080 },
  });
  const windows = model.presentWindows(snapshot, copy);

  assert.equal(windows.length, 1);
  assert.equal(model.summaryFor(windows, copy), "weekly 28%");
  assert.doesNotMatch(model.summaryFor(windows, copy), /5h/);
});

test("short and long windows sort by actual duration", async () => {
  const model = await loadModel();
  const copy = model.selectLocale("en");
  const snapshot = model.normalizeSnapshot({
    primary: { usedPercent: 72, windowDurationMins: 10080 },
    secondary: { usedPercent: 77, windowDurationMins: 300 },
  });

  assert.equal(
    model.summaryFor(model.presentWindows(snapshot, copy), copy),
    "5h 23% / weekly 28%",
  );
});

test("invalid percentages are omitted and finite percentages are clamped", async () => {
  const model = await loadModel();
  const copy = model.selectLocale("en");
  for (const usedPercent of [undefined, null, Number.NaN, Number.POSITIVE_INFINITY, "72"]) {
    const snapshot = model.normalizeSnapshot({ primary: { usedPercent } });
    assert.equal(model.presentWindows(snapshot, copy).length, 0);
  }

  const low = model.normalizeSnapshot({ primary: { usedPercent: -8, windowDurationMins: 300 } });
  const high = model.normalizeSnapshot({ primary: { usedPercent: 108, windowDurationMins: 300 } });
  assert.equal(model.presentWindows(low, copy)[0].remaining, 100);
  assert.equal(model.presentWindows(high, copy)[0].remaining, 0);
});

test("partial notifications merge by field presence", async () => {
  const model = await loadModel();
  const current = model.normalizeSnapshot({
    primary: { usedPercent: 77, windowDurationMins: 300 },
    secondary: { usedPercent: 72, windowDurationMins: 10080 },
  });
  const notification = model.normalizeSnapshot({
    primary: { usedPercent: 50 },
  });
  const merged = model.mergeSnapshot(current, notification);
  const copy = model.selectLocale("en");

  assert.equal(
    model.summaryFor(model.presentWindows(merged, copy), copy),
    "5h 50% / weekly 28%",
  );

  const cleared = model.mergeSnapshot(
    merged,
    model.normalizeSnapshot({ primary: null }),
  );
  assert.equal(
    model.summaryFor(model.presentWindows(cleared, copy), copy),
    "weekly 28%",
  );
});

test("reset time uses readable relative and local forms without losing the exact value", async () => {
  const model = await loadModel();
  const english = model.selectLocale("en-US");
  const chinese = model.selectLocale("zh-CN");
  const now = Date.UTC(2030, 0, 1, 0, 0, 0);
  const resetsAt = (now + (6 * 24 + 16) * 60 * 60 * 1000) / 1000;
  const snapshot = model.normalizeSnapshot({
    primary: {
      usedPercent: 40,
      windowDurationMins: 1440,
      resetsAt,
    },
  });
  const englishReset = model.presentWindows(snapshot, english, "en-US", now)[0].reset;
  const chineseReset = model.presentWindows(snapshot, chinese, "zh-CN", now)[0].reset;
  const resetDate = new Date(resetsAt * 1000);
  const expectedDate = new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "numeric",
  }).format(resetDate);
  const expectedWeekday = new Intl.DateTimeFormat("en-US", {
    weekday: "short",
  }).format(resetDate);
  const expectedTime = new Intl.DateTimeFormat("en-US", {
    hour: "numeric",
    minute: "2-digit",
  }).format(resetDate);
  const expectedFull = new Intl.DateTimeFormat("en-US", {
    dateStyle: "full",
    timeStyle: "long",
  }).format(resetDate);
  assert.equal(englishReset.relative, "6d 16h");
  assert.equal(
    model.formatMessage(english.resetRelative, englishReset),
    "Resets in 6d 16h",
  );
  assert.equal(
    model.formatMessage(english.resetAbsolute, englishReset),
    `${expectedWeekday}, ${expectedDate} · ${expectedTime} (local time)`,
  );
  assert.equal(englishReset.full, expectedFull);
  assert.equal(chineseReset.relative, "6天16小时");
  assert.equal(
    model.formatMessage(chinese.resetRelative, chineseReset),
    "6天16小时后重置",
  );
  assert.match(model.formatMessage(chinese.resetAbsolute, chineseReset), /本地时间/);
  assert.doesNotMatch(englishReset.full, /…|\.\.\./);

  const empty = model.normalizeSnapshot({ primary: null, secondary: { usedPercent: null } });
  assert.equal(model.presentWindows(empty, english).length, 0);
  assert.equal(model.summaryFor(model.presentWindows(empty, english), english), "");
});

test("locale JSON selects an exact locale, then language family, then English", async () => {
  const model = await loadModel();
  assert.equal(model.selectLocale("zh-Hans-CN").locale, "zh-CN");
  assert.equal(model.selectLocale("fr-FR").locale, "en");
  const chinese = model.selectLocale("zh-CN");
  const weekly = model.normalizeSnapshot({
    secondary: { usedPercent: 72, windowDurationMins: 10080 },
  });
  assert.equal(
    model.summaryFor(model.presentWindows(weekly, chinese), chinese),
    "周剩余 28%",
  );
});

test("read coordination deduplicates, times out, gates focus, and refreshes only when visible", async () => {
  const model = await loadModel();
  const clock = fakeClock();
  const sent = [];
  const failed = [];
  const coordinator = model.createReadCoordinator({
    now: clock.now,
    setTimer: clock.setTimer,
    clearTimer: clock.clearTimer,
    send: (requestId) => sent.push(requestId),
    onFailure: (requestId) => failed.push(requestId),
  });

  const first = coordinator.request();
  assert.match(first, /^opsail-refit-codex-rate-limits:/);
  assert.equal(coordinator.request(), null);
  assert.equal(sent.length, 1);
  clock.advance(model.REQUEST_TIMEOUT_MS);
  assert.deepEqual(failed, [first]);

  const second = coordinator.request();
  coordinator.finish(second);
  clock.advance(model.FOCUS_REFRESH_MIN_MS - 1);
  assert.equal(coordinator.focus(), null);
  clock.advance(1);
  const focusRequest = coordinator.focus();
  assert.ok(focusRequest);
  coordinator.finish(focusRequest);

  assert.equal(coordinator.visibleTick(false), null);
  assert.ok(coordinator.visibleTick(true));
  assert.equal(model.VISIBLE_REFRESH_MS, 15 * 60 * 1000);
  coordinator.dispose();
});

test("notification calibration is debounced and waits for an active read", async () => {
  const model = await loadModel();
  const clock = fakeClock();
  const sent = [];
  const coordinator = model.createReadCoordinator({
    now: clock.now,
    setTimer: clock.setTimer,
    clearTimer: clock.clearTimer,
    send: (requestId) => sent.push(requestId),
  });

  const initial = coordinator.request();
  coordinator.scheduleCalibration();
  coordinator.scheduleCalibration();
  clock.advance(model.NOTIFICATION_CALIBRATION_MS);
  assert.equal(sent.length, 1);
  coordinator.finish(initial);
  assert.equal(sent.length, 2);
  assert.equal(model.NOTIFICATION_CALIBRATION_MS, 1200);
  coordinator.dispose();
});

test("tooltip placement clamps to sidebar and viewport and narrow gaps hide the capsule", async () => {
  const model = await loadModel();
  const placement = model.computeTooltipPlacement({
    anchor: { left: 132, top: 700, right: 152, bottom: 720, width: 20, height: 20 },
    sidebar: { left: 0, top: 0, right: 160, bottom: 800, width: 160, height: 800 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 180 },
  });
  assert.ok(placement.left >= 8);
  assert.ok(placement.left + placement.width <= 152);
  assert.ok(placement.top >= 8);
  assert.ok(placement.top + 180 <= 792);
  assert.equal(placement.maximumHeight, 784);

  const oversized = model.computeTooltipPlacement({
    anchor: { left: 20, top: 60, right: 60, bottom: 80, width: 40, height: 20 },
    sidebar: { left: 0, top: 40, right: 180, bottom: 220, width: 180, height: 180 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 600 },
  });
  assert.equal(oversized.maximumHeight, 164);
  assert.ok(oversized.top >= 48);
  assert.ok(oversized.top + oversized.maximumHeight <= 212);

  assert.equal(model.canFitCapsule({
    leftBoundary: 72,
    rightBoundary: 130,
    capsuleWidth: 54,
  }), false);
  assert.equal(model.canFitCapsule({
    leftBoundary: 60,
    rightBoundary: 180,
    capsuleWidth: 70,
  }), true);
});

test("inline capsule validation accepts safe flex reflow and rejects overlap or over-shrink", async () => {
  const model = await loadModel();
  const layout = {
    accountSlot: { left: 8, top: 911, right: 100, bottom: 941, width: 92, height: 30 },
    avatar: { left: 16, top: 917, right: 34, bottom: 935, width: 18, height: 18 },
    host: { left: 108, top: 912, right: 184, bottom: 940, width: 76, height: 28 },
    trailingSlot: { left: 192, top: 910, right: 224, bottom: 942, width: 32, height: 32 },
    sidebar: { left: 0, top: 0, right: 240, bottom: 949, width: 240, height: 949 },
    viewportBottom: 949,
  };
  assert.equal(model.isSafeInlineCapsuleLayout(layout), true);
  assert.equal(model.isSafeInlineCapsuleLayout({
    ...layout,
    host: { ...layout.host, left: 92 },
  }), false);
  assert.equal(model.isSafeInlineCapsuleLayout({
    ...layout,
    host: { ...layout.host, right: 132, width: 24 },
  }), false);
  assert.equal(model.isSafeInlineCapsuleLayout({
    ...layout,
    accountSlot: { ...layout.accountSlot, right: 30, width: 22 },
  }), false);
});

test("runtime and CSS enforce cleanup, quiet failure, theme colors, and complete detail text", async () => {
  const [domAdapter, runtime, css, payloadRust] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-dom-adapter.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-runtime.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage.css", assetRoot), "utf8"),
    fs.readFile(new URL("../src/payload.rs", assetRoot), "utf8"),
  ]);
  assert.match(runtime, /window\[STATE_KEY\]\?\.cleanup/);
  assert.match(runtime, /removeListeners\(\)/);
  assert.match(runtime, /mutationObserver\?\.disconnect/);
  assert.match(runtime, /resizeObserver\?\.disconnect/);
  assert.match(runtime, /observedSidebar === nextSidebar && observedRow === nextRow/);
  assert.match(runtime, /scheduler\.tooltipFrameKind === "timeout"/);
  assert.match(runtime, /createOpsailRefitCodexDomAdapter/);
  assert.doesNotMatch(runtime, /main\.main-surface|app-shell-left-panel/);
  assert.doesNotMatch(runtime, /\b(?:alert|confirm|prompt)\s*\(/);
  assert.doesNotMatch(runtime, /\bfetch\s*\(|new\s+WebSocket|location\.reload/);

  assert.match(domAdapter, /const createOpsailRefitCodexDomAdapter/);
  assert.match(domAdapter, /main\.main-surface/);
  assert.match(domAdapter, /app-shell-left-panel/);
  assert.match(domAdapter, /measureNativeLayout/);
  assert.match(domAdapter, /nodeMayAffectLayout/);
  assert.doesNotMatch(domAdapter, /\b(?:alert|confirm|prompt)\s*\(/);
  assert.doesNotMatch(domAdapter, /\bfetch\s*\(|new\s+WebSocket|location\.reload/);
  assert.doesNotMatch(payloadRust, /\bdocument\.|\bwindow\./);

  assert.match(css, /font-size:\s*10px/);
  assert.match(css, /cursor:\s*default/);
  assert.match(css, /font-variant-numeric:\s*tabular-nums/);
  assert.match(
    css,
    /\.opsail-refit-codex-usage-summary\s*\{[\s\S]*?text-overflow:\s*ellipsis/,
  );
  assert.match(css, /white-space:\s*pre-line/);
  assert.match(css, /overflow-y:\s*auto/);
  assert.match(css, /prefers-reduced-motion/);
  assert.doesNotMatch(css, /(?:rgb|rgba|hsl|hsla)\s*\(/i);
  assert.doesNotMatch(css, /#[0-9a-f]{3,8}\b/i);
  assert.match(css, /var\(--color-token-/);
});

test("DOM adapter owns renderer discovery and fails closed when native nodes disappear", async () => {
  const source = await fs.readFile(
    new URL("opsail-refit-codex-dom-adapter.js", assetRoot),
    "utf8",
  );
  const shell = {};
  const sidebar = {};
  const selectors = [];
  const document = {
    querySelector(selector) {
      selectors.push(selector);
      if (selector === "main.main-surface") return shell;
      if (selector.startsWith("aside.app-shell-left-panel")) return sidebar;
      return null;
    },
  };
  const context = {
    document,
    location: { protocol: "app:" },
    window: { electronBridge: { sendMessageFromView() {} } },
  };
  const adapter = vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexDomAdapter();`,
    context,
  );
  assert.equal(adapter.VERSION, 1);
  assert.deepEqual(
    JSON.parse(JSON.stringify(adapter.probeRenderer())),
    {
      appProtocol: true,
      bridge: true,
      domAdapterVersion: 1,
      shell: true,
      sidebar: true,
    },
  );
  assert.ok(selectors.includes(adapter.SELECTORS.shell));
  assert.ok(selectors.includes(adapter.SELECTORS.sidebar));

  context.document.querySelector = () => null;
  assert.deepEqual(
    JSON.parse(JSON.stringify(adapter.probeRenderer())),
    {
      appProtocol: true,
      bridge: true,
      domAdapterVersion: 1,
      shell: false,
      sidebar: false,
    },
  );
});

test("DOM adapter reports the Codex language before system fallbacks", async () => {
  const source = await fs.readFile(
    new URL("opsail-refit-codex-dom-adapter.js", assetRoot),
    "utf8",
  );
  const document = {
    documentElement: { lang: "zh-CN" },
    querySelector() { return null; },
  };
  const adapter = vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexDomAdapter();`,
    {
      document,
      location: { protocol: "app:" },
      navigator: { language: "en-US", languages: ["en-US"] },
      window: { electronBridge: { sendMessageFromView() {} } },
    },
  );

  assert.deepEqual(
    JSON.parse(JSON.stringify(adapter.languageCandidates())),
    ["zh-CN", "en-US"],
  );
  document.documentElement.lang = "en";
  assert.deepEqual(
    JSON.parse(JSON.stringify(adapter.languageCandidates())),
    ["en", "en-US"],
  );
});

test("DOM adapter ignores offscreen controls when measuring the native account row", async () => {
  const source = await fs.readFile(
    new URL("opsail-refit-codex-dom-adapter.js", assetRoot),
    "utf8",
  );
  const adapter = vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexDomAdapter();`,
    {
      document: { querySelector() { return null; } },
      location: { protocol: "app:" },
      window: { electronBridge: { sendMessageFromView() {} } },
    },
  );
  const element = (rect) => ({
    parentElement: null,
    closest() { return null; },
    getBoundingClientRect() { return { ...rect }; },
    querySelectorAll() { return []; },
  });
  const sidebar = element({
    left: 0, top: 0, right: 240, bottom: 949, width: 240, height: 949,
  });
  const row = element({
    left: 0, top: 903, right: 240, bottom: 949, width: 240, height: 46,
  });
  const accountSlot = element({
    left: 8, top: 911.5, right: 192, bottom: 940.5, width: 184, height: 29,
  });
  const accountControl = element({
    left: 8, top: 911.5, right: 192, bottom: 940.5, width: 184, height: 29,
  });
  const avatar = element({
    left: 16, top: 917, right: 34, bottom: 935, width: 18, height: 18,
  });
  const validAction = element({
    left: 200, top: 910, right: 232, bottom: 942, width: 32, height: 32,
  });
  const offscreenContainer = element({
    left: 0, top: 1200, right: 240, bottom: 1320, width: 240, height: 120,
  });
  const offscreenAction = element({
    left: 208, top: 1281, right: 228, bottom: 1301, width: 20, height: 20,
  });
  sidebar.marker = "sidebar";
  row.marker = "row";
  accountSlot.marker = "account-slot";
  avatar.marker = "avatar";
  validAction.marker = "valid-action";
  offscreenAction.marker = "offscreen-action";

  row.parentElement = sidebar;
  accountSlot.parentElement = row;
  accountControl.parentElement = accountSlot;
  avatar.parentElement = accountControl;
  validAction.parentElement = row;
  offscreenContainer.parentElement = sidebar;
  offscreenAction.parentElement = offscreenContainer;
  avatar.closest = () => accountControl;
  sidebar.querySelectorAll = (selector) => {
    if (selector === adapter.SELECTORS.avatar) return [avatar];
    if (selector === adapter.SELECTORS.action) return [validAction, offscreenAction];
    return [];
  };

  const measured = adapter.measureNativeLayout(sidebar);
  assert.equal(measured.avatar.element.marker, "avatar");
  assert.equal(measured.trailingAction.element.marker, "valid-action");
  assert.equal(measured.row.marker, "row");
  assert.equal(measured.accountSlot.marker, "account-slot");
  assert.equal(measured.trailingSlot.marker, "valid-action");
});

test("DOM adapter prefers the footer action aligned with the account avatar", async () => {
  const source = await fs.readFile(
    new URL("opsail-refit-codex-dom-adapter.js", assetRoot),
    "utf8",
  );
  const adapter = vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexDomAdapter();`,
    {
      document: { querySelector() { return null; } },
      location: { protocol: "app:" },
      window: { electronBridge: { sendMessageFromView() {} } },
    },
  );
  const element = (marker, rect) => ({
    marker,
    parentElement: null,
    closest() { return null; },
    getBoundingClientRect() { return { ...rect }; },
    querySelectorAll() { return []; },
  });
  const sidebar = element("sidebar", {
    left: 0, top: 0, right: 240, bottom: 949, width: 240, height: 949,
  });
  const content = element("content", {
    left: 0, top: 46, right: 240, bottom: 949, width: 240, height: 903,
  });
  const row = element("row", {
    left: 0, top: 903, right: 240, bottom: 949, width: 240, height: 46,
  });
  const accountSlot = element("account-slot", {
    left: 8, top: 911.5, right: 192, bottom: 940.5, width: 184, height: 29,
  });
  const accountControl = element("account-control", {
    left: 8, top: 911.5, right: 192, bottom: 940.5, width: 184, height: 29,
  });
  const avatar = element("avatar", {
    left: 16, top: 917, right: 34, bottom: 935, width: 18, height: 18,
  });
  const alignedAction = element("aligned-action", {
    left: 200, top: 910, right: 232, bottom: 942, width: 32, height: 32,
  });
  const nearbyAction = element("nearby-action", {
    left: 208, top: 920, right: 228, bottom: 940, width: 20, height: 20,
  });

  content.parentElement = sidebar;
  row.parentElement = content;
  accountSlot.parentElement = row;
  accountControl.parentElement = accountSlot;
  avatar.parentElement = accountControl;
  alignedAction.parentElement = row;
  nearbyAction.parentElement = content;
  avatar.closest = () => accountControl;
  sidebar.querySelectorAll = (selector) => {
    if (selector === adapter.SELECTORS.avatar) return [avatar];
    if (selector === adapter.SELECTORS.action) return [alignedAction, nearbyAction];
    return [];
  };

  const measured = adapter.measureNativeLayout(sidebar);
  assert.equal(measured.trailingAction.element.marker, "aligned-action");
  assert.equal(measured.row.marker, "row");
  assert.equal(measured.accountSlot.marker, "account-slot");
  assert.equal(measured.trailingSlot.marker, "aligned-action");
});

test("the Rust-assembled renderer payload is valid JavaScript", async () => {
  const { replacements, source } = await assembleRuntimeSource();
  const [domAdapter, probeTemplate, earlyTemplate, statusTemplate, disable] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-dom-adapter.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-renderer-probe.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-early.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-status.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-disable.js", assetRoot), "utf8"),
  ]);
  const probe = probeTemplate
    .split("__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__")
    .join(domAdapter);
  const early = earlyTemplate
    .split("__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__")
    .join(domAdapter)
    .split("__OPSAIL_REFIT_CODEX_EARLY_REVISION_JSON__")
    .join(JSON.stringify("test-revision"))
    .split("__OPSAIL_REFIT_CODEX_CURRENT_PAYLOAD__")
    .join(source);
  const status = statusTemplate
    .split("__OPSAIL_REFIT_CODEX_STATUS_REVISION_JSON__")
    .join(JSON.stringify("test-revision"));

  for (const [marker] of replacements) assert.doesNotMatch(source, new RegExp(marker));
  assert.doesNotThrow(() => new vm.Script(source));
  for (const marker of [
    "__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__",
    "__OPSAIL_REFIT_CODEX_EARLY_REVISION_JSON__",
    "__OPSAIL_REFIT_CODEX_CURRENT_PAYLOAD__",
  ]) {
    assert.doesNotMatch(probe, new RegExp(marker));
    assert.doesNotMatch(early, new RegExp(marker));
  }
  assert.doesNotThrow(() => new vm.Script(probe));
  assert.doesNotThrow(() => new vm.Script(early));
  assert.doesNotMatch(status, /__OPSAIL_REFIT_CODEX_STATUS_REVISION_JSON__/);
  assert.doesNotThrow(() => new vm.Script(status));
  assert.doesNotThrow(() => new vm.Script(disable));
});

test("runtime mounts one inline capsule and keeps it stable across remeasurement", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly();

  const host = harness.document.getElementById("opsail-refit-codex-usage");
  let diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(host.hidden, false);
  assert.equal(host.dataset.opsailRefitCodexLayout, "inline");
  assert.equal(host.parentElement, harness.nativeLayout.row);
  assert.equal(host.nextElementSibling, harness.nativeLayout.trailingAction);
  assert.equal(host.children[0].textContent, "weekly 28%");
  assert.equal(
    host.style.values.get("--opsail-refit-usage-inline-max-width"),
    "100px",
  );

  harness.triggerResize();
  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(host.parentElement, harness.nativeLayout.row);
  assert.equal(host.nextElementSibling, harness.nativeLayout.trailingAction);
  assert.equal(
    harness.nativeLayout.row.children.filter((child) => child === host).length,
    1,
  );
});

test("runtime follows Codex language changes instead of the system language", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({
    resetsAt: Math.floor(Date.now() / 1000) + (6 * 24 + 16) * 60 * 60,
  });

  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const details = harness.document.getElementById("opsail-refit-codex-usage-details");
  const meta = details.children[1].children[1];
  assert.equal(host.children[0].textContent, "weekly 28%");
  assert.match(meta.textContent, /^72% used\nResets in 6d 16h\n.*\(local time\)$/);
  harness.triggerLanguage("zh-CN");
  assert.equal(host.children[0].textContent, "周剩余 28%");
  assert.match(meta.textContent, /^已用 72%\n6天16小时后重置\n.*（本地时间）$/);
  assert.match(meta.attributes.get("aria-label"), /^已用 72%。.*重置$/);
  assert.equal(
    details.attributes.get("aria-label"),
    "使用额度",
  );
});

test("repeated renderer installation stays singular and cleanup releases every resource", async () => {
  const { source } = await assembleRuntimeSource();
  const script = new vm.Script(source);
  const harness = createRuntimeHarness();

  script.runInContext(harness.context);
  let diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 0);
  assert.equal(diagnostics.visible, false);
  harness.respondWithWeekly();
  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 1);
  assert.equal(diagnostics.styleCount, 1);
  assert.equal(diagnostics.detailsCount, 1);
  assert.equal(diagnostics.listenerCount, 10);
  assert.equal(diagnostics.domAdapterVersion, 1);
  assert.equal(diagnostics.sessionMode, "once");
  assert.equal(diagnostics.managerToken, "opsail-refit-codex:test");
  assert.equal(
    harness.document.getElementById("opsail-refit-codex-usage").parentElement,
    harness.document.body,
  );
  const firstCounts = harness.activeCounts();
  assert.equal(firstCounts.mutationObservers, 1);
  assert.equal(firstCounts.resizeObservers, 1);
  assert.equal(firstCounts.intervals, 1);
  assert.equal(firstCounts.timeouts, 0);
  const layoutsBeforeResize = harness.window.__OPSAIL_REFIT_CODEX_STATE__.metrics.layoutCalls;
  harness.triggerResize();
  assert.ok(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.metrics.layoutCalls > layoutsBeforeResize,
  );

  script.runInContext(harness.context);
  harness.respondWithWeekly();
  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 1);
  assert.equal(diagnostics.styleCount, 1);
  assert.equal(diagnostics.detailsCount, 1);
  assert.deepEqual(harness.activeCounts(), firstCounts);

  const cleanup = harness.window.__OPSAIL_REFIT_CODEX_STATE__.cleanup;
  assert.equal(cleanup(), true);
  assert.equal(cleanup(), false);
  assert.equal(harness.document.querySelectorAll("#opsail-refit-codex-usage").length, 0);
  assert.equal(
    harness.document.querySelectorAll("#opsail-refit-codex-usage-style").length,
    0,
  );
  assert.equal(
    harness.document.querySelectorAll("#opsail-refit-codex-usage-details").length,
    0,
  );
  assert.deepEqual(harness.activeCounts(), {
    animationFrames: 0,
    eventListeners: 0,
    intervals: 0,
    mutationObservers: 0,
    resizeObservers: 0,
    timeouts: 0,
  });
});

test("missing bridge and sidebar fail quietly without renderer control side effects", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({
    bridgeAvailable: false,
    sidebarAvailable: false,
  });

  assert.doesNotThrow(() => new vm.Script(source).runInContext(harness.context));
  const state = harness.window.__OPSAIL_REFIT_CODEX_STATE__;
  const diagnostics = state.diagnostics();
  assert.equal(diagnostics.bridgeAvailable, false);
  assert.equal(diagnostics.hostCount, 0);
  assert.equal(diagnostics.visible, false);
  assert.equal(harness.sent.length, 0);
  assert.equal(state.cleanup(), true);
});

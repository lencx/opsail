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
  const [model, runtime, css, en, zhCN] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-model.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-runtime.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage.css", assetRoot), "utf8"),
    fs.readFile(new URL("locales/en.json", assetRoot), "utf8").then(JSON.parse),
    fs.readFile(new URL("locales/zh-CN.json", assetRoot), "utf8").then(JSON.parse),
  ]);
  const replacements = [
    ["__OPSAIL_REFIT_CODEX_MODEL_SOURCE__", model],
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

function createRuntimeHarness({ bridgeAvailable = true, sidebarAvailable = true } = {}) {
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
    respondWithWeekly() {
      const requestId = sent.at(-1)?.request?.id;
      assert.ok(requestId);
      window.dispatch("message", {
        data: {
          hostId: "local",
          type: "mcp-response",
          message: {
            id: requestId,
            result: {
              rateLimits: {
                secondary: { usedPercent: 72, windowDurationMins: 10080 },
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

test("reset time is fully localized and no valid windows means no summary", async () => {
  const model = await loadModel();
  const copy = model.selectLocale("en-US");
  const snapshot = model.normalizeSnapshot({
    primary: {
      usedPercent: 40,
      windowDurationMins: 1440,
      resetsAt: 1893456000,
    },
  });
  const reset = model.presentWindows(snapshot, copy, "en-GB")[0].reset;
  const expectedReset = new Intl.DateTimeFormat("en-GB", {
    dateStyle: "full",
    timeStyle: "long",
  }).format(new Date(1893456000 * 1000));
  assert.equal(reset, expectedReset);
  assert.ok(reset.length > 12);
  assert.doesNotMatch(reset, /…|\.\.\./);

  const empty = model.normalizeSnapshot({ primary: null, secondary: { usedPercent: null } });
  assert.equal(model.presentWindows(empty, copy).length, 0);
  assert.equal(model.summaryFor(model.presentWindows(empty, copy), copy), "");
});

test("locale JSON selects an exact locale, then language family, then English", async () => {
  const model = await loadModel();
  assert.equal(model.selectLocale("zh-Hans-CN").locale, "zh-CN");
  assert.equal(model.selectLocale("fr-FR").locale, "en");
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

test("runtime and CSS enforce cleanup, quiet failure, theme colors, and complete detail text", async () => {
  const [runtime, css] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-runtime.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage.css", assetRoot), "utf8"),
  ]);
  assert.match(runtime, /window\[STATE_KEY\]\?\.cleanup/);
  assert.match(runtime, /removeListeners\(\)/);
  assert.match(runtime, /mutationObserver\?\.disconnect/);
  assert.match(runtime, /resizeObserver\?\.disconnect/);
  assert.match(runtime, /observedSidebar === nextSidebar && observedRow === nextRow/);
  assert.match(runtime, /scheduler\.tooltipFrameKind === "timeout"/);
  assert.doesNotMatch(runtime, /\b(?:alert|confirm|prompt)\s*\(/);
  assert.doesNotMatch(runtime, /\bfetch\s*\(|new\s+WebSocket|location\.reload/);

  assert.match(css, /font-size:\s*10px/);
  assert.match(css, /cursor:\s*default/);
  assert.match(css, /font-variant-numeric:\s*tabular-nums/);
  assert.match(css, /white-space:\s*pre-line/);
  assert.match(css, /overflow-y:\s*auto/);
  assert.match(css, /prefers-reduced-motion/);
  assert.doesNotMatch(css, /(?:rgb|rgba|hsl|hsla)\s*\(/i);
  assert.doesNotMatch(css, /#[0-9a-f]{3,8}\b/i);
  assert.match(css, /var\(--color-token-/);
});

test("the Rust-assembled renderer payload is valid JavaScript", async () => {
  const { replacements, source } = await assembleRuntimeSource();

  for (const [marker] of replacements) assert.doesNotMatch(source, new RegExp(marker));
  assert.doesNotThrow(() => new vm.Script(source));
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
  assert.equal(diagnostics.sessionMode, "once");
  assert.equal(diagnostics.managerToken, "opsail-refit-codex:test");
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

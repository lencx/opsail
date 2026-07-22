import assert from "node:assert/strict";
import fs from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const assetRoot = new URL(
  "../../../crates/opsail-refit-codex/assets/",
  import.meta.url,
);

async function loadModel() {
  const [source, localeBundle] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-model.js", assetRoot), "utf8"),
    fs.readFile(new URL("locales.json", assetRoot), "utf8").then(JSON.parse),
  ]);
  return vm.runInNewContext(
    `${source}\ncreateOpsailRefitCodexUsageModel(${JSON.stringify(localeBundle)});`,
    { Date, Intl, Promise, clearTimeout, setTimeout },
  );
}

async function assembleRuntimeSource({
  sessionMode = "once",
  managerToken = "opsail-refit-codex:test",
} = {}) {
  const [model, domAdapter, runtime, css, localeBundle] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-usage-model.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-dom-adapter.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage-runtime.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-usage.css", assetRoot), "utf8"),
    fs.readFile(new URL("locales.json", assetRoot), "utf8").then(JSON.parse),
  ]);
  const replacements = [
    ["__OPSAIL_REFIT_CODEX_MODEL_SOURCE__", model],
    ["__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__", domAdapter],
    ["__OPSAIL_REFIT_CODEX_VERSION_JSON__", JSON.stringify("test")],
    ["__OPSAIL_REFIT_CODEX_REVISION_JSON__", JSON.stringify("test-revision")],
    ["__OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__", JSON.stringify(sessionMode)],
    ["__OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__", JSON.stringify(managerToken)],
    ["__OPSAIL_REFIT_CODEX_CSS_JSON__", JSON.stringify(css)],
    ["__OPSAIL_REFIT_CODEX_LOCALES_JSON__", JSON.stringify(localeBundle)],
  ];
  const source = replacements.reduce(
    (value, [marker, replacement]) => value.split(marker).join(replacement),
    runtime,
  );
  return { replacements, source };
}

async function assembleControlSource(operation, {
  currentPayload = "void 0",
  revision = "test-revision",
} = {}) {
  const [control, domAdapter] = await Promise.all([
    fs.readFile(new URL("opsail-refit-codex-renderer-control.js", assetRoot), "utf8"),
    fs.readFile(new URL("opsail-refit-codex-dom-adapter.js", assetRoot), "utf8"),
  ]);
  const replacements = [
    ["__OPSAIL_REFIT_CODEX_OPERATION_JSON__", JSON.stringify(operation)],
    ["__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__",
      operation === "probe" || operation === "early" ? domAdapter : ""],
    ["__OPSAIL_REFIT_CODEX_EARLY_REVISION_JSON__",
      operation === "early" ? JSON.stringify(revision) : "null"],
    ["__OPSAIL_REFIT_CODEX_CURRENT_PAYLOAD__",
      operation === "early" ? currentPayload : "void 0"],
    ["__OPSAIL_REFIT_CODEX_STATUS_REVISION_JSON__",
      operation === "status" ? JSON.stringify(revision) : "null"],
  ];
  const source = replacements.reduce(
    (value, [marker, replacement]) => value.split(marker).join(replacement),
    control,
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
  documentInitiallyUnavailable = false,
  nativeAccountRow = false,
  sidebarAvailable = true,
} = {}) {
  const eventRegistry = { count: 0 };
  let runtimeNow = Date.now();
  let runtimeSidebarAvailable = sidebarAvailable;

  class HarnessDate extends Date {
    constructor(...arguments_) {
      if (arguments_.length === 0) super(runtimeNow);
      else super(...arguments_);
    }

    static now() {
      return runtimeNow;
    }
  }

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

    matches(selector) {
      return this === this.ownerDocument.sidebar
        && selector.startsWith("aside.app-shell-left-panel");
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
      if (runtimeSidebarAvailable) this.body.append(this.sidebar);
      if (nativeAccountRow) {
        this.nativeLayoutQueryRoots = [];
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
        const installNativeQueries = (root) => {
          const querySelectorAll = root.querySelectorAll.bind(root);
          root.querySelectorAll = (selector) => {
            if (selector === "img, [data-testid*='avatar' i], [class*='avatar' i]") {
              this.nativeLayoutQueryRoots.push(root);
              return root.contains(avatar) ? [avatar] : [];
            }
            if (selector === "button, [role='button']") {
              this.nativeLayoutQueryRoots.push(root);
              return [accountControl, trailingAction]
                .filter((element) => root.contains(element));
            }
            return querySelectorAll(selector);
          };
        };
        installNativeQueries(this.sidebar);
        installNativeQueries(row);
        this.nativeLayout = { accountControl, accountSlot, avatar, row, trailingAction };
      }
      this.visibilityState = "visible";
      this.readyState = documentInitiallyUnavailable ? "loading" : "complete";
    }

    createElement(tagName) {
      return new FakeElement(tagName, this);
    }

    getElementById(id) {
      return this.querySelector(`#${id}`);
    }

    querySelector(selector) {
      if (selector.startsWith("aside.app-shell-left-panel")) {
        return runtimeSidebarAvailable && this.sidebar.isConnected ? this.sidebar : null;
      }
      if (this.documentElement?.id === selector.slice(1)) return this.documentElement;
      return this.documentElement?.querySelector(selector) || null;
    }

    querySelectorAll(selector) {
      const values = this.documentElement?.querySelectorAll(selector) || [];
      if (selector.startsWith("#") && this.documentElement?.id === selector.slice(1)) {
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
      this.observations = [];
    }

    observe(target, options) {
      this.observations.push({ options, target });
      activeMutationObservers.add(this);
    }

    disconnect() {
      this.observations = [];
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
  const mountedDocument = {
    body: document.body,
    documentElement: document.documentElement,
    head: document.head,
  };
  if (documentInitiallyUnavailable) {
    document.body = null;
    document.documentElement = null;
    document.head = null;
  }
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
  const bridgeMessages = [];
  const sent = [];
  const sendBridgeMessage = (message) => {
    bridgeMessages.push(message);
    if (message?.request?.method === "account/rateLimits/read") sent.push(message);
  };
  Object.assign(window, {
    document,
    electronBridge: bridgeAvailable
      ? { sendMessageFromView: sendBridgeMessage }
      : undefined,
    innerHeight: 800,
    innerWidth: 1200,
  });
  const context = vm.createContext({
    Date: HarnessDate,
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
    advanceNow: (milliseconds) => { runtimeNow += milliseconds; },
    attachSidebar(parent = document.body) {
      parent.append(document.sidebar);
      runtimeSidebarAvailable = true;
    },
    bridgeMessages,
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
    mountDocument() {
      document.body = mountedDocument.body;
      document.documentElement = mountedDocument.documentElement;
      document.head = mountedDocument.head;
      document.readyState = "complete";
    },
    nativeLayout: document.nativeLayout || null,
    now: () => runtimeNow,
    runPendingTimeouts() {
      const pending = [...timeouts.entries()];
      for (const [id] of pending) timeouts.delete(id);
      for (const [, callback] of pending) callback();
      flushAnimationFrames();
    },
    respondWithWeekly({ resetsAt, resetCredits } = {}) {
      const requestId = sent.at(-1)?.request?.id;
      assert.ok(requestId);
      const secondary = { usedPercent: 72, windowDurationMins: 10080 };
      if (Number.isFinite(resetsAt)) secondary.resetsAt = resetsAt;
      const result = {
        rateLimits: {
          secondary,
        },
      };
      if (resetCredits !== undefined) result.rateLimitResetCredits = resetCredits;
      window.dispatch("message", {
        data: {
          hostId: "local",
          type: "mcp-response",
          message: {
            id: requestId,
            result,
          },
        },
      });
      flushAnimationFrames();
    },
    sent,
    setBridgeAvailable(available) {
      window.electronBridge = available
        ? { sendMessageFromView: sendBridgeMessage }
        : undefined;
    },
    mutationObservations: () => [...activeMutationObservers]
      .flatMap((observer) => observer.observations),
    triggerMutations(records) {
      for (const observer of [...activeMutationObservers]) observer.callback(records);
      flushAnimationFrames();
    },
    triggerObservedMutations(records) {
      for (const observer of [...activeMutationObservers]) {
        const matching = records.filter((record) => observer.observations.some((observation) => (
          record.type === "childList"
          && observation.options.childList === true
          && (observation.target === record.target
            || (observation.options.subtree === true
              && observation.target.contains(record.target)))
        )));
        if (matching.length > 0) observer.callback(matching);
      }
      flushAnimationFrames();
    },
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

test("window reset exposes a conservative countdown and exact local timestamp", async () => {
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
  const part = (number, width = 2) => String(number).padStart(width, "0");
  const expectedDisplay = `${part(resetDate.getFullYear(), 4)}-${part(resetDate.getMonth() + 1)}`
    + `-${part(resetDate.getDate())} ${part(resetDate.getHours())}`
    + `:${part(resetDate.getMinutes())}:${part(resetDate.getSeconds())}`;
  const expectedFull = new Intl.DateTimeFormat("en-US", {
    dateStyle: "full",
    timeStyle: "long",
  }).format(resetDate);
  assert.equal(
    model.formatMessage(english.windowReset, englishReset),
    expectedDisplay,
  );
  assert.equal(
    model.formatMessage(english.windowResetCountdown, englishReset),
    "Resets in 6d 16h",
  );
  assert.equal(englishReset.display, expectedDisplay);
  assert.equal(englishReset.full, expectedFull);
  assert.equal(
    model.formatMessage(chinese.windowResetCountdown, chineseReset),
    "距重置还有 6 天 16 小时",
  );
  assert.equal(
    model.formatMessage(chinese.windowReset, chineseReset),
    expectedDisplay,
  );
  assert.doesNotMatch(englishReset.full, /…|\.\.\./);

  const empty = model.normalizeSnapshot({ primary: null, secondary: { usedPercent: null } });
  assert.equal(model.presentWindows(empty, english).length, 0);
  assert.equal(model.summaryFor(model.presentWindows(empty, english), english), "");
});

test("available reset credits become a sorted 24-hour local expiry list", async () => {
  const model = await loadModel();
  const english = model.selectLocale("en-US");
  const chinese = model.selectLocale("zh-CN");
  const now = new Date(2030, 6, 22, 0, 3, 4).getTime();
  const first = new Date(2030, 7, 1, 14, 43, 44).getTime() / 1000;
  const second = new Date(2030, 7, 12, 18, 3, 4).getTime() / 1000;
  const normalized = model.normalizeResetCredits({
    availableCount: 5,
    credits: [
      { id: "opaque-second", status: "available", expiresAt: second, title: "Full reset" },
      { id: "redeemed", status: "redeemed", expiresAt: first },
      { id: "missing-expiry", status: "available", expiresAt: null },
      { id: "invalid-expiry", status: "available", expiresAt: Number.NaN },
      { id: "expired", status: "available", expiresAt: now / 1000 - 1 },
      { id: "opaque-first", status: "available", expiresAt: first },
    ],
  });
  const englishItems = model.presentResetCredits(normalized, english, "en-US", now);
  const chineseItems = model.presentResetCredits(normalized, chinese, "zh-CN", now);
  const expectedDateTime = "2030-08-01 14:43:44";

  assert.equal(englishItems.length, 2);
  assert.equal(englishItems[0].expiresAt, first);
  assert.equal(englishItems[1].expiresAt, second);
  assert.equal(englishItems[0].dateTime, expectedDateTime);
  assert.doesNotMatch(englishItems[0].dateTime, /\b(?:AM|PM)\b/i);
  assert.equal(
    model.formatMessage(english.resetCreditExpires, englishItems[0]),
    `Expires ${expectedDateTime}`,
  );
  assert.equal(
    model.formatMessage(english.resetCreditCountdown, englishItems[0]),
    "10d 14h remaining",
  );
  assert.equal(
    model.formatMessage(chinese.resetCreditCountdown, chineseItems[0]),
    "剩余 10 天 14 小时",
  );
  assert.match(model.formatMessage(chinese.resetCreditExpires, chineseItems[0]), /过期$/);
  assert.ok(englishItems[0].nextUpdateMs > 0);
  assert.ok(englishItems[0].nextUpdateMs <= 60 * 60 * 1000);
  const nearExpiry = model.presentResetCredits(
    [{ expiresAt: (now + 59 * 1000) / 1000 }],
    english,
    "en-US",
    now,
  )[0];
  assert.equal(nearExpiry.countdown, "0m");
  assert.equal(nearExpiry.nextUpdateMs, 59 * 1000);
  const exactHour = model.presentResetCredits(
    [{ expiresAt: (now + 60 * 60 * 1000) / 1000 }],
    english,
    "en-US",
    now,
  )[0];
  assert.equal(exactHour.countdown, "1h");
  assert.equal(exactHour.nextUpdateMs, 1000);
  assert.doesNotMatch(JSON.stringify(englishItems), /opaque|Full reset|Use reset/);
  assert.deepEqual(
    JSON.parse(JSON.stringify(model.normalizeResetCredits(null))),
    [],
  );
  assert.equal(model.normalizeResetCreditsUpdate(null), null);
  assert.deepEqual(
    JSON.parse(JSON.stringify(model.normalizeResetCreditsUpdate({ credits: [] }))),
    [],
  );
});

test("locale JSON selects an exact locale, then language family, then English", async () => {
  const model = await loadModel();
  const localeBundle = JSON.parse(
    await fs.readFile(new URL("locales.json", assetRoot), "utf8"),
  );
  assert.equal(localeBundle.supportedLocales.length, 65);
  assert.equal(new Set(localeBundle.supportedLocales).size, 65);
  for (const locale of localeBundle.supportedLocales) {
    const copy = model.selectLocale(locale);
    assert.equal(copy.locale.toLowerCase(), locale.toLowerCase());
    assert.match(copy.timeFormatNote, /24/);
  }
  assert.equal(model.selectLocale("zh-Hans-CN").locale, "zh-Hans-CN");
  assert.equal(model.selectLocale("fr-CA").usageTitle, "Limites d’utilisation");
  assert.equal(model.selectLocale("ja-JP").usageTitle, "使用上限");
  assert.equal(model.selectLocale("zh-TW").usageTitle, "使用額度");
  assert.equal(model.selectLocale("unknown").locale, "en-US");
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

  coordinator.finish(sent[1]);
  coordinator.scheduleCalibration(3000);
  clock.advance(2999);
  assert.equal(sent.length, 2);
  clock.advance(1);
  assert.equal(sent.length, 3);
  coordinator.dispose();
});

test("tooltip stays in the sidebar when possible and overflows only while attached", async () => {
  const model = await loadModel();
  const placement = model.computeTooltipPlacement({
    anchor: { left: 132, top: 700, right: 152, bottom: 720, width: 20, height: 20 },
    sidebar: { left: 0, top: 0, right: 160, bottom: 800, width: 160, height: 800 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 180 },
  });
  assert.equal(placement.left, 16);
  assert.ok(placement.left + placement.width > 160);
  assert.ok(placement.left + placement.width <= 884);
  assert.ok(placement.top >= 8);
  assert.ok(placement.top + 180 <= 792);
  assert.equal(placement.maximumHeight, 784);

  const oversized = model.computeTooltipPlacement({
    anchor: { left: 20, top: 60, right: 60, bottom: 80, width: 40, height: 20 },
    sidebar: { left: 0, top: 40, right: 180, bottom: 220, width: 180, height: 180 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 600 },
  });
  assert.equal(oversized.maximumHeight, 784);
  assert.ok(oversized.top >= 8);
  assert.ok(oversized.top + 600 <= 792);

  const leftEdge = model.computeTooltipPlacement({
    anchor: { left: -24, top: 300, right: 16, bottom: 320, width: 40, height: 20 },
    sidebar: { left: 0, top: 0, right: 180, bottom: 800, width: 180, height: 800 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 180 },
  });
  assert.equal(leftEdge.left, 16);

  const wideSidebar = model.computeTooltipPlacement({
    anchor: { left: 600, top: 300, right: 720, bottom: 320, width: 120, height: 20 },
    sidebar: { left: 0, top: 0, right: 760, bottom: 800, width: 760, height: 800 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 180 },
  });
  assert.equal(wideSidebar.left, 480);
  assert.ok(wideSidebar.left <= 600);
  assert.ok(wideSidebar.left + wideSidebar.width >= 720);
  assert.ok(wideSidebar.left >= 16);
  assert.ok(wideSidebar.left + wideSidebar.width <= 744);

  const rightEdge = model.computeTooltipPlacement({
    anchor: { left: 840, top: 300, right: 880, bottom: 320, width: 40, height: 20 },
    sidebar: { left: 700, top: 0, right: 900, bottom: 800, width: 200, height: 800 },
    viewport: { left: 0, top: 0, right: 900, bottom: 800, width: 900, height: 800 },
    tooltip: { width: 240, height: 180 },
  });
  assert.equal(rightEdge.left, 644);

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
  assert.match(runtime, /rateLimitResetCredits/);
  assert.doesNotMatch(runtime, /rateLimitResetCredit\/consume/);
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
  assert.match(css, /--opsail-refit-usage-details-width,\s*240px/);
  assert.match(css, /\.opsail-refit-codex-reset-credits-table/);
  assert.match(css, /\.opsail-refit-codex-reset-credits-row\s*\+\s*\.opsail-refit-codex-reset-credits-row/);
  assert.match(css, /\.opsail-refit-codex-time-format-note/);
  const noticeCss = css.match(/#opsail-refit-codex-launch-notice\s*\{[\s\S]*?\n\}/)?.[0];
  assert.ok(noticeCss);
  const noticeDeclaration = (property) => noticeCss
    .match(new RegExp(`\\n\\s*${property}:\\s*([^;]+);`))?.[1];
  const noticeBackground = noticeDeclaration("background");
  const noticeForeground = noticeDeclaration("color");
  assert.equal(noticeDeclaration("top"), "30%");
  assert.match(noticeBackground, /--color-token-activity-bar-badge-background/);
  assert.match(noticeForeground, /--color-token-activity-bar-badge-foreground/);
  const codexThemeFixture = new Map([
    ["--color-token-button-background", "same-button-color"],
    ["--color-token-button-foreground", "same-button-color"],
    ["--color-token-activity-bar-badge-background", "accent-background"],
    ["--color-token-activity-bar-badge-foreground", "accent-foreground"],
  ]);
  const resolveFixtureToken = (declaration) => [...declaration.matchAll(/var\((--[\w-]+)/g)]
    .map(([, token]) => codexThemeFixture.get(token))
    .find(Boolean);
  assert.notEqual(
    resolveFixtureToken(noticeBackground),
    resolveFixtureToken(noticeForeground),
  );
  assert.match(
    css,
    /\.opsail-refit-codex-launch-notice-message\s*\{[\s\S]*?color:\s*inherit/,
  );
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
  const controls = await Promise.all([
    assembleControlSource("probe"),
    assembleControlSource("early", { currentPayload: source }),
    assembleControlSource("status"),
    assembleControlSource("launch-notice"),
    assembleControlSource("disable"),
  ]);

  for (const [marker] of replacements) assert.doesNotMatch(source, new RegExp(marker));
  assert.doesNotThrow(() => new vm.Script(source));
  for (const control of controls) {
    for (const [marker] of control.replacements) {
      assert.doesNotMatch(control.source, new RegExp(marker));
    }
    assert.doesNotThrow(() => new vm.Script(control.source));
  }
});

test("persistent bootstrap waits for document initialization before installing observers", async () => {
  const { source } = await assembleRuntimeSource({ sessionMode: "persistent" });
  const harness = createRuntimeHarness({
    documentInitiallyUnavailable: true,
    nativeAccountRow: true,
  });

  assert.doesNotThrow(() => new vm.Script(source).runInContext(harness.context));
  let diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 0);
  assert.equal(diagnostics.mutationObserver, false);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  harness.mountDocument();
  harness.window.dispatch("load", {});
  harness.runPendingTimeouts();

  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 1);
  assert.equal(diagnostics.mutationObserver, true);
  assert.equal(diagnostics.visible, true);
  assert.equal(
    harness.document.getElementById("opsail-refit-codex-usage").children[0].textContent,
    "weekly 28%",
  );
});

test("persistent bootstrap retries local reads when the preload bridge becomes ready", async () => {
  const { source } = await assembleRuntimeSource({ sessionMode: "persistent" });
  const harness = createRuntimeHarness({
    bridgeAvailable: false,
    documentInitiallyUnavailable: true,
    nativeAccountRow: true,
  });
  new vm.Script(source).runInContext(harness.context);
  assert.equal(harness.sent.length, 0);

  harness.setBridgeAvailable(true);
  harness.mountDocument();
  harness.window.dispatch("load", {});

  assert.equal(harness.sent.length, 1);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().visible,
    true,
  );
  harness.window.__OPSAIL_REFIT_CODEX_STATE__.cleanup();
  assert.equal(harness.activeCounts().timeouts, 0);
});

test("bootstrap observes a nested app shell only until the account row is discovered", async () => {
  const { source } = await assembleRuntimeSource({ sessionMode: "persistent" });
  const harness = createRuntimeHarness({
    nativeAccountRow: true,
    sidebarAvailable: false,
  });
  const shellRoot = harness.document.createElement("main");
  harness.document.body.append(shellRoot);
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  assert.ok(harness.mutationObservations().some(({ options, target }) => (
    target === harness.document.body
    && options.childList === true
    && options.subtree === true
  )));

  harness.attachSidebar(shellRoot);
  harness.triggerObservedMutations([{
    type: "childList",
    target: shellRoot,
    addedNodes: [harness.document.sidebar],
    removedNodes: [],
  }]);
  harness.runPendingTimeouts();

  const diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.ok(harness.mutationObservations().some(({ options, target }) => (
    target === harness.nativeLayout.row
    && options.childList === true
    && options.subtree === true
  )));
  assert.equal(harness.mutationObservations().some(({ options, target }) => (
    target === harness.document.sidebar && options.subtree === true
  )), false);
});

test("runtime mounts one inline capsule and keeps it stable across remeasurement", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

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

  harness.document.nativeLayoutQueryRoots.length = 0;
  harness.triggerResize();
  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(host.parentElement, harness.nativeLayout.row);
  assert.equal(host.nextElementSibling, harness.nativeLayout.trailingAction);
  assert.equal(
    harness.nativeLayout.row.children.filter((child) => child === host).length,
    1,
  );
  assert.ok(harness.document.nativeLayoutQueryRoots.length > 0);
  assert.equal(
    harness.document.nativeLayoutQueryRoots.some((root) => root === harness.document.sidebar),
    false,
  );
  assert.ok(
    harness.document.nativeLayoutQueryRoots.every((root) => root === harness.nativeLayout.row),
  );
});

test("stable runtime observes the account row without reacting to chat-list mutations", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const observations = harness.mutationObservations();
  assert.ok(observations.some(({ options, target }) => (
    target === harness.nativeLayout.row
    && options.childList === true
    && options.subtree === true
  )));
  assert.equal(observations.some(({ options, target }) => (
    target === harness.document.sidebar && options.subtree === true
  )), false);

  const state = harness.window.__OPSAIL_REFIT_CODEX_STATE__;
  const layoutsBefore = state.metrics.layoutCalls;
  const chatAction = harness.document.createElement("button");
  chatAction.matches = (selector) => selector === "button, [role='button']";
  harness.document.sidebar.append(chatAction);
  harness.triggerObservedMutations([{
    type: "childList",
    target: harness.document.sidebar,
    addedNodes: [chatAction],
    removedNodes: [],
  }]);

  assert.equal(state.metrics.layoutCalls, layoutsBefore);
});

test("settings removal widens only to structural ancestors and narrows after account recovery", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  const sessionList = harness.document.createElement("div");
  harness.document.sidebar.append(sessionList);
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const state = harness.window.__OPSAIL_REFIT_CODEX_STATE__;
  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const row = harness.nativeLayout.row;
  row.remove();
  harness.triggerObservedMutations([{
    type: "childList",
    target: harness.document.sidebar,
    addedNodes: [],
    removedNodes: [row],
  }]);
  harness.runPendingTimeouts();

  assert.equal(state.diagnostics().visible, false);
  assert.ok(harness.mutationObservations().some(({ options, target }) => (
    target === harness.document.sidebar
    && options.childList === true
    && options.subtree === false
  )));
  assert.equal(harness.mutationObservations().some(({ options, target }) => (
    target === harness.document.sidebar && options.subtree === true
  )), false);

  const ensuresBeforeLoading = state.metrics.ensureCalls;
  const layoutsBeforeLoading = state.metrics.layoutCalls;
  const loadingNode = harness.document.createElement("div");
  sessionList.append(loadingNode);
  harness.triggerObservedMutations([{
    type: "childList",
    target: sessionList,
    addedNodes: [loadingNode],
    removedNodes: [],
  }]);
  assert.equal(state.metrics.ensureCalls, ensuresBeforeLoading);
  assert.equal(state.metrics.layoutCalls, layoutsBeforeLoading);

  harness.document.sidebar.append(row);
  harness.triggerObservedMutations([{
    type: "childList",
    target: harness.document.sidebar,
    addedNodes: [row],
    removedNodes: [],
  }]);
  harness.runPendingTimeouts();

  assert.equal(state.diagnostics().visible, true);
  assert.equal(host.parentElement, row);
  assert.ok(harness.mutationObservations().some(({ options, target }) => (
    target === row && options.childList === true && options.subtree === true
  )));
  assert.equal(harness.mutationObservations().some(({ options, target }) => (
    target === harness.document.sidebar && options.subtree === true
  )), false);
});

test("runtime follows Codex language changes instead of the system language", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({
    resetsAt: Math.floor(Date.now() / 1000) + (6 * 24 + 16) * 60 * 60,
    resetCredits: {
      availableCount: 3,
      credits: [
        {
          id: "later",
          status: "available",
          expiresAt: Date.UTC(2030, 7, 12, 12, 0, 0) / 1000,
          title: "Full reset",
        },
        {
          id: "redeemed",
          status: "redeemed",
          expiresAt: Date.UTC(2030, 7, 2, 12, 0, 0) / 1000,
        },
        {
          id: "earlier",
          status: "available",
          expiresAt: Date.UTC(2030, 7, 1, 12, 0, 0) / 1000,
        },
      ],
    },
  });

  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const details = harness.document.getElementById("opsail-refit-codex-usage-details");
  const meta = details.children[1].children[1];
  const resetCredits = details.children[3];
  const resetCreditTitle = resetCredits.children[0];
  const resetCreditTable = resetCredits.children[1];
  const resetCreditBody = resetCreditTable.children[0];
  const timeFormatNote = details.children[4];
  assert.equal(details.parentElement, harness.document.body);
  assert.equal(details.attributes.get("role"), "tooltip");
  host.dispatch("pointerenter", {});
  harness.triggerResize();
  assert.equal(details.dataset.opsailRefitCodexOpen, "true");
  assert.equal(details.attributes.get("aria-hidden"), "false");
  assert.equal(host.children[0].textContent, "weekly 28%");
  assert.match(meta.textContent, /^72% used\nResets /);
  assert.match(
    meta.textContent,
    /^72% used\nResets in \d+d \d+h\n\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/,
  );
  assert.equal(resetCredits.hidden, false);
  assert.equal(resetCreditTitle.textContent, "Usage limit resets");
  assert.equal(resetCreditTable.tagName, "TABLE");
  assert.equal(resetCreditBody.tagName, "TBODY");
  assert.equal(resetCreditBody.children.length, 2);
  assert.equal(resetCreditBody.children[0].children.length, 2);
  assert.match(
    resetCreditBody.children[0].children[0].textContent,
    /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/,
  );
  assert.match(resetCreditBody.children[0].children[1].textContent, /^\d+d \d+h$/);
  assert.equal(timeFormatNote.textContent, "Times use local time (24-hour clock).");
  assert.equal(harness.activeCounts().timeouts, 1);
  assert.doesNotMatch(
    resetCreditBody.children
      .flatMap((item) => item.children.map((part) => part.textContent))
      .join(" "),
    /Full reset|Use reset|Expires|remaining/,
  );
  harness.triggerLanguage("zh-CN");
  assert.equal(host.children[0].textContent, "周剩余 28%");
  assert.match(
    meta.textContent,
    /^已用 72%\n距重置还有 \d+ 天 \d+ 小时\n\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/,
  );
  assert.match(meta.attributes.get("aria-label"), /^已用 72%。.*重置$/);
  assert.equal(resetCreditTitle.textContent, "可用重置");
  assert.match(
    resetCreditBody.children[0].children[0].textContent,
    /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/,
  );
  assert.match(resetCreditBody.children[0].children[1].textContent, /^\d+ 天/);
  assert.equal(timeFormatNote.textContent, "时间均为本地时间（24 小时制）");
  assert.equal(
    details.attributes.get("aria-label"),
    "使用额度",
  );
  harness.window.__OPSAIL_REFIT_CODEX_STATE__.cleanup();
  assert.equal(harness.activeCounts().timeouts, 0);
});

test("runtime reads localeOverride and recalibrates it after Settings restores the account row", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  const localeRequests = () => harness.bridgeMessages.filter(
    (message) => message?.request?.method === "config/read",
  );
  const respondWithLocale = (request, localeOverride) => {
    harness.window.dispatch("message", {
      data: {
        hostId: "local",
        type: "mcp-response",
        message: {
          id: request.request.id,
          result: {
            config: {
              desktop: {
                localeOverride,
                unrelatedSensitiveValue: "must-not-be-retained",
              },
            },
          },
        },
      },
    });
  };

  assert.equal(localeRequests().length, 1);
  assert.deepEqual(
    JSON.parse(JSON.stringify(localeRequests()[0].request.params)),
    { cwd: null, includeLayers: false },
  );
  respondWithLocale(localeRequests()[0], "en-US");
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const state = harness.window.__OPSAIL_REFIT_CODEX_STATE__;
  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const row = harness.nativeLayout.row;
  assert.equal(host.children[0].textContent, "weekly 28%");
  assert.equal(state.diagnostics().language, "en-US");
  assert.equal(Object.hasOwn(state, "config"), false);

  row.remove();
  harness.triggerObservedMutations([{
    type: "childList",
    target: harness.document.sidebar,
    addedNodes: [],
    removedNodes: [row],
  }]);
  harness.runPendingTimeouts();
  harness.document.sidebar.append(row);
  harness.triggerObservedMutations([{
    type: "childList",
    target: harness.document.sidebar,
    addedNodes: [row],
    removedNodes: [],
  }]);
  harness.runPendingTimeouts();

  assert.equal(localeRequests().length, 2);
  respondWithLocale(localeRequests()[1], "zh-CN");
  assert.equal(host.children[0].textContent, "周剩余 28%");
  assert.equal(state.diagnostics().language, "zh-CN");
  assert.doesNotMatch(JSON.stringify(state.diagnostics()), /must-not-be-retained/);

  harness.window.dispatch("focus", {});
  assert.equal(localeRequests().length, 2);
  harness.advanceNow(1_001);
  harness.window.dispatch("focus", {});
  assert.equal(localeRequests().length, 3);
  respondWithLocale(localeRequests()[2], "zh-CN");
  assert.equal(state.metrics.localeRequests, 3);
});

test("opening usage details recalibrates reset countdowns from the current time", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({
    resetCredits: {
      credits: [{
        status: "available",
        expiresAt: (harness.now() + (2 * 60 + 45) * 60 * 1000) / 1000,
      }],
    },
  });

  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const details = harness.document.getElementById("opsail-refit-codex-usage-details");
  const countdownText = () => (
    details.children[3].children[1].children[0].children[0].children[1].textContent
  );
  assert.equal(countdownText(), "2h");
  const beforeHover = new vm.Script("Date.now()").runInContext(harness.context);
  harness.advanceNow(60 * 60 * 1000);
  assert.equal(
    new vm.Script("Date.now()").runInContext(harness.context),
    beforeHover + 60 * 60 * 1000,
  );
  assert.equal(countdownText(), "2h");

  host.dispatch("pointerenter", {});
  assert.equal(details.dataset.opsailRefitCodexOpen, "true");
  assert.equal(countdownText(), "1h");
});

test("an initially omitted reset-credit field gets one bounded calibration and preserves later data", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  assert.equal(harness.sent.length, 1);
  harness.respondWithWeekly();
  assert.equal(harness.activeCounts().timeouts, 1);

  harness.runPendingTimeouts();
  assert.equal(harness.sent.length, 2);
  harness.respondWithWeekly({
    resetCredits: {
      credits: [{
        status: "available",
        expiresAt: (harness.now() + 10 * 24 * 60 * 60 * 1000) / 1000,
      }],
    },
  });
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditCount,
    1,
  );
  assert.equal(harness.activeCounts().timeouts, 0);

  harness.advanceNow(60 * 1000);
  harness.window.dispatch("focus", {});
  assert.equal(harness.sent.length, 3);
  harness.respondWithWeekly();
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditCount,
    1,
  );
  assert.equal(harness.activeCounts().timeouts, 0);
});

test("null reset-credit snapshots stay provisional and cannot erase a valid list", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  harness.respondWithWeekly({ resetCredits: null });
  assert.equal(harness.activeCounts().timeouts, 1);
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditsResolved,
    false,
  );

  harness.runPendingTimeouts();
  assert.equal(harness.sent.length, 2);
  harness.respondWithWeekly({
    resetCredits: {
      credits: [{
        status: "available",
        expiresAt: (harness.now() + 10 * 24 * 60 * 60 * 1000) / 1000,
      }],
    },
  });
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditCount,
    1,
  );
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditsResolved,
    true,
  );

  harness.advanceNow(60 * 1000);
  harness.window.dispatch("focus", {});
  harness.respondWithWeekly({ resetCredits: null });
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditCount,
    1,
  );

  harness.advanceNow(60 * 1000);
  harness.window.dispatch("focus", {});
  harness.respondWithWeekly({ resetCredits: { credits: [] } });
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditCount,
    0,
  );
});

test("quota windows and delayed reset credits merge through the same payload path", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  harness.respondWithWeekly({ resetCredits: null });
  let diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(diagnostics.resetCreditCount, 0);

  harness.runPendingTimeouts();
  const requestId = harness.sent.at(-1)?.request?.id;
  assert.ok(requestId);
  harness.window.dispatch("message", {
    data: {
      hostId: "local",
      type: "mcp-response",
      message: {
        id: requestId,
        result: {
          rateLimitResetCredits: {
            credits: [{
              status: "available",
              expiresAt: (harness.now() + 10 * 24 * 60 * 60 * 1000) / 1000,
            }],
          },
        },
      },
    },
  });

  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(diagnostics.stale, false);
  assert.equal(diagnostics.resetCreditCount, 1);
  assert.equal(diagnostics.resetCreditsResolved, true);

  harness.window.dispatch("message", {
    data: {
      hostId: "local",
      type: "mcp-notification",
      method: "account/rateLimits/updated",
      params: { rateLimitResetCredits: { credits: [] } },
    },
  });
  diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.visible, true);
  assert.equal(diagnostics.resetCreditCount, 0);
});

test("unresolved reset credits use bounded startup calibration delays", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  for (let response = 0; response < 4; response += 1) {
    harness.respondWithWeekly({ resetCredits: null });
    const expectedPending = response < 3 ? 1 : 0;
    assert.equal(harness.activeCounts().timeouts, expectedPending);
    if (expectedPending) harness.runPendingTimeouts();
  }

  assert.equal(harness.sent.length, 4);
  assert.equal(
    harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().resetCreditsResolved,
    false,
  );
});

test("an unusable startup snapshot gets one bounded calibration before the capsule stays hidden", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);

  const requestId = harness.sent.at(-1)?.request?.id;
  assert.ok(requestId);
  harness.window.dispatch("message", {
    data: {
      hostId: "local",
      type: "mcp-response",
      message: {
        id: requestId,
        result: {
          rateLimits: {
            primary: null,
            secondary: {
              usedPercent: null,
              windowDurationMins: 10080,
            },
          },
          rateLimitResetCredits: null,
        },
      },
    },
  });
  assert.equal(harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().visible, false);
  assert.equal(harness.activeCounts().timeouts, 1);

  harness.runPendingTimeouts();
  assert.equal(harness.sent.length, 2);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });
  assert.equal(harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().visible, true);
  assert.equal(harness.activeCounts().timeouts, 0);
});

test("runtime restores the capsule when a routed sidebar returns under a new wrapper", async () => {
  const { source } = await assembleRuntimeSource();
  const harness = createRuntimeHarness({ nativeAccountRow: true });
  new vm.Script(source).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const host = harness.document.getElementById("opsail-refit-codex-usage");
  const sidebar = harness.document.sidebar;
  const steadyCounts = harness.activeCounts();
  for (let cycle = 0; cycle < 3; cycle += 1) {
    const previousParent = sidebar.parentElement;
    sidebar.remove();
    harness.triggerObservedMutations([{
      type: "childList",
      target: previousParent,
      addedNodes: [],
      removedNodes: [sidebar],
    }]);
    harness.runPendingTimeouts();
    assert.equal(harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().visible, false);

    const routeWrapper = harness.document.createElement("div");
    harness.document.body.append(routeWrapper);
    harness.triggerObservedMutations([{
      type: "childList",
      target: harness.document.body,
      addedNodes: [routeWrapper],
      removedNodes: [],
    }]);
    host.remove();
    routeWrapper.append(sidebar);
    harness.triggerObservedMutations([{
      type: "childList",
      target: routeWrapper,
      addedNodes: [sidebar],
      removedNodes: [],
    }]);
    harness.runPendingTimeouts();

    const restored = harness.document.getElementById("opsail-refit-codex-usage");
    assert.ok(restored);
    assert.equal(restored === host, true);
    assert.equal(restored.hidden, false);
    assert.equal(restored.parentElement, harness.nativeLayout.row);
    assert.equal(harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics().visible, true);
    assert.deepEqual(harness.activeCounts(), steadyCounts);
  }
});

test("repeated renderer installation stays singular and cleanup releases every resource", async () => {
  const { source } = await assembleRuntimeSource();
  const script = new vm.Script(source);
  const harness = createRuntimeHarness();

  script.runInContext(harness.context);
  let diagnostics = harness.window.__OPSAIL_REFIT_CODEX_STATE__.diagnostics();
  assert.equal(diagnostics.hostCount, 0);
  assert.equal(diagnostics.visible, false);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });
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
  harness.respondWithWeekly({ resetCredits: { credits: [] } });
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

test("shared renderer control routes status and disable without leaking resources", async () => {
  const [{ source: runtime }, { source: status }, { source: disable }] = await Promise.all([
    assembleRuntimeSource(),
    assembleControlSource("status"),
    assembleControlSource("disable"),
  ]);
  const harness = createRuntimeHarness();
  new vm.Script(runtime).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const statusResult = new vm.Script(status).runInContext(harness.context);
  assert.equal(statusResult.installed, true);
  assert.equal(statusResult.hostCount, 1);
  assert.equal(statusResult.styleCount, 1);
  assert.equal(statusResult.detailsCount, 1);

  const disableResult = new vm.Script(disable).runInContext(harness.context);
  assert.equal(disableResult.clean, true);
  assert.equal(harness.window.__OPSAIL_REFIT_CODEX_STATE__, undefined);
  assert.deepEqual(harness.activeCounts(), {
    animationFrames: 0,
    eventListeners: 0,
    intervals: 0,
    mutationObservers: 0,
    resizeObservers: 0,
    timeouts: 0,
  });
});

test("launch success notice is localized, centered, singular, and cleaned up", async () => {
  const [
    { source: runtime },
    { source: launchNotice },
    { source: disable },
  ] = await Promise.all([
    assembleRuntimeSource(),
    assembleControlSource("launch-notice"),
    assembleControlSource("disable"),
  ]);
  const harness = createRuntimeHarness();
  new vm.Script(runtime).runInContext(harness.context);
  harness.respondWithWeekly({ resetCredits: { credits: [] } });

  const first = new vm.Script(launchNotice).runInContext(harness.context);
  assert.equal(first.shown, true);
  let notice = harness.document.getElementById("opsail-refit-codex-launch-notice");
  assert.ok(notice);
  assert.equal(notice.parentElement, harness.document.body);
  assert.equal(notice.attributes.get("role"), "status");
  assert.equal(notice.attributes.get("aria-live"), "polite");
  assert.equal(notice.children[0].textContent, "Opsail mode enabled");
  assert.equal(notice.children[1].textContent, "Usage display is ready.");
  assert.equal(harness.activeCounts().timeouts, 1);

  harness.triggerLanguage("zh-CN");
  const second = new vm.Script(launchNotice).runInContext(harness.context);
  assert.equal(second.shown, true);
  assert.equal(
    harness.document.querySelectorAll("#opsail-refit-codex-launch-notice").length,
    1,
  );
  notice = harness.document.getElementById("opsail-refit-codex-launch-notice");
  assert.equal(notice.children[0].textContent, "已进入 Opsail 模式");
  assert.equal(notice.children[1].textContent, "额度显示已成功注入");
  assert.equal(harness.activeCounts().timeouts, 1);

  const result = new vm.Script(disable).runInContext(harness.context);
  assert.equal(result.clean, true);
  assert.equal(harness.document.getElementById("opsail-refit-codex-launch-notice"), null);
  assert.equal(harness.activeCounts().timeouts, 0);
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
  const bodyObservation = harness.mutationObservations()
    .find(({ target }) => target === harness.document.body);
  assert.ok(bodyObservation);
  assert.equal(bodyObservation.options.childList, true);
  assert.equal(bodyObservation.options.subtree, true);
  const ensureCalls = state.metrics.ensureCalls;
  const unrelatedContainer = harness.document.createElement("div");
  const unrelatedContent = harness.document.createElement("article");
  harness.document.body.append(unrelatedContainer);
  unrelatedContainer.append(unrelatedContent);
  harness.triggerObservedMutations([{
    type: "childList",
    target: unrelatedContainer,
    addedNodes: [unrelatedContent],
    removedNodes: [],
  }]);
  assert.equal(state.metrics.ensureCalls, ensureCalls);
  assert.equal(state.cleanup(), true);
});

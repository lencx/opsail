(async () => {
  const PATCH_KEY = "__opsailCodexModelPickerUnlockV1";
  const MAIN_RENDERER_URL = "app://-/index.html";
  const MODEL_CONFIG_KEY = "107580212";
  const MAX_MODEL_COUNT = 2048;
  const MAX_GRAPH_NODES = 5000;
  const MAX_OBJECT_KEYS = 256;
  if (location.href !== MAIN_RENDERER_URL) {
    return {
      title: document.title,
      url: location.href,
      status: "ignored-non-primary-renderer",
    };
  }

  const state = window[PATCH_KEY] || { failures: [] };
  window[PATCH_KEY] = state;
  if (state.installed) {
    return {
      title: document.title,
      url: location.href,
      status: "already-installed",
    };
  }
  state.installed = true;

  const isModelDescriptor = (value) =>
    value && typeof value === "object" && typeof value.model === "string";
  const isModelArray = (value) =>
    Array.isArray(value) &&
    value.length <= MAX_MODEL_COUNT &&
    value.every(isModelDescriptor);

  const patchModelArray = (models) => {
    if (!isModelArray(models)) return false;
    let changed = false;
    for (const model of models) {
      if (model.hidden !== false) {
        model.hidden = false;
        changed = true;
      }
    }
    return changed;
  };

  const patchModelContainer = (value) => {
    if (!value || typeof value !== "object") return false;
    let changed = false;
    for (const candidate of [
      value.models,
      value.data,
      value.result,
      value.result?.data,
      value.result?.models,
      value.message?.result?.data,
      value.message?.result?.models,
    ]) {
      if (patchModelArray(candidate)) changed = true;
    }
    const resemblesModelConfig =
      "availableModels" in value ||
      "available_models" in value ||
      "useHiddenModels" in value ||
      "use_hidden_models" in value;
    if (resemblesModelConfig && value.useHiddenModels !== false) {
      value.useHiddenModels = false;
      changed = true;
    }
    if (resemblesModelConfig && value.use_hidden_models !== false) {
      value.use_hidden_models = false;
      changed = true;
    }
    return changed;
  };

  const patchObjectGraph = (
    root,
    visited = new WeakSet(),
    budget = { remaining: MAX_GRAPH_NODES },
    depth = 0,
  ) => {
    if (
      !root ||
      typeof root !== "object" ||
      visited.has(root) ||
      budget.remaining <= 0 ||
      depth > 6
    ) {
      return false;
    }
    budget.remaining -= 1;
    visited.add(root);
    let changed = patchModelContainer(root);
    if (
      root instanceof Element ||
      root === window ||
      root === document ||
      root === document.body
    ) {
      return changed;
    }
    for (const key of Object.keys(root).slice(0, MAX_OBJECT_KEYS)) {
      if (
        [
          "ownerDocument",
          "parentElement",
          "parentNode",
          "children",
          "childNodes",
        ].includes(key)
      ) {
        continue;
      }
      try {
        if (patchObjectGraph(root[key], visited, budget, depth + 1)) {
          changed = true;
        }
      } catch (_) {}
    }
    return changed;
  };

  const patchStatsigConfig = (config) => {
    if (!config?.value || typeof config.value !== "object") return config;
    const next = {
      ...config.value,
      use_hidden_models: false,
      useHiddenModels: false,
    };
    try {
      config.value = next;
      return config;
    } catch (_) {
      return { ...config, value: next };
    }
  };

  const statsigClients = () => {
    const root = window.__STATSIG__ || globalThis.__STATSIG__;
    if (!root || typeof root !== "object") return [];
    const clients = [
      root.firstInstance,
      typeof root.instance === "function" ? root.instance() : null,
    ];
    if (root.instances && typeof root.instances === "object") {
      clients.push(...Object.values(root.instances));
    }
    return clients.filter(
      (client, index, all) =>
        client && typeof client === "object" && all.indexOf(client) === index,
    );
  };

  const patchStatsig = () => {
    for (const client of statsigClients()) {
      if (typeof client.getDynamicConfig !== "function") continue;
      if (!client.__opsailModelWhitelistPatched) {
        const original = client.getDynamicConfig.bind(client);
        client.getDynamicConfig = (name, options) => {
          const config = original(name, options);
          return name === MODEL_CONFIG_KEY ? patchStatsigConfig(config) : config;
        };
        client.__opsailModelWhitelistPatched = true;
      }
      try {
        patchStatsigConfig(
          client.getDynamicConfig(MODEL_CONFIG_KEY, {
            disableExposureLog: true,
          }),
        );
      } catch (_) {}
    }
  };

  if (!state.responsePatchInstalled && typeof Response !== "undefined") {
    state.responsePatchInstalled = true;
    state.originalResponseJson = Response.prototype.json;
    Response.prototype.json = async function opsailPatchedJson(...args) {
      const data = await state.originalResponseJson.apply(this, args);
      try {
        patchModelContainer(data);
        patchObjectGraph(data);
      } catch (_) {}
      return data;
    };
  }

  const reactFiberKeys = (element) =>
    Object.keys(element || {}).filter(
      (key) =>
        key.startsWith("__reactFiber") ||
        key.startsWith("__reactInternalInstance") ||
        key.startsWith("__reactProps"),
    );

  const patchReactState = () => {
    const visited = new WeakSet();
    const budget = { remaining: MAX_GRAPH_NODES };
    const nodes = [
      document.body,
      ...document.querySelectorAll(
        "button, [role='menu'], [role='dialog'], [data-radix-popper-content-wrapper]",
      ),
    ].filter(Boolean);
    for (const node of nodes.slice(0, 200)) {
      for (const key of reactFiberKeys(node)) {
        patchObjectGraph(node[key], visited, budget);
      }
    }
  };

  const sweep = () => {
    try {
      patchStatsig();
      patchReactState();
      state.lastSweepAt = Date.now();
    } catch (error) {
      if (state.failures.length < 8) {
        state.failures.push(String(error?.message || error).slice(0, 256));
      }
    }
  };

  sweep();
  if (!state.interval) state.interval = setInterval(sweep, 1500);

  return {
    title: document.title,
    url: location.href,
    status: "installed",
    patchKey: PATCH_KEY,
  };
})()

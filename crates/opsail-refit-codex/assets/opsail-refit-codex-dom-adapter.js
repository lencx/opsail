const createOpsailRefitCodexDomAdapter = () => {
  const VERSION = 1;
  const SELECTORS = Object.freeze({
    shell: "main.main-surface",
    sidebar: "aside.app-shell-left-panel, aside[data-testid='app-shell-floating-left-panel']",
    avatar: "img, [data-testid*='avatar' i], [class*='avatar' i]",
    action: "button, [role='button']",
    accountControl: "button, [role='button'], a",
  });

  const queryOne = (root, selector) => {
    try {
      return root?.querySelector?.(selector) || null;
    } catch {
      return null;
    }
  };

  const findShell = (root = document) => queryOne(root, SELECTORS.shell);
  const findSidebar = (root = document) => queryOne(root, SELECTORS.sidebar);

  const bridgeAvailable = () => (
    typeof window.electronBridge?.sendMessageFromView === "function"
  );

  const languageCandidates = () => {
    const systemLanguages = typeof navigator === "object"
      ? [...(navigator.languages || []), navigator.language]
      : [];
    return [...new Set([
      document.documentElement?.lang,
      ...systemLanguages,
    ].map((value) => String(value || "").trim()).filter(Boolean))];
  };

  const probeRenderer = () => ({
    appProtocol: typeof location === "object" && location.protocol === "app:",
    shell: Boolean(findShell()),
    sidebar: Boolean(findSidebar()),
    bridge: bridgeAvailable(),
    domAdapterVersion: VERSION,
  });

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

  const closestAccountControl = (element) => {
    try {
      return element?.closest?.(SELECTORS.accountControl) || null;
    } catch {
      return null;
    }
  };

  const measureNativeLayout = (sidebar) => {
    const sidebarRect = elementRect(sidebar);
    if (!sidebarRect) return null;
    const footerTop = sidebarRect.bottom - Math.min(112, sidebarRect.height * 0.3);
    const avatars = queryRects(sidebar, SELECTORS.avatar)
      .filter(({ rect }) => rect.width >= 16 && rect.width <= 48
        && rect.height >= 16 && rect.height <= 48
        && Math.abs(rect.width - rect.height) <= 8
        && rect.centerY >= footerTop
        && rect.centerY <= sidebarRect.bottom
        && rect.centerX >= sidebarRect.left
        && rect.centerX <= sidebarRect.left + sidebarRect.width * 0.55)
      .sort((left, right) => right.rect.centerY - left.rect.centerY);
    const avatar = avatars[0] || null;
    const actions = queryRects(sidebar, SELECTORS.action)
      .filter(({ rect }) => rect.width >= 20 && rect.width <= 48
        && rect.height >= 20 && rect.height <= 48
        && rect.centerY >= footerTop
        && rect.centerY <= sidebarRect.bottom
        && rect.centerX >= sidebarRect.left + sidebarRect.width * 0.5
        && rect.centerX <= sidebarRect.right)
      .sort((left, right) => {
        const avatarCenterY = avatar?.rect.centerY ?? sidebarRect.bottom;
        return Math.abs(left.rect.centerY - avatarCenterY)
          - Math.abs(right.rect.centerY - avatarCenterY)
          || right.rect.width * right.rect.height - left.rect.width * left.rect.height
          || right.rect.centerY - left.rect.centerY
          || right.rect.centerX - left.rect.centerX;
      });
    const accountControlElement = closestAccountControl(avatar?.element);
    const accountControlRect = elementRect(accountControlElement);
    const accountControl = accountControlElement && accountControlRect
      ? { element: accountControlElement, rect: accountControlRect }
      : null;
    const layoutForAction = (trailingAction) => {
      const row = nearestCommonAncestor(accountControlElement, trailingAction?.element);
      const accountSlot = directChildContaining(row, accountControlElement);
      const trailingSlot = directChildContaining(row, trailingAction?.element);
      const rowRect = elementRect(row);
      const inline = Boolean(
        row && row !== sidebar && accountSlot && trailingSlot && accountSlot !== trailingSlot
        && rowRect && rowRect.width >= 120 && rowRect.height >= 28 && rowRect.height <= 72
        && Math.abs((avatar?.rect.centerY || 0) - (trailingAction?.rect.centerY || 0)) <= 16,
      );
      return { accountSlot, inline, row, trailingAction, trailingSlot };
    };
    const layouts = actions.map(layoutForAction);
    const selected = layouts.find(({ inline }) => inline) || layouts[0] || layoutForAction(null);
    return {
      sidebarRect,
      avatar,
      accountControl,
      trailingAction: selected.trailingAction,
      row: selected.inline ? selected.row : null,
      accountSlot: selected.inline ? selected.accountSlot : null,
      trailingSlot: selected.inline ? selected.trailingSlot : null,
    };
  };

  const nodeMayAffectLayout = (node) => {
    if (!node || typeof node !== "object") return false;
    try {
      return Boolean(
        node.matches?.(SELECTORS.avatar)
        || node.matches?.(SELECTORS.action)
        || node.querySelector?.(SELECTORS.avatar)
        || node.querySelector?.(SELECTORS.action),
      );
    } catch {
      return true;
    }
  };

  return Object.freeze({
    VERSION,
    SELECTORS,
    bridgeAvailable,
    elementRect,
    findShell,
    findSidebar,
    languageCandidates,
    measureNativeLayout,
    nodeMayAffectLayout,
    probeRenderer,
  });
};

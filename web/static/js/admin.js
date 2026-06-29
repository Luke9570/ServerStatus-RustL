(function () {
  const tokenKey = "ssr_admin_token";
  const adminThemeKey = "ssr_admin_theme";
  const homepageThemeKey = "chakra-ui-color-mode";
  const $ = (selector) => document.querySelector(selector);
  const $$ = (selector) => [...document.querySelectorAll(selector)];

  const state = {
    token: localStorage.getItem(tokenKey) || sessionStorage.getItem(tokenKey) || "",
    config: null,
    stats: null,
    settings: {},
    deletedHosts: new Set(),
    deletedAccessKeys: new Set(),
    editor: null,
    dirty: false,
    saving: false,
    theme: localStorage.getItem(adminThemeKey) || localStorage.getItem(homepageThemeKey) || "system",
    localDirty: {
      tg: false,
      bark: false,
      access: false,
      expire: false,
    },
    localBaseline: {
      tg: "",
      bark: "",
      access: "",
      expire: "",
    },
  };

  const pageMeta = {
    servers: ["服务器", "节点列表、备注、账单和展示排序"],
    "server-groups": ["服务器分组", "按用途归类节点，不参与 agent 接入认证"],
    "alert-rules": ["告警规则", "离线、CPU、内存、硬盘和负载持续触发提醒"],
    notifications: ["通知方式", "Telegram 与 Bark 通知通道"],
    settings: ["设置", "接入地址、到期提醒和后台基础设置"],
  };

  const cycleOptions = [
    ["", "未设置"],
    ["Day", "每天"],
    ["Week", "每周"],
    ["Month", "每月"],
    ["Quarter", "每季度"],
    ["HalfYear", "每半年"],
    ["Year", "每年"],
  ];
  const currencyOptions = [
    ["USD", "USD"],
    ["EUR", "EUR"],
    ["GBP", "GBP"],
    ["CNY", "CNY"],
    ["HKD", "HKD"],
    ["MOP", "MOP"],
    ["TWD", "TWD"],
    ["JPY", "JPY"],
    ["KRW", "KRW"],
    ["SGD", "SGD"],
    ["AUD", "AUD"],
    ["CAD", "CAD"],
    ["NZD", "NZD"],
    ["MYR", "MYR"],
    ["THB", "THB"],
    ["VND", "VND"],
    ["PHP", "PHP"],
    ["IDR", "IDR"],
    ["INR", "INR"],
    ["BRL", "BRL"],
    ["TRY", "TRY"],
    ["RUB", "RUB"],
    ["CUSTOM", "自定义"],
  ];
  const currencySymbols = new Map([
    ["$", "USD"],
    ["€", "EUR"],
    ["£", "GBP"],
    ["¥", "JPY"],
    ["￥", "CNY"],
    ["₩", "KRW"],
  ]);
  const metricOptions = [
    ["offline", "离线"],
    ["cpu", "CPU 使用率"],
    ["memory", "内存使用率"],
    ["disk", "硬盘使用率"],
    ["load1", "1 分钟负载"],
    ["load5", "5 分钟负载"],
    ["load15", "15 分钟负载"],
  ];
  const permanentValues = new Set(["0000-00-00", "never", "permanent", "lifetime", "forever"]);
  const freeValues = new Set(["0", "free", "免费", "免費"]);

  function ensureSettings() {
    const settings = state.settings || {};
    settings.hosts ||= {};
    settings.groups ||= {};
    settings.deleted_hosts ||= [];
    settings.server_groups ||= [];
    settings.access_keys ||= {};
    settings.deleted_access_keys ||= [];
    settings.notification_groups ||= [];
    settings.alert_rules ||= [];
    if (settings.notification_groups.length) {
      settings.alert_rules = settings.alert_rules.map((rule) => {
        if (rule.notifications?.length || !rule.notification_group) {
          return rule;
        }
        const group = settings.notification_groups.find((item) => item.id === rule.notification_group);
        return { ...rule, notification_group: "", notifications: group?.notifications || [] };
      });
    }
    settings.notification_groups = [];
    state.settings = settings;
    state.deletedHosts = new Set(settings.deleted_hosts || []);
    state.deletedAccessKeys = new Set(settings.deleted_access_keys || []);
    return settings;
  }

  function text(target, content) {
    const element = typeof target === "string" ? $(target) : target;
    if (element) {
      element.textContent = content || "";
    }
  }

  function setView(name) {
    $("#login").hidden = name !== "login";
    $("#dashboard").hidden = name !== "dashboard";
  }

  function authHeaders(withJson) {
    const headers = {};
    if (state.token) {
      headers.Authorization = `Bearer ${state.token}`;
    }
    if (withJson) {
      headers["Content-Type"] = "application/json";
    }
    return headers;
  }

  function clearSession() {
    state.token = "";
    sessionStorage.removeItem(tokenKey);
    localStorage.removeItem(tokenKey);
  }

  function updateSaveButton() {
    const saveButton = $("#save");
    if (!saveButton) {
      return;
    }
    saveButton.disabled = state.saving || !state.dirty;
    saveButton.textContent = state.saving ? "保存中..." : "保存";
  }

  function markDirty(message = "有未保存更改") {
    state.dirty = true;
    if (message) {
      text("#save-message", message);
    }
    updateSaveButton();
  }

  function markPristine(message = "") {
    state.dirty = false;
    state.saving = false;
    if (message !== undefined) {
      text("#save-message", message);
    }
    updateSaveButton();
  }

  function setSaveBusy(isBusy, message = "") {
    state.saving = isBusy;
    if (message) {
      text("#save-message", message);
    }
    updateSaveButton();
  }

  function showToast(message, tone = "success") {
    const toast = $("#toast");
    if (!toast) {
      return;
    }
    window.clearTimeout(Number(toast.dataset.timer || 0));
    toast.className = `toast ${tone}`;
    toast.textContent = message;
    toast.hidden = false;
    toast.dataset.timer = String(
      window.setTimeout(() => {
        toast.hidden = true;
        toast.textContent = "";
      }, tone === "warn" ? 4600 : 3200),
    );
  }

  function setButtonBusy(button, busy, busyText = "保存中...") {
    if (!button) {
      return;
    }
    if (!button.dataset.defaultText) {
      button.dataset.defaultText = button.textContent;
    }
    button.disabled = busy;
    button.textContent = busy ? busyText : button.dataset.defaultText;
  }

  function localButton(scope) {
    return $(`#${scope}-save`);
  }

  function localMessage(scope) {
    return $(`#${scope}-save-message`);
  }

  function updateLocalSaveButton(scope) {
    const button = localButton(scope);
    if (!button) {
      return;
    }
    button.disabled = !state.localDirty[scope];
    button.textContent = "保存";
  }

  function setLocalDirty(scope, dirty, message = "") {
    if (!(scope in state.localDirty)) {
      return;
    }
    state.localDirty[scope] = dirty;
    const messageNode = localMessage(scope);
    if (messageNode) {
      messageNode.textContent = message;
    }
    updateLocalSaveButton(scope);
  }

  function stableJson(value) {
    return JSON.stringify(value);
  }

  function localSnapshot(scope) {
    if (scope === "tg") {
      return stableJson(collectTgbotSettings());
    }
    if (scope === "bark") {
      return stableJson(collectBarkSettings());
    }
    if (scope === "access") {
      return stableJson(collectAccessSettings());
    }
    if (scope === "expire") {
      return stableJson(collectExpireNotifySettings());
    }
    return "";
  }

  function resetLocalBaseline(scope, message = "") {
    if (!(scope in state.localBaseline)) {
      return;
    }
    state.localBaseline[scope] = localSnapshot(scope);
    setLocalDirty(scope, false, message);
  }

  function refreshLocalDirty(scope) {
    if (!(scope in state.localBaseline)) {
      return;
    }
    const dirty = localSnapshot(scope) !== state.localBaseline[scope];
    setLocalDirty(scope, dirty, dirty ? "有未保存更改" : "");
  }

  function hasUnsavedChanges() {
    return state.dirty || Object.values(state.localDirty).some(Boolean);
  }

  function settingsPayloadFromState(overrides = {}) {
    ensureSettings();
    return {
      hosts: state.settings.hosts || {},
      groups: state.settings.groups || {},
      deleted_hosts: [...state.deletedHosts],
      server_groups: state.settings.server_groups || [],
      access_keys: state.settings.access_keys || {},
      deleted_access_keys: state.settings.deleted_access_keys || [],
      notification_groups: [],
      alert_rules: state.settings.alert_rules || [],
      access_base_url: state.settings.access_base_url || "",
      agent_base_url: state.settings.agent_base_url || "",
      expire_notify: state.settings.expire_notify,
      tgbot: state.settings.tgbot,
      bark: state.settings.bark,
      ...overrides,
    };
  }

  async function postSettings(payload) {
    const response = await fetch("/api/admin/settings", {
      method: "POST",
      headers: authHeaders(true),
      body: JSON.stringify(payload),
    });
    return readJson(response);
  }

  async function postNotifyTest(kind, payload) {
    const response = await fetch(`/api/admin/notify-test/${encodeURIComponent(kind)}`, {
      method: "POST",
      headers: authHeaders(true),
      body: JSON.stringify(payload),
    });
    return readJson(response);
  }

  async function saveSettingsPayload(payload, options = {}) {
    const {
      successMessage = "已同步到后端",
      messageTarget = "#save-message",
      render = "all",
      busyButton = null,
      markClean = true,
    } = options;
    setButtonBusy(busyButton, true);
    try {
      const responsePayload = await postSettings(payload);
      state.settings = responsePayload.data || {};
      ensureSettings();
      if (render === "all") {
        renderAll();
      } else if (render === "tables") {
        renderTables();
      } else if (render === "notifications") {
        renderNotifications();
        renderAlertRules();
      } else if (render === "settings") {
        renderSettings();
      }
      if (markClean) {
        markPristine("");
      }
      text(messageTarget, successMessage);
      showToast(successMessage);
      return true;
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      const message = `保存失败: ${err.message}`;
      text(messageTarget, message);
      showToast(message, "warn");
      return false;
    } finally {
      setButtonBusy(busyButton, false);
    }
  }

  async function readJson(response) {
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = payload.error || payload.message || `${response.status} ${response.statusText}`;
      const err = new Error(message);
      err.authExpired = response.status === 401 || response.status === 403;
      if (err.authExpired) {
        clearSession();
      }
      throw err;
    }
    return payload;
  }

  async function getJson(url) {
    const response = await fetch(url, {
      headers: authHeaders(false),
      cache: "no-store",
    });
    return readJson(response);
  }

  async function deleteJson(url) {
    const response = await fetch(url, {
      method: "DELETE",
      headers: authHeaders(false),
      cache: "no-store",
    });
    return readJson(response);
  }

  function el(tag, className, content) {
    const node = document.createElement(tag);
    if (className) {
      node.className = className;
    }
    if (content !== undefined && content !== null) {
      node.textContent = content;
    }
    return node;
  }

  function input(className, value, type = "text") {
    const node = document.createElement("input");
    node.className = className || "";
    node.type = type;
    node.value = value ?? "";
    return node;
  }

  function datePicker(className, value) {
    const wrap = el("div", "date-field");
    const node = input(className, normalizeDateValue(value), "date");
    node.placeholder = "YYYY-MM-DD";
    node.min = "1970-01-01";
    node.max = "9999-12-31";
    node.title = "请选择日期，保存格式固定为 YYYY-MM-DD";
    const button = iconButton("打开日历", "calendar");
    button.classList.add("date-picker-button");
    button.addEventListener("click", () => {
      if (typeof node.showPicker === "function") {
        node.showPicker();
      } else {
        node.focus();
      }
    });
    wrap.append(node, button);
    return wrap;
  }

  function moneyControl(value) {
    const parsed = parsedAmount(value);
    const wrap = el("div", "money-field");
    const amount = input("js-server-amount", parsed.amount, "number");
    amount.min = "0";
    amount.step = "0.01";
    amount.inputMode = "decimal";
    amount.placeholder = "金额";
    const currency = select(currencyOptions, parsed.currency, "js-server-currency");
    const custom = input("js-server-currency-custom", parsed.customCurrency);
    custom.placeholder = "货币";
    custom.maxLength = 3;
    custom.pattern = "[A-Za-z]{3}";
    custom.title = "自定义货币请输入 3 位字母代码，例如 USD、EUR、HKD";
    wrap.append(amount, currency, custom);
    return wrap;
  }

  function checkbox(checked) {
    const node = document.createElement("input");
    node.type = "checkbox";
    node.checked = Boolean(checked);
    return node;
  }

  function select(options, value, className = "") {
    const node = document.createElement("select");
    node.className = className;
    for (const [optionValue, label] of options) {
      const option = document.createElement("option");
      option.value = optionValue;
      option.textContent = label;
      node.append(option);
    }
    node.value = value ?? "";
    return node;
  }

  function field(labelText, control, className = "") {
    const label = document.createElement("label");
    if (className) {
      label.className = className;
    }
    label.append(document.createTextNode(labelText), control);
    return label;
  }

  function hintField(labelText, control, hint, className = "") {
    const label = field(labelText, control, className);
    label.append(el("span", "field-hint", hint));
    return label;
  }

  function svgIcon(name) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    const paths = {
      refresh: ["M21 12a9 9 0 0 1-15.5 6.2", "M3 12A9 9 0 0 1 18.5 5.8", "M3 18v-6h6", "M21 6v6h-6"],
      eye: [
        "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z",
        "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
      ],
      "eye-off": [
        "M3 3l18 18",
        "M10.6 10.6A3 3 0 0 0 13.4 13.4",
        "M9.9 5.2A10.8 10.8 0 0 1 12 5c6.5 0 10 7 10 7a18.8 18.8 0 0 1-3 4.1",
        "M6.7 6.7C3.7 8.6 2 12 2 12s3.5 7 10 7c1.5 0 2.8-.4 4-.9",
      ],
      copy: ["M8 8h10v10H8Z", "M6 14H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7a2 2 0 0 1 2 2v1"],
      trash: ["M3 6h18", "M8 6V4h8v2", "M6 6l1 14h10l1-14", "M10 11v6", "M14 11v6"],
      sun: ["M12 4V2", "M12 22v-2", "M4.93 4.93 3.52 3.52", "M20.48 20.48l-1.41-1.41", "M4 12H2", "M22 12h-2", "M4.93 19.07l-1.41 1.41", "M20.48 3.52l-1.41 1.41", "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10Z"],
      moon: ["M21 12.8A8.5 8.5 0 1 1 11.2 3 6.7 6.7 0 0 0 21 12.8Z"],
      monitor: ["M4 5h16v11H4Z", "M9 21h6", "M12 16v5"],
      user: ["M20 21a8 8 0 0 0-16 0", "M12 13a5 5 0 1 0 0-10 5 5 0 0 0 0 10Z"],
      "log-out": ["M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4", "M16 17l5-5-5-5", "M21 12H9"],
      calendar: [
        "M8 2v4",
        "M16 2v4",
        "M3 10h18",
        "M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
      ],
    };
    for (const d of paths[name] || []) {
      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute("d", d);
      svg.append(path);
    }
    return svg;
  }

  function iconButton(label, iconName) {
    const button = document.createElement("button");
    button.className = "icon-button";
    button.type = "button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.append(svgIcon(iconName));
    return button;
  }

  function setIconButtonIcon(button, label, iconName) {
    button.title = label;
    button.setAttribute("aria-label", label);
    button.replaceChildren(svgIcon(iconName));
  }

  function resolveTheme(mode = state.theme) {
    if (mode === "light" || mode === "dark") {
      return mode;
    }
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  function themeLabel(mode = state.theme) {
    return {
      light: "浅色",
      dark: "深色",
      system: "跟随系统",
    }[mode] || "跟随系统";
  }

  function themeIcon(mode = state.theme) {
    return {
      light: "sun",
      dark: "moon",
      system: "monitor",
    }[mode] || "monitor";
  }

  function applyTheme(mode = state.theme) {
    const nextMode = ["system", "light", "dark"].includes(mode) ? mode : "system";
    const resolved = resolveTheme(nextMode);
    state.theme = nextMode;
    localStorage.setItem(adminThemeKey, nextMode);
    if (nextMode === "light" || nextMode === "dark") {
      localStorage.setItem(homepageThemeKey, nextMode);
    }
    document.documentElement.dataset.theme = nextMode;
    document.documentElement.classList.toggle("dark", resolved === "dark");
    document.body.style.backgroundColor = resolved === "dark" ? "#334155" : "#edf2f7";
    const button = $("#theme-toggle");
    if (button) {
      setIconButtonIcon(button, `主题: ${themeLabel(nextMode)}`, themeIcon(nextMode));
    }
  }

  function cycleTheme() {
    const modes = ["system", "light", "dark"];
    const index = modes.indexOf(state.theme);
    applyTheme(modes[(index + 1) % modes.length]);
    showToast(`已切换为${themeLabel(state.theme)}`);
  }

  function actionButton(label, className = "secondary", iconName = "") {
    const button = document.createElement("button");
    button.type = "button";
    button.className = className;
    if (iconName) {
      button.classList.add("has-icon");
      button.append(svgIcon(iconName), document.createTextNode(label));
    } else {
      button.textContent = label;
    }
    return button;
  }

  function stableId(prefix) {
    const bytes = new Uint8Array(4);
    if (window.crypto && window.crypto.getRandomValues) {
      window.crypto.getRandomValues(bytes);
    } else {
      bytes.forEach((_, index) => {
        bytes[index] = Math.floor(Math.random() * 256);
      });
    }
    return `${prefix}-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  }

  function randomSecret(length = 24) {
    const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789_-";
    const bytes = new Uint8Array(length);
    if (window.crypto && window.crypto.getRandomValues) {
      window.crypto.getRandomValues(bytes);
    } else {
      bytes.forEach((_, index) => {
        bytes[index] = Math.floor(Math.random() * 256);
      });
    }
    return Array.from(bytes, (byte) => chars[byte % chars.length]).join("");
  }

  async function copyText(value) {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(value);
      return;
    }
    const textarea = document.createElement("textarea");
    textarea.value = value;
    textarea.setAttribute("readonly", "readonly");
    textarea.style.position = "fixed";
    textarea.style.left = "-9999px";
    document.body.append(textarea);
    textarea.select();
    document.execCommand("copy");
    textarea.remove();
  }

  function normalizeBaseUrl(value) {
    let url = String(value || "").trim().replace(/\/+$/, "");
    if (url.endsWith("/report")) {
      url = url.slice(0, -"/report".length);
    }
    return url;
  }

  function labelsToMap(labels) {
    const map = new Map();
    String(labels || "")
      .split(";")
      .forEach((part) => {
        const index = part.indexOf("=");
        if (index <= 0) {
          return;
        }
        const key = part.slice(0, index).trim();
        const value = part.slice(index + 1).trim();
        if (key) {
          map.set(key, value);
        }
      });
    return map;
  }

  function labelValue(labels, key) {
    return labelsToMap(labels).get(key) || "";
  }

  function setLabelValue(labels, key, value) {
    const map = labelsToMap(labels);
    const nextValue = String(value || "").trim();
    if (nextValue) {
      map.set(key, nextValue);
    } else {
      map.delete(key);
    }
    return [...map.entries()].map(([itemKey, itemValue]) => `${itemKey}=${itemValue}`).join(";");
  }

  function isPermanentExpire(value) {
    const normalized = String(value || "").trim().toLowerCase();
    return normalized.startsWith("0000-00-00") || permanentValues.has(normalized);
  }

  function isFreeAmount(value) {
    return freeValues.has(String(value || "").trim().toLowerCase());
  }

  function normalizeDateValue(value) {
    const textValue = String(value || "").trim();
    const match = /^(\d{4})[-/](\d{1,2})[-/](\d{1,2})$/.exec(textValue);
    if (!match) {
      return "";
    }
    const year = Number(match[1]);
    const month = Number(match[2]);
    const day = Number(match[3]);
    const date = new Date(Date.UTC(year, month - 1, day));
    if (date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day) {
      return "";
    }
    return `${String(year).padStart(4, "0")}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
  }

  function parsedAmount(value) {
    const raw = String(value || "").trim();
    if (!raw || isFreeAmount(raw)) {
      return { amount: "", currency: "USD", customCurrency: "" };
    }

    const symbol = raw.slice(0, 1);
    const symbolCurrency = currencySymbols.get(symbol);
    if (symbolCurrency) {
      return {
        amount: raw.slice(1).trim().replace(",", "."),
        currency: symbolCurrency,
        customCurrency: "",
      };
    }

    const trailing = /^(\d+(?:[.,]\d+)?)\s*([A-Za-z]{3})$/.exec(raw);
    if (trailing) {
      const code = trailing[2].toUpperCase();
      const known = currencyOptions.some(([item]) => item === code);
      return {
        amount: trailing[1].replace(",", "."),
        currency: known ? code : "CUSTOM",
        customCurrency: known ? "" : code,
      };
    }

    const leading = /^([A-Za-z]{3})\s*(\d+(?:[.,]\d+)?)$/.exec(raw);
    if (leading) {
      const code = leading[1].toUpperCase();
      const known = currencyOptions.some(([item]) => item === code);
      return {
        amount: leading[2].replace(",", "."),
        currency: known ? code : "CUSTOM",
        customCurrency: known ? "" : code,
      };
    }

    const numeric = /^(\d+(?:[.,]\d+)?)$/.exec(raw);
    if (numeric) {
      return { amount: numeric[1].replace(",", "."), currency: "USD", customCurrency: "" };
    }

    return { amount: raw, currency: "CUSTOM", customCurrency: "" };
  }

  function composeAmount(amountValue, currencyValue, customCurrencyValue) {
    const amount = String(amountValue || "").trim();
    if (!amount) {
      return "";
    }
    const currency = currencyValue === "CUSTOM" ? String(customCurrencyValue || "").trim().toUpperCase() : currencyValue;
    return currency ? `${amount} ${currency}` : amount;
  }

  function amountDisplay(value) {
    return isFreeAmount(value) ? "免费" : value || "";
  }

  function cycleLabel(value) {
    return cycleOptions.find(([item]) => item === value)?.[1] || value || "";
  }

  function metricLabel(value) {
    return metricOptions.find(([item]) => item === value)?.[1] || value || "-";
  }

  function formatExpireSummary(billing) {
    const endDate = billing?.end_date || "";
    if (!endDate) {
      return "未设置";
    }
    if (isPermanentExpire(endDate)) {
      return amountDisplay(billing?.amount) ? `永久 · ${amountDisplay(billing.amount)}` : "永久";
    }
    const auto = ["1", "true", "yes", "on"].includes(String(billing.auto_renewal).toLowerCase());
    const cycle = billing.cycle ? ` · ${cycleLabel(billing.cycle)}` : "";
    const amount = amountDisplay(billing.amount);
    return `${endDate}${auto ? " · 自动续期" : ""}${cycle}${amount ? ` · ${amount}` : ""}`;
  }

  function statOs(stat, labels) {
    return labelValue(labels, "os") || stat?.sys_info?.os_release || stat?.host_type || "";
  }

  function statRegion(stat) {
    const ipInfo = stat?.ip_info || {};
    return stat?.location || ipInfo.country_code || ipInfo.country || "";
  }

  function billingFrom(configItem, overrideItem, statItem) {
    const billing = configItem?.billing || {};
    const overrideBilling = overrideItem?.billing || {};
    return {
      end_date:
        overrideBilling.end_date ??
        overrideItem?.expire ??
        configItem?.expire ??
        billing.end_date ??
        statItem?.expire?.raw ??
        "",
      auto_renewal:
        overrideBilling.auto_renewal ??
        billing.auto_renewal ??
        (statItem?.expire?.auto_renewal ? "1" : "0"),
      cycle: overrideBilling.cycle ?? billing.cycle ?? statItem?.expire?.cycle ?? "",
      amount: overrideBilling.amount ?? billing.amount ?? statItem?.expire?.amount ?? "",
    };
  }

  function serverGroups() {
    ensureSettings();
    const configured = state.settings.server_groups || [];
    if (configured.length) {
      return configured;
    }
    const groups = new Map();
    for (const server of serverItems(false)) {
      if (!server.gid) {
        continue;
      }
      if (!groups.has(server.gid)) {
        groups.set(server.gid, { id: server.gid, name: server.gid, servers: [] });
      }
      groups.get(server.gid).servers.push(server.id);
    }
    return [...groups.values()];
  }

  function serverGroupsForServer(id) {
    return serverGroups()
      .filter((group) => (group.servers || []).includes(id))
      .map((group) => group.name || group.id);
  }

  function serverItems(includeRuntimeGroups = true) {
    ensureSettings();
    const deletedHosts = new Set([...(state.settings.deleted_hosts || []), ...state.deletedHosts]);
    const configHosts = new Map((state.config?.hosts || []).map((host) => [host.name, host]));
    const stats = state.stats?.servers || [];
    for (const server of stats) {
      if (!configHosts.has(server.name)) {
        configHosts.set(server.name, {
          name: server.name,
          alias: server.alias,
          gid: server.gid,
          labels: server.labels || "",
          weight: server.weight,
          expire_notify: server.expire_notify,
          billing: {},
        });
      }
    }

    return [...configHosts.values()].filter((host) => !deletedHosts.has(host.name)).map((host) => {
      const stat = stats.find((server) => server.name === host.name);
      const override = state.settings.hosts?.[host.name] || {};
      const labels = host.labels || stat?.labels || "";
      const groups = includeRuntimeGroups ? serverGroupsForServer(host.name) : [];
      return {
        id: host.name,
        alias: override.alias ?? host.alias ?? stat?.alias ?? host.name,
        gid: host.gid || stat?.gid || "",
        stat,
        labels,
        groups,
        os: statOs(stat, labels),
        region: statRegion(stat),
        note: override.note ?? labelValue(labels, "note"),
        public_note: override.public_note ?? labelValue(labels, "public_note"),
        spec: override.spec ?? labelValue(labels, "spec"),
        weight: override.weight ?? host.weight ?? stat?.weight ?? "",
        expire_notify: override.expire_notify ?? host.expire_notify,
        billing: billingFrom(host, override, stat),
      };
    });
  }

  function accessKeyItems() {
    ensureSettings();
    const deleted = new Set([...(state.settings.deleted_access_keys || []), ...state.deletedAccessKeys]);
    const items = new Map();
    (state.config?.hosts_group || []).forEach((group) => {
      if (deleted.has(group.gid)) {
        return;
      }
      const override = state.settings.access_keys?.[group.gid] || {};
      items.set(group.gid, {
        id: group.gid,
        source_gid: override.source_gid || group.gid,
        runtime: Boolean(state.settings.access_keys?.[group.gid]),
        location: override.location || group.location || "",
        type: override.type || group.type || "",
        notify: override.notify ?? group.notify,
        labels: override.labels || group.labels || "",
        expire: override.expire || group.expire || "",
        billing: billingFrom(group, override, null),
        expire_notify: override.expire_notify ?? group.expire_notify,
        weight: override.weight ?? group.weight ?? "",
      });
    });

    Object.entries(state.settings.access_keys || {}).forEach(([id, value]) => {
      if (deleted.has(id) || items.has(id)) {
        return;
      }
      items.set(id, {
        id,
        source_gid: value.source_gid || id,
        runtime: true,
        location: value.location || "",
        type: value.type || "",
        notify: value.notify !== false,
        labels: value.labels || "",
        expire: value.expire || "",
        billing: value.billing || {},
        expire_notify: value.expire_notify !== false,
        weight: value.weight || "",
      });
    });
    return [...items.values()].sort((a, b) => a.id.localeCompare(b.id));
  }

  function statusPill(server) {
    const span = el("span", `status-pill ${server?.online4 || server?.online6 ? "online" : "offline"}`);
    span.textContent = server?.online4 || server?.online6 ? "在线" : "离线";
    return span;
  }

  function tagList(items, emptyText = "-") {
    const wrap = el("div", "tag-list");
    if (!items || !items.length) {
      wrap.append(el("span", "muted", emptyText));
      return wrap;
    }
    items.forEach((item) => wrap.append(el("span", "tag", item)));
    return wrap;
  }

  function serverRow(item) {
    const row = document.createElement("tr");
    row.dataset.id = item.id;
    const online = Boolean(item.stat?.online4 || item.stat?.online6);

    const name = document.createElement("td");
    const nameWrap = el("div", "node-name");
    nameWrap.append(el("strong", "", item.alias || item.id), el("span", "", `ID: ${item.id}`));
    name.append(nameWrap);

    const status = document.createElement("td");
    status.append(statusPill(item.stat));

    const group = document.createElement("td");
    group.append(tagList(item.groups, "未分组"));

    const system = document.createElement("td");
    const systemWrap = el("div", "stack");
    systemWrap.append(el("strong", "", item.os || "-"), el("span", "muted", item.region || "未知地区"));
    system.append(systemWrap);

    const note = document.createElement("td");
    note.append(el("span", "pill", item.public_note || item.spec || "无备注"));

    const expire = document.createElement("td");
    expire.append(el("span", "pill", formatExpireSummary(item.billing)));

    const weight = document.createElement("td");
    weight.append(el("span", "muted", item.weight ? String(item.weight) : "默认"));

    const actions = document.createElement("td");
    const actionWrap = el("div", "row-actions");
    const edit = actionButton("编辑", "secondary compact-action");
    edit.addEventListener("click", () => openServerEditor(item.id));
    actionWrap.append(edit);
    const remove = iconButton(online ? "停止 Agent 或离线后删除" : "删除服务器", "trash");
    remove.classList.add("danger-icon");
    remove.addEventListener("click", () => deleteServer(item, row));
    actionWrap.append(remove);
    actionWrap.append(el("span", "row-message js-row-message"));
    actions.append(actionWrap);

    row.append(name, status, group, system, note, expire, weight, actions);
    return row;
  }

  function renderServers() {
    $("#server-rows").replaceChildren(...serverItems().map(serverRow));
  }

  function renderServerGroups() {
    const rows = serverGroups().map((group) => {
      const row = document.createElement("tr");
      const servers = serverItems(false)
        .filter((server) => (group.servers || []).includes(server.id))
        .map((server) => server.alias || server.id);

      const name = document.createElement("td");
      name.append(el("strong", "", group.name || group.id), el("div", "muted", group.id));
      const serverCell = document.createElement("td");
      serverCell.append(tagList(servers, "无服务器"));
      const actions = document.createElement("td");
      const edit = actionButton("编辑");
      edit.addEventListener("click", () => openServerGroupEditor(group.id));
      actions.append(edit);
      row.append(name, serverCell, actions);
      return row;
    });
    $("#server-group-rows").replaceChildren(...rows);
  }

  function renderAlertRules() {
    ensureSettings();
    const rows = state.settings.alert_rules.map((rule) => {
      const methods = document.createElement("td");
      methods.append(notificationTagsForRule(rule));
      const row = document.createElement("tr");
      row.append(
        el("td", "", rule.name || rule.id),
        el("td", "", metricLabel(rule.metric)),
        el("td", "", rule.metric === "offline" ? "-" : String(rule.threshold ?? "-")),
        el("td", "", `${rule.duration || 0}s`),
        alertRuleTargetCell(rule),
        methods,
        el("td", "", rule.enabled === false ? "停用" : "启用"),
      );
      const actions = document.createElement("td");
      const edit = actionButton("编辑");
      edit.addEventListener("click", () => openAlertRuleEditor(rule.id));
      actions.append(edit);
      row.append(actions);
      return row;
    });
    $("#alert-rule-rows").replaceChildren(...rows);
  }

  function alertRuleTargetCell(rule) {
    const cell = document.createElement("td");
    const targets = [];
    const groups = serverGroups();
    (rule.server_groups || []).forEach((id) => {
      const group = groups.find((item) => item.id === id);
      targets.push(`分组: ${group?.name || id}`);
    });
    (rule.servers || []).forEach((id) => {
      const server = serverItems(false).find((item) => item.id === id);
      targets.push(`节点: ${server?.alias || id}`);
    });
    cell.append(tagList(targets, "全部"));
    return cell;
  }

  function renderNotifications() {
    renderTgbotNotification();
    renderBarkNotification();
  }

  function renderTgbotNotification() {
    const tg = state.settings?.tgbot || state.config?.tgbot || {};
    $("#tg-enabled").checked = Boolean(tg.enabled);
    $("#tg-token").value = "";
    $("#tg-chat").value = "";
    $("#tg-title").value = tg.title || "";
    $("#tg-expire").value = tg.expire_tpl || "";
    $("#tg-health").value = tg.health_tpl || "";
    resetLocalBaseline("tg", "");
  }

  function renderBarkNotification() {
    const bark = state.settings?.bark || state.config?.bark || {};
    $("#bark-enabled").checked = Boolean(bark.enabled);
    $("#bark-server").value = bark.server || "https://api.day.app";
    $("#bark-key").value = "";
    $("#bark-title").value = bark.title || "ServerStatus";
    $("#bark-group").value = bark.group || "ServerStatus";
    $("#bark-expire").value = bark.expire_tpl || "";
    $("#bark-health").value = bark.health_tpl || "";
    resetLocalBaseline("bark", "");
  }

  function renderSettings() {
    const expireNotify = state.settings?.expire_notify || state.config?.expire_notify || {};
    $("#access-base-url").value = state.settings?.access_base_url || "";
    $("#agent-base-url").value = state.settings?.agent_base_url || "";
    $("#expire-enabled").checked = Boolean(expireNotify.enabled);
    $("#expire-days").value = (expireNotify.days || [30, 14, 7, 3, 1, 0]).join(",");
    $("#expire-interval").value = expireNotify.interval || 86400;
    $("#admin-username").value = state.config?.admin?.username || $("#username").value.trim() || "admin";
    text("#topbar-user", $("#admin-username").value || "admin");
    renderDeletedHosts();
    resetLocalBaseline("access", "");
    resetLocalBaseline("expire", "");
  }

  function renderDeletedHosts() {
    const wrap = $("#deleted-hosts");
    if (!wrap) {
      return;
    }
    const ids = [...state.deletedHosts].sort((a, b) => a.localeCompare(b));
    if (!ids.length) {
      wrap.replaceChildren(el("span", "muted", "没有已删除服务器"));
      return;
    }
    const toolbar = el("div", "restore-toolbar");
    toolbar.append(el("span", "muted", `${ids.length} 台已删除服务器`));
    const clear = actionButton("一键清空", "danger secondary compact-action");
    clear.addEventListener("click", clearDeletedHosts);
    toolbar.append(clear);
    wrap.replaceChildren(
      toolbar,
      ...ids.map((id) => {
        const row = el("div", "restore-row");
        row.append(el("span", "", id));
        const actions = el("div", "restore-actions");
        const restore = actionButton("恢复", "secondary compact-action");
        restore.addEventListener("click", async () => {
          restore.disabled = true;
          state.deletedHosts.delete(id);
          state.settings.deleted_hosts = [...state.deletedHosts];
          renderTables();
          await saveSettingsPayload(settingsPayloadFromState(), {
            successMessage: "服务器已恢复并同步到后端",
            render: "tables",
          });
        });
        const purge = actionButton("彻底删除", "danger secondary compact-action");
        purge.addEventListener("click", () => purgeDeletedHost(id));
        actions.append(restore, purge);
        row.append(actions);
        return row;
      }),
    );
  }

  async function purgeDeletedHost(id) {
    if (!window.confirm(`彻底删除 ${id} 的已删除记录和当前缓存？如果同 ID 的 Agent 之后继续上报，会作为新记录重新出现。`)) {
      return;
    }
    try {
      const payload = await deleteJson(`/api/admin/deleted-hosts/${encodeURIComponent(id)}`);
      state.settings = payload.data || {};
      ensureSettings();
      renderAll();
      showToast(`${id} 已彻底删除，后续上报会重新接入`);
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      showToast(`彻底删除失败: ${err.message}`, "warn");
    }
  }

  async function clearDeletedHosts() {
    if (!state.deletedHosts.size) {
      return;
    }
    if (!window.confirm("确定清空所有已删除服务器记录和当前缓存？如果这些 Agent 之后继续上报，会作为新记录重新出现。")) {
      return;
    }
    try {
      const payload = await deleteJson("/api/admin/deleted-hosts");
      state.settings = payload.data || {};
      ensureSettings();
      renderAll();
      showToast("已清空已删除服务器，后续上报会重新接入");
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      showToast(`清空失败: ${err.message}`, "warn");
    }
  }

  function renderAccessKeys() {
    $("#access-key-rows").replaceChildren(...accessKeyItems().map(accessKeyRow));
  }

  function accessKeyRow(item) {
    const row = document.createElement("tr");
    row.dataset.id = item.id;
    row.dataset.sourceId = item.source_gid || item.id;
    row.dataset.runtime = item.runtime ? "1" : "0";
    row.dataset.unsaved = item.unsaved ? "1" : "0";

    const idCell = document.createElement("td");
    const idInput = input("js-access-id", item.id);
    idCell.append(idInput);

    const passwordCell = document.createElement("td");
    const passwordInput = input("js-access-password", "", "password");
    passwordInput.placeholder = item.unsaved ? "新密钥必填" : "留空保持原密码";
    const passwordWrap = el("div", "password-field");
    const randomButton = iconButton("随机密码", "refresh");
    randomButton.addEventListener("click", () => {
      passwordInput.value = randomSecret();
      passwordInput.type = "password";
      row.dataset.secretLoaded = "1";
      rowMessage(row, "已生成", "success");
    });
    const showButton = iconButton("显示密码", "eye");
    showButton.addEventListener("click", () => toggleAccessSecret(row, passwordInput, showButton));
    passwordWrap.append(passwordInput, randomButton, showButton);
    passwordCell.append(passwordWrap);

    const locationCell = document.createElement("td");
    const locationInput = input("js-access-location", item.location || "");
    locationInput.placeholder = "留空自动识别";
    locationCell.append(locationInput);
    const typeCell = document.createElement("td");
    const typeInput = input("js-access-type", item.type || "");
    typeInput.placeholder = "留空自动识别虚拟化";
    typeCell.append(typeInput);
    const labelsCell = document.createElement("td");
    labelsCell.append(input("js-access-labels", item.labels || ""));

    const actions = document.createElement("td");
    const actionWrap = el("div", "access-row-actions");
    const message = el("span", "row-message js-row-message");
    const remove = iconButton("删除", "trash");
    remove.classList.add("danger-icon");
    remove.addEventListener("click", () => {
      if (row.dataset.unsaved !== "1" && row.dataset.id) {
        state.deletedAccessKeys.add(row.dataset.id);
      }
      row.remove();
      markDirty("接入密钥已删除，记得保存");
    });
    actionWrap.append(remove, message);
    actions.append(actionWrap);

    row.append(idCell, passwordCell, locationCell, typeCell, labelsCell, actions);
    return row;
  }

  function rowMessage(row, message, tone = "muted") {
    const messageNode = row.querySelector(".js-row-message");
    if (!messageNode) {
      text("#save-message", message);
      return;
    }
    messageNode.className = `row-message js-row-message ${tone}`;
    messageNode.textContent = message || "";
    if (message) {
      window.clearTimeout(Number(messageNode.dataset.timer || 0));
      messageNode.dataset.timer = String(
        window.setTimeout(() => {
          messageNode.textContent = "";
        }, tone === "success" ? 2200 : 4200),
      );
    }
  }

  async function deleteServer(item, row) {
    const online = Boolean(item.stat?.online4 || item.stat?.online6);
    if (online) {
      rowMessage(row, "请先停止 Agent 或等待节点离线后再删除", "warn");
      return;
    }
    ensureSettings();
    state.deletedHosts.add(item.id);
    delete state.settings.hosts[item.id];
    state.settings.deleted_hosts = [...state.deletedHosts];
    state.settings.server_groups = serverGroups().map((group) => ({
      ...group,
      servers: (group.servers || []).filter((serverId) => serverId !== item.id),
    }));
    state.settings.alert_rules = (state.settings.alert_rules || []).map((rule) => ({
      ...rule,
      servers: (rule.servers || []).filter((serverId) => serverId !== item.id),
    }));
    rowMessage(row, "同步中...");
    row.remove();
    await saveSettingsPayload(settingsPayloadFromState(), {
      successMessage: "服务器已删除并同步到后端",
      render: "tables",
    });
  }

  async function toggleAccessSecret(row, inputNode, button) {
    if (inputNode.type === "text") {
      inputNode.type = "password";
      setIconButtonIcon(button, "显示密码", "eye");
      return;
    }
    const id = row.querySelector(".js-access-id").value.trim();
    if (row.dataset.unsaved === "1") {
      inputNode.type = "text";
      setIconButtonIcon(button, "隐藏密码", "eye-off");
      return;
    }
    if (!inputNode.value) {
      button.disabled = true;
      try {
        const payload = await getJson(`/api/admin/access-secret/${encodeURIComponent(id)}`);
        inputNode.value = payload.data?.password || "";
      } catch (err) {
        rowMessage(row, `读取失败: ${err.message}`, "warn");
        return;
      } finally {
        button.disabled = false;
      }
    }
    inputNode.type = "text";
    setIconButtonIcon(button, "隐藏密码", "eye-off");
  }

  function accessAddressChanged() {
    const savedAgentUrl = normalizeBaseUrl(state.settings?.agent_base_url || "");
    const currentAgentUrl = normalizeBaseUrl($("#agent-base-url")?.value || "");
    const savedAccessUrl = normalizeBaseUrl(state.settings?.access_base_url || "");
    const currentAccessUrl = normalizeBaseUrl($("#access-base-url")?.value || "");
    return savedAgentUrl !== currentAgentUrl || savedAccessUrl !== currentAccessUrl;
  }

  function commandOption(labelText, className, checked) {
    const label = document.createElement("label");
    const box = checkbox(checked);
    box.className = className;
    label.append(box, document.createTextNode(labelText));
    return label;
  }

  function openServerAccessCommandEditor(item = {}, row = null) {
    if (accessAddressChanged()) {
      if (row) {
        rowMessage(row, "接入地址已改，请先保存", "warn");
      } else {
        showToast("接入地址已改，请先保存", "warn");
      }
      return;
    }
    state.editor = { type: "access-command", row };
    openDialog("复制接入指令", item.id || "新服务器");
    $("#editor-apply").textContent = "复制指令";
    const uid = input("js-command-uid", item.id || "");
    uid.placeholder = "留空自动生成";
    const alias = input("js-command-alias", "");
    alias.placeholder = "留空使用 agent 上报名称";
    const interval = input("js-command-interval", "1", "number");
    interval.min = "1";
    interval.max = "86400";
    interval.step = "1";
    const location = input("js-command-location", "");
    location.placeholder = "留空自动识别";
    const type = input("js-command-type", "");
    type.placeholder = "留空自动识别虚拟化";
    const weight = input("js-command-weight", "10000", "number");
    weight.min = "1";
    const grid = el("div", "editor-grid");
    grid.append(
      field("服务器 ID", uid),
      field("显示名", alias),
      field("上报间隔秒", interval),
      field("位置", location),
      field("类型", type),
      field("权重", weight),
    );
    const options = el("div", "check-grid command-options");
    options.append(
      commandOption("禁用 Ping", "js-command-disable-ping", false),
      commandOption("禁用连接/进程统计", "js-command-disable-tupd", false),
      commandOption("禁用扩展采集", "js-command-disable-extra", false),
      commandOption("启用通知", "js-command-notify", true),
    );
    const optionsBlock = el("div", "option-block wide");
    optionsBlock.append(el("span", "option-label", "采集选项"), options);
    $("#editor-body").append(grid, optionsBlock);
  }

  function updateSummary() {
    const servers = serverItems();
    const online = servers.filter((server) => server.stat?.online4 || server.stat?.online6).length;
    text("#summary", `${servers.length} 个节点，${online} 在线`);
  }

  function renderTables() {
    renderServers();
    renderServerGroups();
    renderAlertRules();
    renderDeletedHosts();
    updateSummary();
  }

  function renderAll() {
    renderTables();
    renderNotifications();
    renderSettings();
  }

  function notificationName(id) {
    return { tg: "Telegram", bark: "Bark" }[id] || id;
  }

  function notificationMethods() {
    return allNotificationMethods().filter((method) => notificationEnabled(method.id));
  }

  function allNotificationMethods() {
    return [
      { id: "tg", name: "Telegram" },
      { id: "bark", name: "Bark" },
    ];
  }

  function notificationEnabled(id) {
    const config = {
      tg: state.settings?.tgbot || state.config?.tgbot || {},
      bark: state.settings?.bark || state.config?.bark || {},
    }[id];
    return Boolean(config?.enabled);
  }

  function alertRuleNotifications(rule) {
    if (rule?.notifications?.length) {
      return rule.notifications;
    }
    return [];
  }

  function activeAlertRuleNotifications(rule) {
    const enabled = notificationMethods().map((method) => method.id);
    if (!enabled.length) {
      return [];
    }
    const enabledSet = new Set(enabled);
    const selected = alertRuleNotifications(rule);
    const values = selected.length ? selected : enabled;
    return values.filter((id) => enabledSet.has(id));
  }

  function notificationTagsForRule(rule) {
    const active = activeAlertRuleNotifications(rule);
    if (active.length) {
      return tagList(active.map(notificationName));
    }
    const message = notificationMethods().length ? "未选择已启用通道" : "请配置通知方式";
    const wrap = el("div", "tag-list");
    wrap.append(el("span", "notice-inline", message));
    return wrap;
  }

  function notificationMethodPicker(rule) {
    const methods = notificationMethods();
    if (!methods.length) {
      const notice = el("div", "editor-notice wide", "尚未启用 Telegram 或 Bark，请先到「通知方式」页配置。");
      return notice;
    }
    return multiCheck(
      "通知方式（只显示已启用，留空表示全部）",
      "js-rule-notifications",
      methods,
      activeAlertRuleNotifications(rule),
      (item) => item.id,
      (item) => item.name,
    );
  }

  function openDialog(title, subtitle) {
    $("#editor-title").textContent = title;
    $("#editor-subtitle").textContent = subtitle || "";
    $("#editor-body").replaceChildren();
    $("#editor-delete").hidden = true;
    $("#editor-apply").disabled = false;
    $("#editor-apply").textContent = "确认";
    const dialog = $("#editor");
    if (dialog.showModal) {
      dialog.showModal();
    } else {
      dialog.setAttribute("open", "open");
    }
  }

  function closeDialog() {
    const dialog = $("#editor");
    if (dialog.close) {
      dialog.close();
    } else {
      dialog.removeAttribute("open");
    }
    state.editor = null;
  }

  function openServerEditor(id) {
    const item = serverItems().find((server) => server.id === id);
    if (!item) {
      return;
    }
    state.editor = { type: "server", id };
    openDialog("编辑服务器", id);
    const body = $("#editor-body");
    const grid = el("div", "editor-grid");
    const expireMode = isPermanentExpire(item.billing.end_date) ? "permanent" : item.billing.end_date ? "date" : "none";
    const feeMode = isFreeAmount(item.billing.amount) ? "free" : "paid";
    grid.append(
      field("名称", input("js-server-alias", item.alias || "")),
      field("排序", input("js-server-weight", item.weight || "", "number")),
      field("公开备注", input("js-server-public-note", item.public_note || ""), "wide"),
      field("私有备注", input("js-server-note", item.note || ""), "wide"),
      field("规格", input("js-server-spec", item.spec || "")),
      field("期限类型", select([["date", "到期日"], ["permanent", "永久"], ["none", "未设置"]], expireMode, "js-server-expire-mode")),
      hintField("到期日", datePicker("js-server-expire", isPermanentExpire(item.billing.end_date) ? "" : item.billing.end_date || ""), "固定格式 YYYY-MM-DD，例如 2026-11-25"),
      field("周期", select(cycleOptions, item.billing.cycle || "", "js-server-cycle")),
      field("费用类型", select([["paid", "付费"], ["free", "免费"]], feeMode, "js-server-fee-mode")),
      hintField("金额 / 货币", moneyControl(isFreeAmount(item.billing.amount) ? "" : item.billing.amount || ""), "保存为“金额 货币”，例如 200 EUR"),
    );
    const auto = checkbox(["1", "true", "yes", "on"].includes(String(item.billing.auto_renewal).toLowerCase()));
    auto.className = "js-server-auto";
    const notify = checkbox(item.expire_notify !== false);
    notify.className = "js-server-notify";
    grid.append(field("自动续期", auto), field("到期提醒", notify));
    body.append(grid, multiCheck("所属分组", "js-server-groups", serverGroups(), item.groups, (group) => group.id, (group) => group.name || group.id));
    syncServerEditorControls();
    $(".js-server-expire-mode").addEventListener("change", syncServerEditorControls);
    $(".js-server-fee-mode").addEventListener("change", syncServerEditorControls);
    $(".js-server-currency").addEventListener("change", syncServerEditorControls);
    $(".js-server-amount").addEventListener("input", syncServerEditorControls);
    $(".js-server-currency-custom").addEventListener("input", (event) => {
      event.target.value = event.target.value.toUpperCase();
    });
  }

  function syncServerEditorControls() {
    const expireMode = $(".js-server-expire-mode")?.value || "date";
    const feeMode = $(".js-server-fee-mode")?.value || "paid";
    const dateMode = expireMode === "date";
    $(".js-server-expire").disabled = !dateMode;
    $(".js-server-expire").required = dateMode;
    $(".js-server-cycle").disabled = !dateMode;
    $(".js-server-auto").disabled = !dateMode;
    if (!dateMode) {
      $(".js-server-auto").checked = false;
    }
    const freeMode = feeMode === "free";
    const customCurrency = $(".js-server-currency")?.value === "CUSTOM";
    $(".money-field")?.classList.toggle("has-custom-currency", !freeMode && customCurrency);
    $(".js-server-amount").disabled = freeMode;
    $(".js-server-currency").disabled = freeMode;
    $(".js-server-currency-custom").disabled = freeMode || !customCurrency;
    $(".js-server-currency-custom").hidden = freeMode || !customCurrency;
    $(".js-server-currency-custom").required = !freeMode && customCurrency && Boolean($(".js-server-amount").value.trim());
    if (feeMode === "free") {
      $(".js-server-amount").value = "";
      $(".js-server-currency-custom").value = "";
    }
  }

  function multiCheck(labelText, className, options, selected, idFn, labelFn) {
    const wrap = document.createElement("label");
    wrap.className = "wide";
    wrap.append(document.createTextNode(labelText));
    const grid = el("div", `check-grid ${className}`);
    const selectedSet = new Set(selected || []);
    options.forEach((item) => {
      const id = idFn(item);
      const label = document.createElement("label");
      const box = checkbox(selectedSet.has(id) || selectedSet.has(labelFn(item)));
      box.value = id;
      label.append(box, document.createTextNode(labelFn(item)));
      grid.append(label);
    });
    wrap.append(grid);
    return wrap;
  }

  function checkedValues(selector) {
    return $$(`${selector} input:checked`).map((inputNode) => inputNode.value);
  }

  function readServerExpireDate(expireMode) {
    if (expireMode === "permanent") {
      return "0000-00-00";
    }
    if (expireMode === "none") {
      return "";
    }

    const inputNode = $(".js-server-expire");
    const normalized = normalizeDateValue(inputNode.value);
    inputNode.setCustomValidity("");
    if (!normalized) {
      inputNode.setCustomValidity("请选择有效到期日，格式为 YYYY-MM-DD");
      inputNode.reportValidity();
      inputNode.focus();
      throw new Error("请选择有效到期日，格式为 YYYY-MM-DD");
    }
    return normalized;
  }

  function readServerAmount(feeMode) {
    if (feeMode === "free") {
      return "free";
    }

    const amountInput = $(".js-server-amount");
    const currencySelect = $(".js-server-currency");
    const customInput = $(".js-server-currency-custom");
    const amount = amountInput.value.trim();
    amountInput.setCustomValidity("");
    customInput.setCustomValidity("");

    if (!amount) {
      return "";
    }

    const currency = currencySelect.value === "CUSTOM" ? customInput.value.trim().toUpperCase() : currencySelect.value;
    if (!/^[A-Z]{3}$/.test(currency)) {
      customInput.setCustomValidity("请输入 3 位货币代码，例如 USD、EUR、HKD");
      customInput.reportValidity();
      customInput.focus();
      throw new Error("请输入 3 位货币代码，例如 USD、EUR、HKD");
    }
    return composeAmount(amount, currencySelect.value, currency);
  }

  function applyServerEditor() {
    const id = state.editor?.id;
    const expireMode = $(".js-server-expire-mode").value;
    const feeMode = $(".js-server-fee-mode").value;
    let expireDate = "";
    let amount = "";
    try {
      expireDate = readServerExpireDate(expireMode);
      amount = readServerAmount(feeMode);
    } catch (err) {
      text("#editor-subtitle", err.message);
      showToast(err.message, "warn");
      return false;
    }
    ensureSettings();
    state.settings.hosts[id] = {
      alias: $(".js-server-alias").value.trim(),
      note: $(".js-server-note").value.trim(),
      public_note: $(".js-server-public-note").value.trim(),
      spec: $(".js-server-spec").value.trim(),
      expire: expireDate,
      billing: {
        end_date: expireDate,
        auto_renewal: expireMode === "date" && $(".js-server-auto").checked ? "1" : "0",
        cycle: expireMode === "date" ? $(".js-server-cycle").value : "",
        amount,
      },
      expire_notify: $(".js-server-notify").checked,
      weight: Number($(".js-server-weight").value || 0),
    };
    const selectedGroups = new Set(checkedValues(".js-server-groups"));
    state.settings.server_groups = serverGroups().map((group) => {
      const servers = new Set(group.servers || []);
      if (selectedGroups.has(group.id)) {
        servers.add(id);
      } else {
        servers.delete(id);
      }
      return { ...group, servers: [...servers] };
    });
    return true;
  }

  function openServerGroupEditor(id) {
    ensureSettings();
    const group = id
      ? serverGroups().find((item) => item.id === id)
      : { id: stableId("group"), name: "", servers: [] };
    state.editor = { type: "server-group", id: group.id, isNew: !id };
    openDialog(id ? "编辑服务器分组" : "新建服务器分组", group.id);
    if (id) {
      $("#editor-delete").hidden = false;
    }
    const body = $("#editor-body");
    const grid = el("div", "editor-grid");
    grid.append(field("名称", input("js-group-name", group.name || "")));
    body.append(grid, multiCheck("服务器", "js-group-servers", serverItems(false), group.servers || [], (server) => server.id, (server) => server.alias || server.id));
  }

  function applyServerGroupEditor() {
    const editor = state.editor;
    const groups = serverGroups().filter((group) => group.id !== editor.id);
    groups.push({
      id: editor.id,
      name: $(".js-group-name").value.trim() || editor.id,
      servers: checkedValues(".js-group-servers"),
    });
    state.settings.server_groups = groups;
  }

  function openAlertRuleEditor(id) {
    ensureSettings();
    const rule = id
      ? state.settings.alert_rules.find((item) => item.id === id)
      : {
          id: stableId("rule"),
          name: "",
          enabled: true,
          metric: "offline",
          threshold: 90,
          duration: 120,
          repeat_interval: 3600,
          notifications: notificationMethods().map((item) => item.id),
          server_groups: [],
          servers: [],
        };
    state.editor = { type: "alert-rule", id: rule.id, isNew: !id };
    openDialog(id ? "编辑告警规则" : "新建告警规则", rule.id);
    if (id) {
      $("#editor-delete").hidden = false;
    }
    const grid = el("div", "editor-grid");
    const enabled = checkbox(rule.enabled !== false);
    enabled.className = "js-rule-enabled";
    grid.append(
      field("名称", input("js-rule-name", rule.name || "")),
      field("类型", select(metricOptions, rule.metric || "offline", "js-rule-metric")),
      field("阈值", input("js-rule-threshold", rule.threshold ?? "", "number")),
      field("持续秒", input("js-rule-duration", rule.duration || 120, "number")),
      field("重复间隔秒", input("js-rule-repeat", rule.repeat_interval || 3600, "number")),
      field("启用", enabled, "checkbox-field"),
    );
    $("#editor-body").append(
      grid,
      notificationMethodPicker(rule),
      multiCheck(
        "应用分组（留空表示不限定）",
        "js-rule-server-groups",
        serverGroups(),
        rule.server_groups || [],
        (group) => group.id,
        (group) => group.name || group.id,
      ),
      multiCheck("限定服务器（留空表示全部）", "js-rule-servers", serverItems(false), rule.servers || [], (server) => server.id, (server) => server.alias || server.id),
    );
    syncRuleEditorControls();
    $(".js-rule-metric").addEventListener("change", syncRuleEditorControls);
  }

  function syncRuleEditorControls() {
    const threshold = $(".js-rule-threshold");
    const thresholdLabel = threshold?.closest("label");
    const offline = $(".js-rule-metric").value === "offline";
    if (!threshold) {
      return;
    }
    if (offline) {
      if (threshold.value) {
        threshold.dataset.savedValue = threshold.value;
      }
      threshold.value = "";
      threshold.placeholder = "不适用";
      threshold.disabled = true;
      thresholdLabel?.classList.add("is-disabled");
    } else {
      threshold.disabled = false;
      threshold.placeholder = "";
      if (!threshold.value && threshold.dataset.savedValue) {
        threshold.value = threshold.dataset.savedValue;
      }
      thresholdLabel?.classList.remove("is-disabled");
    }
  }

  function applyAlertRuleEditor() {
    const editor = state.editor;
    const rules = state.settings.alert_rules.filter((rule) => rule.id !== editor.id);
    const metric = $(".js-rule-metric").value;
    rules.push({
      id: editor.id,
      name: $(".js-rule-name").value.trim() || editor.id,
      enabled: $(".js-rule-enabled").checked,
      metric,
      threshold: metric === "offline" ? null : Number($(".js-rule-threshold").value || 0),
      duration: Number($(".js-rule-duration").value || 120),
      repeat_interval: Number($(".js-rule-repeat").value || 3600),
      notification_group: "",
      notifications: checkedValues(".js-rule-notifications"),
      server_groups: checkedValues(".js-rule-server-groups"),
      servers: checkedValues(".js-rule-servers"),
    });
    state.settings.alert_rules = rules;
  }

  async function deleteCurrentEditorItem() {
    const editor = state.editor;
    if (!editor) {
      return;
    }
    const message = editor.type === "server-group" ? "服务器分组已删除并同步" : "告警规则已删除并同步";
    if (editor.type === "server-group") {
      state.settings.server_groups = serverGroups().filter((group) => group.id !== editor.id);
      state.settings.alert_rules = (state.settings.alert_rules || []).map((rule) => ({
        ...rule,
        server_groups: (rule.server_groups || []).filter((groupId) => groupId !== editor.id),
      }));
    } else if (editor.type === "alert-rule") {
      state.settings.alert_rules = state.settings.alert_rules.filter((rule) => rule.id !== editor.id);
    }
    closeDialog();
    renderTables();
    await saveSettingsPayload(settingsPayloadFromState(), {
      successMessage: message,
      render: "tables",
    });
  }

  function appendCommandParam(params, key, value) {
    const nextValue = String(value || "").trim();
    if (nextValue) {
      params.set(key, nextValue);
    }
  }

  async function applyAccessCommandEditor() {
    const editor = state.editor;
    const row = editor?.row;
    if (row && !row.isConnected) {
      text("#editor-subtitle", "服务器行已不存在");
      return;
    }
    if (accessAddressChanged()) {
      text("#editor-subtitle", "接入地址已改，请先保存");
      if (row) {
        rowMessage(row, "接入地址已改，请先保存", "warn");
      }
      return;
    }
    const params = new URLSearchParams();
    appendCommandParam(params, "uid", $(".js-command-uid").value);
    appendCommandParam(params, "alias", $(".js-command-alias").value);
    appendCommandParam(params, "interval", $(".js-command-interval").value || "1");
    appendCommandParam(params, "loc", $(".js-command-location").value);
    appendCommandParam(params, "type", $(".js-command-type").value);
    appendCommandParam(params, "weight", $(".js-command-weight").value);
    if ($(".js-command-disable-ping").checked) {
      params.set("ping", "0");
    }
    if ($(".js-command-disable-tupd").checked) {
      params.set("tupd", "0");
    }
    if ($(".js-command-disable-extra").checked) {
      params.set("extra", "0");
    }
    if (!$(".js-command-notify").checked) {
      params.set("notify", "0");
    }

    const applyButton = $("#editor-apply");
    applyButton.disabled = true;
    applyButton.textContent = "复制中...";
    try {
      const query = params.toString();
      const payload = await getJson(`/api/admin/access-command${query ? `?${query}` : ""}`);
      const script = payload.data?.script || "";
      if (!script) {
        throw new Error("接入脚本为空");
      }
      const gid = payload.data?.gid || "default";
      state.settings.access_keys ||= {};
      state.settings.access_keys[gid] ||= {
        source_gid: gid,
        password: "",
        notify: true,
        labels: "",
      };
      await copyText(script);
      const successMessage = "接入指令已复制，接入配置已同步";
      if (row) {
        rowMessage(row, "已复制并同步", "success");
      } else {
        text("#save-message", successMessage);
      }
      showToast(successMessage);
      closeDialog();
    } catch (err) {
      text("#editor-subtitle", `复制失败: ${err.message}`);
      if (row) {
        rowMessage(row, `复制失败: ${err.message}`, "warn");
      } else {
        text("#save-message", `复制失败: ${err.message}`);
      }
      showToast(`复制失败: ${err.message}`, "warn");
    } finally {
      if (state.editor?.type === "access-command") {
        applyButton.disabled = false;
        applyButton.textContent = "复制指令";
      }
    }
  }

  async function applyEditor(event) {
    event.preventDefault();
    if (!state.editor) {
      closeDialog();
      return;
    }
    if (state.editor.type === "access-command") {
      await applyAccessCommandEditor();
      return;
    }
    if (state.editor.type === "server") {
      if (!applyServerEditor()) {
        return;
      }
      closeDialog();
      renderTables();
      await saveSettingsPayload(settingsPayloadFromState(), {
        successMessage: "服务器配置已同步到后端",
        render: "tables",
      });
      return;
    } else if (state.editor.type === "server-group") {
      applyServerGroupEditor();
      closeDialog();
      renderTables();
      await saveSettingsPayload(settingsPayloadFromState(), {
        successMessage: "服务器分组已同步到后端",
        render: "tables",
      });
      return;
    } else if (state.editor.type === "alert-rule") {
      applyAlertRuleEditor();
      closeDialog();
      renderTables();
      await saveSettingsPayload(settingsPayloadFromState(), {
        successMessage: "告警规则已同步到后端",
        render: "tables",
      });
      return;
    }
    closeDialog();
    renderAll();
    markDirty("已更新，记得保存");
  }

  function addAccessKey() {
    const row = accessKeyRow({
      id: nextAccessKeyId(),
      source_gid: "",
      runtime: true,
      unsaved: true,
      notify: true,
      labels: "",
    });
    row.querySelector(".js-access-password").value = randomSecret();
    $("#access-key-rows").prepend(row);
  }

  function nextAccessKeyId() {
    const used = new Set();
    $$("#access-key-rows .js-access-id").forEach((node) => used.add(node.value.trim()));
    let index = 1;
    let id = `access-${index}`;
    while (used.has(id)) {
      index += 1;
      id = `access-${index}`;
    }
    return id;
  }

  function collectAccessKeys() {
    const accessKeys = {};
    const deleted = new Set(state.deletedAccessKeys);
    const seen = new Set();
    $$("#access-key-rows tr").forEach((row) => {
      const oldId = row.dataset.id || "";
      const id = row.querySelector(".js-access-id").value.trim();
      if (!id) {
        throw new Error("接入密钥名称不能为空");
      }
      if (seen.has(id)) {
        throw new Error(`接入密钥 ${id} 重复`);
      }
      seen.add(id);
      deleted.delete(id);
      if (oldId && oldId !== id && row.dataset.unsaved !== "1") {
        deleted.add(oldId);
      }
      const password = row.querySelector(".js-access-password").value;
      if (row.dataset.unsaved === "1" && !password.trim()) {
        throw new Error(`接入密钥 ${id} 需要连接密码`);
      }
      accessKeys[id] = {
        source_gid: row.dataset.sourceId || oldId || id,
        password,
        location: row.querySelector(".js-access-location").value.trim(),
        type: row.querySelector(".js-access-type").value.trim(),
        notify: true,
        labels: row.querySelector(".js-access-labels").value.trim(),
      };
    });
    return { access_keys: accessKeys, deleted_access_keys: [...deleted] };
  }

  function collectTgbotSettings() {
    return {
      enabled: $("#tg-enabled").checked,
      bot_token: $("#tg-token").value.trim(),
      chat_id: $("#tg-chat").value.trim(),
      title: $("#tg-title").value,
      expire_tpl: $("#tg-expire").value,
      health_tpl: $("#tg-health").value,
    };
  }

  function collectBarkSettings() {
    return {
      enabled: $("#bark-enabled").checked,
      server: $("#bark-server").value.trim(),
      device_key: $("#bark-key").value.trim(),
      title: $("#bark-title").value,
      group: $("#bark-group").value,
      expire_tpl: $("#bark-expire").value,
      health_tpl: $("#bark-health").value,
    };
  }

  function barkServerContainsDeviceKey(server) {
    try {
      const url = new URL(server);
      const firstPart = url.pathname
        .split("/")
        .map((part) => part.trim())
        .find(Boolean);
      return Boolean(firstPart && firstPart.toLowerCase() !== "push");
    } catch {
      return false;
    }
  }

  function validateBarkSettingsForTest(bark) {
    if (!bark.enabled) {
      return "请先勾选启用 Bark";
    }
    if (!bark.server) {
      return "Bark Server 不能为空";
    }
    if (!bark.device_key && !barkServerContainsDeviceKey(bark.server)) {
      return "Bark Device Key 不能为空";
    }
    return "";
  }

  function collectAccessSettings() {
    return {
      access_base_url: normalizeBaseUrl($("#access-base-url").value),
      agent_base_url: normalizeBaseUrl($("#agent-base-url").value),
    };
  }

  function collectExpireNotifySettings() {
    return {
      enabled: $("#expire-enabled").checked,
      days: $("#expire-days")
        .value.split(",")
        .map((item) => Number(item.trim()))
        .filter((item) => Number.isFinite(item) && item >= 0),
      interval: Number($("#expire-interval").value || 86400),
    };
  }

  async function loadDashboard() {
    const [config, stats, settings] = await Promise.all([
      getJson("/api/admin/config.json"),
      getJson("/api/admin/stats.json"),
      getJson("/api/admin/settings"),
    ]);
    state.config = config;
    state.stats = stats;
    state.settings = settings.data || {};
    ensureSettings();
    renderAll();
    markPristine("");
  }

  async function saveDashboard() {
    if (!state.dirty) {
      text("#save-message", "当前没有需要保存的更改");
      return;
    }
    const settingsPayload = settingsPayloadFromState();
    setSaveBusy(true, "保存中...");
    try {
      const responsePayload = await postSettings(settingsPayload);
      state.settings = responsePayload.data || {};
      ensureSettings();
      renderAll();
      markPristine("已同步到后端");
      showToast("配置已保存并同步到后端");
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      markDirty(`保存失败: ${err.message}`);
      showToast(`保存失败: ${err.message}`, "warn");
    } finally {
      state.saving = false;
      updateSaveButton();
    }
  }

  async function saveTgbotSettings() {
    const tgbot = collectTgbotSettings();
    const ok = await saveSettingsPayload(settingsPayloadFromState({ tgbot }), {
      successMessage: "Telegram 已同步到后端",
      messageTarget: "#tg-save-message",
      render: "none",
      busyButton: $("#tg-save"),
    });
    if (ok) {
      renderTgbotNotification();
      resetLocalBaseline("tg", "Telegram 已同步到后端");
      renderAlertRules();
    }
  }

  async function saveBarkSettings() {
    const bark = collectBarkSettings();
    const ok = await saveSettingsPayload(settingsPayloadFromState({ bark }), {
      successMessage: "Bark 已同步到后端",
      messageTarget: "#bark-save-message",
      render: "none",
      busyButton: $("#bark-save"),
    });
    if (ok) {
      renderBarkNotification();
      resetLocalBaseline("bark", "Bark 已同步到后端");
      renderAlertRules();
    }
  }

  async function testNotification(
    kind,
    scope,
    payload,
    buttonSelector,
    messageSelector,
    successMessage = "测试通知已发送",
  ) {
    const button = $(buttonSelector);
    setButtonBusy(button, true, "测试中...");
    text(messageSelector, "测试中...");
    try {
      const responsePayload = await postNotifyTest(kind, payload);
      const message = responsePayload.message || successMessage;
      text(messageSelector, message);
      showToast(message);
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      const message = `测试失败: ${err.message}`;
      text(messageSelector, message);
      showToast(message, "warn");
    } finally {
      setButtonBusy(button, false, "测试中...");
      state.localDirty[scope] = localSnapshot(scope) !== state.localBaseline[scope];
      updateLocalSaveButton(scope);
    }
  }

  async function testTgbotSettings() {
    await testNotification(
      "tgbot",
      "tg",
      { tgbot: collectTgbotSettings() },
      "#tg-test",
      "#tg-save-message",
    );
  }

  async function testBarkSettings() {
    const bark = collectBarkSettings();
    const validationMessage = validateBarkSettingsForTest(bark);
    if (validationMessage) {
      text("#bark-save-message", validationMessage);
      showToast(validationMessage, "warn");
      return;
    }
    await testNotification(
      "bark",
      "bark",
      { bark },
      "#bark-test",
      "#bark-save-message",
      "Bark 测试请求已发送，请检查手机通知",
    );
  }

  async function saveAccessSettings() {
    const access = collectAccessSettings();
    const ok = await saveSettingsPayload(settingsPayloadFromState(access), {
      successMessage: "接入地址已同步到后端",
      messageTarget: "#access-save-message",
      render: "none",
      busyButton: $("#access-save"),
    });
    if (ok) {
      $("#access-base-url").value = access.access_base_url;
      $("#agent-base-url").value = access.agent_base_url;
      resetLocalBaseline("access", "接入地址已同步到后端");
    }
  }

  async function saveExpireNotifySettings() {
    const expireNotify = collectExpireNotifySettings();
    const ok = await saveSettingsPayload(settingsPayloadFromState({ expire_notify: expireNotify }), {
      successMessage: "到期提醒已同步到后端",
      messageTarget: "#expire-save-message",
      render: "none",
      busyButton: $("#expire-save"),
    });
    if (ok) {
      resetLocalBaseline("expire", "到期提醒已同步到后端");
    }
  }

  function validAdminUsername(username) {
    return /^[A-Za-z0-9_.@-]{1,64}$/.test(username);
  }

  async function changeAdminPassword(event) {
    event.preventDefault();
    const savedUsername = state.config?.admin?.username || "admin";
    const username = $("#admin-username").value.trim();
    const currentPassword = $("#admin-current-password").value;
    const newPassword = $("#admin-new-password").value;
    const confirmPassword = $("#admin-new-password-confirm").value;
    const wantsPasswordChange = Boolean(newPassword || confirmPassword);
    if (!username || !currentPassword) {
      text("#password-message", "请填写用户名和当前密码");
      return;
    }
    if (!validAdminUsername(username)) {
      text("#password-message", "用户名只能包含字母、数字、_、-、.、@，最长 64 字节");
      return;
    }
    if (!wantsPasswordChange && username === savedUsername) {
      text("#password-message", "没有需要保存的账号更改");
      return;
    }
    if (wantsPasswordChange) {
      if (!newPassword || !confirmPassword) {
        text("#password-message", "请完整填写新密码和确认密码");
        return;
      }
      if (newPassword.length < 12) {
        text("#password-message", "新密码至少需要 12 个字符");
        return;
      }
      if (newPassword.length > 256) {
        text("#password-message", "新密码不能超过 256 个字符");
        return;
      }
      if (newPassword !== confirmPassword) {
        text("#password-message", "两次输入的新密码不一致");
        return;
      }
    }
    if (hasUnsavedChanges() && !window.confirm("保存账号设置后需要重新登录，当前未保存的配置会丢失。确定继续？")) {
      return;
    }
    $("#password-submit").disabled = true;
    text("#password-message", "保存中...");
    try {
      const response = await fetch("/api/admin/password", {
        method: "POST",
        headers: authHeaders(true),
        body: JSON.stringify({
          username,
          current_password: currentPassword,
          new_password: wantsPasswordChange ? newPassword : "",
        }),
      });
      await readJson(response);
      $("#password-form").reset();
      clearSession();
      markPristine("");
      setView("login");
      $("#username").value = username;
      $("#password").value = "";
      text("#login-message", "账号设置已修改，请重新登录");
    } catch (err) {
      if (err.authExpired) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      text("#password-message", `修改失败: ${err.message}`);
    } finally {
      $("#password-submit").disabled = false;
    }
  }

  async function enterDashboard() {
    text("#login-message", "正在加载配置...");
    await loadDashboard();
    text("#login-message", "");
    setView("dashboard");
  }

  async function login(event) {
    event.preventDefault();
    $("#login-submit").disabled = true;
    text("#login-message", "登录中...");
    try {
      const response = await fetch("/api/admin/authorize", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          username: $("#username").value.trim(),
          password: $("#password").value,
        }),
      });
      const payload = await readJson(response);
      state.token = payload.access_token;
      localStorage.setItem(tokenKey, state.token);
      sessionStorage.removeItem(tokenKey);
      await enterDashboard();
      $("#password").value = "";
    } catch (err) {
      clearSession();
      setView("login");
      $("#password").value = "";
      text("#login-message", err.message || "登录失败");
    } finally {
      $("#login-submit").disabled = false;
    }
  }

  function logout() {
    if (hasUnsavedChanges() && !window.confirm("有未保存的修改，退出后会丢失。确定退出登录？")) {
      return;
    }
    clearSession();
    markPristine("");
    setView("login");
    text("#login-message", "已退出登录");
  }

  function closeUserMenu() {
    const panel = $("#user-menu-panel");
    const button = $("#user-menu-toggle");
    if (panel) {
      panel.hidden = true;
    }
    if (button) {
      button.setAttribute("aria-expanded", "false");
    }
  }

  function toggleUserMenu(event) {
    event.stopPropagation();
    const panel = $("#user-menu-panel");
    const button = $("#user-menu-toggle");
    if (!panel || !button) {
      return;
    }
    const open = panel.hidden;
    panel.hidden = !open;
    button.setAttribute("aria-expanded", open ? "true" : "false");
  }

  function bindTabs() {
    $$(".nav-tab").forEach((tab) => {
      tab.addEventListener("click", () => {
        $$(".nav-tab").forEach((item) => item.classList.remove("active"));
        $$(".tab-page").forEach((item) => item.classList.remove("active"));
        tab.classList.add("active");
        $(`#tab-${tab.dataset.tab}`).classList.add("active");
        const [title, subtitle] = pageMeta[tab.dataset.tab] || ["后台", ""];
        text("#page-title", title);
        text("#page-subtitle", subtitle);
      });
    });
  }

  function bindDirtyTracking() {
    $("#dashboard").addEventListener("input", handleDashboardFieldChange);
    $("#dashboard").addEventListener("change", handleDashboardFieldChange);
  }

  function handleDashboardFieldChange(event) {
    const target = event.target;
    if (!(target instanceof Element)) {
      return;
    }
    if (!target.closest(".content")) {
      return;
    }
    if (target.closest("#password-form") || target.closest("#editor")) {
      return;
    }
    if (!["INPUT", "SELECT", "TEXTAREA"].includes(target.tagName)) {
      return;
    }
    const localBlock = target.closest(".local-save-block");
    if (localBlock?.dataset.localScope) {
      refreshLocalDirty(localBlock.dataset.localScope);
    }
  }

  $("#login-form").addEventListener("submit", login);
  $("#tg-save").addEventListener("click", saveTgbotSettings);
  $("#tg-test").addEventListener("click", testTgbotSettings);
  $("#bark-save").addEventListener("click", saveBarkSettings);
  $("#bark-test").addEventListener("click", testBarkSettings);
  $("#access-save").addEventListener("click", saveAccessSettings);
  $("#expire-save").addEventListener("click", saveExpireNotifySettings);
  $("#password-form").addEventListener("submit", changeAdminPassword);
  $("#theme-toggle").addEventListener("click", cycleTheme);
  $("#user-menu-toggle").addEventListener("click", toggleUserMenu);
  $("#logout").addEventListener("click", logout);
  $("#add-server-access").addEventListener("click", () => openServerAccessCommandEditor());
  $("#add-server-group").addEventListener("click", () => openServerGroupEditor(""));
  $("#add-alert-rule").addEventListener("click", () => openAlertRuleEditor(""));
  $("#editor-form").addEventListener("submit", applyEditor);
  $("#editor-close").addEventListener("click", closeDialog);
  $("#editor-cancel").addEventListener("click", closeDialog);
  $("#editor-delete").addEventListener("click", deleteCurrentEditorItem);
  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element) || !event.target.closest(".user-menu")) {
      closeUserMenu();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      closeUserMenu();
    }
  });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (state.theme === "system") {
      applyTheme("system");
    }
  });
  bindTabs();
  bindDirtyTracking();
  setIconButtonIcon($("#user-menu-toggle"), "用户菜单", "user");
  applyTheme(state.theme);
  updateSaveButton();

  setView("login");
  const hadStoredToken = Boolean(state.token);
  if (state.token) {
    enterDashboard().catch((err) => {
      clearSession();
      setView("login");
      if (hadStoredToken) {
        text("#login-message", err.message === "Invalid token" ? "登录已过期，请重新登录" : err.message);
      }
    });
  }
})();

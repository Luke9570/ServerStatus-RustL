(function () {
  const tokenKey = "ssr_admin_token";
  const state = {
    token: sessionStorage.getItem(tokenKey) || "",
    config: null,
    stats: null,
    settings: null,
  };

  const $ = (selector) => document.querySelector(selector);

  function authHeaders(withJson) {
    const headers = {
      Authorization: `Bearer ${state.token}`,
    };
    if (withJson) {
      headers["Content-Type"] = "application/json";
    }
    return headers;
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

  function clearSession() {
    state.token = "";
    sessionStorage.removeItem(tokenKey);
  }

  function field(tag, className, valueText, type) {
    const input = document.createElement(tag);
    input.className = className || "";
    if (tag === "textarea") {
      input.value = valueText || "";
      return input;
    }
    input.type = type || "text";
    if (valueText !== undefined && valueText !== null) {
      input.value = valueText;
    }
    return input;
  }

  function checkbox(checked) {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = Boolean(checked);
    return input;
  }

  function statusPill(server) {
    const span = document.createElement("span");
    const online = server && (server.online4 || server.online6);
    span.className = `status-pill ${online ? "online" : "offline"}`;
    span.textContent = online ? "在线" : "离线";
    return span;
  }

  function billingFrom(configItem, overrideItem, statItem) {
    const billing = configItem?.billing || {};
    const overrideBilling = overrideItem?.billing || {};
    return {
      end_date:
        overrideBilling.end_date ??
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

  function nodeRow(item) {
    const row = document.createElement("tr");
    row.dataset.kind = item.kind;
    row.dataset.id = item.id;

    const name = document.createElement("td");
    const nameWrap = document.createElement("div");
    nameWrap.className = "node-name";
    const strong = document.createElement("strong");
    strong.textContent = item.alias || item.id;
    const small = document.createElement("span");
    small.textContent = item.kind === "group" ? item.id : [item.id, item.gid].filter(Boolean).join(" / ");
    nameWrap.append(strong, small);
    name.append(nameWrap);

    const status = document.createElement("td");
    if (item.kind === "host") {
      status.append(statusPill(item.stat));
    } else {
      status.textContent = "-";
    }

    const weight = document.createElement("td");
    const weightInput = field("input", "mini-input js-weight", item.weight || "", "number");
    weightInput.min = "0";
    weightInput.step = "1";
    weight.append(weightInput);

    const expire = document.createElement("td");
    expire.append(field("input", "date-input js-expire", item.billing.end_date || ""));

    const auto = document.createElement("td");
    const autoInput = checkbox(["1", "true", "yes", "on"].includes(String(item.billing.auto_renewal).toLowerCase()));
    autoInput.className = "js-auto";
    auto.append(autoInput);

    const cycle = document.createElement("td");
    cycle.append(field("input", "cycle-input js-cycle", item.billing.cycle || ""));

    const amount = document.createElement("td");
    amount.append(field("input", "amount-input js-amount", item.billing.amount || ""));

    const notify = document.createElement("td");
    const notifyInput = checkbox(item.expire_notify !== false);
    notifyInput.className = "js-notify";
    notify.append(notifyInput);

    if (item.kind === "group") {
      row.append(name, weight, expire, auto, cycle, amount, notify);
    } else {
      row.append(name, status, weight, expire, auto, cycle, amount, notify);
    }
    return row;
  }

  function hostItems() {
    const configHosts = new Map((state.config?.hosts || []).map((host) => [host.name, host]));
    const stats = state.stats?.servers || [];
    for (const server of stats) {
      if (!configHosts.has(server.name)) {
        configHosts.set(server.name, {
          name: server.name,
          alias: server.alias,
          gid: server.gid,
          weight: server.weight,
          expire_notify: server.expire_notify,
          billing: {},
        });
      }
    }

    return [...configHosts.values()].map((host) => {
      const stat = stats.find((server) => server.name === host.name);
      const override = state.settings?.hosts?.[host.name] || {};
      return {
        kind: "host",
        id: host.name,
        alias: host.alias || stat?.alias || host.name,
        gid: host.gid || stat?.gid || "",
        stat,
        weight: override.weight ?? host.weight ?? stat?.weight ?? "",
        expire_notify: override.expire_notify ?? host.expire_notify,
        billing: billingFrom(host, override, stat),
      };
    });
  }

  function groupItems() {
    return (state.config?.hosts_group || []).map((group) => {
      const override = state.settings?.groups?.[group.gid] || {};
      return {
        kind: "group",
        id: group.gid,
        alias: group.gid,
        weight: override.weight ?? group.weight ?? "",
        expire_notify: override.expire_notify ?? group.expire_notify,
        billing: billingFrom(group, override, null),
      };
    });
  }

  function renderRows() {
    const nodeRows = $("#node-rows");
    const groupRows = $("#group-rows");
    nodeRows.replaceChildren(...hostItems().map(nodeRow));
    groupRows.replaceChildren(...groupItems().map(nodeRow));

    const servers = state.stats?.servers || [];
    const online = servers.filter((server) => server.online4 || server.online6).length;
    text("#summary", `${servers.length} 个节点，${online} 在线`);
  }

  function renderNotify() {
    const expireNotify = state.settings?.expire_notify || state.config?.expire_notify || {};
    $("#expire-enabled").checked = Boolean(expireNotify.enabled);
    $("#expire-days").value = (expireNotify.days || [30, 14, 7, 3, 1, 0]).join(",");
    $("#expire-interval").value = expireNotify.interval || 86400;

    const tg = state.settings?.tgbot || state.config?.tgbot || {};
    $("#tg-enabled").checked = Boolean(tg.enabled);
    $("#tg-token").value = tg.bot_token || "";
    $("#tg-chat").value = tg.chat_id || "";
    $("#tg-title").value = tg.title || "";
    $("#tg-expire").value = tg.expire_tpl || "";

    const bark = state.settings?.bark || state.config?.bark || {};
    $("#bark-enabled").checked = Boolean(bark.enabled);
    $("#bark-server").value = bark.server || "https://api.day.app";
    $("#bark-key").value = bark.device_key || "";
    $("#bark-title").value = bark.title || "ServerStatus";
    $("#bark-group").value = bark.group || "ServerStatus";
    $("#bark-expire").value = bark.expire_tpl || "";
  }

  function collectRows(selector) {
    const rows = {};
    document.querySelectorAll(selector).forEach((row) => {
      const id = row.dataset.id;
      rows[id] = {
        billing: {
          end_date: row.querySelector(".js-expire").value.trim(),
          auto_renewal: row.querySelector(".js-auto").checked ? "1" : "0",
          cycle: row.querySelector(".js-cycle").value.trim(),
          amount: row.querySelector(".js-amount").value.trim(),
        },
        expire_notify: row.querySelector(".js-notify").checked,
        weight: Number(row.querySelector(".js-weight").value || 0),
      };
    });
    return rows;
  }

  function collectSettings() {
    return {
      hosts: collectRows("#node-rows tr"),
      groups: collectRows("#group-rows tr"),
      expire_notify: {
        enabled: $("#expire-enabled").checked,
        days: $("#expire-days")
          .value.split(",")
          .map((item) => Number(item.trim()))
          .filter((item) => Number.isFinite(item) && item >= 0),
        interval: Number($("#expire-interval").value || 86400),
      },
      tgbot: {
        enabled: $("#tg-enabled").checked,
        bot_token: $("#tg-token").value.trim(),
        chat_id: $("#tg-chat").value.trim(),
        title: $("#tg-title").value,
        expire_tpl: $("#tg-expire").value,
      },
      bark: {
        enabled: $("#bark-enabled").checked,
        server: $("#bark-server").value.trim(),
        device_key: $("#bark-key").value.trim(),
        title: $("#bark-title").value,
        group: $("#bark-group").value,
        expire_tpl: $("#bark-expire").value,
      },
    };
  }

  async function readJson(response) {
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = payload.error || payload.message || `${response.status} ${response.statusText}`;
      if (response.status === 401 || response.status === 403) {
        clearSession();
      }
      throw new Error(message);
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

  async function loadDashboard() {
    if (!state.token) {
      throw new Error("请先登录");
    }
    const [config, stats, settings] = await Promise.all([
      getJson("/api/admin/config.json"),
      getJson("/api/admin/stats.json"),
      getJson("/api/admin/settings"),
    ]);
    state.config = config;
    state.stats = stats;
    state.settings = settings.data || {};
    renderRows();
    renderNotify();
  }

  async function saveDashboard() {
    $("#save").disabled = true;
    text("#save-message", "保存中...");
    try {
      const response = await fetch("/api/admin/settings", {
        method: "POST",
        headers: authHeaders(true),
        body: JSON.stringify(collectSettings()),
      });
      const payload = await readJson(response);
      state.settings = payload.data || {};
      text("#save-message", "已保存");
    } catch (err) {
      if (!state.token) {
        setView("login");
        text("#login-message", "登录已过期，请重新登录");
      }
      text("#save-message", `保存失败: ${err.message}`);
    } finally {
      $("#save").disabled = false;
    }
  }

  async function enterDashboard() {
    text("#login-message", "正在加载配置...");
    await loadDashboard();
    text("#login-message", "");
    text("#save-message", "");
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
      sessionStorage.setItem(tokenKey, state.token);
      await enterDashboard();
    } catch (err) {
      clearSession();
      setView("login");
      text("#login-message", err.message || "登录失败");
    } finally {
      $("#login-submit").disabled = false;
    }
  }

  function logout() {
    clearSession();
    setView("login");
    text("#login-message", "已退出登录");
  }

  function bindTabs() {
    document.querySelectorAll(".tab").forEach((tab) => {
      tab.addEventListener("click", () => {
        document.querySelectorAll(".tab").forEach((item) => item.classList.remove("active"));
        document.querySelectorAll(".tab-page").forEach((item) => item.classList.remove("active"));
        tab.classList.add("active");
        $(`#tab-${tab.dataset.tab}`).classList.add("active");
      });
    });
  }

  $("#login-form").addEventListener("submit", login);
  $("#refresh").addEventListener("click", () => {
    loadDashboard().catch((err) => text("#save-message", `刷新失败: ${err.message}`));
  });
  $("#save").addEventListener("click", saveDashboard);
  $("#logout").addEventListener("click", logout);
  bindTabs();

  setView("login");
  if (state.token) {
    enterDashboard().catch((err) => {
      clearSession();
      setView("login");
      text("#login-message", err.message === "Invalid token" ? "登录已过期，请重新登录" : err.message);
    });
  }
})();

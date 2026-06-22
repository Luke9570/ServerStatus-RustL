(function () {
  const PANEL_ID = "ssr-expiry-panel";
  const SOURCE_URL = "/json/stats.json";
  const ATTENTION_DAYS = 30;

  function hasExpiry(server) {
    return Boolean(server && server.expire && server.expire.configured);
  }

  function isOnline(server) {
    return Boolean(server && (server.online4 || server.online6));
  }

  function displayName(server) {
    return server.alias || server.name || "-";
  }

  function statusClass(status) {
    return `ssr-expiry-${status || "unknown"}`;
  }

  function autoRenewalHealthy(expire) {
    return (
      expire &&
      expire.auto_renewal &&
      !["missing_cycle", "invalid_cycle", "invalid_date", "too_many_cycles"].includes(expire.renewal_status)
    );
  }

  function treatAsMissing(server) {
    if (!hasExpiry(server)) {
      return true;
    }
    return hasExpiry(server) && server.expire.status === "expired" && isOnline(server);
  }

  function daysText(expire) {
    if (!expire || expire.status === "permanent") {
      return "永久";
    }
    if (expire.status === "unknown") {
      return "日期无效";
    }
    if (expire.days_left < 0) {
      return `已过期 ${Math.abs(expire.days_left)} 天`;
    }
    if (expire.days_left === 0) {
      return "今天";
    }
    return `剩余 ${expire.days_left} 天`;
  }

  function renewalText(expire) {
    if (!expire || !expire.auto_renewal) {
      return "";
    }

    const cycle = expire.cycle ? ` / ${expire.cycle}` : "";
    if (expire.renewal_status === "missing_cycle") {
      return "自动续期 / 缺少周期";
    }
    if (expire.renewal_status === "invalid_cycle") {
      return "自动续期 / 周期无效";
    }
    if (expire.auto_renewed) {
      return `已自动推算 x${expire.renewal_count}${cycle}`;
    }
    return `自动续期${cycle}`;
  }

  function moneyText(expire) {
    return [expire.amount, expire.cycle].filter(Boolean).join(" / ");
  }

  function needsAttention(server) {
    if (treatAsMissing(server)) {
      return true;
    }

    const expire = server.expire;
    if (expire.status === "expired" || expire.status === "unknown") {
      return true;
    }
    if (autoRenewalHealthy(expire)) {
      return false;
    }

    return (
      expire.days_left <= ATTENTION_DAYS ||
      expire.renewal_status === "missing_cycle" ||
      expire.renewal_status === "invalid_cycle"
    );
  }

  function riskRank(server) {
    if (treatAsMissing(server)) {
      return 0;
    }
    if (hasExpiry(server) && server.expire.status === "expired") {
      return 1;
    }
    if (hasExpiry(server) && server.expire.status === "unknown") {
      return 2;
    }
    if (
      hasExpiry(server) &&
      ["missing_cycle", "invalid_cycle", "invalid_date", "too_many_cycles"].includes(server.expire.renewal_status)
    ) {
      return 3;
    }
    if (hasExpiry(server) && server.expire.days_left >= 0 && server.expire.days_left <= 7) {
      return 4;
    }
    if (hasExpiry(server) && server.expire.days_left <= ATTENTION_DAYS) {
      return 5;
    }
    return 6;
  }

  function sortByDue(a, b) {
    const ad = hasExpiry(a) ? a.expire.days_left : Number.MAX_SAFE_INTEGER;
    const bd = hasExpiry(b) ? b.expire.days_left : Number.MAX_SAFE_INTEGER;
    if (ad !== bd) {
      return ad - bd;
    }
    return displayName(a).localeCompare(displayName(b));
  }

  function sortByRisk(a, b) {
    const ar = riskRank(a);
    const br = riskRank(b);
    if (ar !== br) {
      return ar - br;
    }
    return sortByDue(a, b);
  }

  function metric(label, value, tone) {
    const item = document.createElement("div");
    item.className = `ssr-expiry-metric ${tone ? `ssr-expiry-metric-${tone}` : ""}`;

    const number = document.createElement("span");
    number.className = "ssr-expiry-metric-value";
    number.textContent = value;

    const text = document.createElement("span");
    text.className = "ssr-expiry-metric-label";
    text.textContent = label;

    item.append(number, text);
    return item;
  }

  function makeMissingChip(server) {
    const chip = document.createElement("div");
    chip.className = "ssr-expiry-chip ssr-expiry-missing";
    chip.title = `${displayName(server)}: 未配置 VPS 到期时间`;

    const name = document.createElement("span");
    name.className = "ssr-expiry-name";
    name.textContent = displayName(server);

    const date = document.createElement("span");
    date.className = "ssr-expiry-date";
    date.textContent = "未配置到期";

    const label = document.createElement("span");
    label.className = "ssr-expiry-label";
    label.textContent = "建议补充";

    chip.append(name, date, label);
    return chip;
  }

  function makeChip(server, compact) {
    if (treatAsMissing(server)) {
      return makeMissingChip(server);
    }

    const expire = server.expire;
    const chip = document.createElement("div");
    chip.className = `ssr-expiry-chip ${statusClass(expire.status)}`;
    chip.title = [
      displayName(server),
      expire.date || expire.raw,
      expire.label,
      renewalText(expire),
      expire.source ? `source: ${expire.source}` : "",
    ]
      .filter(Boolean)
      .join(" | ");

    const name = document.createElement("span");
    name.className = "ssr-expiry-name";
    name.textContent = displayName(server);

    const date = document.createElement("span");
    date.className = "ssr-expiry-date";
    date.textContent = expire.date || expire.raw || "-";

    const label = document.createElement("span");
    label.className = "ssr-expiry-label";
    label.textContent = daysText(expire);

    chip.append(name, date, label);

    const renew = renewalText(expire);
    if (renew && !compact) {
      const renewal = document.createElement("span");
      renewal.className = "ssr-expiry-renewal";
      renewal.textContent = renew;
      chip.append(renewal);
    }

    const billing = moneyText(expire);
    if (billing && !compact) {
      const amount = document.createElement("span");
      amount.className = "ssr-expiry-billing";
      amount.textContent = billing;
      chip.append(amount);
    }

    return chip;
  }

  function ensurePanel() {
    let panel = document.getElementById(PANEL_ID);
    if (panel) {
      return panel;
    }

    const body = document.getElementById("body") || document.body;
    panel = document.createElement("section");
    panel.id = PANEL_ID;
    panel.className = "ssr-expiry-panel";
    body.prepend(panel);
    return panel;
  }

  function renderPanel(servers) {
    const panel = ensurePanel();

    if (servers.length === 0) {
      panel.hidden = true;
      panel.replaceChildren();
      return;
    }

    const configured = servers.filter((server) => hasExpiry(server) && !treatAsMissing(server)).sort(sortByDue);
    const missing = servers.filter(treatAsMissing);
    const expired = configured.filter((server) => server.expire.status === "expired");
    const warning = configured.filter(
      (server) =>
        !autoRenewalHealthy(server.expire) &&
        server.expire.days_left >= 0 &&
        server.expire.days_left <= 7,
    );
    const soon = configured.filter(
      (server) =>
        !autoRenewalHealthy(server.expire) &&
        server.expire.days_left > 7 &&
        server.expire.days_left <= ATTENTION_DAYS,
    );
    const auto = configured.filter((server) => server.expire.auto_renewal);
    const attention = servers.filter(needsAttention).sort(sortByRisk).slice(0, 8);
    const next = configured.find((server) => server.expire.days_left >= 0);

    const header = document.createElement("div");
    header.className = "ssr-expiry-header";

    const titleWrap = document.createElement("div");
    titleWrap.className = "ssr-expiry-title-wrap";

    const title = document.createElement("div");
    title.className = "ssr-expiry-title";
    title.textContent = "VPS 到期";

    const subtitle = document.createElement("div");
    subtitle.className = "ssr-expiry-subtitle";
    subtitle.textContent = next
      ? `下一台: ${displayName(next)} ${next.expire.date || next.expire.raw} (${daysText(next.expire)})`
      : "暂无有效到期日";

    titleWrap.append(title, subtitle);

    const metrics = document.createElement("div");
    metrics.className = "ssr-expiry-metrics";
    metrics.append(
      metric("已配置", `${configured.length}/${servers.length}`, "neutral"),
      metric("未配置", missing.length, missing.length ? "missing" : "neutral"),
      metric("已过期", expired.length, expired.length ? "danger" : "neutral"),
      metric("7 天风险", warning.length, warning.length ? "warning" : "neutral"),
      metric("30 天风险", soon.length, soon.length ? "soon" : "neutral"),
      metric("自动", auto.length, auto.length ? "auto" : "neutral"),
    );

    header.append(titleWrap, metrics);

    const list = document.createElement("div");
    list.className = "ssr-expiry-list";
    if (attention.length === 0) {
      const quiet = document.createElement("div");
      quiet.className = "ssr-expiry-quiet";
      quiet.textContent = "暂无到期风险";
      list.append(quiet);
    } else {
      attention.forEach((server) => list.append(makeChip(server, false)));
    }

    panel.hidden = false;
    panel.replaceChildren(header, list);
  }

  function rowLine(server) {
    if (treatAsMissing(server)) {
      const line = document.createElement("div");
      line.className = "ssr-row-expiry ssr-expiry-missing";

      const label = document.createElement("span");
      label.className = "ssr-row-expiry-label";
      label.textContent = "未配置到期";

      line.append(label);
      return line;
    }

    const expire = server.expire;
    const line = document.createElement("div");
    line.className = `ssr-row-expiry ${statusClass(expire.status)}`;

    const date = document.createElement("span");
    date.className = "ssr-row-expiry-date";
    date.textContent = expire.date || expire.raw || "-";

    const label = document.createElement("span");
    label.className = "ssr-row-expiry-label";
    label.textContent = daysText(expire);

    line.append(date, label);

    if (expire.auto_renewal) {
      const auto = document.createElement("span");
      auto.className = "ssr-row-expiry-auto";
      auto.textContent = "续期";
      line.append(auto);
    }

    return line;
  }

  function detailLine(server) {
    if (treatAsMissing(server)) {
      const wrap = document.createElement("div");
      wrap.className = "ssr-expanded-expiry ssr-expiry-missing";
      wrap.textContent = "到期: 未配置 | 状态: 建议补充 VPS 到期时间";
      return wrap;
    }

    const expire = server.expire;
    const wrap = document.createElement("div");
    wrap.className = `ssr-expanded-expiry ${statusClass(expire.status)}`;

    const parts = [
      `到期: ${expire.date || expire.raw || "-"}`,
      `状态: ${daysText(expire)}`,
      expire.original_date && expire.original_date !== expire.date ? `原始: ${expire.original_date}` : "",
      renewalText(expire),
      expire.amount ? `金额: ${expire.amount}` : "",
      expire.source ? `来源: ${expire.source}` : "",
    ].filter(Boolean);

    wrap.textContent = parts.join(" | ");
    return wrap;
  }

  function decorateRows(servers) {
    servers.forEach((server, index) => {
      const row = document.getElementById(`r-${index}`);
      const cell = row ? row.querySelector("td:first-child") : null;
      if (cell) {
        cell.querySelectorAll(".ssr-row-expiry").forEach((node) => node.remove());
        cell.append(rowLine(server));
      }

      const expanded = document.getElementById(`r-${index}-expand`);
      const expandedGrid = expanded ? expanded.querySelector("td > div") : null;
      if (expandedGrid) {
        expandedGrid.querySelectorAll(".ssr-expanded-expiry").forEach((node) => node.remove());
        expandedGrid.append(detailLine(server));
      }
    });
  }

  function render(data) {
    const servers = Array.isArray(data.servers) ? data.servers : [];
    renderPanel(servers);
    decorateRows(servers);
  }

  async function refresh() {
    try {
      const response = await fetch(SOURCE_URL, { cache: "no-store" });
      if (!response.ok) {
        return;
      }
      render(await response.json());
    } catch (err) {
      console.debug("expiry refresh failed", err);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", refresh, { once: true });
  } else {
    refresh();
  }
  setInterval(refresh, 5_000);
})();

import { CAPTURE_FULL, CAPTURE_SNAPSHOT, PASSWORD_FILL, PAGE_CONTEXT, PASSWORD_BADGE_REFRESH } from "../shared/messages.js";
import { convertPage } from "../features/org-capture/capture.js";
import { listEntries } from "../features/password-query/api.js";
import { rankEntries } from "../features/password-search/search.js";
import { clearAfter, copyText } from "../features/password-copy/copy.js";
import { generateTotp } from "../features/password-otp/otp.js";
import { loadEntry, copyableValue } from "../features/password-manager/manager.js";

const $ = (id) => document.getElementById(id);
const state = {
  tab: null, files: [], domainOnly: true, view: null, manualView: false,
  loaded: false, loading: false, contextReady: false, loadError: "", listRequest: 0, selectedFile: null,
  entry: null, detailRequest: 0, otpTimer: null, listScroll: 0, busy: false,
};

function icon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.classList.add("icon");
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS(svg.namespaceURI, "use");
  use.setAttribute("href", `#icon-${name}`);
  svg.append(use);
  return svg;
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function actionButton(name, title, handler) {
  const button = element("button", "icon-button");
  button.type = "button";
  button.title = title;
  button.setAttribute("aria-label", title);
  button.append(icon(name));
  button.addEventListener("click", handler);
  return button;
}

function setStatus(message, error = false) {
  $("password-status").textContent = message;
  $("password-status").classList.toggle("error", error);
}

function setConnection(label, detail, status) {
  $("connection-label").textContent = label;
  $("connection-detail").textContent = detail;
  $("connection").classList.toggle("connected", status === "connected");
  $("connection").classList.toggle("failed", status === "failed");
}

function currentOrigin() {
  try {
    const url = new URL(state.tab?.url || "");
    return /^https?:$/u.test(url.protocol) ? url : null;
  } catch { return null; }
}

function rankedEntries(query = $("search").value, domainOnly = state.domainOnly) {
  const origin = currentOrigin();
  return rankEntries(state.files, { query, host: origin?.hostname || "", port: origin?.port || "", domainOnly });
}

function clearDetails() {
  state.detailRequest += 1;
  clearInterval(state.otpTimer);
  state.otpTimer = null;
  state.entry = null;
  $("details-fields").replaceChildren();
  $("details-actions").hidden = true;
}

function showView(view, { manual = false, focus = true } = {}) {
  if (manual) state.manualView = true;
  if (state.view === "password" && view !== "password") state.listScroll = $("content").scrollTop;
  if (state.view === "details" && view !== "details") clearDetails();
  state.view = view;
  const passwordMode = view !== "tools";
  $("connection").hidden = !passwordMode;
  $("page-context").hidden = !passwordMode;
  $("password-footer").hidden = !passwordMode;
  $("match-count").hidden = !passwordMode || !state.loaded;
  if (!passwordMode) {
    $("diagnostics").hidden = true;
    $("connection").setAttribute("aria-expanded", "false");
  }
  $("initial-loading").hidden = true;
  for (const name of ["tools", "password", "details"]) $(name + "-view").hidden = name !== view;
  for (const tab of document.querySelectorAll(".tab")) {
    const active = tab.dataset.view === (view === "details" ? "password" : view);
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-pressed", String(active));
  }
  $("shortcut-hint").hidden = view !== "password";
  $("content").scrollTop = view === "password" ? state.listScroll : 0;
  if (focus) {
    const target = view === "tools" ? document.querySelector("#tools-view .tool-tile:not(:disabled)") || document.querySelector('.tab[data-view="tools"]') : view === "password" ? $("search") : $("details-back");
    target.focus({ preventScroll: true });
  }
  if (view === "password" && state.contextReady && !state.loaded && !state.loading) void loadFiles();
}

function renderEntries() {
  const ranked = rankedEntries();
  if (!ranked.some((item) => item.file === state.selectedFile)) state.selectedFile = ranked[0]?.file || null;
  $("entries").replaceChildren();
  $("domain-scope").hidden = !state.domainOnly || !currentOrigin();
  $("domain-toggle").textContent = state.domainOnly ? "全部条目" : "当前网站";
  $("domain-toggle").hidden = !currentOrigin();
  const matches = currentOrigin() ? rankedEntries("", true).length : null;
  $("match-count").textContent = state.loaded && matches !== null ? String(matches) : "—";
  $("match-count").hidden = state.view === "tools" || !state.loaded;
  $("result-count").textContent = state.loaded ? `${$("search").value.trim() ? "搜索结果" : state.domainOnly && currentOrigin() ? "当前网站" : "全部条目"} · ${ranked.length} 项` : state.loadError ? "密码条目未加载" : "正在读取密码条目…";
  if (!state.loaded || !ranked.length) {
    const empty = element("div", "empty");
    const message = !state.loaded ? state.loadError || "正在读取密码条目…" : $("search").value.trim() ? "没有搜索结果，可清空搜索词或查看全部条目。" : state.domainOnly && currentOrigin() ? `没有匹配 ${currentOrigin().hostname} 的条目。` : "没有找到密码条目。";
    empty.append(element("p", "", message));
    if (state.loadError || (state.loaded && state.domainOnly && currentOrigin())) {
      const button = element("button", "text-button", state.loadError ? "重试连接" : "查看全部条目");
      button.addEventListener("click", state.loadError ? loadFiles : () => setScope(false));
      empty.append(button);
    }
    $("entries").append(empty);
  }
  for (const item of ranked) $("entries").append(renderEntry(item));
}

function renderEntry(item) {
  const card = element("article", "entry");
  card.dataset.file = item.file;
  card.classList.toggle("selected", item.file === state.selectedFile);
  const main = element("button", "entry-main");
  main.title = "填充用户名和密码";
  const name = element("div", "entry-name", item.file.replace(/\.gpg$/u, "").split("/").at(-1));
  const path = element("div", "entry-path", item.file);
  path.title = item.file;
  main.append(name, path);
  main.addEventListener("focus", () => selectEntry(item.file));
  main.addEventListener("click", () => fill(item.file));
  const actions = element("div", "entry-actions");
  actions.append(
    actionButton("user", "复制用户名", () => copyField(item.file, "login")),
    actionButton("key", "复制密码", () => copyField(item.file, "secret")),
    actionButton("right", "查看详情", () => showDetails(item.file)),
  );
  card.append(main, actions);
  if (item.current) {
    const match = element("div", "entry-match");
    match.append(icon("check"), document.createTextNode(`${item.entryHost === currentOrigin()?.host ? "匹配当前域名" : "匹配父域名"} ${item.entryHost}`));
    card.append(match);
  }
  return card;
}

function selectEntry(file, scroll = false) {
  state.selectedFile = file;
  for (const card of $("entries").children) {
    const selected = card.dataset.file === file;
    card.classList.toggle("selected", selected);
    if (selected && scroll) card.scrollIntoView({ block: "nearest" });
  }
}

function setScope(domainOnly) {
  state.domainOnly = domainOnly;
  renderEntries();
  $("search").focus({ preventScroll: true });
}

async function loadFiles() {
  if (state.loading) return;
  state.loading = true;
  const request = ++state.listRequest;
  state.loadError = "";
  state.loaded = false;
  state.files = [];
  renderEntries();
  setConnection("正在连接", "正在查询本机服务…", "loading");
  setStatus("正在读取密码条目…");
  try {
    const files = await listEntries();
    if (request !== state.listRequest) return;
    state.files = files;
    state.loaded = true;
    setConnection("服务已连接", "本机服务连接正常。", "connected");
    setStatus(`已读取 ${files.length} 个密码条目。`);
  } catch (error) {
    if (request !== state.listRequest) return;
    state.loadError = error.message;
    setConnection("服务未连接", error.message, "failed");
    setStatus(error.message, true);
  } finally {
    if (request === state.listRequest) state.loading = false;
  }
  renderEntries();
  void sendMessage({ type: PASSWORD_BADGE_REFRESH }).catch(() => {});
}

function sendMessage(message) {
  return new Promise((resolve, reject) => chrome.runtime.sendMessage(message, (response) => {
    if (chrome.runtime.lastError) reject(new Error(chrome.runtime.lastError.message));
    else resolve(response);
  }));
}

async function copyValue(value, label) {
  await copyText(value);
  clearAfter(value);
  setStatus(`${label}已复制到剪贴板。`);
}

async function copyField(file, kind) {
  try {
    const entry = await loadEntry(file);
    await copyValue(await copyableValue(entry, kind), kind === "secret" ? "密码" : kind === "otp" ? "OTP" : "用户名");
  } catch (error) { setStatus(error.message, true); }
}

async function fill(file) {
  if (state.busy) return;
  state.busy = true;
  setStatus("正在填充当前页面…");
  try {
    const response = await sendMessage({ type: PASSWORD_FILL, file });
    if (!response?.ok) throw new Error(response?.error || "填充失败。");
    setStatus(`已填充 ${response.filled} 个字段。`);
    try {
      const entry = await loadEntry(file);
      if (entry.fields.otp) {
        await copyValue(await copyableValue(entry, "otp"), "OTP");
        setStatus(`已填充 ${response.filled} 个字段，OTP 已复制。`);
      }
    } catch (error) { setStatus(`已填充 ${response.filled} 个字段；OTP 检查或复制失败：${error.message}`, true); }
  } catch (error) { setStatus(error.message, true); }
  finally { state.busy = false; }
}

function detailField(label, value, { secret = false, copy = null } = {}) {
  const row = element("div", "detail-row");
  row.append(element("span", "detail-key", label));
  const body = element("div", "detail-body");
  const text = element("span", `detail-value${secret ? " secret" : ""}`, secret ? "••••••••••••" : value);
  body.append(text);
  if (secret) {
    let shown = false;
    const toggle = actionButton("eye", "显示密码", () => {
      shown = !shown;
      text.textContent = shown ? value : "••••••••••••";
      toggle.title = shown ? "隐藏密码" : "显示密码";
      toggle.setAttribute("aria-label", toggle.title);
      toggle.setAttribute("aria-pressed", String(shown));
    });
    toggle.setAttribute("aria-pressed", "false");
    body.append(toggle);
  }
  body.append(actionButton("copy", `复制${label}`, async () => {
    try { await copyValue(copy ? await copy() : value, label); }
    catch (error) { setStatus(error.message, true); }
  }));
  row.append(body);
  $("details-fields").append(row);
  return row;
}

async function showDetails(file) {
  clearDetails();
  const request = state.detailRequest;
  $("details-title").textContent = file.replace(/\.gpg$/u, "").split("/").at(-1);
  $("details-path").textContent = file;
  $("details-status").textContent = "正在读取条目…";
  $("details-status").classList.remove("error");
  showView("details", { manual: true });
  try {
    const entry = await loadEntry(file);
    if (request !== state.detailRequest || state.view !== "details") return;
    state.entry = entry;
    $("details-status").textContent = "";
    const labels = { login: "用户名", secret: "密码", url: "网址", openid: "OpenID" };
    for (const key of ["login", "secret", "url", "openid"]) {
      if (entry.fields[key]) detailField(labels[key], entry.fields[key], { secret: key === "secret" });
    }
    for (const { key, value } of entry.extra) detailField(key, value);
    if (entry.fields.otp) {
      const row = detailField("一次性验证码", "正在计算…", { copy: () => copyableValue(entry, "otp") });
      row.classList.add("otp");
      const expiry = element("span", "otp-expiry");
      row.querySelector(".detail-body").insertBefore(expiry, row.querySelector("button"));
      let lastCounter = -1;
      const update = async () => {
        const period = entry.fields.otp.params.period > 0 ? entry.fields.otp.params.period : 30;
        const now = Math.floor(Date.now() / 1000);
        const counter = Math.floor(now / period);
        expiry.textContent = `${period - now % period} 秒`;
        if (lastCounter === counter) return;
        lastCounter = counter;
        try {
          const code = await generateTotp(entry.fields.otp);
          if (request === state.detailRequest) row.querySelector(".detail-value").textContent = code;
        } catch (error) {
          if (request !== state.detailRequest) return;
          row.querySelector(".detail-value").textContent = "不可用";
          expiry.textContent = "";
          setStatus(error.message, true);
          clearInterval(state.otpTimer);
        }
      };
      state.otpTimer = setInterval(update, 1000);
      await update();
    }
    if (request !== state.detailRequest) return;
    $("details-actions").hidden = false;
    $("details-open").hidden = !entry.fields.url;
    $("details-fill").disabled = !currentOrigin();
  } catch (error) {
    if (request !== state.detailRequest) return;
    $("details-status").textContent = error.message;
    $("details-status").classList.add("error");
  }
}

async function openEntryUrl() {
  if (!state.entry?.fields.url || !Number.isInteger(state.tab?.id)) return;
  try {
    const value = state.entry.fields.url;
    const url = new URL(/^[a-z][a-z\d+.-]*:\/\//iu.test(value) ? value : `https://${value}`);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("此条目的网址不是 HTTP(S) 地址。");
    await chrome.tabs.update(state.tab.id, { url: url.href });
  } catch { setStatus("无法打开条目网址，请检查其是否为有效的 HTTP(S) 地址。", true); }
}

async function capture() {
  const button = $("org-capture");
  button.disabled = true;
  button.setAttribute("aria-busy", "true");
  $("tool-status").classList.remove("error");
  $("tool-status").textContent = "正在读取并转换当前页面…";
  try {
    const snapshot = await sendMessage({ type: CAPTURE_SNAPSHOT });
    if (!snapshot?.ok) throw new Error(snapshot?.error || "未能读取页面内容。");
    const response = await sendMessage({ type: CAPTURE_FULL, page: convertPage(snapshot.page) });
    if (!response?.ok) throw new Error(response?.error || "捕获失败。");
    $("tool-status").textContent = response.status;
  } catch (error) {
    $("tool-status").textContent = error.message;
    $("tool-status").classList.add("error");
  } finally {
    button.disabled = !currentOrigin();
    button.setAttribute("aria-busy", "false");
  }
}

for (const tab of document.querySelectorAll(".tab")) tab.addEventListener("click", () => showView(tab.dataset.view, { manual: true }));
$("search").addEventListener("input", () => { state.selectedFile = null; renderEntries(); });
$("domain-scope").addEventListener("click", () => setScope(false));
$("domain-toggle").addEventListener("click", () => setScope(!state.domainOnly));
$("connection").addEventListener("click", () => {
  $("diagnostics").hidden = !$("diagnostics").hidden;
  $("connection").setAttribute("aria-expanded", String(!$("diagnostics").hidden));
});
$("refresh").addEventListener("click", loadFiles);
$("org-capture").addEventListener("click", capture);
$("details-back").addEventListener("click", () => showView("password", { manual: true }));
$("details-fill").addEventListener("click", () => state.entry && fill(state.entry.file));
$("details-open").addEventListener("click", openEntryUrl);
$("search").addEventListener("keydown", (event) => {
  const entries = rankedEntries();
  const selected = entries.findIndex((item) => item.file === state.selectedFile);
  const file = state.selectedFile;
  if (["ArrowDown", "ArrowUp"].includes(event.key) && entries.length) {
    event.preventDefault();
    const next = (Math.max(selected, 0) + (event.key === "ArrowDown" ? 1 : -1) + entries.length) % entries.length;
    selectEntry(entries[next].file, true);
  }
  if (event.key === "Enter" && file) { event.preventDefault(); fill(file); }
  const hasSelection = event.target.selectionStart !== event.target.selectionEnd;
  if (event.ctrlKey && !event.metaKey && !hasSelection && event.key.toLowerCase() === "c" && file) {
    event.preventDefault(); copyField(file, event.shiftKey ? "login" : "secret");
  }
  if (event.ctrlKey && !event.metaKey && event.key.toLowerCase() === "o" && file) { event.preventDefault(); showDetails(file); }
  if (event.key === "Backspace" && !event.target.value && state.domainOnly) { event.preventDefault(); setScope(false); }
});
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (!$("diagnostics").hidden) {
    event.preventDefault();
    $("diagnostics").hidden = true;
    $("connection").setAttribute("aria-expanded", "false");
  } else if (state.view === "details") {
    event.preventDefault(); showView("password", { manual: true });
  }
});
window.addEventListener("pagehide", clearDetails);

async function initialize() {
  try {
    const context = await sendMessage({ type: PAGE_CONTEXT });
    if (!context?.ok) throw new Error(context?.error || "无法读取当前页面，可手动选择功能。");
    state.tab = context.tab;
    state.contextReady = true;
    const origin = currentOrigin();
    $("hostname").textContent = origin?.hostname || "当前页面不支持填充或全文保存";
    $("hostname").title = origin?.hostname || "仍可搜索和复制密码";
    $("org-capture").disabled = !origin;
    if (!origin) $("org-capture").title = "当前页面不支持全文保存";
    if (!state.manualView) showView(context.hasPasswordInput ? "password" : "tools");
    if (context.detectionError) setStatus(context.detectionError, true);
  } catch (error) {
    state.contextReady = true;
    $("hostname").textContent = "无法读取当前页面";
    $("org-capture").disabled = true;
    if (!state.manualView) showView("tools");
    $("tool-status").textContent = "无法读取当前页面，请重新打开插件后重试。";
    $("tool-status").classList.add("error");
    setStatus(error.message, true);
  }
  if (state.view === "password") {
    if (state.loaded) renderEntries();
    else if (!state.loading) void loadFiles();
  }
}
initialize();

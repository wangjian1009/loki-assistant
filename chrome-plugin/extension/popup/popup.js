import { CAPTURE_FULL, PASSWORD_FILL } from "../shared/messages.js";
import { snapshotPage, convertPage } from "../features/org-capture/capture.js";
import { listEntries } from "../features/password-query/api.js";
import { rankEntries } from "../features/password-search/search.js";
import { clearAfter, copyText } from "../features/password-copy/copy.js";
import { generateTotp } from "../features/password-otp/otp.js";
import { loadEntry } from "../features/password-manager/manager.js";

const state = { tab: null, files: [], domainOnly: true, selected: null };
const $ = (id) => document.getElementById(id);
const entriesNode = $("entries");
const statusNode = $("password-status");

function setStatus(message, error = false) { statusNode.textContent = message; statusNode.classList.toggle("error", error); }
function currentOrigin() { try { const url = new URL(state.tab?.url || ""); return /^https?:$/u.test(url.protocol) ? url : null; } catch { return null; } }

function renderEntries() {
  const origin = currentOrigin();
  const ranked = rankEntries(state.files, { query: $("search").value, host: origin?.hostname || "", port: origin?.port || "", domainOnly: state.domainOnly });
  entriesNode.replaceChildren();
  if (!ranked.length) { const empty = document.createElement("div"); empty.className = "empty"; empty.textContent = state.domainOnly && origin ? `没有匹配 ${origin.hostname} 的条目。` : "没有找到密码条目。"; entriesNode.append(empty); }
  for (const item of ranked) entriesNode.append(renderEntry(item));
  $("domain-toggle").classList.toggle("active", state.domainOnly); $("domain-toggle").textContent = state.domainOnly ? "当前网站" : "全部条目";
}
function renderEntry(item) {
  const card = document.createElement("article"); card.className = "entry";
  const main = document.createElement("button"); main.className = "entry-main"; main.title = "填充用户名和密码";
  const name = document.createElement("div"); name.className = "entry-name"; name.textContent = item.file.replace(/\.gpg$/u, "").split("/").at(-1);
  const path = document.createElement("div"); path.className = "entry-path"; path.textContent = item.file; main.append(name, path);
  if (item.entryHost) { const host = document.createElement("span"); host.className = "entry-host"; host.textContent = item.entryHost; main.append(host); }
  main.addEventListener("click", () => fill(item.file));
  const actions = document.createElement("div"); actions.className = "entry-actions";
  actions.append(actionButton("👤", "复制用户名", () => copyField(item.file, "login")), actionButton("🔑", "复制密码", () => copyField(item.file, "secret")), actionButton("⋯", "查看详情", () => showDetails(item.file)));
  card.append(main, actions); return card;
}
function actionButton(label, title, handler) { const button = document.createElement("button"); button.type = "button"; button.textContent = label; button.title = title; button.setAttribute("aria-label", title); button.addEventListener("click", (event) => { event.stopPropagation(); handler(); }); return button; }
async function loadFiles() { setStatus("正在读取密码条目…"); try { state.files = await listEntries(); renderEntries(); setStatus(`${state.files.length} 个密码条目`); } catch (error) { state.files = []; entriesNode.replaceChildren(); setStatus(error.message, true); } }
function sendMessage(message) { return new Promise((resolve, reject) => chrome.runtime.sendMessage(message, (response) => { if (chrome.runtime.lastError) reject(new Error(chrome.runtime.lastError.message)); else resolve(response); })); }
async function fill(file) { setStatus("正在填充当前页面…"); try { const response = await sendMessage({ type: PASSWORD_FILL, file }); if (!response?.ok) throw new Error(response?.error || "填充失败。"); const entry = await loadEntry(file); if (entry.fields.otp) { const token = await generateTotp(entry.fields.otp); await copyText(token); clearAfter(token); setStatus(`已填充 ${response.filled} 个字段，OTP 已复制。`); } else setStatus(`已填充 ${response.filled} 个字段。`); } catch (error) { setStatus(error.message, true); } }
async function copyField(file, kind) { try { const entry = await loadEntry(file); const value = kind === "otp" ? await generateTotp(entry.fields.otp) : entry.fields[kind]; await copyText(value); clearAfter(value); setStatus(kind === "secret" ? "密码已复制到剪贴板。" : kind === "otp" ? "OTP 已复制到剪贴板。" : "用户名已复制到剪贴板。"); } catch (error) { setStatus(error.message, true); } }
async function showDetails(file) {
  const dialog = $("details-dialog"); $("details-title").textContent = file.replace(/\.gpg$/u, "").split("/").at(-1); $("details-path").textContent = file; $("details-fields").replaceChildren(); $("details-status").textContent = "正在读取…"; $("details-status").classList.remove("error"); dialog.showModal();
  try {
    state.selected = await loadEntry(file); $("details-status").textContent = "";
    const fields = { ...state.selected.fields, ...Object.fromEntries(state.selected.extra.map(({ key, value }) => [key, value])) };
    for (const [key, value] of Object.entries(fields)) { const row = document.createElement("div"); row.className = "detail-row"; const keyNode = document.createElement("span"); keyNode.className = "detail-key"; keyNode.textContent = key; const valueNode = document.createElement("span"); valueNode.className = "detail-value"; valueNode.textContent = typeof value === "object" ? value.raw : value; const copy = actionButton("复制", `复制 ${key}`, () => copyField(file, key === "otp" ? "otp" : key === "secret" ? "secret" : "login")); copy.className = "detail-copy"; row.append(keyNode, valueNode, copy); $("details-fields").append(row); }
    $("details-fill").disabled = false; $("details-open").disabled = !state.selected.fields.url;
  } catch (error) { $("details-status").textContent = error.message; $("details-status").classList.add("error"); $("details-fill").disabled = true; $("details-open").disabled = true; }
}
async function readPage() { const [tab] = await chrome.tabs.query({ active: true, currentWindow: true }); if (!Number.isInteger(tab?.id) || !/^https?:\/\//iu.test(tab.url || "")) throw new Error("请在普通 HTTP 或 HTTPS 网页上使用插件。"); const [result] = await chrome.scripting.executeScript({ target: { tabId: tab.id }, func: snapshotPage }); return convertPage(result.result); }
async function capture() { const button = $("org-capture"); const captureStatus = $("capture-status"); button.disabled = true; captureStatus.textContent = "正在转换和保存当前页面…"; try { const response = await sendMessage({ type: CAPTURE_FULL, page: await readPage() }); captureStatus.textContent = response?.ok ? response.status : `捕获失败：${response?.error || "未知错误"}`; } catch (error) { captureStatus.textContent = error.message; } finally { button.disabled = false; } }

$("search").addEventListener("input", renderEntries); $("domain-toggle").addEventListener("click", () => { state.domainOnly = !state.domainOnly; renderEntries(); }); $("refresh").addEventListener("click", loadFiles); $("org-capture").addEventListener("click", capture); $("details-fill").addEventListener("click", () => state.selected && fill(state.selected.file)); $("details-open").addEventListener("click", () => { if (!state.selected?.fields.url) return; try { const value = /^https?:\/\//iu.test(state.selected.fields.url) ? state.selected.fields.url : `http://${state.selected.fields.url}`; const url = new URL(value); if (!["http:", "https:"].includes(url.protocol)) throw new Error(); chrome.tabs.update(state.tab.id, { url: url.href }); } catch { $("details-status").textContent = "此条目的网址不是 HTTP(S) 地址。"; $("details-status").classList.add("error"); } });
for (const tab of document.querySelectorAll(".tab")) tab.addEventListener("click", () => { for (const other of document.querySelectorAll(".tab")) other.classList.toggle("active", other === tab); $("password-view").hidden = tab.dataset.view !== "password"; $("capture-view").hidden = tab.dataset.view !== "capture"; });
$("search").addEventListener("keydown", (event) => { const origin = currentOrigin(); const first = rankEntries(state.files, { query: event.target.value, host: origin?.hostname || "", port: origin?.port || "", domainOnly: state.domainOnly })[0]; if (event.key === "Enter" && first) { event.preventDefault(); fill(first.file); } if (event.ctrlKey && event.key.toLowerCase() === "c" && first) { event.preventDefault(); copyField(first.file, event.shiftKey ? "login" : "secret"); } if (event.ctrlKey && event.key.toLowerCase() === "o" && first) { event.preventDefault(); showDetails(first.file); } if (event.key === "Backspace" && !event.target.value && state.domainOnly) { state.domainOnly = false; renderEntries(); } });

(async () => { [state.tab] = await chrome.tabs.query({ active: true, currentWindow: true }); await loadFiles(); $("search").focus(); })();

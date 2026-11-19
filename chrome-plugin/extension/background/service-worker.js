import { captureFull } from "../shared/services-client.js";
import { CAPTURE_FULL } from "../shared/messages.js";
import { PASSWORD_FILL, PASSWORD_FILL_BEST } from "../shared/messages.js";
import { fetchEntry, listEntries } from "../features/password-query/api.js";
import { parsePasswordEntry } from "../features/password-query/entry.js";
import { rankEntries } from "../features/password-search/search.js";
import { fillPage } from "../features/password-fill/fill.js";

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type === PASSWORD_FILL || message?.type === PASSWORD_FILL_BEST) {
    fillPassword(message)
      .then((result) => sendResponse({ ok: true, ...result }))
      .catch((error) => sendResponse({ ok: false, error: error?.message || String(error) }));
    return true;
  }
  if (message?.type !== CAPTURE_FULL) return undefined;
  if (!message.page || typeof message.page !== "object") {
    sendResponse({ ok: false, error: "没有收到有效的网页内容。" });
    return false;
  }
  captureFull(message.page)
    .then((status) => sendResponse({ ok: true, status }))
    .catch((error) => sendResponse({ ok: false, error: error?.message || String(error) }));
  return true;
});

chrome.commands?.onCommand.addListener((command) => {
  if (command !== "fillBest") return;
  fillPassword({ type: PASSWORD_FILL_BEST })
    .catch((error) => console.warn("填充当前网站密码失败：", error));
});

async function fillPassword(message) {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!Number.isInteger(tab?.id) || !/^https?:\/\//iu.test(tab.url || "")) {
    throw new Error("请在普通 HTTP 或 HTTPS 网页上使用密码填充。");
  }
  let file = message.file;
  if (message.type === PASSWORD_FILL_BEST) {
    const url = new URL(tab.url);
    const entries = await listEntries("");
    const best = rankEntries(entries, { host: url.hostname, port: url.port, domainOnly: true })[0];
    if (!best) throw new Error("没有找到匹配当前网站的密码条目。");
    file = best.file;
  }
  if (typeof file !== "string") throw new Error("密码条目标识无效。");
  const entry = parsePasswordEntry(file, await fetchEntry(file));
  const results = await chrome.scripting.executeScript({
    target: { tabId: tab.id, allFrames: true },
    func: fillPage,
    args: [{
      login: entry.fields.login || "",
      secret: entry.fields.secret || "",
      openid: entry.fields.openid || "",
      autoSubmit: entry.settings.autoSubmit === true,
    }],
  });
  const filled = results.reduce((count, result) => count + Number(result.result?.filled || 0), 0);
  if (!filled) throw new Error("当前页面没有找到可填充的登录表单。");
  return { file, filled };
}

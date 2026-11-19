import { snapshotPage } from "./capture.js";

export async function collectCurrentPage(browser = chrome) {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  if (!Number.isInteger(tab?.id) || !/^https?:\/\//iu.test(tab.url || "")) {
    throw new Error("当前页面不支持全文保存，请在普通 HTTP 或 HTTPS 网页上使用。");
  }
  const [result] = await browser.scripting.executeScript({ target: { tabId: tab.id }, func: snapshotPage });
  if (typeof result?.result?.html !== "string") throw new Error("未能读取页面内容，请刷新网页后重试。");
  return result.result;
}

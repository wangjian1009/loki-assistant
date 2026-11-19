import { listEntries } from "../password-query/api.js";
import { rankEntries } from "../password-search/search.js";

export function createPasswordBadge(browser = chrome, queryEntries = listEntries) {
  const requests = new Map();
  const writes = new Map();
  let pendingEntries = null;

  // 只合并同时发生的查询；完成后不缓存密码条目名称。
  function entries() {
    if (!pendingEntries) pendingEntries = Promise.resolve().then(queryEntries).finally(() => { pendingEntries = null; });
    return pendingEntries;
  }

  function write(tabId, request, text, title) {
    const previous = writes.get(tabId) || Promise.resolve();
    const next = previous.catch(() => {}).then(async () => {
      if (requests.get(tabId) !== request) return;
      await browser.action.setBadgeText({ tabId, text });
      if (requests.get(tabId) === request) await browser.action.setTitle({ tabId, title });
    });
    writes.set(tabId, next);
    return next.finally(() => { if (writes.get(tabId) === next) writes.delete(tabId); });
  }

  async function refresh(tabId) {
    if (!Number.isInteger(tabId) || tabId < 0) return;
    const request = {};
    requests.set(tabId, request);
    try {
      // 新导航开始时先移除上一页面的数字。
      await write(tabId, request, "", "Loki Assistant");
      const tab = await browser.tabs.get(tabId);
      if (requests.get(tabId) !== request) return;
      if (!tab.active || !/^https?:\/\//iu.test(tab.url || "") || (tab.pendingUrl && tab.pendingUrl !== tab.url)) return;
      const url = new URL(tab.url);
      const files = await entries();
      const latest = await browser.tabs.get(tabId);
      if (requests.get(tabId) !== request || !latest.active || latest.url !== tab.url || (latest.pendingUrl && latest.pendingUrl !== latest.url)) return;
      const count = rankEntries(files, { host: url.hostname, port: url.port, domainOnly: true }).length;
      await write(tabId, request, count ? String(count) : "", `Loki Assistant：${url.hostname} 匹配 ${count} 个密码条目`);
    } catch {
      // 标签页关闭及服务失败都不能保留上一页面的计数，也不能显示成零匹配。
      try { await write(tabId, request, "", "Loki Assistant：密码查询未完成，请点击查看或重试"); }
      catch { /* 标签页可能已经关闭。 */ }
    }
  }

  async function refreshActive(windowId) {
    const tabs = await browser.tabs.query(Number.isInteger(windowId) ? { active: true, windowId } : { active: true, lastFocusedWindow: true });
    await Promise.all(tabs.map((tab) => refresh(tab.id)));
  }

  function forget(tabId) { requests.delete(tabId); }
  return { refresh, refreshActive, forget };
}

export function installPasswordBadge(browser = chrome) {
  const badge = createPasswordBadge(browser);
  const refreshActive = () => badge.refreshActive().catch(() => {});
  browser.tabs.onActivated.addListener(({ tabId }) => { void badge.refresh(tabId); });
  browser.tabs.onUpdated.addListener((tabId, change) => {
    if (change.url !== undefined || change.status === "loading" || change.status === "complete") void badge.refresh(tabId);
  });
  browser.tabs.onRemoved.addListener(badge.forget);
  browser.windows.onFocusChanged.addListener((windowId) => {
    if (windowId >= 0) void badge.refreshActive(windowId).catch(() => {});
  });
  browser.runtime.onInstalled.addListener(refreshActive);
  browser.runtime.onStartup.addListener(refreshActive);
  void browser.action.setBadgeBackgroundColor({ color: "#245c49" }).catch(() => {});
  // service worker 因重新加载或休眠恢复而启动时，也刷新当前页面。
  refreshActive();
  return badge;
}

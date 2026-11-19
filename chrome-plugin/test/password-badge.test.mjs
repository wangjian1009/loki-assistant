import test from "node:test";
import assert from "node:assert/strict";
import { createPasswordBadge, installPasswordBadge } from "../extension/features/password-manager/badge.js";

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}
const tick = () => new Promise((resolve) => setImmediate(resolve));

function harness() {
  const tabs = new Map([[7, { id: 7, active: true, url: "https://id1.cloud.huawei.com/login" }]]);
  const texts = new Map();
  const titles = new Map();
  const listeners = {};
  const event = (name) => ({ addListener: (listener) => { listeners[name] = listener; } });
  const browser = {
    tabs: {
      get: async (id) => { if (!tabs.has(id)) throw new Error("标签页已关闭"); return { ...tabs.get(id) }; },
      query: async () => [...tabs.values()].filter((tab) => tab.active),
      onActivated: event("activated"), onUpdated: event("updated"), onRemoved: event("removed"),
    },
    windows: { onFocusChanged: event("focus") },
    runtime: { onInstalled: event("installed"), onStartup: event("startup") },
    action: {
      setBadgeText: async ({ tabId, text }) => { texts.set(tabId, text); },
      setTitle: async ({ tabId, title }) => { titles.set(tabId, title); },
      setBadgeBackgroundColor: async () => {},
    },
  };
  return { browser, tabs, texts, titles, listeners };
}

test("华为父域条目在未打开 popup 时显示数字，无匹配则清空", async () => {
  const h = harness();
  const badge = createPasswordBadge(h.browser, async () => ["huawei.com.gpg", "huawei.com.evil.example.gpg", "example.com.gpg"]);
  await badge.refreshActive();
  assert.equal(h.texts.get(7), "1");
  assert.match(h.titles.get(7), /id1\.cloud\.huawei\.com 匹配 1/u);
  h.tabs.get(7).url = "https://unmatched.example/";
  await badge.refresh(7);
  assert.equal(h.texts.get(7), "");
  assert.match(h.titles.get(7), /匹配 0/u);
});

test("角标沿用端口与域名边界，不按页面是否有密码框计数", async () => {
  const h = harness();
  h.tabs.get(7).url = "https://login.example.com:8443/article";
  const badge = createPasswordBadge(h.browser, async () => ["example.com:8443/a.gpg", "example.com:9000/b.gpg", "example.com/c.gpg"]);
  await badge.refresh(7);
  assert.equal(h.texts.get(7), "2");
});

test("内部页面、后台标签页和待跳转页面不查询密码列表", async () => {
  const h = harness();
  let queries = 0;
  const badge = createPasswordBadge(h.browser, async () => { queries += 1; return []; });
  for (const tab of [
    { active: true, url: "chrome://extensions/" },
    { active: false, url: "https://huawei.com/" },
    { active: true, url: "https://huawei.com/", pendingUrl: "https://example.com/" },
  ]) {
    h.tabs.set(7, { id: 7, ...tab });
    await badge.refresh(7);
    assert.equal(h.texts.get(7), "");
  }
  assert.equal(queries, 0);
});

test("服务失败清除旧数字并显示查询未完成，恢复后可以重试", async () => {
  const h = harness();
  let offline = false;
  const badge = createPasswordBadge(h.browser, async () => {
    if (offline) throw new Error("离线");
    return ["huawei.com.gpg"];
  });
  await badge.refresh(7);
  assert.equal(h.texts.get(7), "1");
  offline = true;
  await badge.refresh(7);
  assert.equal(h.texts.get(7), "");
  assert.match(h.titles.get(7), /未完成/u);
  offline = false;
  await badge.refresh(7);
  assert.equal(h.texts.get(7), "1");
});

test("旧页面的迟到查询不能覆盖新页面，并发请求只读取一次条目列表", async () => {
  const h = harness();
  const pending = deferred();
  let queries = 0;
  const badge = createPasswordBadge(h.browser, () => { queries += 1; return pending.promise; });
  const old = badge.refresh(7);
  await tick();
  h.tabs.get(7).url = "https://example.com/";
  const current = badge.refresh(7);
  await tick();
  pending.resolve(["huawei.com/a.gpg", "huawei.com/b.gpg", "example.com.gpg"]);
  await Promise.all([old, current]);
  assert.equal(queries, 1);
  assert.equal(h.texts.get(7), "1");
  assert.match(h.titles.get(7), /example\.com 匹配 1/u);
});

test("查询期间 URL 已变更或标签页已关闭时，丢弃结果", async () => {
  for (const removed of [false, true]) {
    const h = harness();
    const pending = deferred();
    const badge = createPasswordBadge(h.browser, () => pending.promise);
    const request = badge.refresh(7);
    await tick();
    if (removed) { h.tabs.delete(7); badge.forget(7); }
    else h.tabs.get(7).url = "https://example.com/";
    pending.resolve(["huawei.com.gpg"]);
    await request;
    assert.equal(h.texts.get(7), "");
  }
});

test("安装、启动、激活、导航、窗口切换均可自动刷新，不调用密码详情", async () => {
  const h = harness();
  const originalFetch = globalThis.fetch;
  const urls = [];
  globalThis.fetch = async (url) => {
    urls.push(String(url));
    return Response.json({ api_version: "1", files: ["huawei.com.gpg"] });
  };
  try {
    installPasswordBadge(h.browser);
    await tick();
    assert.equal(h.texts.get(7), "1");
    for (const [name, args] of [
      ["installed", []], ["startup", []], ["activated", [{ tabId: 7 }]],
      ["updated", [7, { url: "https://id1.cloud.huawei.com/login" }]], ["focus", [1]],
    ]) {
      h.texts.set(7, "");
      h.listeners[name](...args);
      await tick();
      assert.equal(h.texts.get(7), "1", name);
    }
    assert.ok(urls.length >= 6);
    assert.ok(urls.every((url) => url === "http://127.0.0.1:47321/password/entries"));
  } finally { globalThis.fetch = originalFetch; }
});

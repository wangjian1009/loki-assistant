import test from "node:test";
import assert from "node:assert/strict";
import { collectCurrentPage } from "../../dist/chrome-plugin/features/org-capture/current-page.js";

test("后台读取当前页面快照，不依赖后台不存在的 DOM", async () => {
  let target;
  const page = await collectCurrentPage({
    tabs: { query: async () => [{ id: 9, url: "https://example.com/article" }] },
    scripting: { executeScript: async (request) => {
      target = request.target;
      return [{ result: { title: "测试文章", url: "https://example.com/article", capturedAt: "2026-11-19T00:00:00.000Z", html: "<h1>正文</h1><p>测试内容</p>" } }];
    } },
  });
  assert.deepEqual(target, { tabId: 9 });
  assert.equal(page.title, "测试文章");
  assert.equal(page.url, "https://example.com/article");
  assert.match(page.html, /测试内容/u);
  assert.equal(page.capturedAt, "2026-11-19T00:00:00.000Z");
  assert.equal(page.body, undefined);
});

test("全文保存拒绝内部页面与缺失快照", async () => {
  await assert.rejects(collectCurrentPage({
    tabs: { query: async () => [{ id: 9, url: "chrome://extensions/" }] },
    scripting: { executeScript: async () => assert.fail("内部页面不得注入") },
  }), /不支持全文保存/u);
  await assert.rejects(collectCurrentPage({
    tabs: { query: async () => [{ id: 9, url: "https://example.com" }] },
    scripting: { executeScript: async () => [] },
  }), /未能读取页面内容/u);
});

import test from "node:test";
import assert from "node:assert/strict";
import { captureCurrentLink, deleteReadLater, listReadLater, markReadLaterRead } from "../extension/features/read-later/api.js";

test("稍后阅读只提交当前网页的标题和 URL，并使用同一 Org ID 标记已读", async (t) => {
  const entry = { id: "日记/链接", title: "一篇文章", url: "https://example.org/story#section" };
  const responses = [
    { entry },
    { entries: [entry] },
    { id: entry.id },
    { entries: [] },
  ];
  const fetch = t.mock.method(globalThis, "fetch", async () => Response.json({
    api_version: "1",
    ...responses.shift(),
  }));
  const browser = { tabs: {
    async query(options) {
      assert.deepEqual(options, { active: true, currentWindow: true });
      return [{ title: "一篇\n文章", url: entry.url }];
    },
  } };

  await captureCurrentLink(browser);
  assert.deepEqual(await listReadLater(), [entry]);
  await markReadLaterRead(entry.id);
  assert.deepEqual(await listReadLater(), []);

  const [
    [captureUrl, capture],
    [listUrl, list],
    [markUrl, mark],
  ] = fetch.mock.calls.map((call) => call.arguments);
  assert.equal(captureUrl, "http://127.0.0.1:47321/doc/read-later/entries");
  assert.equal(listUrl, captureUrl);
  assert.equal(list.method, undefined);
  assert.equal(capture.method, "POST");
  assert.deepEqual(JSON.parse(capture.body), { title: "一篇 文章", url: entry.url });
  assert.equal(markUrl, captureUrl + "/" + encodeURIComponent(entry.id) + "/read");
  assert.equal(mark.method, "POST");
  assert.equal(mark.body, undefined);
});

test("稍后阅读拒绝浏览器内部地址并显示 services 错误", async (t) => {
  const fetch = t.mock.method(globalThis, "fetch", async () => Response.json({
    error: { message: "日记有尚未保存的更改。" },
  }, { status: 409 }));
  const browser = { tabs: { async query() { return [{ url: "chrome://settings/" }]; } } };
  await assert.rejects(captureCurrentLink(browser), /HTTP 或 HTTPS/u);
  assert.equal(fetch.mock.callCount(), 0);
  await assert.rejects(markReadLaterRead("文档"), { message: "日记有尚未保存的更改。" });
});

test("删除稍后阅读向条目 URL 发送 DELETE，并核对 Emacs 返回的 ID", async (t) => {
  const id = "日记/链接";
  const fetch = t.mock.method(globalThis, "fetch", async () => Response.json({ api_version: "1", id }));
  await deleteReadLater(id);
  const [url, options] = fetch.mock.calls[0].arguments;
  assert.equal(url, "http://127.0.0.1:47321/doc/read-later/entries/" + encodeURIComponent(id));
  assert.equal(options.method, "DELETE");
  assert.equal(options.body, undefined);
  await assert.rejects(deleteReadLater("不匹配"), /无法识别的结果/u);
});

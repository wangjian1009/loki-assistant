import test from "node:test";
import assert from "node:assert/strict";
import { captureFull, SERVICES_CAPTURE_URL } from "../extension/shared/services-client.js";

test("services client 使用固定地址并解释 saved 响应", async () => {
  const originalFetch = globalThis.fetch;
  let request;
  globalThis.fetch = async (url, options) => {
    request = { url, options };
    return new Response(JSON.stringify({ status: "saved", document: { id: "example-id" } }), {
      status: 201,
      headers: { "Content-Type": "application/json" },
    });
  };
  try {
    const message = await captureFull({
      title: "标题",
      url: "https://example.com/article",
      captured_at: "2026-11-18T12:30:00.000Z",
      body: "* 正文\n",
    });
    assert.equal(message, "网页全文已保存。");
    assert.equal(SERVICES_CAPTURE_URL, "http://127.0.0.1:47321/doc/article");
    assert.equal(request.url, SERVICES_CAPTURE_URL);
    assert.equal(request.options.method, "POST");
    assert.equal(JSON.parse(request.options.body).title, "标题");
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("services client 将 Emacs 错误消息传递给插件界面", async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response(JSON.stringify({
    api_version: "1",
    status: 404,
    error: {
      source: "business",
      code: "capture-target-not-found",
      message: "Emacs 不支持此全文捕获目标。",
    },
  }), {
    status: 404,
    headers: { "Content-Type": "application/json" },
  });
  try {
    await assert.rejects(
      captureFull({
        title: "标题",
        url: "https://example.com/article",
        captured_at: "2026-11-18T12:30:00.000Z",
        body: "* 正文\n",
      }),
      { message: "Emacs 不支持此全文捕获目标。" },
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("services client 显示没有 JSON 正文的 HTTP 错误", async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response(null, { status: 404 });
  try {
    await assert.rejects(
      captureFull({ title: "标题", url: "https://example.com/article", captured_at: "2026-11-18T12:30:00.000Z", body: "正文" }),
      { message: "保存失败（HTTP 404）。" },
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

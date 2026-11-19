import test from "node:test";
import assert from "node:assert/strict";
import { readFile, realpath } from "node:fs/promises";

test("dist/chrome-plugin/ 的 popup 和后台只加载构建后的 JavaScript", async () => {
  const root = new URL("../../dist/chrome-plugin/", import.meta.url);
  const manifest = JSON.parse(await readFile(new URL("manifest.json", root), "utf8"));
  const htmlURL = new URL(manifest.action.default_popup, root);
  const html = await readFile(htmlURL, "utf8");
  const script = html.match(/<script\b[^>]*\bsrc="([^"]+)"/);
  assert.ok(script, "popup 应包含模块入口");
  const builtRoot = await realpath(root);
  const visited = new Set();
  async function checkModule(url) {
    if (visited.has(url.href)) return;
    visited.add(url.href);
    assert.equal(url.pathname.endsWith(".js"), true, `不能加载非 JavaScript 模块：${url.pathname}`);
    assert.ok((await realpath(url)).startsWith(`${builtRoot}/`), "模块必须位于构建目录内");
    const javascript = await readFile(url, "utf8");
    for (const match of javascript.matchAll(/\bfrom\s*["']([^"']+)["']/g)) {
      await checkModule(new URL(match[1], url));
    }
  }
  await checkModule(new URL(script[1], htmlURL));
  await checkModule(new URL(manifest.background.service_worker, root));
  assert.ok([...visited].some((url) => url.endsWith("/html2org/turndown.js")));
  assert.ok([...visited].some((url) => url.endsWith("/password-query/api.js")));
  assert.ok([...visited].some((url) => url.endsWith("/password-otp/otp.js")));
});

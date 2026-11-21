import assert from "node:assert/strict";
import test from "node:test";
import { acceptSlackToken, slackTokenImport } from "../extension/features/slack-token/import.js";

const localPage = "http://127.0.0.1:47321/slack/token-import?team=TEXAMPLE";

test("Slack 导入仅匹配本机 services 的导入页", () => {
  assert.equal(slackTokenImport(localPage)?.teamId, "TEXAMPLE");
  for (const url of [
    localPage.replace("127.0.0.1", "example.com"),
    localPage.replace("47321", "8888"),
    localPage.replace("/slack/token-import", "/other"),
    localPage.replace("TEXAMPLE", "invalid"),
  ]) assert.equal(slackTokenImport(url), null);
});

test("Slack 用户令牌随请求体提交", async () => {
  await acceptSlackToken(slackTokenImport(localPage), "xoxp-example", async (url, options) => {
    assert.equal(url, "http://127.0.0.1:47321/slack/token-import");
    assert.equal(options.redirect, "error");
    assert.deepEqual(options.headers, { "Content-Type": "application/json" });
    assert.deepEqual(JSON.parse(options.body), { team_id: "TEXAMPLE", token: "xoxp-example" });
    return { status: 204 };
  });
});

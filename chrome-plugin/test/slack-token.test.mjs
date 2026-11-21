import assert from "node:assert/strict";
import test from "node:test";
import { acceptSlackToken, slackTokenImport } from "../extension/features/slack-token/import.js";

const attempt = "723588fe-df01-40df-9074-9db552cb5c27";
const localPage = `http://127.0.0.1:47321/slack/token-import?team=TEXAMPLE#${attempt}`;

test("Slack 导入仅匹配本机 services 的导入页", () => {
  assert.equal(slackTokenImport(localPage)?.teamId, "TEXAMPLE");
  for (const url of [
    localPage.replace("127.0.0.1", "example.com"),
    localPage.replace("47321", "8888"),
    localPage.replace("/slack/token-import", "/other"),
    localPage.replace(attempt, "invalid"),
  ]) assert.equal(slackTokenImport(url), null);
});

test("Slack 令牌随请求体提交，一次性标识随授权头提交", async () => {
  await acceptSlackToken(slackTokenImport(localPage), "xoxp-example", async (url, options) => {
    assert.equal(url, "http://127.0.0.1:47321/slack/token-import");
    assert.equal(options.redirect, "error");
    assert.equal(options.headers.Authorization, `Bearer ${attempt}`);
    assert.deepEqual(JSON.parse(options.body), { team_id: "TEXAMPLE", token: "xoxp-example" });
    return { status: 204 };
  });
});

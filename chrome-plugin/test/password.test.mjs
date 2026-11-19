import test from "node:test";
import assert from "node:assert/strict";
import { parsePasswordEntry } from "../extension/features/password-query/entry.js";
import { rankEntries } from "../extension/features/password-search/search.js";
import { generateTotp } from "../extension/features/password-otp/otp.js";
import { fetchEntry, listEntries, SERVICES_PASSWORD_URL } from "../extension/features/password-query/api.js";

test("密码原文解析沿用 browserpass 字段别名、首行密码和 autosubmit", () => {
  const entry = parsePasswordEntry("accounts/example.com/alice.gpg", [
    "first-secret",
    "username: alice",
    "website: https://example.com/login",
    "otpauth://totp/Example:alice?secret=JBSWY3DPEHPK3PXP&digits=8&period=60",
    "autosubmit: yes",
    "note: personal",
  ].join("\n"));
  assert.equal(entry.fields.secret, "first-secret");
  assert.equal(entry.fields.login, "alice");
  assert.equal(entry.fields.url, "https://example.com/login");
  assert.equal(entry.fields.otp.params.digits, 8);
  assert.equal(entry.fields.otp.params.period, 60);
  assert.equal(entry.settings.autoSubmit, true);
  assert.deepEqual(entry.extra, [{ key: "note", value: "personal" }]);
});

test("密码搜索优先当前域名并支持其余词的字面过滤", () => {
  const files = ["example.com/alice.gpg", "login.example.com/work.gpg", "github.com/alice.gpg", "notes/example.gpg"];
  const current = rankEntries(files, { host: "login.example.com", query: "login work", domainOnly: false });
  assert.deepEqual(current.map((entry) => entry.file), ["login.example.com/work.gpg"]);
  const domainOnly = rankEntries(files, { host: "example.com", domainOnly: true });
  assert.deepEqual(domainOnly.map((entry) => entry.file), ["example.com/alice.gpg"]);
});

test("OTP 使用 RFC 6238 的 SHA1 八位测试向量", async () => {
  const otp = { params: { type: "totp", secret: "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", algorithm: "SHA1", digits: 8, period: 30 } };
  assert.equal(await generateTotp(otp, 59000), "94287082");
});

test("密码客户端只使用 services 的 GET 查询接口并编码资源路径", async () => {
  const originalFetch = globalThis.fetch;
  const requests = [];
  globalThis.fetch = async (url, options) => {
    requests.push({ url: String(url), options });
    const body = String(url).includes("%E4%B8%AD")
      ? { api_version: "1", file: "中文 网站/a#?%.gpg", contents: "secret" }
      : { api_version: "1", files: ["example.com/alice.gpg"] };
    return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
  };
  try {
    assert.deepEqual(await listEntries("example.com"), ["example.com/alice.gpg"]);
    assert.equal(await fetchEntry("中文 网站/a#?%.gpg"), "secret");
    assert.equal(requests[0].options.method, undefined);
    assert.equal(requests[1].options.method, undefined);
    assert.equal(requests[0].url, `${SERVICES_PASSWORD_URL}?query=example.com`);
    assert.equal(requests[1].url, `${SERVICES_PASSWORD_URL}/%E4%B8%AD%E6%96%87%20%E7%BD%91%E7%AB%99/a%23%3F%25.gpg`);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

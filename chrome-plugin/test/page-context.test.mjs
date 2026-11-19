import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { hasPasswordInput, readPageContext } from "../extension/features/password-fill/context.js";
import { generateTotp } from "../extension/features/password-otp/otp.js";

function input({ disabled = false, readOnly = false, rects = [{ width: 100, height: 30 }], parent = null, style = {} } = {}) {
  return {
    matches: () => disabled, readOnly, getClientRects: () => rects, parentElement: parent,
    getRootNode: () => ({}), style,
  };
}
function root(inputs = [], elements = []) {
  return { querySelectorAll: (selector) => selector === "*" ? elements : inputs };
}
function detect(document) {
  return vm.runInNewContext(`(${hasPasswordInput.toString()})()`, {
    document,
    getComputedStyle: (node) => ({ display: "block", visibility: "visible", opacity: "1", ...node.style }),
  });
}

test("默认页检测只接受可见且可编辑的密码框，不读取输入值", () => {
  const field = input();
  Object.defineProperty(field, "value", { get() { throw new Error("不得读取密码内容"); } });
  assert.equal(detect(root([field])), true);
  for (const config of [
    { disabled: true }, { readOnly: true }, { rects: [] }, { rects: [{ width: 0, height: 30 }] },
    { style: { visibility: "hidden" } }, { style: { opacity: "0" } },
    { parent: input({ style: { display: "none" } }) },
    { parent: input({ style: { contentVisibility: "hidden" } }) },
  ]) assert.equal(detect(root([input(config)])), false);
  assert.equal(detect(root()), false);
});

test("默认页检测支持 open Shadow DOM，并排除隐藏的宿主", () => {
  const host = input();
  const field = input();
  field.getRootNode = () => ({ host });
  host.shadowRoot = root([field]);
  assert.equal(detect(root([], [host])), true);
  host.style.opacity = "0";
  assert.equal(detect(root([], [host])), false);
});

test("后台汇总全部可访问 frame，有密码框即选择密码，与条目数量无关", async () => {
  let request;
  const context = await readPageContext({
    tabs: { query: async () => [{ id: 7, url: "https://id1.cloud.huawei.com/login", title: "登录" }] },
    scripting: { executeScript: async (args) => { request = args; return [{ frameId: 0, result: false }, { frameId: 1, result: true }]; } },
  });
  assert.equal(context.hasPasswordInput, true);
  assert.equal(context.tab.id, 7);
  assert.deepEqual(request.target, { tabId: 7, allFrames: true });
  assert.equal(request.func, hasPasswordInput);
});

test("受限页面不注入，检测失败降级工具页并返回原因", async () => {
  for (const tab of [undefined, { id: 3, url: "chrome://extensions/" }]) {
    const context = await readPageContext({
      tabs: { query: async () => tab ? [tab] : [] },
      scripting: { executeScript: async () => { assert.fail("受限页面不得注入"); } },
    });
    assert.equal(context.hasPasswordInput, false);
  }
  const context = await readPageContext({
    tabs: { query: async () => [{ id: 3, url: "https://example.com" }] },
    scripting: { executeScript: async () => { throw new Error("无权访问"); } },
  });
  assert.equal(context.hasPasswordInput, false);
  assert.match(context.detectionError, /手动切换/u);
});

test("HOTP 条目不得伪装成 TOTP 验证码", async () => {
  await assert.rejects(generateTotp({ type: "hotp", secret: "JBSWY3DPEHPK3PXP" }), /只支持 TOTP/u);
});

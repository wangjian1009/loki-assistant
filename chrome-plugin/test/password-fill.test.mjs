import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { fillPage, fillAccessibleFrames } from "../extension/features/password-fill/fill.js";

function fixture() {
  class Input {
    get value() { return this.contents || ""; }
    set value(value) { this.contents = value; }
  }
  const document = { inputs: [], querySelectorAll: (selector) => selector === "input" ? document.inputs : [] };
  function input(type, options = {}) {
    const field = Object.assign(new Input(), {
      type, name: "", id: "", className: "", autocomplete: "", placeholder: "", maxLength: -1,
      readOnly: false, disabled: false, isConnected: true, form: null, parentElement: null,
      events: [], style: {}, getRootNode: () => ({}), getAttribute: () => "",
      matches: () => field.disabled,
      getBoundingClientRect: () => ({ width: 200, height: 32, left: 0, top: 0, right: 200, bottom: 32, ...field.rect }),
      ownerDocument: { defaultView: { HTMLInputElement: Input } },
      dispatchEvent: (event) => { field.events.push(event); field.onEvent?.(event); },
    }, options);
    document.inputs.push(field);
    return field;
  }
  const window = {
    innerWidth: 1000, innerHeight: 800,
    getComputedStyle: (node) => ({ display: "block", visibility: "visible", opacity: "1", ...node.style }),
  };
  window.top = window;
  const fill = (fields = {}) => vm.runInNewContext(`(${fillPage.toString()})(fields)`, {
    document, window, location: { origin: "https://example.com" }, Event,
    fields: { login: "fixture-user", secret: "fixture-secret", ...fields },
  });
  return { input, document, fill };
}

test("华为式隐藏诱饵框不参与填充，计数只包含可见框", () => {
  const f = fixture();
  const trapUser = f.input("text", { name: "username", rect: { width: 0, height: 0 } });
  const trapPassword = f.input("password", { rect: { width: 0, height: 0 } });
  const user = f.input("text", { placeholder: "手机号/邮件地址/账号名" });
  const password = f.input("password", { placeholder: "密码" });
  const result = f.fill();
  assert.equal(result.filled, 2);
  assert.equal(user.value, "fixture-user");
  assert.equal(password.value, "fixture-secret");
  assert.equal(trapUser.value, "");
  assert.equal(trapPassword.value, "");
});

test("密码框不能被当成用户名，用户名与密码限定在登录表单", () => {
  const f = fixture();
  const searchForm = { isConnected: true, getAttribute: () => "search" };
  const loginForm = { isConnected: true, getAttribute: () => "login" };
  const search = f.input("text", { name: "user-search", form: searchForm });
  const password = f.input("password", { name: "loginPassword", form: loginForm });
  const user = f.input("email", { autocomplete: "username", form: loginForm });
  assert.equal(f.fill().filled, 2);
  assert.equal(search.value, "");
  assert.equal(user.value, "fixture-user");
  assert.equal(password.value, "fixture-secret");
  assert.equal(password.events.filter((event) => event.type === "blur").length, 1);
});

test("父元素透明、只读、禁用、尺寸过小和视口外输入框不填充", () => {
  for (const options of [
    { parentElement: { style: { opacity: "0" }, getRootNode: () => ({}) } },
    { readOnly: true }, { disabled: true }, { rect: { width: 1, height: 1 } },
    { rect: { left: 1100, right: 1300 } },
  ]) {
    const f = fixture();
    const input = f.input("password", options);
    assert.equal(f.fill().filled, 0);
    assert.equal(input.value, "");
  }
});

test("聚焦替换输入框后重新查找，派发填充前后事件且不写 value attribute", () => {
  const f = fixture();
  let replacement;
  const original = f.input("text", { name: "username", onEvent: (event) => {
    if (event.type === "focus" && !replacement) {
      replacement = f.input("text", { name: "username" });
      f.document.inputs = f.document.inputs.filter((input) => input !== original);
      original.isConnected = false;
    }
  } });
  const password = f.input("password");
  assert.equal(f.fill().filled, 2);
  assert.equal(original.value, "");
  assert.equal(replacement.value, "fixture-user");
  assert.equal(password.value, "fixture-secret");
  const events = replacement.events.map((event) => event.type);
  assert.ok(events.indexOf("focus") < events.indexOf("input"));
  assert.equal(events.filter((name) => name === "input").length, 2);
  assert.equal(events.at(-1), "blur");
  assert.ok(replacement.events.every((event) => event.composed));
});

test("原生 setter 可绕过受控输入框的实例 setter，失败不虚报成功", () => {
  const f = fixture();
  const password = f.input("password");
  Object.defineProperty(password, "value", { get() { return this.contents || ""; }, set() { assert.fail("不能使用实例 setter"); } });
  assert.equal(f.fill().fields.secret, true);
  assert.equal(password.value, "fixture-secret");
  const g = fixture();
  g.input("password", { onEvent(event) { if (event.type === "blur") this.contents = ""; } });
  assert.equal(g.fill().filled, 0);
});

test("主文档填充成功就停止，不受受限 iframe 影响", async () => {
  const calls = [];
  const browser = { scripting: { executeScript: async (request) => {
    calls.push(request);
    assert.equal(request.target.allFrames, undefined);
    return [{ frameId: 0, result: { filled: 2, fields: { login: true, secret: true } } }];
  } } };
  const result = await fillAccessibleFrames(browser, { id: 9, url: "https://example.com/login" }, { login: "fixture-user", secret: "fixture-secret" });
  assert.equal(calls.length, 1);
  assert.equal(result[0].result.filled, 2);
  assert.equal(calls[0].args[0].origin, "https://example.com");
});

test("主文档缺少密码框才尝试可访问 frame，失败时保留主文档结果", async () => {
  for (const denied of [false, true]) {
    let calls = 0;
    const browser = { scripting: { executeScript: async (request) => {
      calls += 1;
      if (!request.target.allFrames) return [{ frameId: 0, result: { filled: 1, fields: { login: true } } }];
      assert.equal(request.args[0].skipTopFrame, true);
      if (denied) throw new Error("受限 frame");
      return [{ frameId: 0, result: { filled: 0 } }, { frameId: 1, result: { filled: 2, fields: { secret: true } } }];
    } } };
    const result = await fillAccessibleFrames(browser, { id: 9, url: "https://example.com" }, { login: "fixture-user", secret: "fixture-secret" });
    assert.equal(calls, 2);
    assert.equal(result.length, denied ? 1 : 2);
    assert.equal(result[0].result.filled, 1);
  }
});

test("跨 origin frame 不写入凭据", () => {
  const f = fixture();
  const password = f.input("password");
  assert.equal(f.fill({ origin: "https://another.example" }).filled, 0);
  assert.equal(password.value, "");
});

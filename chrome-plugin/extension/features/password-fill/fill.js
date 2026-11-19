// 该函数会被 chrome.scripting.executeScript 序列化到网页上下文，不能引用模块变量。
export function fillPage(fields) {
  const result = { filled: 0, fields: { login: false, secret: false, openid: false } };
  if (fields.skipTopFrame && window === window.top) return result;
  if (fields.origin && location.origin !== fields.origin) return result;

  const isVisible = (element) => {
    if (element.matches(":disabled") || element.readOnly) return false;
    const rect = element.getBoundingClientRect();
    // 与 Browserpass 一样排除隐藏父元素、尺寸过小和视口外的诱饵输入框。
    if (rect.width < 30 || rect.height < 10 || rect.right < 0 || rect.bottom < 0 || rect.left > window.innerWidth || rect.top > window.innerHeight) return false;
    let opacity = 1;
    for (let node = element; node; node = node.parentElement || node.getRootNode()?.host) {
      const style = window.getComputedStyle(node);
      if (style.display === "none" || ["hidden", "collapse"].includes(style.visibility) || style.contentVisibility === "hidden") return false;
      opacity *= Number(style.opacity);
      if (opacity < 0.1) return false;
    }
    return true;
  };
  const inputs = () => {
    const collect = (root) => {
      const found = [...root.querySelectorAll("input")];
      for (const element of root.querySelectorAll("*")) {
        if (element.shadowRoot) found.push(...collect(element.shadowRoot));
      }
      return found;
    };
    return collect(document).filter(isVisible);
  };
  const isText = (element) => ["text", "email", "tel"].includes(element.type.toLowerCase());
  const isPassword = (element) => element.type.toLowerCase() === "password";
  const marker = (form) => /login|log[-_]in|signin|sign[-_]in/iu.test(["id", "name", "class", "action"].map((key) => form.getAttribute(key) || "").join(" "));
  const initial = inputs();
  const forms = [...new Set(initial.filter((input) => isText(input) || isPassword(input)).map((input) => input.form).filter(Boolean))];
  const formWithPassword = (form) => initial.some((input) => input.form === form && isPassword(input));
  let loginForm = forms.find((form) => marker(form) && formWithPassword(form)) || forms.find(formWithPassword) || forms.find(marker) || forms[0];

  function find(kind) {
    const available = inputs();
    // 聚焦可能替换整个 form，此时重新定位，不继续使用已脱离文档的表单。
    if (loginForm && !loginForm.isConnected) loginForm = available.find(isPassword)?.form || available.find(isText)?.form;
    const scoped = available.filter((input) => !loginForm || input.form === loginForm);
    if (kind === "secret") return scoped.find((input) => isPassword(input) && input.autocomplete.toLowerCase() === "current-password") || scoped.find(isPassword);
    const candidates = scoped.filter(isText);
    if (kind === "openid") return candidates.find((input) => /openid/iu.test(`${input.name} ${input.id} ${input.className}`));
    return candidates.find((input) => input.autocomplete.toLowerCase() === "username") ||
      candidates.find((input) => /user|login|email|account|alias|用户名|账号|手机号|邮件/iu.test(`${input.name} ${input.id} ${input.className} ${input.getAttribute("aria-label") || ""} ${input.placeholder}`)) ||
      candidates.find((input) => input.type === "email") || candidates[0];
  }

  function update(kind, value) {
    if (typeof value !== "string" || !value) return false;
    let input = find(kind);
    if (!input) return false;
    const dispatch = (names) => {
      for (const name of names) input.dispatchEvent(new Event(name, { bubbles: true, composed: true }));
    };
    dispatch(["click", "focus"]);
    input = find(kind);
    if (!input) return false;
    dispatch(["click", "focus"]);
    input = find(kind);
    if (!input) return false;
    dispatch(["keydown", "keypress", "keyup", "input", "change"]);
    const targetValue = input.maxLength > 0 ? value.slice(0, input.maxLength) : value;
    const setter = Object.getOwnPropertyDescriptor(input.ownerDocument.defaultView.HTMLInputElement.prototype, "value")?.set;
    const assign = () => { if (setter) setter.call(input, targetValue); else input.value = targetValue; };
    // 使用原生 setter 兼容受控输入框；密码不写入 value attribute。
    assign();
    dispatch(["keydown", "keypress", "keyup", "input", "change"]);
    if (input.value !== targetValue) assign();
    dispatch(["blur"]);
    const current = find(kind);
    return Boolean(current && current.value === targetValue);
  }

  if (fields.openid) result.fields.openid = update("openid", fields.openid);
  else result.fields.login = update("login", fields.login);
  result.fields.secret = update("secret", fields.secret);
  result.filled = Object.values(result.fields).filter(Boolean).length;
  if (fields.autoSubmit && result.filled) {
    const input = find(result.fields.secret ? "secret" : result.fields.openid ? "openid" : "login");
    const form = input?.form;
    if (form) {
      if (typeof form.requestSubmit === "function") form.requestSubmit();
      else form.submit();
    }
  }
  return result;
}

export async function fillAccessibleFrames(browser, tab, fields) {
  const origin = new URL(tab.url).origin;
  const args = [{ ...fields, origin }];
  const top = await browser.scripting.executeScript({ target: { tabId: tab.id }, func: fillPage, args });
  const important = fields.openid ? "openid" : "secret";
  if (top.some((frame) => frame.result?.fields?.[important])) return top;
  try {
    const frames = await browser.scripting.executeScript({
      target: { tabId: tab.id, allFrames: true }, func: fillPage,
      args: [{ ...fields, origin, skipTopFrame: true }],
    });
    return [...top, ...frames.filter((frame) => frame.frameId !== 0)];
  } catch {
    // 无权注入某个 iframe 时，保留主文档已经成功填充的结果。
    return top;
  }
}

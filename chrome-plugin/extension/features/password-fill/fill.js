import { fetchEntry } from "../password-query/api.js";
import { parsePasswordEntry } from "../password-query/entry.js";

// 该函数会被 chrome.scripting.executeScript 序列化到网页上下文，不能引用外部变量。
export function fillPage(fields) {
  const isVisible = (element) => {
    const style = window.getComputedStyle(element);
    return style.display !== "none" && style.visibility !== "hidden" && !element.disabled;
  };
  const setValue = (element, value) => {
    if (!element || typeof value !== "string") return false;
    const descriptor = Object.getOwnPropertyDescriptor(element.ownerDocument.defaultView.HTMLInputElement.prototype, "value");
    if (descriptor?.set) descriptor.set.call(element, value); else element.value = value;
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  };
  const candidates = [...document.querySelectorAll("input, textarea")].filter(isVisible);
  const passwords = candidates.filter((input) => input.matches("input[type=password]"));
  const textInputs = candidates.filter((input) => !input.matches("input[type=hidden], input[type=submit], input[type=button], input[type=checkbox], input[type=radio]"));
  let filled = 0;
  const username = textInputs.find((input) => /user|login|email|account|name/iu.test(`${input.name} ${input.id} ${input.autocomplete} ${input.placeholder}`));
  if (fields.openid && setValue(textInputs.find((input) => /openid/iu.test(`${input.name} ${input.id}`)), fields.openid)) filled += 1;
  else if (setValue(username || textInputs[0], fields.login)) filled += 1;
  if (setValue(passwords[0], fields.secret)) filled += 1;
  if (fields.autoSubmit) {
    const form = passwords[0]?.form || username?.form || textInputs[0]?.form;
    if (form) {
      if (typeof form.requestSubmit === "function") form.requestSubmit();
      else form.submit();
    }
  }
  return { filled, fields: { login: Boolean(username || textInputs[0]), secret: Boolean(passwords[0]) } };
}

export async function fillEntryInCurrentTab(file) {
  const raw = await fetchEntry(file);
  return parsePasswordEntry(file, raw);
}

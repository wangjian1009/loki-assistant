import { fetchEntry } from "../password-query/api.js";
import { parsePasswordEntry } from "../password-query/entry.js";
import { generateTotp } from "../password-otp/otp.js";

export async function loadEntry(file) {
  return parsePasswordEntry(file, await fetchEntry(file));
}

export async function copyableValue(entry, kind) {
  if (kind === "otp") {
    if (!entry.fields.otp) throw new Error("此条目没有 OTP 配置。");
    return generateTotp(entry.fields.otp);
  }
  const value = entry.fields[kind];
  if (!value) throw new Error(`此条目没有${kind === "secret" ? "密码" : "用户名"}字段。`);
  return value;
}

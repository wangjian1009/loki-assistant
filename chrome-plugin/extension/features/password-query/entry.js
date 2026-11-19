import { parseOtp } from "../password-otp/otp.js";

const FIELD_ALIASES = {
  secret: ["secret", "password", "pass"],
  login: ["login", "username", "user"],
  openid: ["openid"],
  otp: ["otp", "totp"],
  url: ["url", "uri", "website", "site", "link", "launch"],
};

function splitFile(file) {
  const withoutExtension = file.replace(/\.gpg$/u, "");
  const parts = withoutExtension.split("/");
  return {
    file,
    name: parts.at(-1) || withoutExtension,
    path: parts.slice(0, -1).join("/"),
    label: withoutExtension,
  };
}

export function parsePasswordEntry(file, raw) {
  const entry = { ...splitFile(file), raw, fields: {}, settings: {}, extra: [] };
  const lines = raw.split(/[\r\n]+/u).filter((line) => line.trim().length > 0);
  for (const original of lines) {
    const line = /^otpauth:\/\/.+/iu.test(original) ? `otp: ${original}` : original;
    const match = line.match(/^(.+?):(.*)$/u);
    if (!match) continue;
    const key = match[1].trim().toLowerCase();
    const value = match[2].trim();
    const field = Object.entries(FIELD_ALIASES).find(([, aliases]) => aliases.includes(key))?.[0];
    if (field && (value || field === "login")) {
      entry.fields[field] = value;
      continue;
    }
    if (key === "autosubmit") {
      entry.settings.autoSubmit = ["true", "yes", "1"].includes(value.toLowerCase());
      continue;
    }
    if (value) entry.extra.push({ key: match[1].trim(), value });
  }

  if (!entry.fields.secret && lines[0]) entry.fields.secret = lines[0];
  if (!entry.fields.login) entry.fields.login = entry.name;
  if (entry.fields.otp) entry.fields.otp = parseOtp(entry.fields.otp);
  return entry;
}

export { FIELD_ALIASES };

function base32Decode(input) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  const normalized = input.replace(/[=\s-]/gu, "").toUpperCase();
  let bits = "";
  for (const character of normalized) {
    const value = alphabet.indexOf(character);
    if (value < 0) throw new Error("OTP secret 不是有效的 Base32 字符串。");
    bits += value.toString(2).padStart(5, "0");
  }
  const bytes = [];
  for (let index = 0; index + 8 <= bits.length; index += 8) bytes.push(Number.parseInt(bits.slice(index, index + 8), 2));
  return new Uint8Array(bytes);
}

export function parseOtp(value) {
  if (!value) return null;
  if (!/^otpauth:\/\//iu.test(value)) return { raw: value, params: { type: "totp", secret: value, algorithm: "SHA1", digits: 6, period: 30 } };
  try {
    const url = new URL(value.replace(/^otpauth:/iu, "https:"));
    const secret = url.searchParams.get("secret");
    if (!secret) throw new Error("OTP URI 缺少 secret 参数。");
    return {
      raw: value,
      params: {
        type: url.hostname.toLowerCase(),
        secret: secret.toUpperCase(),
        algorithm: (url.searchParams.get("algorithm") || "SHA1").toUpperCase(),
        digits: Number.parseInt(url.searchParams.get("digits") || "6", 10),
        period: Number.parseInt(url.searchParams.get("period") || "30", 10),
        issuer: url.searchParams.get("issuer") || "",
      },
    };
  } catch (error) {
    throw new Error(`无法解析 OTP URI：${error.message}`, { cause: error });
  }
}

export async function generateTotp(otp, timestamp = Date.now()) {
  const params = otp?.params || otp;
  const period = Number.isFinite(params.period) && params.period > 0 ? params.period : 30;
  const digits = [6, 7, 8].includes(params.digits) ? params.digits : 6;
  const counter = Math.floor(timestamp / 1000 / period);
  const message = new ArrayBuffer(8);
  const view = new DataView(message);
  view.setUint32(0, Math.floor(counter / 0x100000000));
  view.setUint32(4, counter >>> 0);
  const algorithm = ({ SHA1: "SHA-1", SHA256: "SHA-256", SHA512: "SHA-512" })[params.algorithm] || "SHA-1";
  const key = await crypto.subtle.importKey("raw", base32Decode(params.secret), { name: "HMAC", hash: algorithm }, false, ["sign"]);
  const digest = new Uint8Array(await crypto.subtle.sign("HMAC", key, message));
  const offset = digest.at(-1) & 0x0f;
  const code = ((digest[offset] & 0x7f) << 24) | (digest[offset + 1] << 16) | (digest[offset + 2] << 8) | digest[offset + 3];
  return String(code % (10 ** digits)).padStart(digits, "0");
}

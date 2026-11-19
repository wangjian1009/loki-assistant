const SERVICES_PASSWORD_URL = "http://127.0.0.1:47321/password/entries";

async function request(url, timeout = 10000) {
  let response;
  try {
    response = await fetch(url, {
      cache: "no-store",
      signal: AbortSignal.timeout(timeout),
    });
  } catch (error) {
    if (error?.name === "TimeoutError" || error?.name === "AbortError") {
      throw new Error("密码查询超时，请稍后重试。", { cause: error });
    }
    throw new Error("无法连接 Loki Assistant，请确认 services 已启动。", { cause: error });
  }
  let body = null;
  try {
    body = await response.json();
  } catch (error) {
    throw new Error(`services 返回了无法识别的结果（HTTP ${response.status}）。`, { cause: error });
  }
  if (!response.ok) {
    throw new Error(body?.error?.message || `密码查询失败（HTTP ${response.status}）。`);
  }
  if (body?.api_version !== "1") throw new Error("services 返回了不兼容的密码查询版本。");
  return body;
}

export async function listEntries(query = "") {
  const url = new URL(SERVICES_PASSWORD_URL);
  if (query) url.searchParams.set("query", query);
  const body = await request(url);
  if (!Array.isArray(body.files) || body.files.some((file) => typeof file !== "string")) {
    throw new Error("services 返回了无效的密码条目列表。");
  }
  return body.files;
}

export async function fetchEntry(file) {
  if (typeof file !== "string" || !file.endsWith(".gpg")) {
    throw new Error("密码条目标识无效。");
  }
  const path = file.split("/").map((part) => encodeURIComponent(part)).join("/");
  const body = await request(`${SERVICES_PASSWORD_URL}/${path}`);
  if (body.file !== file || typeof body.contents !== "string") {
    throw new Error("services 返回了无效的密码条目。");
  }
  return body.contents;
}

export { SERVICES_PASSWORD_URL };

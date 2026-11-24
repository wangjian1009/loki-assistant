const ENTRIES_URL = "http://127.0.0.1:47321/doc/read-later/entries";

export function webUrl(value) {
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) ? url : null;
  } catch {
    return null;
  }
}

async function request(action, path = "", options = {}) {
  let response;
  try {
    response = await fetch(ENTRIES_URL + path, {
      ...options,
      signal: AbortSignal.timeout(35000),
    });
  } catch (error) {
    const message = error?.name === "TimeoutError" || error?.name === "AbortError"
      ? `${action}响应超时，请稍后刷新列表确认。`
      : `无法${action}，请确认 Loki Assistant 已启动。`;
    throw new Error(message, { cause: error });
  }
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new Error(data?.error?.message || `${action}失败（HTTP ${response.status}）。`);
  if (data?.api_version !== "1") throw new Error(`${action}返回了无法识别的结果。`);
  return data;
}

export async function captureCurrentLink(browser = chrome) {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  const url = webUrl(tab?.url);
  if (!url) throw new Error("请在 HTTP 或 HTTPS 网页上使用稍后阅读。");
  const title = tab.title?.replace(/[\u0000-\u001f\u007f-\u009f]+/gu, " ").trim() || url.hostname;
  const { entry } = await request("保存链接", "", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title, url: url.href }),
  });
  if (!entry?.id) throw new Error("保存链接返回了无法识别的结果。");
}

export async function listReadLater() {
  const { entries } = await request("查询稍后阅读");
  if (!Array.isArray(entries)) throw new Error("查询稍后阅读返回了无法识别的结果。");
  return entries;
}

export async function markReadLaterRead(id) {
  const { id: savedId } = await request("标记已读", `/${encodeURIComponent(id)}/read`, { method: "POST" });
  if (savedId !== id) throw new Error("标记已读返回了无法识别的结果。");
}

export async function deleteReadLater(id) {
  const { id: deletedId } = await request("删除稍后阅读", `/${encodeURIComponent(id)}`, { method: "DELETE" });
  if (deletedId !== id) throw new Error("删除稍后阅读返回了无法识别的结果。");
}

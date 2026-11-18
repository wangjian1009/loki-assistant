const SERVICES_CAPTURE_URL = "http://127.0.0.1:47321/emacs/capture/full";

export async function captureFull(page, timeout = 10000) {
  let response;
  try {
    response = await fetch(SERVICES_CAPTURE_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(page),
      signal: AbortSignal.timeout(timeout),
    });
  } catch (error) {
    if (error?.name === "TimeoutError" || error?.name === "AbortError") {
      throw new Error("保存响应超时，本次操作可能仍在处理中，请稍后检查。", { cause: error });
    }
    throw new Error("无法完成保存，请确认 Loki Assistant 已启动。", { cause: error });
  }

  if (!response.ok) {
    let data = null;
    try {
      data = await response.json();
    } catch {
      // 空错误响应仍使用 HTTP 状态向用户报告。
    }
    throw new Error(data?.error?.message || data?.message || `保存失败（HTTP ${response.status}）。`);
  }
  let data;
  try {
    data = await response.json();
  } catch (error) {
    throw new Error("保存返回了无法识别的结果，请稍后重试。", { cause: error });
  }
  if (response.status === 202 && data.status === "pending") {
    return "已在 Emacs 打开捕获，请检查内容后使用 C-c C-c 保存。";
  }
  if (response.status === 200 && data.status === "existing") {
    return "此页面已有文档，已在 Emacs 打开并保留原文。";
  }
  throw new Error("保存返回了无法识别的结果。");
}

export { SERVICES_CAPTURE_URL };

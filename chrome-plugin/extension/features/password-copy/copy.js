export async function copyText(value) {
  if (typeof value !== "string" || !value) throw new Error("没有可复制的内容。");
  try {
    await navigator.clipboard.writeText(value);
  } catch (error) {
    throw new Error("浏览器拒绝了剪贴板写入，请在扩展弹窗保持打开时重试。", { cause: error });
  }
  return "已复制到剪贴板。";
}

export function clearAfter(text, delay = 30000) {
  const timer = setTimeout(async () => {
    try {
      if (navigator.clipboard.readText && (await navigator.clipboard.readText()) === text) {
        await navigator.clipboard.writeText("");
      }
    } catch {
      // 读取剪贴板需要额外权限；清理失败不影响本次复制。
    }
  }, delay);
  return () => clearTimeout(timer);
}

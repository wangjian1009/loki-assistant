// 该函数会注入各可访问 frame，不能引用模块变量，也不读取输入框内容。
export function hasPasswordInput() {
  const visible = (input) => {
    if (input.matches(":disabled") || input.readOnly || !input.getClientRects().length) return false;
    for (let node = input; node; node = node.parentElement || node.getRootNode()?.host) {
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse" || Number(style.opacity) === 0 || style.contentVisibility === "hidden") return false;
    }
    return [...input.getClientRects()].some((rect) => rect.width > 0 && rect.height > 0);
  };
  const inspect = (root) => {
    for (const input of root.querySelectorAll('input[type="password" i]')) {
      if (visible(input)) return true;
    }
    for (const element of root.querySelectorAll("*")) {
      if (element.shadowRoot && inspect(element.shadowRoot)) return true;
    }
    return false;
  };
  return inspect(document);
}

export async function readPageContext(browser = chrome) {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  const context = { tab: tab ? { id: tab.id, url: tab.url || "", title: tab.title || "" } : null, hasPasswordInput: false, detectionError: "" };
  if (!Number.isInteger(tab?.id) || !/^https?:\/\//iu.test(tab.url || "")) return context;
  try {
    const frames = await browser.scripting.executeScript({ target: { tabId: tab.id, allFrames: true }, func: hasPasswordInput });
    context.hasPasswordInput = frames.some((frame) => frame.result === true);
  } catch {
    context.detectionError = "无法检测当前页面的密码框，可手动切换到密码。";
  }
  return context;
}

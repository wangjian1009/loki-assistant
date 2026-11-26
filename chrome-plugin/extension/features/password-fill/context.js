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

export async function readPageContext(browser = chrome, detectionTimeoutMs = 1000) {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  const context = { tab: tab ? { id: tab.id, url: tab.url || "", title: tab.title || "" } : null, hasPasswordInput: false, detectionError: "" };
  if (!Number.isInteger(tab?.id) || !/^https?:\/\//iu.test(tab.url || "")) return context;
  let timeout;
  try {
    const detect = async () => {
      const target = { tabId: tab.id };
      const main = await browser.scripting.executeScript({ target, func: hasPasswordInput });
      if (main.some((frame) => frame.result === true)) return true;
      const frames = await browser.scripting.executeScript({ target: { ...target, allFrames: true }, func: hasPasswordInput });
      return frames.some((frame) => frame.result === true);
    };
    // frame 注入可能一直等待，超时后仍返回当前标签页供保存工具使用。
    context.hasPasswordInput = await Promise.race([
      detect(),
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error("密码框检测超时。")), detectionTimeoutMs);
      }),
    ]);
  } catch {
    context.detectionError = "无法检测当前页面的密码框，可手动切换到密码。";
  } finally {
    clearTimeout(timeout);
  }
  return context;
}

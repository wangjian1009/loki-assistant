import { CAPTURE_FULL } from "../shared/messages.js";
import { snapshotPage, convertPage } from "../features/org-capture/capture.js";

const button = document.getElementById("org-capture");
const dialog = document.getElementById("capture-dialog");
const status = document.getElementById("status");
const close = document.getElementById("dialog-close");

close.addEventListener("click", () => dialog.close());

button.addEventListener("click", () => {
  button.disabled = true;
  close.disabled = true;
  dialog.showModal();
  let phase = "转换";
  status.textContent = "正在转换当前页面…";
  readPage().then((page) => {
    phase = "保存";
    status.textContent = "正在保存当前页面…";
    return new Promise((resolve, reject) => {
    chrome.runtime.sendMessage({ type: CAPTURE_FULL, page }, (response) => {
      if (chrome.runtime.lastError) reject(new Error(chrome.runtime.lastError.message));
      else resolve(response);
    });
    });
  }).then((response) => {
    if (response?.ok) {
      status.textContent = response.status;
    } else {
      status.textContent = `捕获失败：${response?.error || "未知错误"}`;
    }
  }).catch((error) => {
    status.textContent = `${phase}失败：${error?.message || String(error)}`;
  }).finally(() => {
    button.disabled = false;
    close.disabled = false;
    window.setTimeout(() => {
      if (dialog.open) dialog.close();
    }, 2500);
  });
});

async function readPage() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!Number.isInteger(tab?.id) || !/^https?:\/\//i.test(tab.url ?? "")) {
    throw new Error("请在普通 HTTP 或 HTTPS 网页上使用插件，浏览器内部页面和 PDF 查看器不受支持。");
  }
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: snapshotPage,
  });
  return convertPage(result.result);
}

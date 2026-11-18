import { captureFull } from "../shared/services-client.js";
import { CAPTURE_FULL } from "../shared/messages.js";

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type !== CAPTURE_FULL) return undefined;
  if (!message.page || typeof message.page !== "object") {
    sendResponse({ ok: false, error: "没有收到有效的网页内容。" });
    return false;
  }
  captureFull(message.page)
    .then((status) => sendResponse({ ok: true, status }))
    .catch((error) => sendResponse({ ok: false, error: error?.message || String(error) }));
  return true;
});

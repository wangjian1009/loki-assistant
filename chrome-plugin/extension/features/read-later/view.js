import { READ_LATER_CAPTURE, READ_LATER_DELETE, READ_LATER_LIST, READ_LATER_MARK_READ } from "../../shared/messages.js";
import { webUrl } from "./api.js";

export function installReadLater({ save, saveStatus, list, listStatus, refreshButton, sendMessage }) {
  let requestNumber = 0;
  let canSave = false;
  let saving = false;

  function report(status, text, error = false) {
    status.classList.toggle("error", error);
    status.textContent = text;
  }

  async function send(type, payload) {
    const result = await sendMessage({ type, ...payload });
    if (!result?.ok) throw new Error(result?.error || "稍后阅读操作失败。");
    return result;
  }

  function row(entry) {
    const item = document.createElement("li");
    item.className = "entry";
    const url = webUrl(entry.url);
    const content = document.createElement(url ? "a" : "div");
    content.className = "entry-main read-later-link";
    const title = document.createElement("span");
    title.className = "entry-name";
    title.textContent = entry.title || url?.href || "无标题";
    content.append(title);
    if (url) {
      content.href = url.href;
      content.target = "_blank";
      content.rel = "noopener noreferrer";
      content.title = url.href;
      const address = document.createElement("span");
      address.className = "entry-path";
      address.textContent = url.href;
      content.append(address);
    }

    const actions = document.createElement("div");
    actions.className = "entry-actions";
    for (const [type, label] of [[READ_LATER_MARK_READ, "标记已读"], [READ_LATER_DELETE, "删除"]]) {
      const button = document.createElement("button");
      button.className = "text-button";
      button.type = "button";
      button.textContent = label;
      button.setAttribute("aria-label", `${label} · ${title.textContent}`);
      button.addEventListener("click", async () => {
        for (const action of actions.children) action.disabled = true;
        try {
          await send(type, { id: entry.id });
          await refresh();
        } catch (error) {
          report(listStatus, error.message, true);
          for (const action of actions.children) action.disabled = false;
        }
      });
      actions.append(button);
    }
    item.append(content, actions);
    return item;
  }

  async function refresh() {
    const request = ++requestNumber;
    refreshButton.disabled = true;
    list.setAttribute("aria-busy", "true");
    list.replaceChildren();
    report(listStatus, "正在读取稍后阅读…");
    try {
      const { entries } = await send(READ_LATER_LIST);
      if (request !== requestNumber) return;
      for (const entry of entries) {
        if (typeof entry?.id === "string" && entry.id) list.append(row(entry));
      }
      report(listStatus, list.children.length ? "" : "没有未读链接。");
    } catch (error) {
      if (request === requestNumber) report(listStatus, error.message, true);
    } finally {
      if (request === requestNumber) {
        list.setAttribute("aria-busy", "false");
        refreshButton.disabled = false;
      }
    }
  }

  save.addEventListener("click", async () => {
    saving = true;
    save.disabled = true;
    save.setAttribute("aria-busy", "true");
    report(saveStatus, "正在保存链接…");
    try {
      await send(READ_LATER_CAPTURE);
      report(saveStatus, "链接已加入稍后阅读。");
    } catch (error) {
      report(saveStatus, error.message, true);
    } finally {
      saving = false;
      save.disabled = !canSave;
      save.setAttribute("aria-busy", "false");
    }
  });
  refreshButton.addEventListener("click", refresh);
  return {
    refresh,
    setPage(url) {
      canSave = Boolean(webUrl(url));
      save.disabled = saving || !canSave;
      save.title = canSave ? "保存当前网页链接" : "请在 HTTP 或 HTTPS 网页上使用稍后阅读";
    },
  };
}

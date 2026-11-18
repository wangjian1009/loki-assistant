import TurndownService from "./html2org/turndown.ts";
import { handleSquareBracketForUri } from "./html2org/utilities.ts";

// 此函数由 Chrome 序列化后注入网页，因此所有辅助函数均定义在内部。
export function snapshotPage() {
  const omitted = new Set([
    "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "IFRAME", "OBJECT", "EMBED", "SOURCE",
  ]);
  const attributes = ["class", "rowspan", "colspan", "start", "type", "alt", "title"];

  function cloneContent(source, targetDocument, inPre = false) {
    if (source.nodeType === Node.TEXT_NODE) {
      return targetDocument.createTextNode(source.data);
    }
    if (source.nodeType !== Node.ELEMENT_NODE || omitted.has(source.tagName)) {
      return null;
    }
    const style = getComputedStyle(source);
    if (source.hidden || style.display === "none" || /^(hidden|collapse)$/.test(style.visibility)) {
      return null;
    }
    if (source.tagName === "BR" && inPre) {
      return targetDocument.createTextNode("\n");
    }

    // 不复制脚本事件和资源加载属性，转换过程中不插入网页、不请求资源。
    const copy = targetDocument.createElement(source.localName);
    for (const attribute of attributes) {
      if (source.hasAttribute(attribute)) {
        copy.setAttribute(attribute, source.getAttribute(attribute));
      }
    }
    if (source.tagName === "INPUT" && source.checked) copy.setAttribute("checked", "");
    if (source.tagName === "A" && source.hasAttribute("href")) {
      try {
        const url = new URL(source.getAttribute("href"), source.baseURI);
        if (/^(https?:|mailto:|tel:)$/.test(url.protocol)) copy.setAttribute("href", url.href);
      } catch { /* 无效链接保留文本。 */ }
    }
    if (source.tagName === "IMG") {
      const value = source.currentSrc || source.getAttribute("src") || "";
      try {
        const url = new URL(value, source.baseURI);
        if (value && /^https?:$/.test(url.protocol)) {
          copy.setAttribute("data-org-src", url.href);
          return copy;
        }
      } catch { /* 无法保存的图片保留替代文字。 */ }
      return targetDocument.createTextNode(source.getAttribute("alt") || "");
    }
    for (const child of source.childNodes) {
      const clone = cloneContent(child, targetDocument, inPre || source.tagName === "PRE");
      if (clone) copy.appendChild(clone);
    }
    return copy;
  }

  if (!document.body || document.contentType !== "text/html") {
    throw new Error("当前文档不是支持的 HTML 网页。");
  }
  const inertDocument = document.implementation.createHTMLDocument("");
  const root = cloneContent(document.body, inertDocument);
  if (!root) throw new Error("页面没有可转换的内容。");
  return {
    title: document.title.replace(/[\r\n]+/g, " ").trim() || "未命名网页",
    url: location.href,
    capturedAt: new Date().toISOString(),
    html: root.innerHTML,
  };
}

export function convertPage(snapshot) {
  if (!snapshot || typeof snapshot.html !== "string") {
    throw new Error("未能读取页面内容，请刷新网页后重试。");
  }
  const converter = new TurndownService({ decodeUri: false });
  converter.addRule("remoteImage", {
    filter: "img",
    replacement: (_content, node) => `[[${handleSquareBracketForUri(node.getAttribute("data-org-src"))}]]`,
  });
  converter.addRule("linkedRemoteImage", {
    filter: (node) => node.nodeName === "A" && node.hasAttribute("href") &&
      node.childNodes.length === 1 && node.firstChild.nodeName === "IMG",
    replacement: (_content, node) => {
      const href = handleSquareBracketForUri(node.getAttribute("href"));
      const src = handleSquareBracketForUri(node.firstChild.getAttribute("data-org-src"));
      return href === src ? `[[${href}]]` : `[[${href}][${src}]]`;
    },
  });
  const body = converter.turndown(snapshot.html);
  if (!body.trim()) throw new Error("页面没有可转换的内容。");
  return { title: snapshot.title, url: snapshot.url, captured_at: snapshot.capturedAt, body };
}

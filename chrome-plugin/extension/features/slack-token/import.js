export function slackTokenImport(tabUrl) {
  let url;
  try { url = new URL(tabUrl); } catch { return null; }
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" ||
      url.port !== "47321" || url.pathname !== "/slack/token-import" || url.username || url.password) return null;
  const teamId = url.searchParams.get("team");
  const attempt = url.hash.slice(1);
  if (!/^T[A-Z0-9]{1,47}$/u.test(teamId ?? "") ||
      !/^[\da-f]{8}-[\da-f]{4}-4[\da-f]{3}-[89ab][\da-f]{3}-[\da-f]{12}$/u.test(attempt)) return null;
  return { endpoint: url.origin + url.pathname, teamId, attempt };
}

export async function acceptSlackToken(target, token, request = globalThis.fetch) {
  let response;
  try {
    response = await request(target.endpoint, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "Authorization": "Bearer " + target.attempt,
      },
      body: JSON.stringify({ team_id: target.teamId, token }),
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
      signal: AbortSignal.timeout(90000),
    });
  } catch (error) {
    throw new Error("未收到本机 services 的导入结果，请查看原保存请求。", { cause: error });
  }
  if (response.status === 204) return;
  const body = await response.json().catch(() => null);
  const error = body?.error?.message || `无法导入 Slack 令牌（HTTP ${response.status}）。`;
  throw new Error(`${error} 请重新提交工作区配置后再导入。`);
}

export function showSlackTokenImport(form, target) {
  if (!target) return;
  form.hidden = false;
  const input = form.querySelector("input");
  const button = form.querySelector("button");
  const status = form.querySelector("[role=status]");
  form.querySelector("[data-slack-team]").textContent = target.teamId;
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const token = input.value.trim();
    if (!token || button.disabled) return;
    button.disabled = true;
    input.value = "";
    status.classList.remove("error");
    status.textContent = "正在校验并保存…";
    try {
      await acceptSlackToken(target, token);
      status.textContent = "用户令牌已保存。";
      input.disabled = true;
    } catch (error) {
      status.classList.add("error");
      status.textContent = error.message;
      button.disabled = false;
    }
  });
}

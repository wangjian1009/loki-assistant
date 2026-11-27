import { NavigatorManager, NavigationResult, parseNavigationPath } from './NavigatorManager'

type HttpResponder = (status: number, body: string) => void

interface NavigationErrorBody {
  error: NavigationErrorDetail
}

interface NavigationErrorDetail {
  code: string
  message: string
}

function navigationError(respond: HttpResponder, status: number, code: string, message: string): void {
  const body: NavigationErrorBody = { error: { code, message } }
  respond(status, JSON.stringify(body))
}

export async function handleNavigation(navigator: NavigatorManager, method: string, body: string,
  respond: HttpResponder): Promise<void> {
  let result = navigator.status()
  if (result === NavigationResult.Ok && method === 'PUT') {
    const path = parseNavigationPath(body)
    if (!path) {
      navigationError(respond, 400, 'invalidInput', '页面路径无效')
      return
    }
    result = await navigator.setPath(path)
    if (result === NavigationResult.Ok) {
      respond(200, JSON.stringify(path))
      return
    }
  }
  switch (result) {
    case NavigationResult.Ok:
      respond(200, JSON.stringify(navigator.snapshot()))
      break
    case NavigationResult.Unavailable:
      navigationError(respond, 503, 'unavailable', '主窗口尚未就绪')
      break
    case NavigationResult.Busy:
      navigationError(respond, 409, 'invalidState', '正在切换页面或编辑手机名称')
      break
    case NavigationResult.Failed:
      navigationError(respond, 500, 'internalError', '切换页面失败')
      break
  }
}

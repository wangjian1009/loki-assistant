import { Workspace } from 'liblokiworkspace.so'
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

export function registerNavigationRoute(workspace: Workspace, navigator: NavigatorManager): void {
  workspace.registerHttpRoute('/api/v1/ui/navigation', ['GET', 'PUT'],
    (id: string, method: string, body: string): void => {
      const respond = (status: number, response: string): void => { workspace.respondHttp(id, status, response) }
      handleNavigation(navigator, method, body, respond).catch((error: Error): void => {
        console.error('处理 HTTP 导航失败：' + error.message)
        navigationError(respond, 500, 'internalError', '处理导航请求失败')
      })
    })
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

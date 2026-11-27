import { Want } from '@kit.AbilityKit'
import { Router, window } from '@kit.ArkUI'

export enum PageName {
  Index = 'Index',
  Devices = 'Devices',
  Settings = 'Settings'
}

interface PageRoute {
  url: PageName
}

export interface NavigationPath {
  pages: PageRoute[]
}

export enum NavigationResult {
  Ok,
  Unavailable,
  Busy,
  Failed
}

function pageName(value: Object | undefined): PageName | undefined {
  if (value === PageName.Index) return PageName.Index
  if (value === PageName.Devices) return PageName.Devices
  if (value === PageName.Settings) return PageName.Settings
  return undefined
}

function arkUIPath(page: PageName): string {
  return 'pages/' + page
}

export function launchPath(want: Want, debug: boolean): NavigationPath | undefined {
  const parameters = want.parameters
  const destination = debug && parameters?.httpApiEnabled === true && parameters.httpApiPage !== undefined
    ? parameters.httpApiPage : parameters?.page
  if (typeof destination !== 'string') return undefined
  const pages: PageRoute[] = []
  for (const value of destination.split(',')) {
    const name = pageName(value)
    if (!name) return undefined
    pages.push({ url: name })
  }
  return { pages }
}

export function parseNavigationPath(body: string): NavigationPath | undefined {
  try {
    const path = JSON.parse(body) as NavigationPath
    if (!path || Object.keys(path).length !== 1 || !Array.isArray(path.pages) ||
      path.pages.length === 0) return undefined
    for (const page of path.pages) {
      if (!page || Object.keys(page).length !== 1 || !pageName(page.url)) return undefined
    }
    return path
  } catch (_) {
    return undefined
  }
}

// 页面路径保存全部返回目标，ArkUI 栈只保存已经通过应用内跳转打开的后缀。
export class NavigatorManager {
  private stage?: window.WindowStage
  private router?: Router
  private path: NavigationPath = { pages: [{ url: PageName.Index }] }
  private pendingPath?: NavigationPath
  private nativeDepth: number = 1
  private navigating: boolean = false
  private leaveGuard?: () => boolean

  async attach(stage: window.WindowStage): Promise<void> {
    this.stage = stage
    const path = this.pendingPath ?? this.path
    this.pendingPath = undefined
    try {
      await stage.loadContent(arkUIPath(this.currentPage(path)))
      if (this.stage !== stage) return
      this.path = path
      this.nativeDepth = 1
      this.router = stage.getMainWindowSync().getUIContext().getRouter()
      this.openPendingPath()
    } catch (error) {
      console.error(`加载首屏失败：${(error as Error).message}`)
    }
  }

  detach(): void {
    this.stage = undefined
    this.router = undefined
    this.pendingPath = undefined
    this.nativeDepth = 1
    this.leaveGuard = undefined
  }

  openFromWant(path: NavigationPath): void {
    this.pendingPath = path
    this.openPendingPath()
  }

  private openPendingPath(): void {
    const path = this.pendingPath
    if (!path || !this.router || this.navigating) return
    this.pendingPath = undefined
    this.setPath(path).then((result: NavigationResult): void => {
      if (result === NavigationResult.Failed) console.error('打开指定页面失败')
    })
  }

  watchLeaveGuard(guard: () => boolean): () => void {
    this.leaveGuard = guard
    return (): void => {
      if (this.leaveGuard === guard) this.leaveGuard = undefined
    }
  }

  status(): NavigationResult {
    if (!this.router) return NavigationResult.Unavailable
    return this.navigating ? NavigationResult.Busy : NavigationResult.Ok
  }

  snapshot(): NavigationPath {
    return this.path
  }

  openPage(page: PageName): Promise<NavigationResult> {
    return this.transition(async (router: Router): Promise<void> => {
      if (page === this.currentPage(this.path)) return
      await router.pushUrl({ url: arkUIPath(page) })
      if (this.router !== router) return
      this.path = { pages: this.path.pages.concat([{ url: page }]) }
      this.nativeDepth++
    })
  }

  openSection(page: PageName): Promise<NavigationResult> {
    const pages: PageRoute[] = [{ url: PageName.Index }]
    if (page !== PageName.Index) pages.push({ url: page })
    return this.setPath({ pages })
  }

  canGoBack(): boolean {
    return this.navigating || this.path.pages.length > 1 || this.currentPage(this.path) !== PageName.Index
  }

  back(): Promise<NavigationResult> {
    return this.transition(async (router: Router): Promise<void> => {
      const current = this.currentPage(this.path)
      const hasParent = this.path.pages.length > 1
      if (!hasParent && current === PageName.Index) return
      const path: NavigationPath = { pages: hasParent ? this.path.pages.slice(0, -1) : [{ url: PageName.Index }] }
      const parent = this.currentPage(path)
      const nativeParent = this.nativeDepth > 1
      if (nativeParent) router.back()
      else if (parent !== current) await router.replaceUrl({ url: arkUIPath(parent) })
      if (this.router !== router) return
      this.path = path
      if (nativeParent) this.nativeDepth--
    })
  }

  setPath(path: NavigationPath): Promise<NavigationResult> {
    return this.transition(async (router: Router): Promise<void> => {
      if (path.pages.length === this.path.pages.length &&
        path.pages.every((page: PageRoute, index: number): boolean => page.url === this.path.pages[index].url)) return
      const target = this.currentPage(path)
      if (target !== this.currentPage(this.path)) await router.replaceUrl({ url: arkUIPath(target) })
      if (this.router !== router) return
      if (this.nativeDepth > 1) router.clear()
      this.path = path
      this.nativeDepth = 1
    }, true)
  }

  private currentPage(path: NavigationPath): PageName {
    return path.pages[path.pages.length - 1].url
  }

  private async transition(action: (router: Router) => Promise<void>, external: boolean = false): Promise<NavigationResult> {
    const router = this.router
    if (!router) return NavigationResult.Unavailable
    if (this.navigating || (external && this.leaveGuard?.() === false)) return NavigationResult.Busy
    this.navigating = true
    try {
      await action(router)
      return this.router === router ? NavigationResult.Ok : NavigationResult.Unavailable
    } catch (error) {
      console.error(`切换页面失败：${(error as Error).message}`)
      return NavigationResult.Failed
    } finally {
      this.navigating = false
      this.openPendingPath()
    }
  }
}

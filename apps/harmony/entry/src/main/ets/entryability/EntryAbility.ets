import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit'
import { deviceInfo } from '@kit.BasicServicesKit'
import { window } from '@kit.ArkUI'
import { HttpResponder } from 'liblokiworkspace.so'
import { Application } from '../app/Application'
import { handleNavigation } from '../app/HttpNavigation'
import { NavigatorManager, launchPath } from '../app/NavigatorManager'
import { Workspace } from '../workspace/Workspace'
import { DEBUG } from 'BuildProfile'

export default class EntryAbility extends UIAbility {
  private application?: Application
  private readonly navigator: NavigatorManager = new NavigatorManager()
  private httpEnabled: boolean = false
  private httpOperation: Promise<void> = Promise.resolve()

  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    this.handleWant(want)
  }

  onNewWant(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    this.handleWant(want)
  }

  private handleWant(want: Want): void {
    const path = launchPath(want, DEBUG)
    if (path) this.navigator.openFromWant(path)
    const enabled = want.parameters?.httpApiEnabled
    if (DEBUG && typeof enabled === 'boolean') {
      this.httpEnabled = enabled
      this.updateHttpServer()
    }
  }

  private updateHttpServer(): void {
    const application = this.application
    if (!application) return
    const navigator = this.navigator
    this.httpOperation = this.httpOperation.then(async (): Promise<void> => {
      if (this.application !== application) return
      if (this.httpEnabled) {
        await application.prepare()
        if (this.application !== application || !this.httpEnabled) return
        await application.workspace.startHttpServer((method: string, body: string, respond: HttpResponder): void => {
          handleNavigation(navigator, method, body, respond).catch((error: Error): void => {
            console.error('处理 HTTP 导航失败：' + error.message)
          })
        })
      } else {
        await application.workspace.stopHttpServer()
      }
    }).catch((error: Error): void => {
      console.error('切换调试 HTTP 服务失败：' + error.message)
    })
  }

  onDestroy(): void {
    this.navigator.detach()
    const application = this.application
    this.application = undefined
    application?.stop().catch(() => {})
  }

  onForeground(): void {
    AppStorage.setOrCreate('lokiForeground', true)
  }

  onBackground(): void {
    AppStorage.setOrCreate('lokiForeground', false)
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    if (!this.application) {
      this.application = new Application(new Workspace(this.context.filesDir), deviceInfo.marketName || 'HarmonyOS 手机')
    }
    AppStorage.setOrCreate('lokiApplication', this.application)
    AppStorage.setOrCreate('lokiNavigator', this.navigator)
    this.navigator.attach(windowStage)
    if (this.httpEnabled) this.updateHttpServer()
  }

  onWindowStageDestroy(): void {
    this.navigator.detach()
  }
}

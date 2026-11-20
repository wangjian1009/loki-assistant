import { UIAbility } from '@kit.AbilityKit'
import { deviceInfo } from '@kit.BasicServicesKit'
import { window } from '@kit.ArkUI'
import { Application } from '../app/Application'
import { Workspace } from '../workspace/Workspace'

export default class EntryAbility extends UIAbility {
  private application?: Application

  onDestroy(): void {
    this.application?.stop().catch(() => {})
  }

  onForeground(): void {
    AppStorage.setOrCreate('lokiForeground', true)
  }

  onBackground(): void {
    AppStorage.setOrCreate('lokiForeground', false)
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    this.application = new Application(new Workspace(this.context.filesDir), deviceInfo.marketName || 'HarmonyOS 手机')
    AppStorage.setOrCreate('lokiApplication', this.application)
    windowStage.loadContent('pages/Index', (error) => {
      if (error.code !== 0) {
        console.error(`加载主页面失败: ${JSON.stringify(error)}`)
      }
    })
  }
}

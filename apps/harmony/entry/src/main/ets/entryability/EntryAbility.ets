import { UIAbility } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { Workspace } from '../workspace/Workspace'

export default class EntryAbility extends UIAbility {
  private workspace?: Workspace

  onDestroy(): void {
    this.workspace?.stop().catch(() => {})
  }

  onForeground(): void {
    AppStorage.setOrCreate('lokiForeground', true)
  }

  onBackground(): void {
    AppStorage.setOrCreate('lokiForeground', false)
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    this.workspace = new Workspace(this.context.filesDir)
    AppStorage.setOrCreate('lokiWorkspace', this.workspace)
    windowStage.loadContent('pages/Index', (error) => {
      if (error.code !== 0) {
        console.error(`加载主页面失败: ${JSON.stringify(error)}`)
      }
    })
  }
}

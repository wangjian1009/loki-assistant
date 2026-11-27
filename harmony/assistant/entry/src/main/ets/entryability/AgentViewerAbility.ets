import { UIAbility } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { setSystemColors } from '../components/AppPage'
import { sharedApplication } from '../app/SharedApplication'

export default class AgentViewerAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    AppStorage.setOrCreate('lokiApplication', sharedApplication(this.context.filesDir))
    windowStage.loadContent('pages/AgentViewer').then(() => {
      setSystemColors(this.context)
    }).catch((error: Error) => {
      console.error('打开AI 远程对话页面失败：' + error.message)
    })
  }

  onForeground(): void {
    AppStorage.setOrCreate('lokiAgentViewerForeground', true)
  }

  onBackground(): void {
    AppStorage.setOrCreate('lokiAgentViewerForeground', false)
  }
}

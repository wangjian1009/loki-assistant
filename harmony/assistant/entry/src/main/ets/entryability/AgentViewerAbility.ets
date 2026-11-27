import { UIAbility, Want } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { setSystemColors } from '../components/AppPage'
import { sharedApplication } from '../app/SharedApplication'

export default class AgentViewerAbility extends UIAbility {
  onCreate(want: Want): void { this.selectFromWant(want) }

  onNewWant(want: Want): void { this.selectFromWant(want) }

  private selectFromWant(want: Want): void {
    const sessionId = want.parameters?.sessionId
    if (typeof sessionId === 'string' && sessionId) {
      AppStorage.setOrCreate('lokiAgentSessionRequest', JSON.stringify([sessionId, Date.now()]))
    }
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    AppStorage.setOrCreate('lokiAgentImageCache', this.context.cacheDir)
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

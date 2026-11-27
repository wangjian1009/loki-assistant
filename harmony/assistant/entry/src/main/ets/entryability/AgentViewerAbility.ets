import { UIAbility } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { setSystemColors } from '../components/AppPage'

export default class AgentViewerAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/AgentViewer').then(() => {
      setSystemColors(this.context)
    }).catch((error: Error) => {
      console.error('打开AI 远程对话页面失败：' + error.message)
    })
  }
}

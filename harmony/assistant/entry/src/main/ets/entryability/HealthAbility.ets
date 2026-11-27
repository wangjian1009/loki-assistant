import { UIAbility } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { setSystemColors } from '../components/AppPage'

export default class HealthAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Health').then(() => {
      setSystemColors(this.context)
    }).catch((error: Error) => {
      console.error('打开个人健康页面失败：' + error.message)
    })
  }
}

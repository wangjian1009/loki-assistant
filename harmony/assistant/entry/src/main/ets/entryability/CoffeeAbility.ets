import { UIAbility } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { setSystemColors } from '../components/AppPage'

export default class CoffeeAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Coffee').then(() => {
      setSystemColors(this.context)
    }).catch((error: Error) => {
      console.error('打开咖啡管理页面失败：' + error.message)
    })
  }
}

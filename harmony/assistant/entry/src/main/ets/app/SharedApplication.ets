import { deviceInfo } from '@kit.BasicServicesKit'
import { Application } from './Application'
import { Workspace } from '../workspace/Workspace'

let application: Application | undefined

// 两个 Ability 共用同一设备配置和网络。Workspace 随应用进程存活。
export function sharedApplication(filesDir: string): Application {
  if (!application) {
    application = new Application(new Workspace(filesDir), deviceInfo.marketName || 'HarmonyOS 手机')
  }
  return application
}

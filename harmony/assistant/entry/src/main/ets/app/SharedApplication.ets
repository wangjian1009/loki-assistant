import { deviceInfo } from '@kit.BasicServicesKit'
import { connection } from '@kit.NetworkKit'
import { Application } from './Application'
import { Workspace } from '../workspace/Workspace'

let application: Application | undefined
let networkObserver: connection.NetConnection | undefined

function watchLocalNetwork(workspace: Workspace): void {
  const publish = (): void => {
    workspace.open().then(native => {
      native.setLocalNetworkAvailable(connection.hasDefaultNetSync())
    }).catch(() => {})
  }
  try {
    networkObserver = connection.createNetConnection()
    networkObserver.on('netAvailable', publish)
    networkObserver.on('netLost', publish)
    networkObserver.on('netCapabilitiesChange', publish)
    networkObserver.on('netUnavailable', publish)
    networkObserver.register(() => { publish() })
  } catch (_) {
    // 注册失败时仍读取当前网络状态。
  }
  publish()
}

// 两个 Ability 共用同一设备配置和网络。Workspace 随应用进程存活。
export function sharedApplication(filesDir: string): Application {
  if (!application) {
    const workspace = new Workspace(filesDir)
    application = new Application(workspace, deviceInfo.marketName || 'HarmonyOS 手机')
    watchLocalNetwork(workspace)
  }
  return application
}

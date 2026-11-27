import { DeviceRefreshEvent, StatusSubscription } from 'liblokiworkspace.so'
import { Workspace } from '../workspace/Workspace'
import { ACCENT, BODY_SIZE, CONTENT_GAP, ERROR_TEXT, SECONDARY_TEXT } from './AppPage'

@Component
export struct DeviceRefresh {
  workspace: () => Workspace | undefined = () => undefined
  @Prop @Watch('activeChanged') active: boolean = false
  @Prop @Watch('reset') endpointId: string = ''
  @State private phase: 'idle' | 'running' | 'success' | 'failure' = 'idle'
  @State private message: string = ''
  private timer: number = -1
  private version: number = 0
  private subscription?: StatusSubscription

  aboutToAppear(): void { this.activeChanged() }

  aboutToDisappear(): void { this.stop() }

  private activeChanged(): void {
    this.stop()
    if (this.active) { this.listen() }
  }

  private stop(): void {
    this.version += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.reset()
  }

  private reset(): void {
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
    this.phase = 'idle'
    this.message = ''
  }

  private async listen(): Promise<void> {
    const workspace = this.workspace()
    if (!workspace) { return }
    const version = this.version
    try {
      const native = await workspace.open()
      if (version !== this.version) { return }
      this.subscription = native.watchRefresh((event: DeviceRefreshEvent) => {
        if (version === this.version && event.endpoint_id === this.endpointId) {
          this.update(event)
        }
      })
    } catch (_) {
      console.error('无法订阅设备刷新事件')
    }
  }

  private update(event: DeviceRefreshEvent): void {
    this.reset()
    switch (event.phase) {
      case 'registering':
        this.phase = 'running'
        this.message = '设备注册中…'
        break
      case 'reconnecting':
        this.phase = 'running'
        this.message = '正在重连电脑…'
        break
      case 'querying':
        this.phase = 'running'
        this.message = '正在同步设备状态…'
        break
      case 'succeeded':
        this.phase = 'success'
        this.timer = setTimeout(() => { this.reset() }, 1000)
        break
      case 'failed':
        this.phase = 'failure'
        this.message = event.error.message
        this.timer = setTimeout(() => { this.reset() }, 2000)
        break
    }
  }

  build() {
    if (this.phase !== 'idle') {
      Row({ space: CONTENT_GAP }) {
        if (this.phase === 'running') {
          LoadingProgress().width(14).height(14).color(ACCENT).accessibilityLevel('no')
          Text(this.message).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
        } else {
          Text(this.phase === 'success' ? '设备同步成功' : '设备同步失败')
            .fontSize(BODY_SIZE).fontColor(this.phase === 'success' ? '#9AE2B9' : ERROR_TEXT)
            .accessibilityDescription(this.phase === 'failure' ? this.message : '')
        }
      }.width('100%').constraintSize({ minHeight: 28 })
    }
  }
}

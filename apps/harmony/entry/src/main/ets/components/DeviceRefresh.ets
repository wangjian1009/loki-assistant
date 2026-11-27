import { DeviceRefreshStep } from 'liblokiworkspace.so'
import { Workspace } from '../workspace/Workspace'
import { ACCENT, BODY_SIZE, CONTENT_GAP, ERROR_TEXT, SECONDARY_TEXT } from './AppPage'

@Component
export struct DeviceRefresh {
  workspace: () => Workspace | undefined = () => undefined
  @Prop @Watch('targetChanged') endpointId: string = ''
  @Prop @Watch('requestRefresh') refreshRequest: number = 0
  @State private phase: 'idle' | 'running' | 'success' | 'failure' = 'idle'
  @State private message: string = ''
  private timer: number = -1
  private version: number = 0

  aboutToDisappear(): void { this.reset() }

  private targetChanged(): void {
    this.reset()
  }

  private requestRefresh(): void {
    this.sync()
  }

  private reset(): void {
    this.version += 1
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
    this.phase = 'idle'
    this.message = ''
  }

  private async sync(): Promise<void> {
    const workspace = this.workspace()
    if (!this.endpointId || !workspace || this.phase !== 'idle') { return }
    const version = ++this.version
    this.phase = 'running'
    this.message = '正在连接电脑…'
    const endpoint = this.endpointId
    try {
      const native = await workspace.open()
      if (version !== this.version) { return }
      const result = await native.refreshDevice(endpoint, (step: DeviceRefreshStep) => {
        if (version !== this.version || this.phase !== 'running') { return }
        this.message = step === 'registering' ? '设备注册中…' : '状态查询中…'
      })
      if (version !== this.version) { return }
      this.phase = result.error ? 'failure' : 'success'
      this.message = result.error?.message ?? ''
    } catch (error) {
      if (version !== this.version) { return }
      this.phase = 'failure'
      this.message = (error as Error).message || '无法同步设备状态'
    }
    this.timer = setTimeout(() => { this.reset() }, this.phase === 'success' ? 1000 : 2000)
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

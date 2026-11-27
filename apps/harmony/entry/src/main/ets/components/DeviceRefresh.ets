import { DeviceRefreshStep } from 'liblokiworkspace.so'
import { Workspace } from '../workspace/Workspace'
import { ACCENT, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_INSET, PAGE_MAX_WIDTH,
  PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE } from './AppPage'

@Component
export struct DeviceRefresh {
  workspace: () => Workspace | undefined = () => undefined
  @Prop @Watch('targetChanged') endpointId: string = ''
  @BuilderParam content: () => void
  @State private phase: 'idle' | 'running' | 'success' | 'failure' = 'idle'
  @State private message: string = ''
  private readonly scroller: Scroller = new Scroller()
  private startedAtTop: boolean = false
  private timer: number = -1
  private version: number = 0

  aboutToDisappear(): void { this.reset() }

  private targetChanged(): void {
    this.reset()
  }

  private reset(): void {
    this.version += 1
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
    this.startedAtTop = false
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

  @Builder
  private progress() {
    Column({ space: 4 }) {
      if (this.phase === 'running') {
        Row({ space: CONTENT_GAP }) {
          LoadingProgress().width(16).height(16).color(ACCENT).accessibilityLevel('no')
          Text(this.message).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
        }
      } else {
        Text(this.phase === 'success' ? '设备同步成功' : '设备同步失败')
          .fontSize(BODY_SIZE).fontWeight(FontWeight.Medium)
          .fontColor(this.phase === 'success' ? '#9AE2B9' : ERROR_TEXT)
        if (this.phase === 'failure') {
          Text(this.message).width('100%').fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
            .textAlign(TextAlign.Center).maxLines(3).textOverflow({ overflow: TextOverflow.Ellipsis })
        }
      }
    }.width('100%').constraintSize({ minHeight: 48 }).padding(CONTENT_GAP)
      .justifyContent(FlexAlign.Center).backgroundColor(SURFACE)
      .borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }

  build() {
    Column() {
      if (this.phase !== 'idle') {
        Column() {
          this.progress()
        }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, top: CONTENT_GAP })
      }
      Scroll(this.scroller) {
        this.content()
      }.width('100%').layoutWeight(1).align(Alignment.Top).edgeEffect(EdgeEffect.None)
        .scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)
        .parallelGesture(
          PanGesture({ direction: PanDirection.Down })
            .onActionStart(() => {
              this.startedAtTop = this.scroller.currentOffset().yOffset <= 0
            })
            .onActionEnd((event: GestureEvent) => {
              if (this.startedAtTop && event.offsetY >= 72) { this.sync() }
              this.startedAtTop = false
            })
            .onActionCancel(() => { this.startedAtTop = false })
        )
    }.width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH }).layoutWeight(1).align(Alignment.Top)
  }
}

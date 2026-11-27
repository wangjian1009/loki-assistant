import { accessibility } from '@kit.AccessibilityKit'
import { preferences } from '@kit.ArkData'
import { CurrentDeviceWidget, currentDeviceOrder, WIDGET_TITLES } from '../app/CurrentDeviceLayout'
import { CurrentDeviceState } from '../app/CurrentDeviceState'
import { MailCard } from '../features/mail/MailCard'
import { SlackCard } from '../features/slack/SlackCard'
import { ACCENT, BODY_SIZE, CONTENT_GAP, ERROR_TEXT, PAGE_INSET, PAGE_MAX_WIDTH,
  PANEL_RADIUS, PRIMARY_TEXT, SURFACE, TOUCH_SIZE } from './AppPage'
import { DeviceRefresh } from './DeviceRefresh'
import { DeviceStatus } from './DeviceStatus'

const ORDER_KEY: string = 'widget_order'

@Component
export struct CurrentDeviceWidgets {
  @ObjectLink device: CurrentDeviceState
  @Link @Watch('onAdjustingOrderChanged') adjustingOrder: boolean
  @State private order: CurrentDeviceWidget[] = currentDeviceOrder()
  @State private orderReady: boolean = false
  @State private orderError: string = ''
  @State private editAngle: number = 0
  private readonly scroller: Scroller = new Scroller()
  private startedAtTop: boolean = false
  private animationTimer: number = -1
  private layoutPreferences?: preferences.Preferences

  aboutToAppear(): void {
    this.loadOrder()
  }

  aboutToDisappear(): void {
    clearTimeout(this.animationTimer)
  }

  private onAdjustingOrderChanged(): void {
    clearTimeout(this.animationTimer)
    this.editAngle = 0
    if (!this.adjustingOrder || accessibility.isAnimationReduceEnabledSync()) { return }
    // 等待可拖动的列表项挂载，再播放进入排序模式的摆动。
    this.animationTimer = setTimeout(() => {
      this.getUIContext().keyframeAnimateTo({ iterations: 3 }, [
        { duration: 110, event: () => { this.editAngle = -0.55 } },
        { duration: 170, event: () => { this.editAngle = 0.55 } },
        { duration: 110, event: () => { this.editAngle = 0 } }
      ])
    }, 60)
  }

  private loadOrder(): void {
    try {
      const context = this.getUIContext().getHostContext()
      if (!context) { throw new Error('无法读取当前页面') }
      const store = preferences.getPreferencesSync(context, { name: 'current_device_layout' })
      const saved = store.getSync(ORDER_KEY, '')
      this.order = currentDeviceOrder(typeof saved === 'string' ? saved : '')
      this.layoutPreferences = store
      this.orderReady = true
      this.orderError = ''
    } catch (_) {
      this.orderError = '无法读取控件顺序'
    }
  }

  private moveWidget(from: number, to: number): void {
    const store = this.layoutPreferences
    if (!store || from === to || from < 0 || to < 0 || from >= this.order.length || to >= this.order.length) {
      return
    }
    const next = this.order.slice()
    const moved = next.splice(from, 1)
    next.splice(to, 0, moved[0])
    try {
      store.putSync(ORDER_KEY, next.join(','))
      store.flushSync()
      this.order = next
      this.orderError = ''
    } catch (_) {
      try { store.putSync(ORDER_KEY, this.order.join(',')) }
      catch (_) { console.error('无法恢复控件顺序的缓存') }
      this.order = this.order.slice()
      this.orderError = '无法保存控件顺序，请重试'
    }
  }

  @Builder
  private widget(widget: CurrentDeviceWidget) {
    if (widget === CurrentDeviceWidget.Device) {
      DeviceStatus({ device: this.device })
    } else if (this.device.hasStatus && this.device.current?.status === 'online' && this.device.workspace) {
      if (widget === CurrentDeviceWidget.Mail) {
        MailCard({ workspace: this.device.workspace })
      } else if (widget === CurrentDeviceWidget.Slack) {
        SlackCard({ workspace: this.device.workspace })
      }
    } else {
      Text(WIDGET_TITLES[widget]).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).width('100%')
        .constraintSize({ minHeight: 48 }).padding({ left: PAGE_INSET, right: PAGE_INSET })
        .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)
    }
  }

  private visibleOrder(): CurrentDeviceWidget[] {
    if (this.device.hasStatus && this.device.current?.status === 'online') { return this.order }
    return this.order.filter((widget: CurrentDeviceWidget) => widget === CurrentDeviceWidget.Device)
  }

  build() {
    Column() {
      DeviceRefresh({
        workspace: () => this.device.workspace,
        active: this.device.active,
        endpointId: this.device.current?.endpoint_id ?? ''
      })
      List({ scroller: this.scroller, space: CONTENT_GAP }) {
        if (this.adjustingOrder && this.orderError) {
          ListItem() {
            Column({ space: CONTENT_GAP }) {
              Text(this.orderError).fontSize(BODY_SIZE).fontColor(ERROR_TEXT).width('100%')
              if (!this.orderReady) {
                Button('重新读取', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ACCENT)
                  .constraintSize({ minHeight: TOUCH_SIZE }).backgroundColor(SURFACE)
                  .onClick(() => { this.loadOrder() })
              }
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
        }
        if (this.adjustingOrder && this.orderReady) {
          ForEach(this.order, (widget: CurrentDeviceWidget) => {
            ListItem() {
              Stack() {
                Column() {
                  this.widget(widget)
                }.width('100%').enabled(false)
              }.width('100%').rotate({ z: 1, angle: this.editAngle })
                .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)
                .shadow({ radius: 12, offsetY: 5, color: '#D9000000' })
                .accessibilityDescription('长按拖动调整位置')
            }
          }, (widget: CurrentDeviceWidget) => widget)
            .onMove((from: number, to: number) => { this.moveWidget(from, to) })
        } else {
          ForEach(this.visibleOrder(), (widget: CurrentDeviceWidget) => {
            ListItem() {
              this.widget(widget)
            }
          }, (widget: CurrentDeviceWidget) => widget)
        }
      }.width('100%').layoutWeight(1)
        .padding({ top: CONTENT_GAP, bottom: PAGE_INSET })
        .edgeEffect(EdgeEffect.None).scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)
        .parallelGesture(
          PanGesture({ direction: PanDirection.Down })
            .onActionStart(() => {
              this.startedAtTop = !this.adjustingOrder && this.scroller.currentOffset().yOffset <= 0
            })
            .onActionEnd((event: GestureEvent) => {
              if (this.startedAtTop && !this.adjustingOrder && event.offsetY >= 72) { this.device.refresh() }
              this.startedAtTop = false
            })
            .onActionCancel(() => { this.startedAtTop = false })
        )
    }.width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH }).layoutWeight(1)
      .padding({ left: PAGE_INSET, right: PAGE_INSET })
  }
}

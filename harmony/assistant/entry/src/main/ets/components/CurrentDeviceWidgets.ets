import { ItemLayout, moveVisibleItem } from '../app/CurrentDeviceLayout'
import { CurrentDevicePanel, CurrentDevicePanelRegistry } from '../app/CurrentDevicePanel'
import { CurrentDeviceState } from '../app/CurrentDeviceState'
import { DeviceLayoutStore } from '../app/DeviceLayoutStore'
import { ACCENT, BODY_SIZE, CONTENT_GAP, ERROR_TEXT, PAGE_INSET, PAGE_MAX_WIDTH,
  PANEL_RADIUS, PRIMARY_TEXT, SURFACE, TOUCH_SIZE } from './AppPage'

@Component
export struct CurrentDeviceWidgets {
  @ObjectLink device: CurrentDeviceState
  @Link editing: boolean
  registry: CurrentDevicePanelRegistry = new CurrentDevicePanelRegistry()
  @State private layout: ItemLayout = { order: [], hidden: [] }
  @State private orderReady: boolean = false
  @State private orderError: string = ''
  private layoutStore?: DeviceLayoutStore

  aboutToAppear(): void {
    this.layout = { order: this.registry.ids(), hidden: [] }
    this.loadOrder()
  }

  private loadOrder(): void {
    try {
      const context = this.getUIContext().getHostContext()
      if (!context) { throw new Error('无法读取当前页面') }
      const store = new DeviceLayoutStore(context, 'panel_layout')
      this.layout = store.load(this.registry.ids())
      this.layoutStore = store
      this.orderReady = true
      this.orderError = ''
    } catch (_) {
      this.orderError = '无法读取控件顺序'
    }
  }

  private moveWidget(from: number, to: number): void {
    const visible = this.visibleOrder()
    const order = moveVisibleItem(this.layout.order, this.layout.hidden, visible[from], visible[to])
    if (order !== this.layout.order) this.saveLayout({ order, hidden: this.layout.hidden })
  }

  private hideWidget(id: string): void {
    if (this.layout.hidden.includes(id)) return
    this.saveLayout({ order: this.layout.order, hidden: this.layout.hidden.concat(id) })
  }

  private restoreWidget(id: string): void {
    this.saveLayout({ order: this.layout.order, hidden: this.layout.hidden.filter((hiddenId: string) => hiddenId !== id) })
  }

  private saveLayout(next: ItemLayout): void {
    const store = this.layoutStore
    if (!store) return
    try {
      store.save(next)
      this.layout = next
      this.orderError = ''
    } catch (_) {
      this.layout = { order: this.layout.order.slice(), hidden: this.layout.hidden }
      this.orderError = '无法保存布局，请重试'
    }
  }

  @Builder
  private widget(panel: CurrentDevicePanel) {
    if (!panel.isAvailable || panel.isAvailable(this.device)) {
      panel.render.builder({ device: this.device, editing: this.editing && this.orderReady && panel.supportsEditing })
    } else {
      Text(panel.title).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).width('100%')
        .constraintSize({ minHeight: 48 }).padding({ left: PAGE_INSET, right: PAGE_INSET })
        .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)
    }
  }

  private visibleOrder(): string[] {
    return this.layout.order.filter((id: string) => {
      if (this.layout.hidden.includes(id)) return false
      const available = this.registry.get(id).isAvailable
      return this.editing || !available || available(this.device)
    })
  }

  @Builder
  private panelAddMenu() {
    Menu() {
      ForEach(this.layout.hidden, (id: string) => {
        MenuItem({ content: this.registry.get(id).title }).onClick(() => { this.restoreWidget(id) })
      }, (id: string) => id)
    }
  }

  @Builder
  private panelDelete(widget: string) {
    Stack() {
      Button('删除', { type: ButtonType.Normal }).fontColor(ERROR_TEXT).fontSize(BODY_SIZE)
        .width('100%').height('100%').padding(0).backgroundColor('#49252D')
        .borderRadius(PANEL_RADIUS).accessibilityDescription(this.registry.get(widget).title)
        .onClick(() => { this.hideWidget(widget) })
    }.width(64).height('100%').padding({ left: CONTENT_GAP })
  }

  build() {
    Column() {
      List({ space: CONTENT_GAP }) {
        if (this.editing && this.orderError) {
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
        if (this.editing && this.orderReady) {
          ForEach(this.visibleOrder(), (widget: string) => {
            ListItem() {
              Column() {
                this.widget(this.registry.get(widget))
              }.width('100%').enabled(this.registry.get(widget).supportsEditing)
                .accessibilityDescription(this.registry.get(widget).title + '，长按拖动，左滑删除')
            }.swipeAction({ end: { builder: () => { this.panelDelete(widget) } }, edgeEffect: SwipeEdgeEffect.None })
              .onGestureRecognizerJudgeBegin((_event: BaseGestureEvent, current: GestureRecognizer,
                recognizers: GestureRecognizer[]) => {
                // 面板内部有横滑手势时，由内部控件处理这次横滑。
                if (current.isBuiltIn() && current.getType() === GestureControl.GestureType.PAN_GESTURE &&
                  recognizers.some((gesture: GestureRecognizer) =>
                    !gesture.isBuiltIn() && gesture.getType() === GestureControl.GestureType.PAN_GESTURE &&
                    (gesture as PanRecognizer).getDirection() === PanDirection.Horizontal)) {
                  return GestureJudgeResult.REJECT
                }
                return GestureJudgeResult.CONTINUE
              })
          }, (widget: string) => widget)
            .onMove((from: number, to: number) => { this.moveWidget(from, to) })
          if (this.layout.hidden.length > 0) {
            ListItem() {
              Row() {
                Button({ type: ButtonType.Normal }) {
                  SymbolGlyph($r('sys.symbol.plus')).fontSize(20).fontColor([ACCENT])
                }.width(TOUCH_SIZE).height(TOUCH_SIZE).padding(0).backgroundColor(SURFACE)
                  .borderRadius(PANEL_RADIUS).accessibilityText('添加面板')
                  .bindMenu(this.panelAddMenu())
              }.width('100%').justifyContent(FlexAlign.Center)
            }
          }
        } else {
          ForEach(this.visibleOrder(), (widget: string) => {
            ListItem() {
              this.widget(this.registry.get(widget))
            }.accessibilityDescription('长按编辑面板')
              .gesture(LongPressGesture().onAction(() => { this.editing = true }))
          }, (widget: string) => widget)
          if (this.visibleOrder().length === 0 && !this.editing) {
            ListItem() {
              Text('长按此处恢复面板').fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
                .width('100%').constraintSize({ minHeight: TOUCH_SIZE }).padding(PAGE_INSET)
                .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)
            }.gesture(LongPressGesture().onAction(() => { this.editing = true }))
          }
        }
      }.width('100%').layoutWeight(1)
        .padding({ top: CONTENT_GAP, bottom: PAGE_INSET })
        .edgeEffect(EdgeEffect.None).scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)
    }.width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH }).layoutWeight(1)
      .padding({ left: PAGE_INSET, right: PAGE_INSET })
  }
}

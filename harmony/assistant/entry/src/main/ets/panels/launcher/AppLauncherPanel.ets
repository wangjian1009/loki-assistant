import { common } from '@kit.AbilityKit'
import { BusinessError } from '@kit.BasicServicesKit'
import { productViewManager } from '@kit.StoreKit'
import { ItemLayout, moveVisibleItem } from '../../app/CurrentDeviceLayout'
import { DeviceLayoutStore } from '../../app/DeviceLayoutStore'
import { ACCENT, BODY_SIZE, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_BACKGROUND, PAGE_INSET, PANEL_RADIUS, PRIMARY_TEXT,
  SECONDARY_TEXT, SELECTED_SURFACE, SURFACE, TOUCH_SIZE } from '../../components/AppPage'
import { AppLauncher, AppLauncherRegistry, appWant } from './AppLaunchers'

const DESKTOP_ACTION_WIDTH: number = 52
const EDIT_ACTIONS_WIDTH: number = 96

@Component
export struct AppLauncherPanel {
  registry: AppLauncherRegistry = new AppLauncherRegistry()
  @Prop @Watch('resetGesture') editing: boolean = false
  @State private layout: ItemLayout = { order: [], hidden: [] }
  @State private openedMenu: string = ''
  private store?: DeviceLayoutStore
  @State private pinning: boolean = false
  @State private dragged: string = ''
  @State private dragOffset: number = 0
  private rowWidth: number = 0

  aboutToAppear(): void {
    const ids = this.registry.ids()
    this.layout = { order: ids, hidden: [] }
    try {
      const context = this.getUIContext().getHostContext()
      if (!context) throw new Error('无法取得应用上下文')
      const store = new DeviceLayoutStore(context, 'launcher_layout')
      this.layout = store.load(ids)
      this.store = store
    } catch (_) {
      this.showToast('无法读取启动器布局')
    }
  }

  private apps(): AppLauncher[] {
    return this.layout.order.filter((id: string) => !this.layout.hidden.includes(id))
      .map((id: string) => this.registry.get(id))
  }

  private save(next: ItemLayout): void {
    try {
      if (!this.store) throw new Error('无法读取启动器布局')
      this.store.save(next)
      this.layout = next
    } catch (_) {
      this.showToast('无法保存启动器布局，请重试')
    }
  }

  private hide(id: string): void {
    this.openedMenu = ''
    if (!this.layout.hidden.includes(id)) {
      this.save({ order: this.layout.order, hidden: this.layout.hidden.concat(id) })
    }
  }

  private restore(id: string): void {
    this.save({ order: this.layout.order, hidden: this.layout.hidden.filter((hidden: string) => hidden !== id) })
  }

  private resetGesture(): void {
    this.openedMenu = ''
    this.dragged = ''
    this.dragOffset = 0
  }

  private async openApp(app: AppLauncher): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext
      await context.startAbility(appWant(context, app))
    } catch (_) {
      this.showToast('无法打开应用，请重试')
    }
  }

  private async addToDesktop(app: AppLauncher): Promise<void> {
    this.openedMenu = ''
    if (this.pinning) return
    this.pinning = true
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext
      const shortcut = await productViewManager.checkPinShortcutPermitted(
        context, app.id, appWant(context, app), app.labelResource, app.iconResource)
      if (shortcut.tid) {
        await productViewManager.requestNewPinShortcut(context, shortcut.tid)
      } else {
        this.showPinError(shortcut.code)
      }
    } catch (error) {
      this.showPinError((error as BusinessError).code)
    } finally {
      this.pinning = false
    }
  }

  private showPinError(code: number): void {
    if (code === 1006620007) return
    console.warn('添加桌面入口失败，错误码 ' + code)
    this.showToast(code === 1006620003 ? '桌面入口已添加' : '无法添加桌面入口')
  }

  private showToast(message: string): void {
    try {
      this.getUIContext().getPromptAction().showToast({ message })
    } catch (_) {
      console.error(message)
    }
  }

  private revealActions(app: AppLauncher, offset: number): void {
    if (offset < -18) this.openedMenu = app.id
    if (offset > 18) this.openedMenu = ''
  }

  private editActionsWidth(): number {
    const count = this.apps().length
    const tileWidth = (this.rowWidth - CONTENT_GAP * (count - 1)) / count
    return tileWidth > 0 ? Math.min(EDIT_ACTIONS_WIDTH, tileWidth) : EDIT_ACTIONS_WIDTH
  }

  @Builder
  private desktopAction(app: AppLauncher) {
    Button('桌面', { type: ButtonType.Normal }).fontSize(META_SIZE).fontColor(ACCENT)
      .layoutWeight(1).constraintSize({ minHeight: TOUCH_SIZE }).padding(0)
      .backgroundColor(PAGE_BACKGROUND).borderRadius(PANEL_RADIUS)
      .accessibilityText('添加到桌面').accessibilityDescription(app.label)
      .enabled(!this.pinning).onClick(() => { this.addToDesktop(app) })
  }

  private finishDrag(app: AppLauncher, offset: number): void {
    this.dragged = ''
    this.dragOffset = 0
    const apps = this.apps()
    const count = apps.length
    if (this.rowWidth <= 0 || count < 2) return
    const index = apps.findIndex((item: AppLauncher) => item.id === app.id)
    if (index < 0) return
    const stride = (this.rowWidth + CONTENT_GAP) / count
    const target = Math.max(0, Math.min(count - 1, index + Math.round(offset / stride)))
    if (index !== target) {
      const order = moveVisibleItem(this.layout.order, this.layout.hidden, app.id, apps[target].id)
      this.save({ order, hidden: this.layout.hidden })
    }
  }

  @Builder
  private appLabel(app: AppLauncher) {
    Column({ space: CONTENT_GAP }) {
      Image(app.icon).width(36).height(36).accessibilityLevel('no')
      Text(app.label).fontSize(BODY_SIZE).fontWeight(FontWeight.Medium)
        .fontColor(PRIMARY_TEXT).textAlign(TextAlign.Center)
    }.width('100%').padding({ top: CONTENT_GAP, bottom: CONTENT_GAP })
  }

  @Builder
  private appAddMenu() {
    Menu() {
      ForEach(this.layout.hidden, (id: string) => {
        MenuItem({ content: this.registry.get(id).label }).onClick(() => { this.restore(id) })
      }, (id: string) => id)
    }
  }

  @Builder
  private editApp(app: AppLauncher) {
    Stack({ alignContent: Alignment.End }) {
      if (this.openedMenu === app.id) {
        Row() {
          this.desktopAction(app)
          Button('删除', { type: ButtonType.Normal }).fontSize(META_SIZE).fontColor(ERROR_TEXT)
            .layoutWeight(1).constraintSize({ minHeight: TOUCH_SIZE }).padding(0)
            .backgroundColor('#49252D').borderRadius(PANEL_RADIUS)
            .accessibilityDescription(app.label).onClick(() => { this.hide(app.id) })
        }.width(this.editActionsWidth())
      }
      Button({ type: ButtonType.Normal }) {
        this.appLabel(app)
      }.width('100%').padding(0).backgroundColor(SELECTED_SURFACE).borderRadius(PANEL_RADIUS)
        .accessibilityText(app.label).accessibilityDescription('长按左右拖动，左滑显示操作')
        .translate({ x: this.openedMenu === app.id ? -this.editActionsWidth() : 0 })
        .priorityGesture(
          GestureGroup(GestureMode.Exclusive,
            GestureGroup(GestureMode.Sequence,
              LongPressGesture({ duration: 330 }),
              PanGesture({ direction: PanDirection.Horizontal, distance: 8 })
                .onActionStart(() => { this.openedMenu = ''
                  this.dragged = app.id })
                .onActionUpdate((event: GestureEvent) => { this.dragOffset = event.offsetX })
                .onActionEnd((event: GestureEvent) => { this.finishDrag(app, event.offsetX) })
                .onActionCancel(() => { this.resetGesture() })),
            PanGesture({ direction: PanDirection.Horizontal, distance: 8 })
              .onActionEnd((event: GestureEvent) => { this.revealActions(app, event.offsetX) })))
    }.layoutWeight(1).clip(true).borderRadius(PANEL_RADIUS)
      .translate({ x: this.dragged === app.id ? this.dragOffset : 0 })
      .zIndex(this.dragged === app.id ? 1 : 0)
  }

  @Builder
  private launchApp(app: AppLauncher) {
    Stack({ alignContent: Alignment.End }) {
      if (this.openedMenu === app.id) {
        Row() {
          this.desktopAction(app)
        }.width(DESKTOP_ACTION_WIDTH)
      }
      Button({ type: ButtonType.Normal }) {
        this.appLabel(app)
      }.width('100%').padding(0).backgroundColor(SELECTED_SURFACE).borderRadius(PANEL_RADIUS)
        .accessibilityText(app.label).accessibilityDescription('左滑可添加到桌面')
        .translate({ x: this.openedMenu === app.id ? -DESKTOP_ACTION_WIDTH : 0 })
        .onClick(() => {
          this.openedMenu = ''
          this.openApp(app)
        })
    }.layoutWeight(1).clip(true).borderRadius(PANEL_RADIUS)
      .gesture(PanGesture({ direction: PanDirection.Horizontal, distance: 8 })
        .onActionEnd((event: GestureEvent) => { this.revealActions(app, event.offsetX) }))
  }

  build() {
    Column() {
      if (this.apps().length === 0) {
        Text('暂无应用入口').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).width('100%')
      } else {
        Row({ space: CONTENT_GAP }) {
          ForEach(this.apps(), (app: AppLauncher) => {
            if (this.editing) {
              this.editApp(app)
            } else {
              this.launchApp(app)
            }
          }, (app: AppLauncher) => app.id)
        }.width('100%').onAreaChange((_oldArea: Area, newArea: Area) => { this.rowWidth = Number(newArea.width) })
      }
      if (this.editing && this.layout.hidden.length > 0) {
        Button({ type: ButtonType.Normal }) {
          SymbolGlyph($r('sys.symbol.plus')).fontSize(20).fontColor([ACCENT])
        }.width(TOUCH_SIZE).height(TOUCH_SIZE).padding(0).margin({ top: CONTENT_GAP })
          .backgroundColor(SELECTED_SURFACE).borderRadius(PANEL_RADIUS).accessibilityText('添加应用入口')
          .bindMenu(this.appAddMenu())
      }
    }.width('100%').padding(PAGE_INSET)
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)
  }
}

import { common } from '@kit.AbilityKit'
import { Application } from '../app/Application'
import { createCurrentDevicePanels } from '../app/BuiltInPanels'
import { CurrentDeviceState } from '../app/CurrentDeviceState'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, AppPage, BODY_SIZE, CONTENT_GAP, PANEL_RADIUS, TOUCH_SIZE,
  setSystemColors } from '../components/AppPage'
import { CurrentDeviceWidgets } from '../components/CurrentDeviceWidgets'
import { PopupMenu } from '../components/PopupMenu'

@Entry
@Component
struct Index {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private device: CurrentDeviceState = new CurrentDeviceState()
  @State private editing: boolean = false
  private readonly panels = createCurrentDevicePanels()
  private visible: boolean = false

  private onForegroundChanged(): void {
    if (this.foreground && this.visible) {
      this.device.activate(AppStorage.get<Application>('lokiApplication'))
    } else {
      this.device.deactivate()
    }
  }

  onPageShow(): void {
    this.visible = true
    setSystemColors(this.getUIContext().getHostContext() as common.UIAbilityContext)
    this.onForegroundChanged()
  }

  onPageHide(): void {
    this.visible = false
    this.editing = false
    this.device.collapseDetails()
    this.device.deactivate()
  }

  aboutToDisappear(): void {
    this.visible = false
    this.device.deactivate()
  }

  onBackPress(): boolean {
    if (this.editing) {
      this.editing = false
      return true
    }
    if (this.device.collapseDetails()) { return true }
    const navigator = AppStorage.get<NavigatorManager>('lokiNavigator')
    if (!navigator || !navigator.canGoBack()) { return false }
    navigator.back()
    return true
  }

  private async openSection(page: PageName): Promise<void> {
    const result = await AppStorage.get<NavigatorManager>('lokiNavigator')?.openSection(page)
    if (result !== NavigationResult.Ok && result !== NavigationResult.Busy) {
      try { this.getUIContext().getPromptAction().showToast({ message: '无法打开页面，请重试' }) }
      catch (_) { console.error('无法显示页面打开失败提示') }
    }
  }

  @Builder
  private pageContent() {
    CurrentDeviceWidgets({ device: this.device, editing: $editing, registry: this.panels })
  }

  @Builder
  private moreIcon() {
    Row({ space: 3 }) {
      Circle().width(3).height(3).fill(ACCENT)
      Circle().width(3).height(3).fill(ACCENT)
      Circle().width(3).height(3).fill(ACCENT)
    }.accessibilityLevel('no')
  }

  @Builder
  private headerActions() {
    if (this.editing) {
      Button('完成', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ACCENT)
        .constraintSize({ minHeight: TOUCH_SIZE, minWidth: TOUCH_SIZE })
        .padding({ left: CONTENT_GAP, right: CONTENT_GAP })
        .backgroundColor(Color.Transparent).borderRadius(PANEL_RADIUS)
        .onClick(() => { this.editing = false })
    } else {
      PopupMenu({
        accessibilityLabel: '更多',
        items: [
          { id: 'devices', label: '设备管理', icon: $r('sys.symbol.computer'),
            action: () => { this.openSection(PageName.Devices) } },
          { id: 'settings', label: '设置', icon: $r('sys.symbol.gearshape'),
            action: () => { this.openSection(PageName.Settings) } }
        ],
        trigger: () => { this.moreIcon() }
      })
    }
  }

  build() {
    Stack() {
      AppPage({ title: $r('app.string.app_name'),
        actions: () => { this.headerActions() }, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

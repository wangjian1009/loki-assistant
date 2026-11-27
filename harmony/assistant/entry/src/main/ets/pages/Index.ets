import { common } from '@kit.AbilityKit'
import { Application } from '../app/Application'
import { createCurrentDevicePanels } from '../app/BuiltInPanels'
import { CurrentDeviceState } from '../app/CurrentDeviceState'
import { NavigatorManager, PageName } from '../app/NavigatorManager'
import { ACCENT, AppPage, BODY_SIZE, CONTENT_GAP, PANEL_RADIUS, PRIMARY_TEXT, TITLE_SIZE, TOUCH_SIZE,
  setSystemColors } from '../components/AppPage'
import { CurrentDeviceWidgets } from '../components/CurrentDeviceWidgets'
import { PopupMenu } from '../components/PopupMenu'

@Entry
@Component
struct Index {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private device: CurrentDeviceState = new CurrentDeviceState()
  @State private sidebarOpen: boolean = false
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
    this.sidebarOpen = false
    this.editing = false
    this.device.collapseDetails()
    this.device.deactivate()
  }

  aboutToDisappear(): void {
    this.visible = false
    this.device.deactivate()
  }

  onBackPress(): boolean {
    if (this.sidebarOpen) {
      this.sidebarOpen = false
      return true
    }
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
  private pageHeader() {
    Text('当前设备').fontSize(TITLE_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
    if (this.editing) {
      Button('完成', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ACCENT)
        .constraintSize({ minHeight: TOUCH_SIZE, minWidth: TOUCH_SIZE })
        .padding({ left: CONTENT_GAP, right: CONTENT_GAP })
        .backgroundColor(Color.Transparent).borderRadius(PANEL_RADIUS)
        .onClick(() => { this.editing = false })
    } else {
      PopupMenu({
        accessibilityLabel: '更多',
        items: [{ id: 'edit', label: '编辑', action: () => { this.editing = true } }],
        trigger: () => { this.moreIcon() }
      })
    }
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Index, sidebarOpen: $sidebarOpen,
        header: () => { this.pageHeader() }, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

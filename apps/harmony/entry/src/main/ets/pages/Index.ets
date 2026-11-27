import { common } from '@kit.AbilityKit'
import { Application, ComputerDevice } from '../app/Application'
import { DeviceRefresh } from '../components/DeviceRefresh'
import { StatusDetail, statusDetails } from '../components/DeviceStatus'
import { OtherDeviceStatus } from '../components/OtherDeviceStatus'
import { MailCard } from '../features/mail/MailCard'
import { SlackCard } from '../features/slack/SlackCard'
import { StatusSubscription } from '../workspace/ServiceStatus'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE,
  PAGE_INSET, PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SELECTED_SURFACE, SURFACE, TITLE_SIZE,
  TOUCH_SIZE, setSystemColors } from '../components/AppPage'

@Entry
@Component
struct Index {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private current: ComputerDevice | undefined = undefined
  @State private sidebarOpen: boolean = false
  @State private detailsOpen: boolean = false
  @State private working: boolean = false
  @State private loaded: boolean = false
  @State private errorMessage: string = ''
  @State private showBusiness: boolean = false
  @State private otherStatus: StatusDetail[] = []
  private application?: Application
  private visible: boolean = false
  private subscription?: StatusSubscription
  private subscriptionVersion: number = 0

  private onForegroundChanged(): void {
    if (this.foreground && this.visible) {
      this.loadDevices()
    } else {
      this.showBusiness = false
      this.stopWatching()
    }
  }

  onPageShow(): void {
    this.visible = true
    setSystemColors(this.getUIContext().getHostContext() as common.UIAbilityContext)
    this.application = AppStorage.get<Application>('lokiApplication')
    this.loadDevices()
  }

  onPageHide(): void {
    this.visible = false
    this.sidebarOpen = false
    this.detailsOpen = false
    this.showBusiness = false
    this.otherStatus = []
    this.stopWatching()
  }

  onBackPress(): boolean {
    if (this.sidebarOpen) {
      this.sidebarOpen = false
      return true
    }
    if (this.detailsOpen) {
      this.detailsOpen = false
      return true
    }
    const navigator = AppStorage.get<NavigatorManager>('lokiNavigator')
    if (!navigator || !navigator.canGoBack()) return false
    navigator.back()
    return true
  }

  aboutToDisappear(): void {
    this.visible = false
    this.stopWatching()
  }

  private stopWatching(): void {
    this.subscriptionVersion += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.working = false
  }

  private updateCurrent(computers: ComputerDevice[]): void {
    const next = computers.find((computer: ComputerDevice) => computer.active)
    this.current = next
    this.showBusiness = this.visible && this.foreground && next?.status === 'online'
    const status = next?.dashboard?.status
    this.otherStatus = status ? statusDetails(status) : []
    if (!next) { this.detailsOpen = false }
  }

  private async loadDevices(): Promise<void> {
    const application = this.application
    if (!application || this.working || this.subscription || !this.visible || !this.foreground) { return }
    const version = ++this.subscriptionVersion
    this.working = true
    try {
      const subscription = await application.watchDevices((devices: ComputerDevice[]) => {
        if (version !== this.subscriptionVersion) { return }
        this.updateCurrent(devices)
        this.loaded = true
        this.errorMessage = ''
      }, (message: string) => {
        if (version === this.subscriptionVersion) { this.errorMessage = message }
      })
      if (version === this.subscriptionVersion) {
        this.subscription = subscription
      } else {
        subscription.cancel()
      }
    } catch (error) {
      if (version === this.subscriptionVersion) { this.errorMessage = (error as Error).message }
    } finally {
      if (version === this.subscriptionVersion) { this.working = false }
    }
  }

  private async openDevices(): Promise<void> {
    const result = await AppStorage.get<NavigatorManager>('lokiNavigator')?.openSection(PageName.Devices)
    if (result !== NavigationResult.Ok && result !== NavigationResult.Busy) {
      this.errorMessage = '无法打开设备管理，请重试'
    }
  }

  private statusText(computer: ComputerDevice): string {
    switch (computer.status) {
      case 'online': return '在线'
      case 'binding': return '绑定中'
      case 'binding_failed': return '绑定失败'
      default: return '离线'
    }
  }

  private statusColor(computer: ComputerDevice): string {
    if (computer.status === 'online') { return '#9AE2B9' }
    if (computer.status === 'binding_failed') { return ERROR_TEXT }
    if (computer.status === 'binding') { return '#F5CF92' }
    return SECONDARY_TEXT
  }

  private checkedTime(computer: ComputerDevice): string {
    return computer.checked_at > 0 ? new Date(computer.checked_at * 1000).toLocaleTimeString() : '尚未检查'
  }

  @Builder
  private pageHeader() {
    if (this.current) {
      Button({ type: ButtonType.Normal }) {
        Row({ space: 8 }) {
          Text(this.current.name).fontSize(TITLE_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
            .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).flexShrink(1)
          if (this.current.status === 'online' || this.current.status === 'offline') {
            Row({ space: 4 }) {
              Circle().width(6).height(6).fill(this.statusColor(this.current))
              Text(this.statusText(this.current)).fontSize(META_SIZE).fontColor(this.statusColor(this.current))
            }.flexShrink(0)
          }
          SymbolGlyph(this.detailsOpen ? $r('sys.symbol.chevron_up') : $r('sys.symbol.chevron_down'))
            .fontSize(12).fontColor([SECONDARY_TEXT]).flexShrink(0).accessibilityLevel('no')
        }
      }.layoutWeight(1).constraintSize({ minHeight: TOUCH_SIZE }).padding({ left: 4, right: CONTENT_GAP }).align(Alignment.Start)
        .borderRadius(PANEL_RADIUS).backgroundColor(this.detailsOpen ? SELECTED_SURFACE : Color.Transparent)
        .accessibilityDescription(this.detailsOpen ? '收起设备详情' : '展开设备详情')
        .onClick(() => { this.detailsOpen = !this.detailsOpen })
    } else {
      Text('当前设备').fontSize(TITLE_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
    }
  }

  @Builder
  private detailRow(label: string, value: string) {
    Row({ space: PAGE_INSET }) {
      Text(label).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).layoutWeight(1)
      Text(value).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).textAlign(TextAlign.End)
        .constraintSize({ maxWidth: '64%' })
    }.width('100%').constraintSize({ minHeight: 36 }).padding({ top: 6, bottom: 6 })
  }

  @Builder
  private deviceDetails(computer: ComputerDevice) {
    Column({ space: CONTENT_GAP }) {
      Column() {
        Row() {
          Text('连接').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
          if (computer.status === 'binding' || computer.status === 'binding_failed') {
            Text(this.statusText(computer)).fontSize(BODY_SIZE).fontColor(this.statusColor(computer))
          }
        }.width('100%')
        this.detailRow('最近检查', this.checkedTime(computer))
        if (computer.status !== 'online') {
          Text(computer.detail).width('100%').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
        }
      }.width('100%').alignItems(HorizontalAlign.Start)

      Divider().color(BORDER).strokeWidth(0.5)
      Column() {
        Text('设备信息').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
        this.detailRow('设备类型', computer.device_type === 'computer' ? '电脑' : '移动端')
        if (computer.bound_at !== null) {
          Divider().color(BORDER).strokeWidth(0.5)
          this.detailRow('绑定时间', new Date(computer.bound_at * 1000).toLocaleString())
        }
        Divider().color(BORDER).strokeWidth(0.5)
        this.detailRow('设备标识', `${computer.endpoint_id.slice(0, 8)}…${computer.endpoint_id.slice(-8)}`)
      }.width('100%').alignItems(HorizontalAlign.Start)
    }.width('100%').padding(PAGE_INSET).backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }

  @Builder
  private feedback() {
    if (this.errorMessage !== '') {
      Column({ space: 8 }) {
        Text(this.errorMessage).width('100%').fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
        if (!this.loaded) {
          Button('重新读取').fontSize(BODY_SIZE).fontColor(ERROR_TEXT).constraintSize({ minHeight: TOUCH_SIZE }).backgroundColor('#523437')
            .enabled(!this.working).onClick(() => { this.loadDevices() })
        }
      }.width('100%').padding(PAGE_INSET).alignItems(HorizontalAlign.Start)
        .backgroundColor('#362529').borderRadius(PANEL_RADIUS)
    }
    if (this.working && !this.loaded) {
      Row({ space: 10 }) {
        LoadingProgress().width(16).height(16).color(ACCENT)
        Text('正在读取当前设备').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
      }.width('100%')
    }
  }

  @Builder
  private emptyDevices() {
    Column({ space: CONTENT_GAP }) {
      Stack() {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(24).height(24).fillColor(ACCENT)
      }.width(44).height(44).borderRadius(PANEL_RADIUS).backgroundColor(SELECTED_SURFACE)
      Text('暂无当前设备').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      Text('扫码添加电脑，或选择已配置的电脑。')
        .fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).textAlign(TextAlign.Center)
      Button('前往设备管理', { type: ButtonType.Normal }).constraintSize({ minHeight: TOUCH_SIZE }).borderRadius(PANEL_RADIUS)
        .fontSize(BODY_SIZE).fontWeight(FontWeight.Bold).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
        .onClick(() => { this.openDevices() })
    }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, top: 24, bottom: 24 })
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }

  @Builder
  private deviceContent() {
    Column({ space: CONTENT_GAP }) {
      if (this.current && this.detailsOpen) {
        this.deviceDetails(this.current)
      }
      this.feedback()
      if (this.loaded && !this.current) {
        this.emptyDevices()
      }
      if (this.showBusiness && this.application) {
        MailCard({ workspace: this.application.workspace })
        SlackCard({ workspace: this.application.workspace })
        OtherDeviceStatus({ details: this.otherStatus, endpointId: this.current?.endpoint_id ?? '' })
      }
    }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, top: CONTENT_GAP, bottom: PAGE_INSET })
  }

  @Builder
  private pageContent() {
    DeviceRefresh({
      workspace: () => this.application?.workspace,
      endpointId: this.foreground ? this.current?.endpoint_id ?? '' : '',
      content: () => { this.deviceContent() }
    })
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Index, sidebarOpen: $sidebarOpen,
        header: () => { this.pageHeader() }, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

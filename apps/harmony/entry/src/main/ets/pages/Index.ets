import { common } from '@kit.AbilityKit'
import { Application, ComputerDevice } from '../app/Application'
import { DeviceRefresh } from '../components/DeviceRefresh'
import { StatusDetail, statusDetails } from '../components/DeviceStatus'
import { OtherDeviceStatus } from '../components/OtherDeviceStatus'
import { MailCard } from '../features/mail/MailCard'
import { SlackCard } from '../features/slack/SlackCard'
import { StatusSubscription } from '../workspace/ServiceStatus'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BORDER, ERROR_TEXT, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE,
  setSystemColors } from '../components/AppPage'

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
          Text(this.current.name).fontSize(22).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
            .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).flexShrink(1)
          if (this.current.status === 'online' || this.current.status === 'offline') {
            Row({ space: 4 }) {
              Circle().width(6).height(6).fill(this.statusColor(this.current))
              Text(this.statusText(this.current)).fontSize(11).fontColor(this.statusColor(this.current))
            }.flexShrink(0)
          }
          SymbolGlyph(this.detailsOpen ? $r('sys.symbol.chevron_up') : $r('sys.symbol.chevron_down'))
            .fontSize(12).fontColor([SECONDARY_TEXT]).flexShrink(0).accessibilityLevel('no')
        }
      }.layoutWeight(1).height(48).padding({ left: 4, right: 8 }).align(Alignment.Start)
        .borderRadius(12).backgroundColor(this.detailsOpen ? BORDER : Color.Transparent)
        .accessibilityDescription(this.detailsOpen ? '收起设备详情' : '展开设备详情')
        .onClick(() => { this.detailsOpen = !this.detailsOpen })
    } else {
      Text('当前设备').fontSize(22).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
        .padding({ left: 4 })
    }
  }

  @Builder
  private detailRow(label: string, value: string) {
    Row({ space: 16 }) {
      Text(label).fontSize(16).fontColor(SECONDARY_TEXT).layoutWeight(1)
      Text(value).fontSize(16).fontColor(PRIMARY_TEXT).textAlign(TextAlign.End)
        .constraintSize({ maxWidth: '64%' })
    }.width('100%').constraintSize({ minHeight: 44 }).padding({ top: 8, bottom: 8 })
  }

  @Builder
  private deviceDetails(computer: ComputerDevice) {
    Column({ space: 12 }) {
      Column() {
        Row() {
          Text('连接').fontSize(18).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
          if (computer.status === 'binding' || computer.status === 'binding_failed') {
            Text(this.statusText(computer)).fontSize(14).fontColor(this.statusColor(computer))
          }
        }.width('100%')
        this.detailRow('最近检查', this.checkedTime(computer))
        if (computer.status !== 'online') {
          Text(computer.detail).width('100%').fontSize(15).lineHeight(22).fontColor(SECONDARY_TEXT)
        }
      }.width('100%').alignItems(HorizontalAlign.Start)

      Divider().color(BORDER).strokeWidth(0.5)
      Column() {
        Text('设备信息').fontSize(18).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
        this.detailRow('设备类型', computer.device_type === 'computer' ? '电脑' : '移动端')
        if (computer.bound_at !== null) {
          Divider().color(BORDER).strokeWidth(0.5)
          this.detailRow('绑定时间', new Date(computer.bound_at * 1000).toLocaleString())
        }
        Divider().color(BORDER).strokeWidth(0.5)
        this.detailRow('设备标识', `${computer.endpoint_id.slice(0, 8)}…${computer.endpoint_id.slice(-8)}`)
      }.width('100%').alignItems(HorizontalAlign.Start)
    }.width('100%').padding(12).backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
  }

  @Builder
  private feedback() {
    if (this.errorMessage !== '') {
      Column({ space: 8 }) {
        Text(this.errorMessage).width('100%').fontSize(14).lineHeight(22).fontColor(ERROR_TEXT)
        if (!this.loaded) {
          Button('重新读取').fontSize(14).fontColor(ERROR_TEXT).height(44).backgroundColor('#523437')
            .enabled(!this.working).onClick(() => { this.loadDevices() })
        }
      }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
        .backgroundColor('#362529').borderRadius(16)
    }
    if (this.working && !this.loaded) {
      Row({ space: 10 }) {
        LoadingProgress().width(16).height(16).color(ACCENT)
        Text('正在读取当前设备').fontSize(14).fontColor(SECONDARY_TEXT)
      }.width('100%')
    }
  }

  @Builder
  private emptyDevices() {
    Column({ space: 14 }) {
      Stack() {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(38).height(38).fillColor(ACCENT)
      }.width(76).height(76).borderRadius(24).backgroundColor('#303951')
      Text('暂无当前设备').fontSize(22).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
      Text('前往设备管理添加电脑，或选择一台已配置的电脑。')
        .fontSize(14).fontColor(SECONDARY_TEXT).textAlign(TextAlign.Center).lineHeight(23)
      Button('前往设备管理', { type: ButtonType.Normal }).height(44).borderRadius(12)
        .fontSize(15).fontWeight(FontWeight.Bold).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
        .onClick(() => { this.openDevices() })
    }.width('100%').padding({ left: 16, right: 16, top: 40, bottom: 40 })
      .backgroundColor(SURFACE).borderRadius(22).border({ width: 1, color: BORDER })
  }

  @Builder
  private deviceContent() {
    Column({ space: 12 }) {
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
    }.width('100%').padding({ left: this.current ? 12 : 24, right: this.current ? 12 : 24,
      top: this.current ? 6 : 16, bottom: 24 })
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

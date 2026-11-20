import { common } from '@kit.AbilityKit'
import { Application, ComputerDevice } from '../app/Application'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BORDER, ERROR_TEXT, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE,
  setSystemColors } from '../components/AppPage'

@Entry
@Component
struct Index {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private current: ComputerDevice[] = []
  @State private sidebarOpen: boolean = false
  @State private working: boolean = false
  @State private loaded: boolean = false
  @State private errorMessage: string = ''
  @State private refreshing: boolean = false
  private application?: Application
  private timer: number = -1
  private visible: boolean = false

  private onForegroundChanged(): void {
    if (this.foreground && this.visible) {
      this.loadDevices()
    } else {
      this.stopTimer()
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
    this.stopTimer()
  }

  onBackPress(): boolean {
    if (this.sidebarOpen) {
      this.sidebarOpen = false
      return true
    }
    const navigator = AppStorage.get<NavigatorManager>('lokiNavigator')
    if (!navigator || !navigator.canGoBack()) return false
    navigator.back()
    return true
  }

  aboutToDisappear(): void {
    this.visible = false
    this.stopTimer()
  }

  private stopTimer(): void {
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
  }

  private scheduleRefresh(): void {
    this.stopTimer()
    if (this.visible && this.foreground && this.current.length > 0) {
      this.timer = setTimeout(() => { this.refreshDevices() }, this.current[0].status === 'binding' ? 1000 : 20000)
    }
  }

  private async loadDevices(): Promise<void> {
    if (this.working || !this.application) { return }
    this.working = true
    try {
      this.current = (await this.application.list()).filter((computer: ComputerDevice) => computer.active)
      this.loaded = true
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
    }
    if (this.current.length > 0) { await this.refreshDevices() }
  }

  private async refreshDevices(): Promise<void> {
    if (this.working || !this.application || !this.visible || !this.foreground) { return }
    this.stopTimer()
    this.working = true
    this.refreshing = true
    try {
      this.current = (await this.application.refresh()).filter((computer: ComputerDevice) => computer.active)
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
      this.refreshing = false
      this.scheduleRefresh()
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
  private deviceSummary(computer: ComputerDevice) {
    Row({ space: 14 }) {
      Stack() {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(28).height(28).fillColor(ACCENT)
      }.width(52).height(52).borderRadius(16).backgroundColor('#303951')

      Column({ space: 8 }) {
        Text(computer.name).fontSize(22).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
          .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%')
        Row({ space: 6 }) {
          Circle().width(6).height(6).fill(this.statusColor(computer))
          Text(this.statusText(computer)).fontSize(13).fontColor(this.statusColor(computer))
        }
      }.layoutWeight(1).alignItems(HorizontalAlign.Start)
    }.width('100%').padding(16).backgroundColor('#222735').borderRadius(22)
      .border({ width: 1, color: '#363F53' })
  }

  @Builder
  private detailRow(label: string, value: string) {
    Row({ space: 16 }) {
      Text(label).fontSize(15).fontColor(SECONDARY_TEXT).layoutWeight(1)
      Text(value).fontSize(15).fontColor(PRIMARY_TEXT).textAlign(TextAlign.End)
        .constraintSize({ maxWidth: '64%' })
    }.width('100%').constraintSize({ minHeight: 54 })
      .padding({ left: 16, right: 16, top: 10, bottom: 10 })
  }

  @Builder
  private deviceDetails(computer: ComputerDevice) {
    Column({ space: 10 }) {
      Row() {
        Text('连接').fontSize(18).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT).layoutWeight(1)
        Button({ type: ButtonType.Normal }) {
          if (this.refreshing) {
            LoadingProgress().width(16).height(16).color(ACCENT)
          } else {
            Text('刷新').fontSize(14).fontColor(ACCENT)
          }
        }.height(44).width(56).borderRadius(12).backgroundColor(SURFACE)
          .enabled(!this.working).onClick(() => { this.refreshDevices() })
          .accessibilityText(this.refreshing ? '正在检查电脑状态' : '刷新连接状态')
      }.width('100%')

      Column() {
        this.detailRow('最近检查', this.checkedTime(computer))
        if (computer.status !== 'online') {
          Divider().color(BORDER).margin({ left: 16, right: 16 })
          Text(computer.detail).width('100%').fontSize(14).lineHeight(22).fontColor(SECONDARY_TEXT)
            .padding({ left: 16, right: 16, top: 12, bottom: 12 })
        }
      }.width('100%').backgroundColor(SURFACE).borderRadius(16).border({ width: 1, color: BORDER })

      Text('设备信息').fontSize(18).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
        .margin({ top: 14 })
      Column() {
        this.detailRow('设备类型', computer.device_type === 'computer' ? '电脑' : '移动端')
        if (computer.bound_at !== null) {
          Divider().color(BORDER).margin({ left: 16, right: 16 })
          this.detailRow('绑定时间', new Date(computer.bound_at * 1000).toLocaleString())
        }
        Divider().color(BORDER).margin({ left: 16, right: 16 })
        this.detailRow('设备标识', `${computer.endpoint_id.slice(0, 8)}…${computer.endpoint_id.slice(-8)}`)
      }.width('100%').backgroundColor(SURFACE).borderRadius(16).border({ width: 1, color: BORDER })
    }.width('100%').alignItems(HorizontalAlign.Start)
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
    if (this.working && !this.loaded && !this.refreshing) {
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
  private pageContent() {
    if (this.current.length > 0) {
      Column() {
        ForEach(this.current, (computer: ComputerDevice) => {
          this.deviceSummary(computer)
        }, (computer: ComputerDevice) => JSON.stringify([computer.endpoint_id, computer.name, computer.status]))
      }.width('100%').constraintSize({ maxWidth: 560 }).padding({ left: 24, right: 24, bottom: 16 })
    }

    Scroll() {
      Column({ space: 12 }) {
        this.feedback()
        if (this.loaded && this.current.length === 0) {
          this.emptyDevices()
        } else {
          ForEach(this.current, (computer: ComputerDevice) => {
            this.deviceDetails(computer)
          }, (computer: ComputerDevice) => JSON.stringify(computer))
        }
      }.width('100%').padding({ left: 24, right: 24, top: 8, bottom: 24 })
    }.width('100%').constraintSize({ maxWidth: 560 }).layoutWeight(1).align(Alignment.Top)
      .scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Index, sidebarOpen: $sidebarOpen }) {
        this.pageContent()
      }
    }.width('100%').height('100%')
  }
}

import { common } from '@kit.AbilityKit'
import { Application, ComputerDevice, DashboardStatus, SlackPreview } from '../app/Application'
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
  @State private refreshing: boolean = false
  @State private dashboard: DashboardStatus | undefined = undefined
  @State private showAllSlack: boolean = false
  @State private showAllMail: boolean = false
  @State private syncId: string = ''
  @State private startingSync: boolean = false
  @State private syncMessage: string = ''
  @State private syncFailed: boolean = false
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
    this.detailsOpen = false
    this.dashboard = undefined
    this.stopTimer()
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
    if (this.visible && this.foreground && this.current) {
      if (this.syncId && this.current.status === 'online') {
        this.timer = setTimeout(() => { this.refreshDevices() }, 2000)
      } else {
        this.timer = setTimeout(() => { this.refreshDevices() }, this.current.status === 'binding' ? 1000 : 20000)
      }
    }
  }

  private updateCurrent(computers: ComputerDevice[]): void {
    const next = computers.find((computer: ComputerDevice) => computer.active)
    if (next?.endpoint_id !== this.current?.endpoint_id) {
      this.syncId = ''
      this.syncMessage = ''
      this.syncFailed = false
      this.showAllSlack = false
      this.showAllMail = false
    }
    this.current = next
    this.dashboard = this.visible && this.foreground ? next?.dashboard : undefined
    if (!next) { this.detailsOpen = false }
  }

  private async loadDevices(): Promise<void> {
    if (this.working || !this.application) { return }
    this.working = true
    try {
      this.updateCurrent(await this.application.list())
      this.loaded = true
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
    }
    if (this.current) { await this.refreshDevices() }
  }

  private async refreshDevices(): Promise<void> {
    if (this.working || !this.application || !this.visible || !this.foreground) { return }
    this.stopTimer()
    this.working = true
    this.refreshing = true
    const syncId = this.syncId
    try {
      this.updateCurrent(await this.application.refresh())
      this.errorMessage = ''
      if (syncId && syncId === this.syncId && this.dashboard) {
        const task = this.dashboard.mail?.sync_runs?.find((run) => run.id === syncId)
        this.updateSyncPhase(task?.phase)
      }
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
      this.refreshing = false
      this.scheduleRefresh()
    }
  }

  private async startMailSync(): Promise<void> {
    if (!this.application || this.current?.status !== 'online' || this.startingSync || this.syncId) { return }
    const endpointId = this.current.endpoint_id
    this.startingSync = true
    this.syncFailed = false
    this.syncMessage = '正在提交同步请求…'
    try {
      const id = await this.application.startMailSync(endpointId)
      if (this.current?.endpoint_id === endpointId) {
        this.syncId = id
        this.syncMessage = '等待同步'
      }
    } catch (_) {
      if (this.current?.endpoint_id === endpointId) {
        this.syncFailed = true
        this.syncMessage = '未收到同步确认，请重试'
      }
    } finally {
      this.startingSync = false
      this.scheduleRefresh()
    }
  }

  private updateSyncPhase(phase?: string): void {
    switch (phase) {
      case 'queued': this.syncMessage = '等待同步'; return
      case 'downloading': this.syncMessage = '正在下载邮件…'; return
      case 'indexing': this.syncMessage = '正在更新索引…'; return
      case 'refreshing': this.syncMessage = '正在刷新未读邮件…'; return
      case 'completed': this.syncMessage = '邮件同步已完成'; break
      case 'failed': this.syncMessage = '邮件同步失败'; break
      default: this.syncMessage = '暂时无法获取同步进度'
    }
    this.syncId = ''
    this.syncFailed = phase !== 'completed'
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

      Button({ type: ButtonType.Normal }) {
        if (this.refreshing) {
          LoadingProgress().width(20).height(20).color(ACCENT)
        } else {
          SymbolGlyph($r('sys.symbol.arrow_clockwise')).fontSize(24).fontColor([ACCENT])
        }
      }.width(48).height(48).padding(0).borderRadius(8).backgroundColor(Color.Transparent)
        .enabled(!this.working).onClick(() => { this.refreshDevices() })
        .accessibilityText(this.refreshing ? '正在检查电脑状态' : '刷新连接状态')
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
  private sectionTitle(title: string) {
    Text(title).fontSize(18).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
  }

  @Builder
  private messageHint(message: string) {
    Text(message).fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(21)
  }

  @Builder
  private listToggle(label: string, expanded: boolean, toggle: () => void) {
    Button(expanded ? '收起' : label, { type: ButtonType.Normal })
      .height(44).padding({ left: 0, right: 12 }).backgroundColor(Color.Transparent)
      .fontSize(14).fontColor(ACCENT).onClick(toggle)
  }

  @Builder
  private slackCard() {
    Column({ space: 10 }) {
      this.sectionTitle('Slack 新消息')
      if (this.dashboard?.slack?.unread_previews?.length) {
        ForEach(this.dashboard.slack.unread_previews.slice(0, this.showAllSlack ? 20 : 2),
          (item: SlackPreview, index: number) => {
          if (index > 0) { Divider().color(BORDER).strokeWidth(0.5) }
          Column({ space: 4 }) {
            Text(item.title).fontSize(16).fontColor(PRIMARY_TEXT).fontWeight(FontWeight.Medium)
              .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
            this.messageHint(`${item.team_name} · ${item.unread_count} 条未读`)
            Text(item.preview).fontSize(14).fontColor(PRIMARY_TEXT).lineHeight(21)
          }.width('100%').alignItems(HorizontalAlign.Start)
        }, (item: SlackPreview, index: number) => `${index}:${JSON.stringify(item)}`)
        if (this.dashboard.slack.unread_previews.length > 2) {
          this.listToggle('查看更多 Slack 消息', this.showAllSlack, () => { this.showAllSlack = !this.showAllSlack })
        }
      } else {
        this.messageHint('暂无未读消息')
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
      .backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
  }

  @Builder
  private mailCard() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column({ space: 4 }) {
          this.sectionTitle('邮件收件箱')
          if (this.dashboard?.mail?.unread_count !== undefined) {
            this.messageHint(`${this.dashboard.mail.unread_count} 封未读`)
          }
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        Button('立即同步邮件', { type: ButtonType.Normal })
          .constraintSize({ minHeight: 44 }).padding({ left: 12, right: 12 }).borderRadius(10)
          .fontSize(14).fontWeight(FontWeight.Medium).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
          .enabled(!this.startingSync && !this.syncId).onClick(() => { this.startMailSync() })
      }.width('100%')
      if (this.syncMessage !== '') {
        Text(this.syncMessage).fontSize(14).fontColor(this.syncFailed ? ERROR_TEXT : SECONDARY_TEXT)
          .accessibilityDescription(this.syncMessage)
      }
      if (this.dashboard?.mail?.unread_subjects?.length) {
        ForEach(this.dashboard.mail.unread_subjects.slice(0, this.showAllMail ? 20 : 2), (subject: string) => {
          Divider().color(BORDER).strokeWidth(0.5)
          Text(subject).fontSize(15).fontColor(PRIMARY_TEXT).lineHeight(22)
        }, (subject: string, index: number) => `${index}:${subject}`)
        if (this.dashboard.mail.unread_subjects.length > 2) {
          this.listToggle('查看更多邮件', this.showAllMail, () => { this.showAllMail = !this.showAllMail })
        }
      } else if (this.dashboard && this.dashboard.mail?.unread_count === undefined) {
        this.messageHint('等待电脑更新邮件索引')
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
      .backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
  }

  @Builder
  private pageContent() {
    Scroll() {
      Column({ space: 12 }) {
        if (this.current && this.detailsOpen) {
          this.deviceDetails(this.current)
        }
        this.feedback()
        if (this.loaded && !this.current) {
          this.emptyDevices()
        }
        if (this.current?.status === 'online' && this.dashboard) {
          this.mailCard()
          this.slackCard()
        }
      }.width('100%').padding({ left: this.current ? 12 : 24, right: this.current ? 12 : 24,
        top: this.current ? 6 : 16, bottom: 24 })
    }.width('100%').constraintSize({ maxWidth: 560 }).layoutWeight(1).align(Alignment.Top)
      .scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Index, sidebarOpen: $sidebarOpen,
        header: () => { this.pageHeader() }, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

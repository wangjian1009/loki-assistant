import { CurrentDeviceState } from '../app/CurrentDeviceState'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_INSET, PANEL_RADIUS,
  PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SELECTED_SURFACE, SURFACE, TITLE_SIZE, TOUCH_SIZE } from './AppPage'

@Component
export struct DeviceStatus {
  @ObjectLink device: CurrentDeviceState

  private async openDevices(): Promise<void> {
    const result = await AppStorage.get<NavigatorManager>('lokiNavigator')?.openSection(PageName.Devices)
    if (result !== NavigationResult.Ok && result !== NavigationResult.Busy) {
      this.device.errorMessage = '无法打开设备管理，请重试'
    }
  }

  private statusText(status: string): string {
    switch (status) {
      case 'online': return '在线'
      case 'binding': return '绑定中'
      case 'binding_failed': return '绑定失败'
      default: return '离线'
    }
  }

  private statusColor(status: string): string {
    switch (status) {
      case 'online': return '#9AE2B9'
      case 'binding': return '#F5CF92'
      case 'binding_failed': return ERROR_TEXT
      default: return SECONDARY_TEXT
    }
  }

  @Builder
  private summary() {
    Button({ type: ButtonType.Normal }) {
      Row({ space: CONTENT_GAP }) {
        Text(this.device.current?.name ?? '').fontSize(TITLE_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).layoutWeight(1)
        Row({ space: 4 }) {
          Circle().width(6).height(6).fill(this.statusColor(this.device.current?.status ?? ''))
          Text(this.statusText(this.device.current?.status ?? '')).fontSize(META_SIZE)
            .fontColor(this.statusColor(this.device.current?.status ?? ''))
        }.flexShrink(0)
        SymbolGlyph(this.device.detailsOpen ? $r('sys.symbol.chevron_up') : $r('sys.symbol.chevron_down'))
          .fontSize(META_SIZE).fontColor([SECONDARY_TEXT]).flexShrink(0).accessibilityLevel('no')
      }.width('100%')
    }.width('100%').constraintSize({ minHeight: 48 }).padding({ left: PAGE_INSET, right: PAGE_INSET })
      .borderRadius(PANEL_RADIUS).backgroundColor(Color.Transparent)
      .accessibilityText(`${this.device.current?.name ?? ''}，${this.statusText(this.device.current?.status ?? '')}`)
      .accessibilityDescription(this.device.detailsOpen ? '收起设备详情' : '展开设备详情')
      .onClick(() => { this.device.detailsOpen = !this.device.detailsOpen })
  }

  @Builder
  private detailRow(label: string, value: () => string) {
    Row({ space: PAGE_INSET }) {
      Text(label).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).layoutWeight(1)
      Text(value()).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).textAlign(TextAlign.End)
        .constraintSize({ maxWidth: '64%' })
    }.width('100%').constraintSize({ minHeight: 33 }).padding({ top: 6, bottom: 6 })
  }

  @Builder
  private details() {
    Column({ space: CONTENT_GAP }) {
      Divider().color(BORDER).strokeWidth(0.5)
      Column() {
        Text('连接').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
        this.detailRow('最近检查', () => this.checkedTime())
        if (this.device.current?.status !== 'online' && this.device.current?.detail) {
          Text(this.device.current?.detail).width('100%').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
        }
      }.width('100%').alignItems(HorizontalAlign.Start)

      Divider().color(BORDER).strokeWidth(0.5)
      Column() {
        Text('设备信息').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
        this.detailRow('设备类型', () => this.device.current?.device_type === 'computer' ? '电脑' : '移动端')
        if (this.device.current?.bound_at != null) {
          Divider().color(BORDER).strokeWidth(0.5)
          this.detailRow('绑定时间', () => new Date((this.device.current?.bound_at ?? 0) * 1000).toLocaleString())
        }
        Divider().color(BORDER).strokeWidth(0.5)
        this.detailRow('设备标识', () => this.shortEndpointId())
      }.width('100%').alignItems(HorizontalAlign.Start)
    }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, bottom: PAGE_INSET })
  }

  private checkedTime(): string {
    const timestamp = this.device.current?.checked_at ?? 0
    return timestamp > 0 ? new Date(timestamp * 1000).toLocaleTimeString() : '尚未检查'
  }

  private shortEndpointId(): string {
    const endpoint = this.device.current?.endpoint_id
    return endpoint ? `${endpoint.slice(0, 8)}…${endpoint.slice(-8)}` : ''
  }

  @Builder
  private emptyDevice() {
    Column({ space: CONTENT_GAP }) {
      Stack() {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(24).height(24).fillColor(ACCENT)
      }.width(44).height(44).borderRadius(PANEL_RADIUS).backgroundColor(SELECTED_SURFACE)
      Text('暂无当前设备').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      Text('扫码添加电脑，或选择已配置的电脑。')
        .fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).textAlign(TextAlign.Center)
      Button('前往设备管理', { type: ButtonType.Normal }).constraintSize({ minHeight: TOUCH_SIZE })
        .borderRadius(PANEL_RADIUS).fontSize(BODY_SIZE).fontWeight(FontWeight.Bold).fontColor(ACCENT_TEXT)
        .backgroundColor(ACCENT).onClick(() => { this.openDevices() })
    }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, top: 24, bottom: 24 })
  }

  @Builder
  private feedback() {
    if (this.device.errorMessage) {
      Column({ space: CONTENT_GAP }) {
        Text(this.device.errorMessage).width('100%').fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
        if (!this.device.loaded) {
          Button('重新读取', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
            .constraintSize({ minHeight: TOUCH_SIZE }).backgroundColor('#523437')
            .enabled(!this.device.loading).onClick(() => { this.device.load() })
        }
      }.width('100%').padding(PAGE_INSET).alignItems(HorizontalAlign.Start)
    }
    if (this.device.loading && !this.device.loaded) {
      Row({ space: CONTENT_GAP }) {
        LoadingProgress().width(16).height(16).color(ACCENT)
        Text('正在读取当前设备').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
      }.width('100%').constraintSize({ minHeight: 48 }).padding({ left: PAGE_INSET, right: PAGE_INSET })
    }
  }

  build() {
    if (this.device.current || this.device.loaded || this.device.loading || this.device.errorMessage) {
      Column() {
        if (this.device.current) {
          this.summary()
          if (this.device.detailsOpen) {
            this.details()
          }
        } else if (this.device.loaded) {
          this.emptyDevice()
        }
        this.feedback()
      }.width('100%').backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
    }
  }
}

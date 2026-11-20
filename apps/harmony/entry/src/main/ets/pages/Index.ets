import { common } from '@kit.AbilityKit'
import { BusinessError } from '@kit.BasicServicesKit'
import { window } from '@kit.ArkUI'
import { scanBarcode, scanCore } from '@kit.ScanKit'
import { Application, ComputerDevice } from '../app/Application'

@Entry
@Component
struct Index {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private computers: ComputerDevice[] = []
  @State private working: boolean = false
  @State private loaded: boolean = false
  @State private progress: string = ''
  @State private errorMessage: string = ''
  @State private notice: string = ''
  @State private currentIndex: number = 0
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
    this.setSystemColors()
    this.application = AppStorage.get<Application>('lokiApplication')
    this.loadDevices()
  }

  onPageHide(): void {
    this.visible = false
    this.stopTimer()
  }

  aboutToDisappear(): void {
    this.visible = false
    this.stopTimer()
  }

  private setSystemColors(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext
    window.getLastWindow(context).then((mainWindow) => {
      mainWindow.setWindowSystemBarProperties({
        statusBarColor: '#2C2C2B', statusBarContentColor: '#E7E7E4',
        navigationBarColor: '#2C2C2B', navigationBarContentColor: '#AEAEAA'
      }).catch(() => {})
    }).catch(() => {})
  }

  private syncCurrentDevice(): void {
    const activeIndex = this.computers.findIndex((computer: ComputerDevice) => computer.active)
    this.currentIndex = activeIndex >= 0 ? activeIndex : Math.min(this.currentIndex, Math.max(0, this.computers.length - 1))
  }

  private stopTimer(): void {
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
  }

  private scheduleRefresh(): void {
    this.stopTimer()
    if (this.visible && this.foreground && this.computers.length > 0) {
      this.timer = setTimeout(() => { this.refreshDevices() }, this.computers.some((computer: ComputerDevice) => computer.active && computer.status === 'binding') ? 1000 : 20000)
    }
  }

  private async loadDevices(): Promise<void> {
    if (this.working || !this.application) { return }
    this.working = true
    this.progress = '正在读取设备'
    try {
      this.computers = await this.application.list()
      this.syncCurrentDevice()
      this.loaded = true
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
      this.progress = ''
    }
    if (this.loaded && this.visible) { await this.refreshDevices() }
  }

  private async refreshDevices(): Promise<void> {
    if (this.working || !this.application || !this.visible || !this.foreground) { return }
    this.stopTimer()
    this.working = true
    this.progress = '正在检查电脑状态'
    try {
      this.computers = await this.application.refresh()
      this.syncCurrentDevice()
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.working = false
      this.progress = ''
      this.scheduleRefresh()
    }
  }

  private async addDevice(): Promise<void> {
    if (this.working || !this.application) { return }
    this.stopTimer()
    this.working = true
    this.errorMessage = ''
    this.notice = ''
    this.progress = '请扫描电脑上的绑定二维码'
    let scanned = false
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext
      const result = await scanBarcode.startScanForResult(context, {
        scanTypes: [scanCore.ScanType.QR_CODE], enableMultiMode: false, enableAlbum: true
      })
      scanned = true
      this.progress = '正在绑定电脑'
      await this.application.addService(result.originalValue)
      this.notice = ''
    } catch (error) {
      if (!scanned) {
        const scanError = error as BusinessError
        if (scanError.code !== 1000500002) {
          this.errorMessage = '无法打开扫码，请检查系统扫码服务后重试'
        }
      } else {
        this.errorMessage = (error as Error).message
      }
    } finally {
      // 重新读取 Network 持有的自动绑定状态和错误。
      try {
        this.computers = await this.application.list()
        this.syncCurrentDevice()
        this.loaded = true
      } catch (error) {
        this.errorMessage = (error as Error).message
      }
      this.working = false
      this.progress = ''
      this.scheduleRefresh()
    }
    if (scanned && this.visible) { await this.refreshDevices() }
  }

  private openSettings(): void {
    this.getUIContext().getRouter().pushUrl({ url: 'pages/Settings' }).catch(() => {
      this.errorMessage = '无法打开设置，请重试'
    })
  }

  private async removeDevice(endpointId: string): Promise<void> {
    if (this.working || !this.application) { return }
    this.stopTimer()
    this.working = true
    this.progress = '正在删除设备'
    try {
      await this.application.remove(endpointId)
      this.computers = await this.application.list()
      this.syncCurrentDevice()
      this.notice = '设备已删除，请重新扫码添加'
      this.errorMessage = ''
    } catch (error) { this.errorMessage = (error as Error).message }
    finally { this.working = false; this.progress = ''; this.scheduleRefresh() }
  }

  private async selectDevice(endpointId: string): Promise<void> {
    if (this.working || !this.application) { return }
    this.working = true
    this.progress = '正在连接电脑'
    this.notice = ''
    try {
      await this.application.select(endpointId)
      this.computers = await this.application.list()
      this.syncCurrentDevice()
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally { this.working = false; this.progress = ''; this.syncCurrentDevice() }
    await this.refreshDevices()
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
    if (computer.status === 'online') { return '#A8CBA0' }
    if (computer.status === 'binding_failed') { return '#F2A7A2' }
    if (computer.status === 'binding') { return '#D8BE87' }
    return '#AEAEAA'
  }

  private async changeDevice(index: number): Promise<void> {
    if (this.working || index < 0 || index >= this.computers.length) { return }
    this.currentIndex = index
    const computer = this.computers[index]
    if (!computer.active) { await this.selectDevice(computer.endpoint_id) }
  }

  private checkedTime(computer: ComputerDevice): string {
    return computer.checked_at > 0 ? new Date(computer.checked_at * 1000).toLocaleTimeString() : '尚未检查'
  }

  @Builder
  private sectionTitle(title: string) {
    Text(title).fontSize(14).fontColor('#AEAEAA').width('100%')
      .padding({ left: 14, top: 26, bottom: 16 })
  }

  @Builder
  private monitorRow(label: string, value: string, color: string) {
    Row({ space: 16 }) {
      Text(label).fontSize(16).fontColor('#E7E7E4').layoutWeight(1)
      Text(value).fontSize(14).fontColor(color).maxLines(2)
        .textAlign(TextAlign.End).constraintSize({ maxWidth: '62%' })
    }.width('100%').constraintSize({ minHeight: 58 }).padding({ left: 18, right: 18, top: 12, bottom: 12 })
  }

  @Builder
  private deviceHeader(computer: ComputerDevice) {
    Row({ space: 16 }) {
      Image($r('sys.media.ohos_ic_public_device_matebook')).width(36).height(36).fillColor('#BCA5E5')
      Column({ space: 8 }) {
        Text(computer.name).fontSize(21).fontWeight(FontWeight.Medium).fontColor('#E7E7E4')
          .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%')
        Row({ space: 8 }) {
          Text(computer.device_type === 'computer' ? '电脑' : '移动端').fontSize(13).fontColor('#AEAEAA')
          Text('·').fontSize(13).fontColor('#777C75')
          Circle().width(6).height(6).fill(this.statusColor(computer))
          Text(this.statusText(computer)).fontSize(13).fontColor(this.statusColor(computer))
        }
      }.layoutWeight(1).alignItems(HorizontalAlign.Start)
    }.width('100%').height(102).padding({ left: 20, right: 20 })
      .backgroundColor('#333633').borderRadius(18)
  }

  @Builder
  private devicePage(computer: ComputerDevice) {
    Column() {
      // 设备概览固定在页顶；后续监控项统一加入下方滚动区域。
      this.deviceHeader(computer)
      if (!computer.active) {
        Button('使用此电脑', { type: ButtonType.Capsule }).fontSize(14).fontColor('#BCA5E5')
          .backgroundColor('#3D3748').height(40).margin({ top: 12 })
          .enabled(!this.working).onClick(() => { this.selectDevice(computer.endpoint_id) })
      }
      Scroll() {
        Column() {
          this.sectionTitle('运行状态')
          Column() {
            this.monitorRow('连接状态', this.statusText(computer), this.statusColor(computer))
            Divider().color('#444643').strokeWidth(0.5)
            this.monitorRow('最近检查', this.checkedTime(computer), '#AEAEAA')
          }.width('100%').backgroundColor('#333633').borderRadius(18).clip(true)
          Text(computer.detail).fontSize(13).fontColor(computer.status === 'binding_failed' ? '#F2A7A2' : '#8F938D')
            .width('100%').padding({ left: 14, right: 14, top: 12 }).lineHeight(21)

          if (computer.status === 'binding_failed') {
            Button('删除设备', { type: ButtonType.Capsule }).fontSize(15).fontColor('#F2A7A2')
              .backgroundColor('#493837').height(44).width('100%').margin({ top: 16 })
              .enabled(!this.working).onClick(() => { this.removeDevice(computer.endpoint_id) })
          }
          this.sectionTitle('设备信息')
          Column() {
            this.monitorRow('设备类型', computer.device_type === 'computer' ? '电脑' : '移动端', '#AEAEAA')
            Divider().color('#444643').strokeWidth(0.5)
            this.monitorRow('设备标识', `${computer.endpoint_id.slice(0, 12)}…${computer.endpoint_id.slice(-8)}`, '#AEAEAA')
            if (computer.bound_at !== null) {
              Divider().color('#444643').strokeWidth(0.5)
              this.monitorRow('绑定时间', new Date(computer.bound_at * 1000).toLocaleString(), '#AEAEAA')
            }
          }.width('100%').backgroundColor('#333633').borderRadius(18).clip(true)
        }.width('100%').padding({ bottom: 40 }).alignItems(HorizontalAlign.Start)
      }.width('100%').layoutWeight(1).align(Alignment.Top).scrollBar(BarState.Auto).scrollBarColor('#656862').scrollBarWidth(3)
    }.width('100%').height('100%').padding({ left: 18, right: 18 })
  }

  @Builder
  private emptyDevices() {
    Column({ space: 20 }) {
      Image($r('sys.media.ohos_ic_public_device_matebook')).width(64).height(64).fillColor('#BCA5E5')
      Text('还没有绑定电脑').fontSize(21).fontWeight(FontWeight.Medium).fontColor('#E7E7E4')
      Text('在电脑上新增设备，\n扫描二维码，让手机与电脑连接。')
        .fontSize(14).fontColor('#AEAEAA').textAlign(TextAlign.Center).lineHeight(24)
      Button('增加设备 · 扫码绑定', { type: ButtonType.Capsule })
        .fontSize(15).fontWeight(FontWeight.Medium).fontColor('#292331').backgroundColor('#BCA5E5')
        .height(46).padding({ left: 24, right: 24 }).margin({ top: 8 })
        .enabled(!this.working).onClick(() => { this.addDevice() })
    }.width('100%').padding({ left: 24, right: 24, top: 44, bottom: 44 })
      .backgroundColor('#333633').borderRadius(18)
  }

  build() {
    Column() {
      Row({ space: 12 }) {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(28).height(28).fillColor('#BCA5E5')
        Text('设备').fontSize(28).fontWeight(FontWeight.Medium).fontColor('#E7E7E4').layoutWeight(1)
        Button('设置', { type: ButtonType.Normal }).fontSize(15).fontColor('#AEAEAA')
          .backgroundColor(Color.Transparent).padding({ left: 12, right: 0 })
          .enabled(!this.working).onClick(() => { this.openSettings() })
      }.width('100%').height(92).padding({ left: 20, right: 24 })
      Divider().color('#414240').strokeWidth(0.5)

      Row({ space: 10 }) {
        Text('我的设备').fontSize(14).fontColor('#AEAEAA')
        if (this.computers.length > 0) {
          Text(`${this.currentIndex + 1} / ${this.computers.length}`).fontSize(12).fontColor('#8F938D')
        }
        Blank()
        if (this.computers.length > 0) {
          Button('刷新', { type: ButtonType.Normal }).fontSize(14).fontColor('#AEAEAA')
            .backgroundColor(Color.Transparent).enabled(!this.working).onClick(() => { this.refreshDevices() })
          Button('添加设备', { type: ButtonType.Capsule }).fontSize(14)
            .fontColor('#BCA5E5').backgroundColor('#3D3748').height(36)
            .enabled(!this.working).onClick(() => { this.addDevice() })
        }
      }.height(72).width('100%').padding({ left: 32, right: 18 })

      if (this.errorMessage !== '') {
        Row({ space: 12 }) {
          Text(this.errorMessage).fontSize(13).fontColor('#F2A7A2').layoutWeight(1)
          if (!this.loaded) {
            Button('重新读取').fontSize(13).fontColor('#F2A7A2').backgroundColor(Color.Transparent)
              .enabled(!this.working).onClick(() => { this.loadDevices() })
          }
        }.width('100%').padding({ left: 24, right: 24, bottom: 14 })
      }
      if (this.notice !== '') {
        Text(this.notice).fontSize(13).fontColor('#B5CFAE').width('100%')
          .padding({ left: 32, right: 24, bottom: 14 })
      }
      if (this.working) {
        Row({ space: 10 }) {
          LoadingProgress().width(16).height(16).color('#BCA5E5')
          Text(this.progress).fontSize(13).fontColor('#AEAEAA')
        }.width('100%').padding({ left: 32, right: 24, bottom: 14 })
      }

      if (this.loaded && this.computers.length === 0) {
        Column() { this.emptyDevices() }
          .width('100%').layoutWeight(1).padding({ left: 18, right: 18 }).alignItems(HorizontalAlign.Start)
      } else if (this.computers.length > 0) {
        if (this.computers.length > 1) {
          Text('左右滑动切换设备').fontSize(12).fontColor('#8F938D')
            .width('100%').padding({ left: 32, bottom: 12 })
        }
        Swiper() {
          // 显示数据变化时重建设备页，避免沿用旧的连接状态。
          ForEach(this.computers, (computer: ComputerDevice) => {
            this.devicePage(computer)
          }, (computer: ComputerDevice) => JSON.stringify(computer))
        }.width('100%').layoutWeight(1).index(this.currentIndex).loop(false).indicator(false)
          .disableSwipe(this.working).onChange((index: number) => { this.changeDevice(index) })
      } else { Blank() }
    }.width('100%').height('100%').backgroundColor('#2C2C2B')
      .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.BOTTOM])
  }
}

import { common } from '@kit.AbilityKit'
import { BusinessError } from '@kit.BasicServicesKit'
import { scanBarcode, scanCore } from '@kit.ScanKit'
import { Application, ComputerDevice } from '../app/Application'
import { StatusSubscription } from '../workspace/ServiceStatus'
import { NavigatorManager } from '../app/NavigatorManager'
import { deviceConnectionAppearance } from '../components/DeviceConnection'
import { ACCENT, ACCENT_TEXT, AppPage, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_INSET,
  PAGE_MAX_WIDTH, PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SELECTED_SURFACE, SURFACE,
  TOUCH_SIZE, setSystemColors } from '../components/AppPage'

@Entry
@Component
struct Devices {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private computers: ComputerDevice[] = []
  @State private loaded: boolean = false
  @State private working: boolean = false
  @State private progress: string = ''
  @State private errorMessage: string = ''
  private application?: Application
  private visible: boolean = false
  private subscription?: StatusSubscription
  private subscriptionVersion: number = 0
  private watching: boolean = false

  onPageShow(): void {
    this.visible = true
    setSystemColors(this.getUIContext().getHostContext() as common.UIAbilityContext)
    this.application = AppStorage.get<Application>('lokiApplication')
    this.loadDevices()
  }

  onPageHide(): void {
    this.visible = false
    this.stopWatching()
  }

  aboutToDisappear(): void {
    this.onPageHide()
  }

  onBackPress(): boolean {
    this.goBack()
    return true
  }

  private goBack(): void {
    AppStorage.get<NavigatorManager>('lokiNavigator')?.back()
  }

  private onForegroundChanged(): void {
    if (this.foreground && this.visible) {
      this.loadDevices()
    } else {
      this.stopWatching()
    }
  }

  private stopWatching(): void {
    this.subscriptionVersion += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.watching = false
  }

  private async loadDevices(): Promise<void> {
    if (this.watching || this.subscription || !this.visible || !this.foreground) { return }
    const application = this.application
    if (!application) {
      this.errorMessage = '应用尚未准备好，请重试'
      return
    }
    const version = ++this.subscriptionVersion
    this.watching = true
    try {
      const subscription = await application.watchDevices((devices: ComputerDevice[]) => {
        if (version === this.subscriptionVersion) {
          this.computers = devices
          this.loaded = true
        }
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
      if (version === this.subscriptionVersion) { this.watching = false }
    }
  }

  private async updateDevices(progress: string, action: (application: Application) => Promise<void>): Promise<void> {
    const application = this.application
    if (this.working || !application) return
    this.working = true
    this.progress = progress
    this.errorMessage = ''
    try {
      await action(application)
      this.computers = await application.list()
      this.loaded = true
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally {
      this.progress = ''
      this.working = false
    }
  }

  private async addDevice(): Promise<void> {
    await this.updateDevices('请扫描电脑上的绑定二维码', async (application: Application): Promise<void> => {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext
      let result: scanBarcode.ScanResult
      try {
        result = await scanBarcode.startScanForResult(context, {
          scanTypes: [scanCore.ScanType.QR_CODE], enableMultiMode: false, enableAlbum: true
        })
      } catch (error) {
        if ((error as BusinessError).code === 1000500002) return
        throw new Error('无法打开扫码，请检查系统扫码服务后重试')
      }
      this.progress = '正在配置电脑'
      await application.addService(result.originalValue)
    })
  }

  @Builder
  private computerRow(computer: ComputerDevice) {
    Column({ space: 4 }) {
      Row({ space: CONTENT_GAP }) {
        Stack() {
          Image($r('sys.media.ohos_ic_public_device_matebook')).width(18).height(18).fillColor(ACCENT)
        }.width(32).height(32).borderRadius(8).backgroundColor(SELECTED_SURFACE).flexShrink(0)
        Column({ space: 2 }) {
          Text(computer.name).fontSize(BODY_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
            .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%')
          Text(`${computer.endpoint_id.slice(0, 8)}…${computer.endpoint_id.slice(-8)}`)
            .fontSize(META_SIZE).fontColor(SECONDARY_TEXT).maxLines(1)
            .textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%')
        }.layoutWeight(1).alignItems(HorizontalAlign.Start)
        if (computer.active || computer.status === 'binding') {
          Column({ space: 2 }) {
            if (computer.active) {
              Text('当前设备').fontSize(META_SIZE).fontWeight(FontWeight.Medium).fontColor(ACCENT)
            }
            Text(deviceConnectionAppearance(computer.status).label).fontSize(META_SIZE)
              .fontColor(deviceConnectionAppearance(computer.status).color)
          }.alignItems(HorizontalAlign.End).flexShrink(0)
        } else if (computer.status !== 'binding_failed') {
          Button('设为当前设备', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ACCENT)
            .constraintSize({ minHeight: TOUCH_SIZE, maxWidth: '40%' })
            .padding({ left: CONTENT_GAP, right: CONTENT_GAP }).borderRadius(8)
            .backgroundColor(SELECTED_SURFACE).enabled(!this.working)
            .onClick(() => {
              this.updateDevices('正在切换设备', (application: Application) => application.select(computer.endpoint_id))
            })
        }
      }.width('100%').constraintSize({ minHeight: TOUCH_SIZE })
      if (computer.status === 'binding_failed') {
        Text(computer.detail).fontSize(BODY_SIZE).fontColor(ERROR_TEXT).width('100%')
        Button('删除设备', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
          .constraintSize({ minHeight: TOUCH_SIZE }).borderRadius(8).backgroundColor('#523437').enabled(!this.working)
          .onClick(() => {
            this.updateDevices('正在删除设备', (application: Application) => application.remove(computer.endpoint_id))
          })
      }
    }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, top: 6, bottom: 6 })
      .alignItems(HorizontalAlign.Start).backgroundColor(computer.active ? '#222735' : SURFACE)
  }

  @Builder
  private pageContent() {
    Scroll() {
      Column({ space: CONTENT_GAP }) {
        if (this.progress !== '') {
          Row({ space: CONTENT_GAP }) {
            LoadingProgress().width(16).height(16).color(ACCENT)
            Text(this.progress).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
          }.width('100%')
        }
        if (this.errorMessage !== '') {
          Column({ space: CONTENT_GAP }) {
            Text(this.errorMessage).fontSize(BODY_SIZE).fontColor(ERROR_TEXT).width('100%')
            if (!this.loaded) {
              Button('重新读取').fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
                .constraintSize({ minHeight: TOUCH_SIZE }).backgroundColor('#523437')
                .enabled(!this.working).onClick(() => { this.loadDevices() })
            }
          }.width('100%').padding(PAGE_INSET).alignItems(HorizontalAlign.Start)
            .backgroundColor('#362529').borderRadius(PANEL_RADIUS)
        }
        if (this.loaded) {
          Text('电脑列表').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          if (this.computers.length === 0) {
            Column({ space: CONTENT_GAP }) {
              Text('还没有配置电脑').fontSize(BODY_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
              Text('在电脑上生成绑定二维码后扫码。')
                .fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
            }.width('100%').padding(PAGE_INSET).alignItems(HorizontalAlign.Start)
              .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
          } else {
            Column() {
              ForEach(this.computers, (computer: ComputerDevice, index: number) => {
                if (index > 0) {
                  Divider().color(BORDER).strokeWidth(0.5)
                }
                this.computerRow(computer)
              }, (computer: ComputerDevice, index: number) =>
                JSON.stringify([index, computer.endpoint_id, computer.name, computer.active, computer.status, computer.detail]))
            }.width('100%').borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER }).clip(true)
          }
        }
      }.width('100%').padding(PAGE_INSET).alignItems(HorizontalAlign.Start)
    }.width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH }).layoutWeight(1).align(Alignment.Top)
      .scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)

    Column() {
      Divider().color(BORDER).strokeWidth(0.5)
      Button('扫码添加设备', { type: ButtonType.Normal }).width('100%')
        .constraintSize({ minHeight: TOUCH_SIZE }).borderRadius(PANEL_RADIUS)
        .fontSize(BODY_SIZE).fontWeight(FontWeight.Medium).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
        .margin({ top: CONTENT_GAP }).enabled(this.loaded && !this.working).onClick(() => { this.addDevice() })
    }.width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH })
      .padding({ left: PAGE_INSET, right: PAGE_INSET, bottom: PAGE_INSET })
  }

  build() {
    Stack() {
      AppPage({ title: '设备管理', onBack: () => { this.goBack() }, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

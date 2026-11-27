import { common } from '@kit.AbilityKit'
import { BusinessError } from '@kit.BasicServicesKit'
import { scanBarcode, scanCore } from '@kit.ScanKit'
import { Application, ComputerDevice } from '../app/Application'
import { StatusSubscription } from '../workspace/ServiceStatus'
import { NavigatorManager, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BORDER, ERROR_TEXT, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE,
  setSystemColors } from '../components/AppPage'

@Entry
@Component
struct Devices {
  @StorageLink('lokiForeground') @Watch('onForegroundChanged') private foreground: boolean = true
  @State private sidebarOpen: boolean = false
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
    this.sidebarOpen = false
    this.stopWatching()
  }

  aboutToDisappear(): void {
    this.onPageHide()
  }

  onBackPress(): boolean {
    if (this.sidebarOpen) {
      this.sidebarOpen = false
    } else {
      AppStorage.get<NavigatorManager>('lokiNavigator')?.back()
    }
    return true
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
  private computerCard(computer: ComputerDevice) {
    Column({ space: 12 }) {
      Row({ space: 12 }) {
        Stack() {
          Image($r('sys.media.ohos_ic_public_device_matebook')).width(22).height(22).fillColor(ACCENT)
        }.width(44).height(44).borderRadius(12).backgroundColor('#303951')
        Column({ space: 4 }) {
          Text(computer.name).fontSize(17).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
            .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis }).width('100%')
          Text(`${computer.endpoint_id.slice(0, 8)}…${computer.endpoint_id.slice(-8)}`)
            .fontSize(12).fontColor(SECONDARY_TEXT)
        }.layoutWeight(1).alignItems(HorizontalAlign.Start)
        if (computer.active) {
          Text('当前设备').fontSize(12).fontWeight(FontWeight.Medium).fontColor(ACCENT)
            .padding({ left: 8, right: 8, top: 5, bottom: 5 }).borderRadius(8).backgroundColor('#303951')
        }
      }.width('100%')
      if (computer.status === 'binding_failed') {
        Text(computer.detail).fontSize(14).lineHeight(22).fontColor(ERROR_TEXT).width('100%')
        Button('删除设备', { type: ButtonType.Normal }).fontSize(14).fontColor(ERROR_TEXT)
          .height(44).borderRadius(12).backgroundColor('#523437').enabled(!this.working)
          .onClick(() => {
            this.updateDevices('正在删除设备', (application: Application) => application.remove(computer.endpoint_id))
          })
      } else if (computer.status === 'binding') {
        Text('绑定中').fontSize(14).fontColor('#F5CF92')
      } else if (!computer.active) {
        Button('设为当前设备', { type: ButtonType.Normal }).fontSize(14).fontColor(ACCENT)
          .height(44).borderRadius(12).backgroundColor('#303951').enabled(!this.working)
          .onClick(() => {
            this.updateDevices('正在切换设备', (application: Application) => application.select(computer.endpoint_id))
          })
      }
    }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
      .backgroundColor(computer.active ? '#222735' : SURFACE).borderRadius(16)
      .border({ width: 1, color: computer.active ? '#515F80' : BORDER })
  }

  @Builder
  private pageContent() {
    Scroll() {
      Column({ space: 12 }) {
        if (this.progress !== '') {
          Row({ space: 10 }) {
            LoadingProgress().width(16).height(16).color(ACCENT)
            Text(this.progress).fontSize(14).fontColor(SECONDARY_TEXT)
          }.width('100%')
        }
        if (this.errorMessage !== '') {
          Column({ space: 8 }) {
            Text(this.errorMessage).fontSize(14).lineHeight(22).fontColor(ERROR_TEXT).width('100%')
            if (!this.loaded) {
              Button('重新读取').fontSize(14).fontColor(ERROR_TEXT).height(44).backgroundColor('#523437')
                .enabled(!this.working).onClick(() => { this.loadDevices() })
            }
          }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
            .backgroundColor('#362529').borderRadius(16)
        }
        if (this.loaded) {
          Text('电脑列表').fontSize(18).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
          if (this.computers.length === 0) {
            Column({ space: 8 }) {
              Text('还没有配置电脑').fontSize(17).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
              Text('在电脑上新增设备，再扫描电脑显示的绑定二维码。')
                .fontSize(14).lineHeight(22).fontColor(SECONDARY_TEXT)
            }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
              .backgroundColor(SURFACE).borderRadius(16).border({ width: 1, color: BORDER })
          } else {
            ForEach(this.computers, (computer: ComputerDevice) => {
              this.computerCard(computer)
            }, (computer: ComputerDevice) =>
              JSON.stringify([computer.endpoint_id, computer.name, computer.active, computer.status, computer.detail]))
          }
        }
      }.width('100%').padding({ left: 24, right: 24, top: 8, bottom: 24 }).alignItems(HorizontalAlign.Start)
    }.width('100%').constraintSize({ maxWidth: 560 }).layoutWeight(1).align(Alignment.Top)
      .scrollBar(BarState.Auto).scrollBarColor('#4D576B').scrollBarWidth(3)

    Column() {
      Divider().color(BORDER)
      Button('扫码添加设备', { type: ButtonType.Normal }).width('100%').height(52).borderRadius(16)
        .fontSize(16).fontWeight(FontWeight.Bold).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
        .margin({ top: 12 }).enabled(this.loaded && !this.working).onClick(() => { this.addDevice() })
    }.width('100%').constraintSize({ maxWidth: 560 }).padding({ left: 24, right: 24, bottom: 12 })
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Devices, sidebarOpen: $sidebarOpen, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
  }
}

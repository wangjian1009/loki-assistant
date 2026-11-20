import { common } from '@kit.AbilityKit'
import { BusinessError } from '@kit.BasicServicesKit'
import { scanBarcode, scanCore } from '@kit.ScanKit'
import { ComputerDevice, DeviceManager } from '../devices/DeviceManager'
import { Workspace } from '../workspace/Workspace'

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
  private manager?: DeviceManager
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
    this.manager = AppStorage.get<Workspace>('lokiWorkspace')?.devices
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

  private stopTimer(): void {
    if (this.timer !== -1) {
      clearTimeout(this.timer)
      this.timer = -1
    }
  }

  private scheduleRefresh(): void {
    this.stopTimer()
    if (this.visible && this.foreground && this.computers.length > 0) {
      this.timer = setTimeout(() => { this.refreshDevices() }, 20000)
    }
  }

  private async loadDevices(): Promise<void> {
    if (this.working || !this.manager) { return }
    this.working = true
    this.progress = '正在读取设备'
    try {
      this.computers = await this.manager.list()
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
    if (this.working || !this.manager || !this.visible || !this.foreground) { return }
    this.stopTimer()
    this.working = true
    this.progress = '正在检查电脑状态'
    try {
      this.computers = await this.manager.refresh()
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
    if (this.working || !this.manager) { return }
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
      await this.manager.bind(result.originalValue)
      this.notice = '电脑绑定成功'
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
      // 响应丢失时也读取预先保存的记录，允许刷新确认绑定结果。
      try {
        this.computers = await this.manager.list()
        this.loaded = true
      } catch (error) {
        this.errorMessage = (error as Error).message
      }
      this.working = false
      this.progress = ''
      this.scheduleRefresh()
    }
    if (this.notice !== '' && this.visible) { await this.refreshDevices() }
  }

  private async selectDevice(endpointId: string): Promise<void> {
    if (this.working || !this.manager) { return }
    this.working = true
    try {
      await this.manager.select(endpointId)
      this.computers = await this.manager.list()
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    } finally { this.working = false }
    await this.refreshDevices()
  }

  private statusText(computer: ComputerDevice): string {
    switch (computer.status) {
      case 'inactive': return '未选用'
      case 'online': return '在线'
      case 'unreachable': return '暂时无法连接'
      case 'unbound': return '已解除绑定'
      case 'pending': return '绑定待确认'
      default: return '等待检查'
    }
  }

  @Builder
  private deviceCard(computer: ComputerDevice) {
    Column({ space: 12 }) {
      Row({ space: 12 }) {
        Image($r('sys.media.ohos_ic_public_device_matebook')).width(36).height(36).fillColor('#176443')
        Column({ space: 4 }) {
          Text(computer.bound_at === null ? '待绑定电脑' : computer.name)
            .fontSize(18).fontWeight(FontWeight.Medium)
          Text(this.statusText(computer)).fontSize(14)
            .fontColor(computer.status === 'online' ? '#176443' : '#6B7280')
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        Circle().width(9).height(9).fill(computer.status === 'online' ? '#29976B' : '#A6ADB7')
      }
      if (!computer.active) {
        Button('使用此电脑').enabled(!this.working).onClick(() => { this.selectDevice(computer.endpoint_id) })
      }
      Text(computer.detail).fontSize(14).fontColor('#6B7280').width('100%')
      Text(`设备标识  ${computer.endpoint_id.slice(0, 12)}…${computer.endpoint_id.slice(-8)}`)
        .fontSize(12).fontColor('#8A919D').width('100%')
      if (computer.bound_at !== null) {
        Text(`绑定于 ${new Date(computer.bound_at * 1000).toLocaleString()}`)
          .fontSize(12).fontColor('#8A919D').width('100%')
      }
      if (computer.checked_at > 0) {
        Text(`检查于 ${new Date(computer.checked_at * 1000).toLocaleTimeString()}`)
          .fontSize(12).fontColor('#8A919D').width('100%')
      }
    }
    .padding(20).width('100%').alignItems(HorizontalAlign.Start)
    .backgroundColor(Color.White).borderRadius(20)
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        Column({ space: 8 }) {
          Text('Loki 执行中心').fontSize(30).fontWeight(FontWeight.Bold).fontColor('#182D25')
          Text('连接电脑，让手机与电脑协同工作').fontSize(15).fontColor('#6B7280')
        }.width('100%').alignItems(HorizontalAlign.Start).margin({ top: 24, bottom: 12 })

        Row() {
          Text('设备管理').fontSize(22).fontWeight(FontWeight.Bold).layoutWeight(1)
          if (this.computers.length > 0) {
            Button('刷新', { type: ButtonType.Normal }).fontColor('#176443')
              .backgroundColor(Color.Transparent).enabled(!this.working)
              .onClick(() => { this.refreshDevices() })
            Button('＋ 增加', { type: ButtonType.Capsule }).fontSize(14).fontColor(Color.White)
              .backgroundColor('#176443').enabled(!this.working).onClick(() => { this.addDevice() })
          }
        }.width('100%')

        if (this.errorMessage !== '') {
          Column({ space: 8 }) {
            Text(this.errorMessage).fontSize(14).fontColor('#AB3838').width('100%')
            if (!this.loaded) {
              Button('重新读取').onClick(() => { this.loadDevices() }).enabled(!this.working)
            }
          }.padding(16).width('100%').backgroundColor('#FFF0EE').borderRadius(14)
        }
        if (this.notice !== '') {
          Text(this.notice).fontSize(14).fontColor('#176443').width('100%')
        }
        if (this.working) {
          Row({ space: 10 }) {
            LoadingProgress().width(20).height(20).color('#176443')
            Text(this.progress).fontSize(14).fontColor('#6B7280')
          }.width('100%')
        }

        if (this.loaded && this.computers.length === 0) {
          Column({ space: 16 }) {
            Image($r('sys.media.ohos_ic_public_device_matebook')).width(64).height(64).fillColor('#83A594')
            Text('还没有绑定电脑').fontSize(20).fontWeight(FontWeight.Medium).fontColor('#182D25')
            Text('在电脑的设备管理中点击“新增设备”，\n再扫描二维码完成绑定。')
              .fontSize(14).fontColor('#6B7280').textAlign(TextAlign.Center).lineHeight(22)
            Button('增加设备 · 扫码绑定', { type: ButtonType.Capsule })
              .fontColor(Color.White).backgroundColor('#176443').padding({ left: 24, right: 24 })
              .height(48).enabled(!this.working).onClick(() => { this.addDevice() })
          }
          .width('100%').padding({ top: 44, bottom: 44, left: 20, right: 20 })
          .backgroundColor(Color.White).borderRadius(24)
        } else {
          ForEach(this.computers, (computer: ComputerDevice) => {
            this.deviceCard(computer)
          }, (computer: ComputerDevice) => computer.endpoint_id)
        }
      }.padding(24).width('100%').alignItems(HorizontalAlign.Start)
    }.width('100%').height('100%').backgroundColor('#F3F6F4').align(Alignment.Top)
  }
}

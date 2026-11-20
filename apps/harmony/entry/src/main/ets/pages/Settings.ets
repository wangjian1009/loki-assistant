import { bundleManager, common } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { Application } from '../app/Application'

@Entry
@Component
struct Settings {
  @State private deviceName: string = ''
  @State private draftName: string = ''
  @State private version: string = '—'
  @State private editing: boolean = false
  @State private working: boolean = false
  @State private loaded: boolean = false
  @State private errorMessage: string = ''
  @State private editError: string = ''
  @State private notice: string = ''
  private application?: Application

  onPageShow(): void {
    this.setSystemColors()
    this.application = AppStorage.get<Application>('lokiApplication')
    this.loadSettings()
    bundleManager.getBundleInfoForSelf(bundleManager.BundleFlag.GET_BUNDLE_INFO_DEFAULT)
      .then((info) => { this.version = info.versionName }).catch(() => {})
  }

  private setSystemColors(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext
    window.getLastWindow(context).then((mainWindow) => {
      mainWindow.setWindowSystemBarProperties({
        statusBarColor: '#2C2C2B',
        statusBarContentColor: '#E7E7E4',
        navigationBarColor: '#2C2C2B',
        navigationBarContentColor: '#AEAEAA'
      }).catch(() => {})
    }).catch(() => {})
  }

  private async loadSettings(): Promise<void> {
    if (this.working) { return }
    if (!this.application) {
      this.errorMessage = '应用尚未准备好，请返回首页后重试'
      return
    }
    this.working = true
    try {
      this.deviceName = await this.application.deviceName()
      this.loaded = true
      this.errorMessage = ''
    } catch (error) { this.errorMessage = (error as Error).message }
    finally { this.working = false }
  }

  private editName(): void {
    this.draftName = this.deviceName
    this.editError = ''
    this.notice = ''
    this.editing = true
  }

  private async saveName(): Promise<void> {
    if (this.working || !this.application || !this.loaded) { return }
    this.working = true
    this.editError = ''
    try {
      await this.application.setDeviceName(this.draftName)
      this.deviceName = await this.application.deviceName()
      this.notice = '手机名称已保存'
      this.editing = false
    } catch (error) { this.editError = (error as Error).message }
    finally { this.working = false }
  }

  @Builder
  private nameEditor() {
    Column({ space: 20 }) {
      Row({ space: 12 }) {
        Text('手机名称').fontSize(22).fontWeight(FontWeight.Medium).fontColor('#E7E7E4').layoutWeight(1)
        Button('取消', { type: ButtonType.Normal }).fontSize(15).fontColor('#AEAEAA')
          .backgroundColor(Color.Transparent).enabled(!this.working)
          .onClick(() => { this.editing = false })
        Button(this.working ? '保存中' : '保存', { type: ButtonType.Capsule })
          .height(36).fontSize(15).fontWeight(FontWeight.Medium)
          .fontColor('#292331').backgroundColor('#BCA5E5').enabled(!this.working)
          .onClick(() => { this.saveName() })
      }.width('100%')
      TextInput({ text: this.draftName, placeholder: '请输入手机名称' })
        .maxLength(128).enabled(!this.working).height(52)
        .fontSize(18).fontColor('#E7E7E4').placeholderColor('#969894')
        .backgroundColor('#414341').borderRadius(12).caretColor('#BCA5E5')
        .onChange((value: string) => { this.draftName = value; this.editError = '' })
        .enterKeyType(EnterKeyType.Done).onSubmit(() => { this.saveName() })
      Text('用于在电脑上显示手机名称，保存后将在下次绑定时使用。')
        .fontSize(14).fontColor('#AEAEAA').lineHeight(22).width('100%')
      if (this.editError !== '') {
        Text(this.editError).fontSize(14).fontColor('#F2A7A2').width('100%')
      }
    }.padding({ left: 24, right: 24, top: 12, bottom: 24 }).width('100%')
  }

  @Builder
  private sectionTitle(title: string) {
    Text(title).fontSize(14).fontColor('#AEAEAA')
      .width('100%').padding({ left: 14, top: 30, bottom: 18 })
  }

  build() {
    Column() {
      Row({ space: 12 }) {
        SymbolGlyph($r('sys.symbol.gearshape_fill')).fontSize(28).fontColor(['#BCA5E5'])
        Text('设置').fontSize(28).fontWeight(FontWeight.Medium).fontColor('#E7E7E4').layoutWeight(1)
        Button('返回', { type: ButtonType.Normal }).fontSize(15).fontColor('#AEAEAA')
          .backgroundColor(Color.Transparent).padding({ left: 12, right: 0 })
          .onClick(() => { this.getUIContext().getRouter().back() })
      }.width('100%').height(92).padding({ left: 20, right: 24 })
      Divider().color('#414240').strokeWidth(0.5)

      Scroll() {
        Column() {
          this.sectionTitle('设备')
          Column() {
            Row({ space: 16 }) {
              Image($r('sys.media.ohos_ic_public_edit')).width(22).height(22).fillColor('#AAAFA9')
              Text('手机名称').fontSize(18).fontColor('#E7E7E4').layoutWeight(1)
              Text(this.loaded ? this.deviceName : '正在读取').fontSize(15).fontColor('#AEAEAA')
                .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).constraintSize({ maxWidth: '45%' })
              SymbolGlyph($r('sys.symbol.chevron_right')).fontSize(13).fontColor(['#AEAEAA'])
            }.height(68).width('100%').padding({ left: 18, right: 20 })
              .enabled(this.loaded && !this.working).onClick(() => { this.editName() })
              .accessibilityGroup(true).accessibilityText(`手机名称，${this.deviceName}，点击修改`)
            Divider().color('#444643').strokeWidth(0.5)
            Row({ space: 16 }) {
              Image($r('sys.media.ohos_ic_public_device_phone')).width(22).height(22).fillColor('#AAAFA9')
              Text('设备类型').fontSize(18).fontColor('#E7E7E4').layoutWeight(1)
              Text('移动端').fontSize(15).fontColor('#AEAEAA')
            }.height(68).width('100%').padding({ left: 18, right: 20 })
          }.width('100%').backgroundColor('#333633').borderRadius(18).clip(true)
          Text('名称将在下次绑定时展示给电脑。').fontSize(12).fontColor('#8F938D')
            .width('100%').padding({ left: 14, top: 12 })

          this.sectionTitle('应用程序')
          Row({ space: 16 }) {
            SymbolGlyph($r('sys.symbol.info_circle')).fontSize(22).fontColor(['#AAAFA9'])
            Text('应用版本').fontSize(18).fontColor('#E7E7E4').layoutWeight(1)
            Text(this.version).fontSize(15).fontColor('#AEAEAA')
          }.height(68).width('100%').padding({ left: 18, right: 20 })
            .backgroundColor('#333633').borderRadius(18)

          if (this.working && !this.loaded) {
            LoadingProgress().width(24).height(24).color('#BCA5E5').margin({ top: 24 })
          }
          if (this.errorMessage !== '') {
            Text(this.errorMessage).fontSize(14).fontColor('#F2A7A2').width('100%').margin({ top: 20 })
            if (!this.loaded) {
              Button('重新读取').fontColor('#BCA5E5').backgroundColor('#333633')
                .enabled(!this.working).onClick(() => { this.loadSettings() })
            }
          }
          if (this.notice !== '') {
            Text(this.notice).fontSize(14).fontColor('#B5CFAE').width('100%').padding({ left: 14, top: 20 })
          }
        }.width('100%').padding({ left: 18, right: 18, bottom: 36 }).alignItems(HorizontalAlign.Start)
      }.layoutWeight(1).width('100%').align(Alignment.Top).scrollBar(BarState.Auto)
    }.width('100%').height('100%').backgroundColor('#2C2C2B')
      .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.BOTTOM])
      .bindSheet($$this.editing, this.nameEditor(), {
        height: SheetSize.MEDIUM, backgroundColor: '#333633',
        showClose: false
      })
  }
}

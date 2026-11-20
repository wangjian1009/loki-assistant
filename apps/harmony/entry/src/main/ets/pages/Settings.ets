import { bundleManager, common } from '@kit.AbilityKit'
import { Application } from '../app/Application'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BORDER, ERROR_TEXT, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE,
  setSystemColors } from '../components/AppPage'

@Entry
@Component
struct Settings {
  @State private sidebarOpen: boolean = false
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
  private stopWatching?: () => void

  onPageShow(): void {
    setSystemColors(this.getUIContext().getHostContext() as common.UIAbilityContext)
    this.application = AppStorage.get<Application>('lokiApplication')
    this.stopWatching?.()
    this.stopWatching = AppStorage.get<NavigatorManager>('lokiNavigator')
      ?.watchLeaveGuard((): boolean => !this.editing)
    this.loadSettings()
    bundleManager.getBundleInfoForSelf(bundleManager.BundleFlag.GET_BUNDLE_INFO_DEFAULT)
      .then((info) => { this.version = info.versionName }).catch(() => {})
  }

  onPageHide(): void {
    this.sidebarOpen = false
    this.stopWatching?.()
    this.stopWatching = undefined
  }

  aboutToDisappear(): void {
    this.onPageHide()
  }

  onBackPress(): boolean {
    if (this.sidebarOpen) this.sidebarOpen = false
    else this.goBack()
    return true
  }

  private async goBack(): Promise<void> {
    if (this.editing) {
      if (!this.working) this.editing = false
      return
    }
    const result = await AppStorage.get<NavigatorManager>('lokiNavigator')?.back()
    if (result !== NavigationResult.Ok && result !== NavigationResult.Busy) {
      try { this.getUIContext().getPromptAction().showToast({ message: '无法返回上一页，请重试' }) }
      catch (error) { console.error('无法显示返回失败提示') }
    }
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
        Text('手机名称').fontSize(22).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
        Button('取消', { type: ButtonType.Normal }).fontSize(15).fontColor(SECONDARY_TEXT)
          .backgroundColor(Color.Transparent).enabled(!this.working)
          .onClick(() => { this.editing = false })
        Button(this.working ? '保存中' : '保存', { type: ButtonType.Capsule })
          .height(36).fontSize(15).fontWeight(FontWeight.Medium)
          .fontColor(ACCENT_TEXT).backgroundColor(ACCENT).enabled(!this.working)
          .onClick(() => { this.saveName() })
      }.width('100%')
      TextInput({ text: this.draftName, placeholder: '请输入手机名称' })
        .maxLength(128).enabled(!this.working).height(52)
        .fontSize(18).fontColor(PRIMARY_TEXT).placeholderColor('#969894')
        .backgroundColor('#414341').borderRadius(12).caretColor(ACCENT)
        .onChange((value: string) => { this.draftName = value; this.editError = '' })
        .enterKeyType(EnterKeyType.Done).onSubmit(() => { this.saveName() })
      Text('用于在电脑上显示手机名称，保存后将在下次绑定时使用。')
        .fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(22).width('100%')
      if (this.editError !== '') {
        Text(this.editError).fontSize(14).fontColor(ERROR_TEXT).width('100%')
      }
    }.padding({ left: 24, right: 24, top: 12, bottom: 24 }).width('100%')
  }

  @Builder
  private sectionTitle(title: string) {
    Text(title).fontSize(14).fontColor(SECONDARY_TEXT)
      .width('100%').padding({ left: 14, top: 16, bottom: 12 })
  }

  @Builder
  private pageContent() {
    Scroll() {
      Column() {
        this.sectionTitle('设备')
        Column() {
          Row({ space: 16 }) {
            Image($r('sys.media.ohos_ic_public_edit')).width(22).height(22).fillColor('#AAAFA9')
            Text('手机名称').fontSize(18).fontColor(PRIMARY_TEXT).layoutWeight(1)
            Text(this.loaded ? this.deviceName : '正在读取').fontSize(15).fontColor(SECONDARY_TEXT)
              .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).constraintSize({ maxWidth: '45%' })
            SymbolGlyph($r('sys.symbol.chevron_right')).fontSize(13).fontColor([SECONDARY_TEXT])
          }.height(68).width('100%').padding({ left: 18, right: 20 })
            .enabled(this.loaded && !this.working).onClick(() => { this.editName() })
            .accessibilityGroup(true).accessibilityText(`手机名称，${this.deviceName}，点击修改`)
          Divider().color(BORDER).strokeWidth(0.5)
          Row({ space: 16 }) {
            Image($r('sys.media.ohos_ic_public_device_phone')).width(22).height(22).fillColor('#AAAFA9')
            Text('设备类型').fontSize(18).fontColor(PRIMARY_TEXT).layoutWeight(1)
            Text('移动端').fontSize(15).fontColor(SECONDARY_TEXT)
          }.height(68).width('100%').padding({ left: 18, right: 20 })
        }.width('100%').backgroundColor(SURFACE).borderRadius(18).clip(true)
        Text('名称将在下次绑定时展示给电脑。').fontSize(12).fontColor('#8F938D')
          .width('100%').padding({ left: 14, top: 12 })

        this.sectionTitle('应用程序')
        Row({ space: 16 }) {
          SymbolGlyph($r('sys.symbol.info_circle')).fontSize(22).fontColor(['#AAAFA9'])
          Text('应用版本').fontSize(18).fontColor(PRIMARY_TEXT).layoutWeight(1)
          Text(this.version).fontSize(15).fontColor(SECONDARY_TEXT)
        }.height(68).width('100%').padding({ left: 18, right: 20 })
          .backgroundColor(SURFACE).borderRadius(18)

        if (this.working && !this.loaded) {
          LoadingProgress().width(24).height(24).color(ACCENT).margin({ top: 24 })
        }
        if (this.errorMessage !== '') {
          Text(this.errorMessage).fontSize(14).fontColor(ERROR_TEXT).width('100%').margin({ top: 20 })
          if (!this.loaded) {
            Button('重新读取').fontColor(ACCENT).backgroundColor(SURFACE)
              .enabled(!this.working).onClick(() => { this.loadSettings() })
          }
        }
        if (this.notice !== '') {
          Text(this.notice).fontSize(14).fontColor('#B5CFAE').width('100%').padding({ left: 14, top: 20 })
        }
      }.width('100%').padding({ left: 24, right: 24, bottom: 36 }).alignItems(HorizontalAlign.Start)
    }.layoutWeight(1).width('100%').constraintSize({ maxWidth: 560 }).align(Alignment.Top).scrollBar(BarState.Auto)
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Settings, sidebarOpen: $sidebarOpen }) {
        this.pageContent()
      }
    }.width('100%').height('100%')
      .bindSheet($$this.editing, this.nameEditor(), {
        height: SheetSize.MEDIUM, backgroundColor: SURFACE,
        showClose: false,
        onDisappear: (): void => { this.editing = false }
      })
  }
}

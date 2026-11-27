import { bundleManager, common } from '@kit.AbilityKit'
import { Application } from '../app/Application'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'
import { ACCENT, ACCENT_TEXT, AppPage, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_BACKGROUND,
  PAGE_INSET, PAGE_MAX_WIDTH, PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SURFACE, TOUCH_SIZE,
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
    Column({ space: PAGE_INSET }) {
      Row({ space: CONTENT_GAP }) {
        Text('手机名称').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
        Button('取消', { type: ButtonType.Normal }).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
          .constraintSize({ minHeight: TOUCH_SIZE, minWidth: TOUCH_SIZE })
          .backgroundColor(Color.Transparent).enabled(!this.working)
          .onClick(() => { this.editing = false })
        Button(this.working ? '保存中' : '保存', { type: ButtonType.Normal })
          .constraintSize({ minHeight: TOUCH_SIZE, minWidth: TOUCH_SIZE }).borderRadius(PANEL_RADIUS)
          .fontSize(BODY_SIZE).fontWeight(FontWeight.Medium)
          .fontColor(ACCENT_TEXT).backgroundColor(ACCENT).enabled(!this.working)
          .onClick(() => { this.saveName() })
      }.width('100%')
      TextInput({ text: this.draftName, placeholder: '请输入手机名称' })
        .maxLength(128).enabled(!this.working).constraintSize({ minHeight: TOUCH_SIZE })
        .fontSize(SECTION_SIZE).fontColor(PRIMARY_TEXT).placeholderColor(SECONDARY_TEXT)
        .backgroundColor(PAGE_BACKGROUND).border({ width: 0.5, color: BORDER })
        .borderRadius(PANEL_RADIUS).caretColor(ACCENT)
        .onChange((value: string) => { this.draftName = value; this.editError = '' })
        .enterKeyType(EnterKeyType.Done).onSubmit(() => { this.saveName() })
      Text(this.editError || '名称将在下次绑定时展示给电脑。')
        .fontSize(BODY_SIZE).fontColor(this.editError ? ERROR_TEXT : SECONDARY_TEXT).width('100%')
    }.padding({ left: PAGE_INSET, right: PAGE_INSET, top: CONTENT_GAP, bottom: PAGE_INSET }).width('100%')
  }

  @Builder
  private sectionTitle(title: string) {
    Text(title).fontSize(META_SIZE).fontWeight(FontWeight.Medium).fontColor(SECONDARY_TEXT)
      .width('100%').padding({ left: PAGE_INSET, top: PAGE_INSET, bottom: CONTENT_GAP })
  }

  @Builder
  private pageContent() {
    Scroll() {
      Column() {
        this.sectionTitle('设备')
        Column() {
          Row({ space: CONTENT_GAP }) {
            Image($r('sys.media.ohos_ic_public_edit')).width(16).height(16).fillColor(SECONDARY_TEXT)
            Text('手机名称').fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).layoutWeight(1)
            Text(this.loaded ? this.deviceName : '正在读取').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
              .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).constraintSize({ maxWidth: '45%' })
            SymbolGlyph($r('sys.symbol.chevron_right')).fontSize(META_SIZE).fontColor([SECONDARY_TEXT])
          }.constraintSize({ minHeight: TOUCH_SIZE }).width('100%')
            .padding({ left: PAGE_INSET, right: PAGE_INSET, top: 6, bottom: 6 })
            .enabled(this.loaded && !this.working).onClick(() => { this.editName() })
            .accessibilityGroup(true).accessibilityText(`手机名称，${this.deviceName}，点击修改`)
          Divider().color(BORDER).strokeWidth(0.5)
          Row({ space: CONTENT_GAP }) {
            Image($r('sys.media.ohos_ic_public_device_phone')).width(16).height(16).fillColor(SECONDARY_TEXT)
            Text('设备类型').fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).layoutWeight(1)
            Text('移动端').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
          }.constraintSize({ minHeight: TOUCH_SIZE }).width('100%')
            .padding({ left: PAGE_INSET, right: PAGE_INSET, top: 6, bottom: 6 })
        }.width('100%').backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).clip(true)
        Text('名称将在下次绑定时展示给电脑。').fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
          .width('100%').padding({ left: PAGE_INSET, top: CONTENT_GAP })

        this.sectionTitle('应用程序')
        Row({ space: CONTENT_GAP }) {
          SymbolGlyph($r('sys.symbol.info_circle')).fontSize(16).fontColor([SECONDARY_TEXT])
          Text('应用版本').fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).layoutWeight(1)
          Text(this.version).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
        }.constraintSize({ minHeight: TOUCH_SIZE }).width('100%')
          .padding({ left: PAGE_INSET, right: PAGE_INSET, top: 6, bottom: 6 })
          .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS)

        if (this.working && !this.loaded) {
          LoadingProgress().width(24).height(24).color(ACCENT).margin({ top: 24 })
        }
        if (this.errorMessage !== '') {
          Text(this.errorMessage).fontSize(BODY_SIZE).fontColor(ERROR_TEXT).width('100%').margin({ top: PAGE_INSET })
          if (!this.loaded) {
            Button('重新读取').fontSize(BODY_SIZE).fontColor(ACCENT).backgroundColor(SURFACE)
              .constraintSize({ minHeight: TOUCH_SIZE })
              .enabled(!this.working).onClick(() => { this.loadSettings() })
          }
        }
        if (this.notice !== '') {
          Text(this.notice).fontSize(BODY_SIZE).fontColor('#B5CFAE').width('100%')
            .padding({ left: PAGE_INSET, top: PAGE_INSET })
        }
      }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, bottom: PAGE_INSET })
        .alignItems(HorizontalAlign.Start)
    }.layoutWeight(1).width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH }).align(Alignment.Top)
      .scrollBar(BarState.Auto)
  }

  build() {
    Stack() {
      AppPage({ page: PageName.Settings, sidebarOpen: $sidebarOpen, content: () => { this.pageContent() } })
    }.width('100%').height('100%')
      .bindSheet($$this.editing, this.nameEditor(), {
        height: SheetSize.MEDIUM, backgroundColor: SURFACE,
        showClose: false,
        onDisappear: (): void => { this.editing = false }
      })
  }
}

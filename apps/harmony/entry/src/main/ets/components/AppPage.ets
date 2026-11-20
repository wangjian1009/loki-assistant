import { common } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'
import { NavigatorManager, NavigationResult, PageName } from '../app/NavigatorManager'

export const PAGE_BACKGROUND: string = '#111318'
export const SURFACE: string = '#1B1F28'
export const BORDER: string = '#303744'
export const PRIMARY_TEXT: string = '#F1F3FA'
export const SECONDARY_TEXT: string = '#A8B1C4'
export const ACCENT: string = '#C2CCFF'
export const ACCENT_TEXT: string = '#182143'
export const ERROR_TEXT: string = '#FFB4B1'

export function setSystemColors(context: common.UIAbilityContext): void {
  window.getLastWindow(context).then((mainWindow) => {
    mainWindow.setWindowSystemBarProperties({
      statusBarColor: PAGE_BACKGROUND, statusBarContentColor: PRIMARY_TEXT,
      navigationBarColor: PAGE_BACKGROUND, navigationBarContentColor: SECONDARY_TEXT
    }).catch(() => {})
  }).catch(() => {})
}

const SECTION_TITLES: Record<PageName, string> = {
  [PageName.Index]: '当前设备',
  [PageName.Devices]: '设备管理',
  [PageName.Settings]: '设置'
}

@Component
export struct AppPage {
  @Prop page: PageName
  @Link sidebarOpen: boolean
  @BuilderParam content: () => void

  private async goTo(page: PageName): Promise<void> {
    this.sidebarOpen = false
    if (page === this.page) return
    const result = await AppStorage.get<NavigatorManager>('lokiNavigator')?.openSection(page)
    if (result !== NavigationResult.Ok && result !== NavigationResult.Busy) {
      try { this.getUIContext().getPromptAction().showToast({ message: '无法打开页面，请重试' }) }
      catch (_) { console.error('无法显示页面打开失败提示') }
    }
  }

  @Builder
  private menuIcon() {
    Column({ space: 4 }) {
      Row().width(18).height(2).borderRadius(1).backgroundColor(ACCENT)
      Row().width(18).height(2).borderRadius(1).backgroundColor(ACCENT)
      Row().width(18).height(2).borderRadius(1).backgroundColor(ACCENT)
    }
  }

  @Builder
  private sidebarItem(page: PageName, icon: Resource) {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 12 }) {
        if (page === PageName.Index) {
          Image(icon).width(18).height(18).fillColor(this.page === page ? ACCENT : SECONDARY_TEXT)
        } else {
          SymbolGlyph(icon).fontSize(18).fontColor([this.page === page ? ACCENT : SECONDARY_TEXT])
        }
        Text(SECTION_TITLES[page]).fontSize(16).fontWeight(this.page === page ? FontWeight.Bold : FontWeight.Medium)
          .fontColor(this.page === page ? ACCENT : PRIMARY_TEXT)
      }.width('100%').padding({ left: 14, right: 14 })
    }.width('100%').height(48).padding(0).borderRadius(12)
      .backgroundColor(this.page === page ? '#303951' : Color.Transparent)
      .accessibilitySelected(this.page === page).onClick(() => { this.goTo(page) })
  }

  build() {
    Stack({ alignContent: Alignment.Start }) {
      Column() {
        Row({ space: 12 }) {
          Button({ type: ButtonType.Normal }) {
            this.menuIcon()
          }.width(44).height(44).padding(0).borderRadius(14).backgroundColor(SURFACE)
            .accessibilityText('打开侧边栏').onClick(() => { this.sidebarOpen = true })
          Text(SECTION_TITLES[this.page]).fontSize(28).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT)
        }.width('100%').constraintSize({ maxWidth: 560 })
          .padding({ left: 24, right: 24, top: 16, bottom: 16 })

        this.content()
      }.width('100%').height('100%')
        .enabled(!this.sidebarOpen).accessibilityLevel(this.sidebarOpen ? 'no-hide-descendants' : 'auto')

      if (this.sidebarOpen) {
        Button({ type: ButtonType.Normal }) {}
          .width('100%').height('100%').borderRadius(0).backgroundColor('#99000000')
          .accessibilityText('关闭侧边栏').onClick(() => { this.sidebarOpen = false })

        Column({ space: 8 }) {
          Row({ space: 8 }) {
            Text('Loki Assistant').fontSize(20).fontWeight(FontWeight.Bold).fontColor(PRIMARY_TEXT).layoutWeight(1)
            Button('关闭', { type: ButtonType.Normal }).fontSize(13).fontColor(SECONDARY_TEXT)
              .height(44).padding({ left: 8, right: 8 }).backgroundColor(Color.Transparent)
              .accessibilityText('关闭侧边栏').onClick(() => { this.sidebarOpen = false })
          }.width('100%').padding({ left: 14, bottom: 12 })
          this.sidebarItem(PageName.Index, $r('sys.media.ohos_ic_public_device_matebook'))
          this.sidebarItem(PageName.Devices, $r('sys.symbol.list_bullet'))
          this.sidebarItem(PageName.Settings, $r('sys.symbol.gearshape_fill'))
        }.width('80%').constraintSize({ maxWidth: 280 }).height('100%')
          .padding({ left: 14, right: 14, top: 16, bottom: 16 })
          .backgroundColor(SURFACE).alignItems(HorizontalAlign.Start)
      }
    }.width('100%').height('100%').backgroundColor(PAGE_BACKGROUND)
      .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.BOTTOM])
  }
}

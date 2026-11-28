import { common } from '@kit.AbilityKit'
import { window } from '@kit.ArkUI'

export const PAGE_BACKGROUND: string = '#111318'
export const SURFACE: string = '#1B1F28'
export const BORDER: string = '#303744'
export const PRIMARY_TEXT: string = '#F1F3FA'
export const SECONDARY_TEXT: string = '#A8B1C4'
export const ACCENT: string = '#C2CCFF'
export const ACCENT_TEXT: string = '#182143'
export const ERROR_TEXT: string = '#FFB4B1'
export const SELECTED_SURFACE: string = '#282F42'

export const TITLE_SIZE: number = 18
export const SECTION_SIZE: number = 16
export const BODY_SIZE: number = 14
export const META_SIZE: number = 12
export const TOUCH_SIZE: number = 44
export const PAGE_INSET: number = 12
export const CONTENT_GAP: number = 8
export const PANEL_RADIUS: number = 10
export const PAGE_MAX_WIDTH: number = 560

export function setSystemColors(context: common.UIAbilityContext): void {
  window.getLastWindow(context).then((mainWindow) => {
    mainWindow.setWindowSystemBarProperties({
      statusBarColor: PAGE_BACKGROUND, statusBarContentColor: PRIMARY_TEXT,
      navigationBarColor: PAGE_BACKGROUND, navigationBarContentColor: SECONDARY_TEXT
    }).catch(() => {})
  }).catch(() => {})
}

@Component
export struct AppPage {
  @Prop title: ResourceStr
  onBack?: () => void
  @BuilderParam content: () => void
  @BuilderParam actions?: () => void

  build() {
    Column() {
      Row() {
        if (this.onBack) {
          Button({ type: ButtonType.Normal }) {
            SymbolGlyph($r('sys.symbol.chevron_left')).fontSize(18).fontColor([ACCENT])
          }.width(TOUCH_SIZE).height(TOUCH_SIZE).padding(0)
            .borderRadius(PANEL_RADIUS).backgroundColor(Color.Transparent)
            .accessibilityText('返回').onClick(() => { this.onBack?.() })
        }
        Text(this.title).fontSize(TITLE_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          .layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        if (this.actions) {
          this.actions()
        }
      }.width('100%').constraintSize({ minHeight: 52, maxWidth: PAGE_MAX_WIDTH })
        .padding({ left: this.onBack ? 4 : PAGE_INSET, right: PAGE_INSET, top: 4, bottom: 4 })
      Divider().color(BORDER).strokeWidth(0.5).width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH })
      this.content()
    }.width('100%').height('100%').backgroundColor(PAGE_BACKGROUND)
      .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.BOTTOM])
  }
}

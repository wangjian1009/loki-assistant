import { SymbolGlyphModifier } from '@kit.ArkUI'
import { BODY_SIZE, PAGE_INSET, PANEL_RADIUS, PRIMARY_TEXT, SURFACE, TOUCH_SIZE } from './AppPage'

const MENU_MAX_WIDTH: number = 280
// 包含系统菜单项的左右内边距与末尾预留空间。
const MENU_HORIZONTAL_PADDING: number = 48
const MENU_ICON_SPACE: number = 28

export interface PopupMenuItem {
  id: string
  label: ResourceStr
  icon?: Resource
  enabled?: boolean
  action: () => void
}

@Component
export struct PopupMenu {
  @Prop items: PopupMenuItem[] = []
  @Prop accessibilityLabel: string = '更多'
  @Prop placement: Placement = Placement.BottomRight
  @BuilderParam trigger: () => void

  private menuWidth(): number {
    const context = this.getUIContext()
    const measure = context.getMeasureUtils()
    let longest = 0
    for (const item of this.items) {
      const textWidth = context.px2vp(measure.measureText({ textContent: item.label, fontSize: BODY_SIZE }))
      longest = Math.max(longest, textWidth + (item.icon ? MENU_ICON_SPACE : 0))
    }
    return Math.min(MENU_MAX_WIDTH, Math.max(TOUCH_SIZE,
      Math.ceil(longest) + MENU_HORIZONTAL_PADDING))
  }

  @Builder
  private menu() {
    Menu() {
      ForEach(this.items, (item: PopupMenuItem) => {
        MenuItem({ content: item.label, symbolStartIcon: item.icon ?
          new SymbolGlyphModifier(item.icon).fontSize(18).fontColor([PRIMARY_TEXT]) : undefined })
          .contentFont({ size: BODY_SIZE }).contentFontColor(PRIMARY_TEXT)
          .enabled(item.enabled !== false)
          .onClick(() => { item.action() })
      }, (item: PopupMenuItem) => item.id)
    }.width(this.menuWidth()).backgroundColor(SURFACE).radius(PANEL_RADIUS)
  }

  build() {
    Button({ type: ButtonType.Normal }) {
      this.trigger()
    }.constraintSize({ minWidth: TOUCH_SIZE, minHeight: TOUCH_SIZE })
      .padding(0).backgroundColor(Color.Transparent).borderRadius(PANEL_RADIUS)
      .accessibilityText(this.accessibilityLabel)
      .enabled(this.items.length > 0)
      .bindMenu(this.menu(), {
        placement: this.placement,
        backgroundColor: SURFACE,
        borderRadius: PANEL_RADIUS,
        layoutRegionMargin: { left: PAGE_INSET, right: PAGE_INSET }
      })
  }
}

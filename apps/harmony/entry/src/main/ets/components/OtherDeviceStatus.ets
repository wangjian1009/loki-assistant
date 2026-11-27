import { BODY_SIZE, BORDER, CONTENT_GAP, META_SIZE, PAGE_INSET, PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT,
  SECTION_SIZE, SURFACE, TOUCH_SIZE } from './AppPage'
import { StatusDetail } from './DeviceStatus'

@Component
export struct OtherDeviceStatus {
  @Prop details: StatusDetail[] = []
  @Prop @Watch('reset') endpointId: string = ''
  @State private expanded: boolean = false

  private reset(): void { this.expanded = false }

  build() {
    if (this.details.length > 0) {
      Column() {
        Button({ type: ButtonType.Normal }) {
          Row({ space: CONTENT_GAP }) {
            Text('其他设备状态').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium)
              .fontColor(PRIMARY_TEXT).layoutWeight(1)
            SymbolGlyph(this.expanded ? $r('sys.symbol.chevron_up') : $r('sys.symbol.chevron_down'))
              .fontSize(META_SIZE).fontColor([SECONDARY_TEXT]).accessibilityLevel('no')
          }.width('100%')
        }.width('100%').constraintSize({ minHeight: TOUCH_SIZE }).padding({ left: PAGE_INSET, right: PAGE_INSET })
          .backgroundColor(Color.Transparent)
          .accessibilityDescription(this.expanded ? '收起其他设备状态' : '展开其他设备状态')
          .onClick(() => { this.expanded = !this.expanded })
        if (this.expanded) {
          Column() {
            ForEach(this.details, (detail: StatusDetail) => {
              Divider().color(BORDER).strokeWidth(0.5)
              Column({ space: 4 }) {
                Text(detail.label).fontSize(META_SIZE).fontColor(SECONDARY_TEXT).wordBreak(WordBreak.BREAK_ALL)
                Text(detail.value).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).wordBreak(WordBreak.BREAK_ALL)
              }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: CONTENT_GAP, bottom: CONTENT_GAP })
            }, (detail: StatusDetail) => JSON.stringify(detail))
          }.width('100%').padding({ left: PAGE_INSET, right: PAGE_INSET, bottom: 4 })
        }
      }.width('100%').backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
    }
  }
}

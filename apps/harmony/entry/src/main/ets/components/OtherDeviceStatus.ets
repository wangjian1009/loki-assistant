import { BORDER, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE } from './AppPage'
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
          Row({ space: 12 }) {
            Text('其他设备状态').fontSize(18).fontWeight(FontWeight.Medium)
              .fontColor(PRIMARY_TEXT).layoutWeight(1)
            SymbolGlyph(this.expanded ? $r('sys.symbol.chevron_up') : $r('sys.symbol.chevron_down'))
              .fontSize(14).fontColor([SECONDARY_TEXT]).accessibilityLevel('no')
          }.width('100%')
        }.width('100%').constraintSize({ minHeight: 48 }).padding({ left: 14, right: 14 })
          .backgroundColor(Color.Transparent)
          .accessibilityDescription(this.expanded ? '收起其他设备状态' : '展开其他设备状态')
          .onClick(() => { this.expanded = !this.expanded })
        if (this.expanded) {
          Column() {
            ForEach(this.details, (detail: StatusDetail) => {
              Divider().color(BORDER).strokeWidth(0.5)
              Column({ space: 4 }) {
                Text(detail.label).fontSize(14).fontColor(SECONDARY_TEXT).wordBreak(WordBreak.BREAK_ALL)
                Text(detail.value).fontSize(15).fontColor(PRIMARY_TEXT).lineHeight(22).wordBreak(WordBreak.BREAK_ALL)
              }.width('100%').alignItems(HorizontalAlign.Start).padding({ top: 10, bottom: 10 })
            }, (detail: StatusDetail) => JSON.stringify(detail))
          }.width('100%').padding({ left: 14, right: 14, bottom: 4 })
        }
      }.width('100%').backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
    }
  }
}

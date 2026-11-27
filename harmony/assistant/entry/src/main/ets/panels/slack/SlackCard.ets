import { ACCENT, BODY_SIZE, BORDER, CONTENT_GAP, META_SIZE, PAGE_INSET, PANEL_RADIUS, PRIMARY_TEXT,
  SECONDARY_TEXT, SECTION_SIZE, SURFACE, TOUCH_SIZE } from '../../components/AppPage'
import { Slack, SlackPreview, SlackStatus, SlackStatusChanged, StatusSubscription } from 'liblokiworkspace.so'
import { Workspace } from '../../workspace/Workspace'

@Component
export struct SlackCard {
  workspace?: Workspace
  @State private slack: SlackStatus | null = null
  @State private expanded: boolean = false
  @State private errorMessage: string = ''
  private module?: Slack
  private subscription?: StatusSubscription
  private version: number = 0
  private generation: number = -1

  aboutToAppear(): void { this.subscribe() }

  aboutToDisappear(): void {
    this.version += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.module = undefined
  }

  private async subscribe(): Promise<void> {
    const workspace = this.workspace
    if (!workspace) { return }
    const version = ++this.version
    try {
      const native = await workspace.open()
      if (version !== this.version) { return }
      this.module = native.openSlack()
      this.subscription = this.module.watchStatus((update: SlackStatusChanged) => {
        if (version !== this.version) { return }
        if (this.generation !== update.generation) {
          this.generation = update.generation
          this.expanded = false
        }
        this.slack = update.status
      })
    } catch (error) {
      if (version === this.version) { this.errorMessage = (error as Error).message }
    }
  }

  build() {
    Column({ space: CONTENT_GAP }) {
      Text('Slack 新消息').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      if (this.slack?.unread_previews?.length) {
        ForEach(this.slack.unread_previews.slice(0, this.expanded ? 20 : 2), (item: SlackPreview, index: number) => {
          if (index > 0) { Divider().color(BORDER).strokeWidth(0.5) }
          Column({ space: 2 }) {
            Text(item.title).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT).fontWeight(FontWeight.Medium)
              .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
            Text(`${item.team_name} · ${item.unread_count} 条未读`).fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
            Text(item.preview).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
          }.width('100%').alignItems(HorizontalAlign.Start)
        }, (item: SlackPreview) => `${item.id}:${item.unread_count}:${item.preview}`)
        if (this.slack.unread_previews.length > 2) {
          Button(this.expanded ? '收起' : '查看更多 Slack 消息', { type: ButtonType.Normal })
            .constraintSize({ minHeight: TOUCH_SIZE }).padding({ left: 0, right: PAGE_INSET }).backgroundColor(Color.Transparent)
            .fontSize(BODY_SIZE).fontColor(ACCENT).onClick(() => { this.expanded = !this.expanded })
        }
      } else {
        Text(this.errorMessage || '暂无未读消息').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(PAGE_INSET)
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }
}

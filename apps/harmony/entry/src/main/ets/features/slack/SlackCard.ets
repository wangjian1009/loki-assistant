import { ACCENT, BORDER, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE } from '../../components/AppPage'
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
    Column({ space: 10 }) {
      Text('Slack 新消息').fontSize(18).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      if (this.slack?.unread_previews?.length) {
        ForEach(this.slack.unread_previews.slice(0, this.expanded ? 20 : 2), (item: SlackPreview, index: number) => {
          if (index > 0) { Divider().color(BORDER).strokeWidth(0.5) }
          Column({ space: 4 }) {
            Text(item.title).fontSize(16).fontColor(PRIMARY_TEXT).fontWeight(FontWeight.Medium)
              .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
            Text(`${item.team_name} · ${item.unread_count} 条未读`).fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(21)
            Text(item.preview).fontSize(14).fontColor(PRIMARY_TEXT).lineHeight(21)
          }.width('100%').alignItems(HorizontalAlign.Start)
        }, (item: SlackPreview) => `${item.id}:${item.unread_count}:${item.preview}`)
        if (this.slack.unread_previews.length > 2) {
          Button(this.expanded ? '收起' : '查看更多 Slack 消息', { type: ButtonType.Normal })
            .height(44).padding({ left: 0, right: 12 }).backgroundColor(Color.Transparent)
            .fontSize(14).fontColor(ACCENT).onClick(() => { this.expanded = !this.expanded })
        }
      } else {
        Text(this.errorMessage || '暂无未读消息').fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(21)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
      .backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
  }
}

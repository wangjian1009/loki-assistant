import { ACCENT, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, META_SIZE, PAGE_INSET,
  PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SELECTED_SURFACE, SURFACE,
  TOUCH_SIZE } from '../../components/AppPage'
import { Mail, MailActivity, MailStatus, MailStatusChanged, StatusSubscription } from 'liblokiworkspace.so'
import { Workspace } from '../../workspace/Workspace'
import { mailSyncPhaseText } from './MailModel'

@Component
export struct MailCard {
  workspace?: Workspace
  @State private mail: MailStatus | null = null
  @State private activity: MailActivity | null = null
  @State private ready: boolean = false
  @State private errorMessage: string = ''
  @State private expanded: boolean = false
  private module?: Mail
  private subscription?: StatusSubscription
  private version: number = 0
  private endpointId: string | null = null
  private generation: number = -1

  aboutToAppear(): void { this.subscribe() }

  aboutToDisappear(): void {
    this.version += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.module = undefined
    this.ready = false
  }

  private async subscribe(): Promise<void> {
    const workspace = this.workspace
    if (!workspace) { return }
    const version = ++this.version
    try {
      const native = await workspace.open()
      if (version !== this.version) { return }
      this.module = native.openMail()
      this.subscription = this.module.watchStatus((update: MailStatusChanged) => {
        if (version !== this.version) { return }
        if (this.generation !== update.generation) {
          this.generation = update.generation
          this.expanded = false
        }
        this.endpointId = update.endpoint_id
        this.ready = this.endpointId !== null
        this.mail = update.status
        this.activity = update.activity
        this.errorMessage = ''
      })
    } catch (error) {
      if (version === this.version) { this.errorMessage = (error as Error).message }
    }
  }

  private startSync(): void {
    const endpoint = this.endpointId
    if (!endpoint) { return }
    try {
      this.module?.startSync(endpoint)
      this.errorMessage = ''
    } catch (error) {
      this.errorMessage = (error as Error).message
    }
  }

  private syncMessage(): string {
    if (this.errorMessage) { return this.errorMessage }
    if (this.activity?.error) { return this.activity.error.message }
    const phase = this.activity?.sync?.phase
    if (phase) { return mailSyncPhaseText(phase) ?? '' }
    return this.activity?.receiving ? '正在提交同步请求…' : ''
  }

  private syncing(): boolean {
    const phase = this.activity?.sync?.phase
    return !this.ready || !!this.activity?.receiving ||
      (!!phase && phase !== 'completed' && phase !== 'failed' && !this.activity?.error)
  }

  build() {
    Column({ space: CONTENT_GAP }) {
      Row({ space: CONTENT_GAP }) {
        Column({ space: 2 }) {
          Text('邮件收件箱').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          if (typeof this.mail?.unread_count === 'number') {
            Text(`${this.mail.unread_count} 封未读`).fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
          }
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        Button({ type: ButtonType.Normal }) {
          Text('同步').fontSize(13).fontWeight(FontWeight.Medium).fontColor(ACCENT)
            .constraintSize({ minWidth: 56, minHeight: 30 }).padding({ left: CONTENT_GAP, right: CONTENT_GAP })
            .textAlign(TextAlign.Center).backgroundColor(SELECTED_SURFACE).borderRadius(7)
        }.constraintSize({ minHeight: TOUCH_SIZE }).padding(0).backgroundColor(Color.Transparent)
          .accessibilityText('同步邮件')
          .flexShrink(0).enabled(!this.syncing()).onClick(() => { this.startSync() })
      }.width('100%')
      if (this.syncMessage()) {
        Text(this.syncMessage()).fontSize(BODY_SIZE)
          .fontColor(this.errorMessage || this.activity?.error || this.activity?.sync?.phase === 'failed' ? ERROR_TEXT : SECONDARY_TEXT)
      }
      if (this.activity?.sync?.error) {
        Text(this.activity.sync.error).fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
      } else if (this.syncing() && this.activity?.sync?.messages?.length) {
        Text(this.activity.sync.messages[this.activity.sync.messages.length - 1])
          .fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
      }
      if (this.mail?.unread_subjects?.length) {
        ForEach(this.mail.unread_subjects.slice(0, this.expanded ? 20 : 2), (subject: string) => {
          Divider().color(BORDER).strokeWidth(0.5)
          Text(subject).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
        }, (subject: string, index: number) => `${index}:${subject}`)
        if (this.mail.unread_subjects.length > 2) {
          Button(this.expanded ? '收起' : '查看更多邮件', { type: ButtonType.Normal })
            .constraintSize({ minHeight: TOUCH_SIZE }).padding({ left: 0, right: PAGE_INSET }).backgroundColor(Color.Transparent)
            .fontSize(BODY_SIZE).fontColor(ACCENT).onClick(() => { this.expanded = !this.expanded })
        }
      } else if (this.mail?.unread_count == null) {
        Text('等待电脑更新邮件索引').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(PAGE_INSET)
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }
}

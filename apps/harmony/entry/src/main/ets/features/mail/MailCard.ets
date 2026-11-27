import { ACCENT, ACCENT_TEXT, BORDER, ERROR_TEXT, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE } from '../../components/AppPage'
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
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column({ space: 4 }) {
          Text('邮件收件箱').fontSize(18).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          if (typeof this.mail?.unread_count === 'number') {
            Text(`${this.mail.unread_count} 封未读`).fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(21)
          }
        }.alignItems(HorizontalAlign.Start).layoutWeight(1)
        Button('立即同步邮件', { type: ButtonType.Normal })
          .constraintSize({ minHeight: 44 }).padding({ left: 12, right: 12 }).borderRadius(10)
          .fontSize(14).fontWeight(FontWeight.Medium).fontColor(ACCENT_TEXT).backgroundColor(ACCENT)
          .enabled(!this.syncing()).onClick(() => { this.startSync() })
      }.width('100%')
      if (this.syncMessage()) {
        Text(this.syncMessage()).fontSize(14)
          .fontColor(this.errorMessage || this.activity?.error || this.activity?.sync?.phase === 'failed' ? ERROR_TEXT : SECONDARY_TEXT)
      }
      if (this.activity?.sync?.error) {
        Text(this.activity.sync.error).fontSize(14).fontColor(ERROR_TEXT).lineHeight(21)
      } else if (this.syncing() && this.activity?.sync?.messages?.length) {
        Text(this.activity.sync.messages[this.activity.sync.messages.length - 1])
          .fontSize(14).fontColor(SECONDARY_TEXT).maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
      }
      if (this.mail?.unread_subjects?.length) {
        ForEach(this.mail.unread_subjects.slice(0, this.expanded ? 20 : 2), (subject: string) => {
          Divider().color(BORDER).strokeWidth(0.5)
          Text(subject).fontSize(15).fontColor(PRIMARY_TEXT).lineHeight(22)
        }, (subject: string, index: number) => `${index}:${subject}`)
        if (this.mail.unread_subjects.length > 2) {
          Button(this.expanded ? '收起' : '查看更多邮件', { type: ButtonType.Normal })
            .height(44).padding({ left: 0, right: 12 }).backgroundColor(Color.Transparent)
            .fontSize(14).fontColor(ACCENT).onClick(() => { this.expanded = !this.expanded })
        }
      } else if (this.mail?.unread_count == null) {
        Text('等待电脑更新邮件索引').fontSize(14).fontColor(SECONDARY_TEXT).lineHeight(21)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
      .backgroundColor(SURFACE).borderRadius(10).border({ width: 0.5, color: BORDER })
  }
}

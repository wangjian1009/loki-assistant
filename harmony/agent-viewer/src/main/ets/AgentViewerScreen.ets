import { OrgRenderer } from 'org_renderer'
import { AgentShellStatusIcon, agentShellAppearance } from './AgentShellStatusIcon'
import { ConversationRow, ViewerBlock, conversationRows } from './ConversationLayout'

export interface ViewerSession {
  id: string
  title: string
  phase: string
}

const BACKGROUND: string = '#111318'
const PANEL: string = '#1B1F28'
const SELECTED: string = '#282F42'
const BORDER: string = '#303744'
const TEXT: string = '#F1F3FA'
const SECONDARY: string = '#A8B1C4'
const ACCENT: string = '#C2CCFF'
const GREEN: string = '#9AE2B9'
const RED: string = '#FFB4B1'
const AMBER: string = '#F5CF92'
const DETAIL: string = '#929DB1'

function blockLabel(kind: string): string {
  switch (kind) {
    case 'user': return '你'
    case 'assistant': return 'Agent'
    case 'thought': return '思考'
    case 'group': return '活动'
    case 'tool': return '工具'
    case 'permission': return '等待授权'
    default: return ''
  }
}

@Component
export struct AgentViewerScreen {
  @Prop title: ResourceStr
  @Prop deviceName: string = ''
  @Prop sessionId: string = ''
  @Prop @Watch('resetConversation') conversationKey: number = 0
  @Prop sessions: ViewerSession[] = []
  @Prop @Watch('refreshRows') blocks: ViewerBlock[] = []
  @Prop notice: string = ''
  @Prop full: boolean = false
  @Prop connected: boolean = false
  onSelect: (id: string) => void = () => {}
  onRetry: () => void = () => {}
  onFull: () => void = () => {}
  onSend: (text: string) => Promise<string> = () => Promise.resolve('无法发送消息')
  readImage: (source: string) => Promise<string> = () => Promise.reject(new Error('图片不可用'))
  parseDocument: (source: string, format: string) => string = () => ''
  @State private rows: ConversationRow[] = []
  private readonly folds: Map<string, boolean> = new Map()
  @State private sidebar: boolean = false
  @State private draft: string = ''
  @State private sending: boolean = false
  @State private sendError: string = ''
  @State private preview: string = ''
  private followTimer: number = -1
  private following: boolean = true
  private readonly history: Scroller = new Scroller()
  private readonly sessionList: Scroller = new Scroller()

  aboutToAppear(): void { this.refreshRows() }

  aboutToDisappear(): void { clearTimeout(this.followTimer) }

  private resetConversation(): void {
    this.folds.clear()
    this.following = true
    this.draft = ''
    this.sending = false
    this.sendError = ''
    this.preview = ''
    this.refreshRows()
  }

  private refreshRows(): void {
    this.rows = conversationRows(this.blocks, this.folds)
    this.followLatest()
  }

  private followLatest(): void {
    if (!this.following) return
    clearTimeout(this.followTimer)
    this.followTimer = setTimeout(() => {
      if (this.following) this.history.scrollEdge(Edge.Bottom)
    }, 50)
  }

  private async send(): Promise<void> {
    if (this.sending || !this.connected || !this.sessionId || !this.draft.trim()) return
    const text = this.draft
    const key = this.conversationKey
    this.sending = true
    this.sendError = ''
    try {
      const error = await this.onSend(text)
      if (this.conversationKey !== key) return
      this.sendError = error
      if (!error && this.draft === text) {
        this.draft = ''
        this.following = true
        this.followLatest()
      }
    } catch (_) {
      if (this.conversationKey === key) this.sendError = '无法发送消息'
    } finally {
      if (this.conversationKey === key) this.sending = false
    }
  }

  private async openImage(source: string): Promise<void> {
    const key = this.conversationKey
    try {
      const path = await this.readImage(source)
      if (key === this.conversationKey) this.preview = path
    } catch (_) {
      if (key === this.conversationKey) this.sendError = '无法打开会话图片'
    }
  }

  private toggleFold(row: ConversationRow): void {
    this.folds.set(row.id, !row.collapsed)
    this.rows = conversationRows(this.blocks, this.folds)
  }

  private selected(): ViewerSession | undefined {
    return this.sessions.find(session => session.id === this.sessionId)
  }

  @Builder
  private chatHeader() {
    Row({ space: 10 }) {
      Button({ type: ButtonType.Normal }) {
        Column({ space: 4 }) {
          Row().width(18).height(1.5).backgroundColor(TEXT).borderRadius(1)
          Row().width(18).height(1.5).backgroundColor(TEXT).borderRadius(1)
          Row().width(18).height(1.5).backgroundColor(TEXT).borderRadius(1)
        }
      }.width(44).height(44).padding(0).borderRadius(12).backgroundColor(PANEL)
        .accessibilityText('打开会话列表').onClick(() => { this.sidebar = true })
      Column({ space: 2 }) {
        Text(this.selected()?.title || this.title).fontSize(18).fontWeight(FontWeight.Medium)
          .fontColor(TEXT).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
        if (this.selected()) {
          Row({ space: 5 }) {
            AgentShellStatusIcon({ phase: this.connected ? this.selected()!.phase : 'disconnected', iconSize: 12 })
            Text(this.connected ? agentShellAppearance(this.selected()!.phase).label : '电脑离线')
              .fontSize(12).fontColor(agentShellAppearance(this.connected ? this.selected()!.phase : 'disconnected').color)
          }
        }
      }.layoutWeight(1).alignItems(HorizontalAlign.Start)
      if (this.sessionId && !this.full && this.connected) {
        Button('全部', { type: ButtonType.Normal }).height(40).fontSize(13).fontColor(ACCENT)
          .padding({ left: 12, right: 12 }).backgroundColor(PANEL)
          .accessibilityText('加载全部会话')
          .onClick(() => { this.following = true; this.onFull() })
      }
    }.width('100%').padding({ top: 5, bottom: 10 })
  }

  @Builder
  private message(row: ConversationRow) {
    Column({ space: 6 }) {
      if (row.foldable) {
        Button({ type: ButtonType.Normal }) {
          Row({ space: 8 }) {
            SymbolGlyph(row.collapsed ? $r('sys.symbol.chevron_right') : $r('sys.symbol.chevron_down'))
              .fontSize(10).fontColor([DETAIL])
            Text(row.title || blockLabel(row.kind)).layoutWeight(1)
              .fontSize(12).fontColor(row.kind === 'permission' ? AMBER : DETAIL)
              .maxLines(row.collapsed ? 1 : 2).textOverflow({ overflow: TextOverflow.Ellipsis })
          }.width('100%')
        }.width('100%').constraintSize({ minHeight: 32 }).padding(0).backgroundColor(Color.Transparent)
          .accessibilityGroup(true)
          .accessibilityText((row.title || blockLabel(row.kind)) + (row.collapsed ? '，已折叠' : '，已展开'))
          .onClick(() => { this.toggleFold(row) })
      } else if (row.title || blockLabel(row.kind)) {
        Text(row.title || blockLabel(row.kind)).width('100%').fontSize(13)
          .fontWeight(FontWeight.Medium).fontColor(row.kind === 'user' ? ACCENT :
            row.kind === 'assistant' ? GREEN : row.kind === 'permission' ? AMBER : SECONDARY)
      }
      if (row.text && !row.collapsed) {
        if (row.kind === 'assistant' || row.kind === 'thought' || row.kind === 'tool') {
          OrgRenderer({ source: row.text, subdued: row.foldable, parse: this.parseDocument, readImage: this.readImage,
            onOpenImage: (source: string): void => { this.openImage(source) },
            onContentReady: (): void => { this.followLatest() } })
        } else {
          Text(row.text).width('100%').fontSize(15).fontColor(TEXT)
            .lineHeight(24).wordBreak(WordBreak.BREAK_ALL).copyOption(CopyOptions.LocalDevice)
        }
      }
    }.width('100%').alignItems(HorizontalAlign.Start)
      .padding(row.foldable ? { bottom: row.collapsed || !row.text ? 0 : 8, left: 8, right: 8 } :
        row.kind === 'user' ? { top: 10, bottom: 10, left: 12, right: 12 } : { top: 6, bottom: 6 })
      .backgroundColor(row.kind === 'user' ? SELECTED : Color.Transparent)
      .borderRadius(12)
  }

  @Builder
  private chat() {
    List({ scroller: this.history, space: 8 }) {
      ForEach(this.rows, (row: ConversationRow) => {
        ListItem() {
          Row() {
            this.message(row)
          }.width('100%').padding({ left: row.depth * 12 })
            .border({ width: { left: row.depth > 0 ? 1 : 0 }, color: BORDER })
        }
      }, (row: ConversationRow) => JSON.stringify([row.id, row.kind, row.title, row.depth, row.foldable,
        row.collapsed, row.collapsed ? '' : row.text]))
    }.width('100%').layoutWeight(1).padding({ top: 12, bottom: 16 })
      .edgeEffect(EdgeEffect.None).scrollBar(BarState.Auto)
      .onTouch(event => {
        if (event.type === TouchType.Move) this.following = false
      })
      .onScrollStop(() => { this.following = this.history.isAtEnd() })
  }

  @Builder
  private composer() {
    Column({ space: 4 }) {
      if (this.sendError) {
        Text(this.sendError).width('100%').fontSize(12).fontColor(RED)
      }
      Row({ space: 8 }) {
        TextInput({ placeholder: '输入消息', text: this.draft }).layoutWeight(1)
          .fontSize(15).fontColor(TEXT).placeholderColor(SECONDARY).backgroundColor(PANEL)
          .height(46).enterKeyType(EnterKeyType.Send).enabled(!!this.sessionId)
          .onChange((text: string) => { this.draft = text })
          .onSubmit(() => { this.send() })
        Button(this.sending ? '发送中' : '发送', { type: ButtonType.Normal }).height(44)
          .fontSize(14).fontColor('#182143').backgroundColor(ACCENT)
          .enabled(this.connected && !!this.sessionId && !!this.draft.trim() && !this.sending)
          .onClick(() => { this.send() })
      }.width('100%')
    }.width('100%').padding({ top: 4, bottom: 8 })
  }

  @Builder
  private drawer() {
    Column({ space: 12 }) {
      Row() {
        Text('会话').fontSize(23).fontWeight(FontWeight.Bold).fontColor(TEXT).layoutWeight(1)
        Button('关闭', { type: ButtonType.Normal }).fontSize(13).fontColor(SECONDARY)
          .height(44).padding({ left: 10, right: 10 }).backgroundColor(PANEL).borderRadius(10)
          .onClick(() => { this.sidebar = false })
      }.width('100%')
      if (this.deviceName) {
        Text(this.deviceName).width('100%').fontSize(13).fontColor(SECONDARY)
          .maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
      }
      Divider().color(BORDER)
      List({ scroller: this.sessionList, space: 6 }) {
        ForEach(this.sessions, (session: ViewerSession) => {
          ListItem() {
            Button({ type: ButtonType.Normal }) {
              Row({ space: 10 }) {
                AgentShellStatusIcon({ phase: session.phase, iconSize: 19 })
                Text(session.title).layoutWeight(1).maxLines(2)
                  .textOverflow({ overflow: TextOverflow.Ellipsis }).fontSize(15).fontColor(TEXT)
                if (session.id === this.sessionId) {
                  SymbolGlyph($r('sys.symbol.checkmark')).fontSize(16).fontColor([ACCENT])
                    .accessibilityText('当前会话')
                }
              }.width('100%').padding({ left: 12, right: 12, top: 12, bottom: 12 })
            }.width('100%').constraintSize({ minHeight: 48 }).padding(0).borderRadius(10)
              .backgroundColor(session.id === this.sessionId ? SELECTED : PANEL)
              .accessibilitySelected(session.id === this.sessionId)
              .onClick(() => {
                this.sidebar = false
                this.onSelect(session.id)
              })
          }
        }, (session: ViewerSession) => JSON.stringify([session.id, session.phase, session.title, this.sessionId]))
      }.layoutWeight(1).width('100%').scrollBar(BarState.Auto)
    }.width('80%').height('100%').constraintSize({ maxWidth: 340 }).padding(16)
      .backgroundColor(PANEL)
  }

  build() {
    Stack({ alignContent: Alignment.TopStart }) {
      Column() {
        this.chatHeader()
        Divider().color(BORDER).strokeWidth(0.5)
        this.chat()
        if (this.notice) {
          Row({ space: 10 }) {
            Text(this.notice).fontSize(13).fontColor(SECONDARY).layoutWeight(1)
            if (this.sessionId) {
              Button('重试', { type: ButtonType.Normal }).fontSize(13).fontColor(ACCENT)
                .height(44).backgroundColor(Color.Transparent).onClick(() => { this.onRetry() })
            }
          }.width('100%').padding({ top: 4, bottom: 4 })
        }
        this.composer()
      }.width('100%').height('100%').padding({ left: 16, right: 16 })

      if (this.sidebar) {
        Button({ type: ButtonType.Normal }) {}
          .width('100%').height('100%').borderRadius(0).backgroundColor('#99000000')
          .accessibilityText('关闭会话列表').onClick(() => { this.sidebar = false })
        this.drawer()
      }
      if (this.preview) {
        Column() {
          Row() {
            Blank()
            Button('关闭', { type: ButtonType.Normal }).height(44).fontColor(TEXT)
              .backgroundColor(PANEL).onClick(() => { this.preview = '' })
          }.width('100%').padding(12)
          Image('file://' + this.preview).width('100%').layoutWeight(1).objectFit(ImageFit.Contain)
        }.width('100%').height('100%').backgroundColor('#F0000000')
      }
    }.width('100%').height('100%').backgroundColor(BACKGROUND)
  }
}

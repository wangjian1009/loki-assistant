export interface ViewerSession {
  id: string
  title: string
  phase: string
}

export interface ViewerBlock {
  id: string
  kind: string
  title: string
  text: string
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

function phaseIcon(phase: string): Resource {
  switch (phase) {
    case 'starting': return $r('sys.symbol.hourglass')
    case 'running': return $r('sys.symbol.play_circle')
    case 'waiting_input': return $r('sys.symbol.ellipsis_bubble')
    case 'completed': return $r('sys.symbol.checkmark_circle')
    case 'cancelled': return $r('sys.symbol.stop_circle')
    case 'failed': return $r('sys.symbol.exclamationmark_circle')
    case 'disconnected': return $r('sys.symbol.wifi_slash')
    default: return $r('sys.symbol.circle')
  }
}

function phaseColor(phase: string): string {
  switch (phase) {
    case 'starting':
    case 'running': return ACCENT
    case 'waiting_input': return AMBER
    case 'completed': return GREEN
    case 'failed': return RED
    default: return SECONDARY
  }
}

function phaseLabel(phase: string): string {
  switch (phase) {
    case 'starting': return '启动中'
    case 'running': return '执行中'
    case 'waiting_input': return '等待输入'
    case 'completed': return '已完成'
    case 'cancelled': return '已取消'
    case 'failed': return '执行失败'
    case 'disconnected': return '已断开'
    default: return '空闲'
  }
}

function blockLabel(kind: string): string {
  switch (kind) {
    case 'user': return '你'
    case 'assistant': return 'Agent'
    case 'thought': return '思考'
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
  @Prop @Watch('scrollToSelection') conversationKey: number = 0
  @Prop sessions: ViewerSession[] = []
  @Prop blocks: ViewerBlock[] = []
  @Prop notice: string = ''
  onSelect: (id: string) => void = () => {}
  onRetry: () => void = () => {}
  @State private sidebar: boolean = false
  private readonly history: Scroller = new Scroller()
  private readonly sessionList: Scroller = new Scroller()

  private scrollToSelection(): void {
    this.history.scrollEdge(Edge.Top)
  }

  private selected(): ViewerSession | undefined {
    return this.sessions.find(session => session.id === this.sessionId)
  }

  @Builder
  private status(phase: string) {
    SymbolGlyph(phaseIcon(phase)).fontSize(19).fontColor([phaseColor(phase)])
      .accessibilityLevel('yes').accessibilityText(phaseLabel(phase))
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
      Text(this.selected()?.title || this.title).fontSize(18).fontWeight(FontWeight.Medium)
        .fontColor(TEXT).layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
      if (this.selected()) {
        this.status(this.selected()!.phase)
      }
    }.width('100%').padding({ top: 5, bottom: 10 })
  }

  @Builder
  private message(block: ViewerBlock) {
    Column({ space: 6 }) {
      if (block.title || blockLabel(block.kind)) {
        Text(block.title || blockLabel(block.kind)).width('100%').fontSize(13)
          .fontWeight(FontWeight.Medium).fontColor(block.kind === 'permission' ? AMBER : SECONDARY)
      }
      if (block.text) {
        Text(block.text).width('100%').fontSize(15).fontColor(TEXT)
          .lineHeight(24).wordBreak(WordBreak.BREAK_ALL).copyOption(CopyOptions.LocalDevice)
      }
    }.width('100%').alignItems(HorizontalAlign.Start)
      .padding(block.kind === 'assistant' ? { top: 6, bottom: 6 } : 12)
      .backgroundColor(block.kind === 'user' ? SELECTED : block.kind === 'assistant' ? BACKGROUND : PANEL)
      .borderRadius(12)
  }

  @Builder
  private chat() {
    List({ scroller: this.history, space: 10 }) {
      ForEach(this.blocks, (block: ViewerBlock) => {
        ListItem() {
          this.message(block)
        }
      }, (block: ViewerBlock) => JSON.stringify([block.id, block.title, block.text]))
    }.width('100%').layoutWeight(1).padding({ top: 12, bottom: 16 })
      .divider({ strokeWidth: 0 }).edgeEffect(EdgeEffect.None).scrollBar(BarState.Auto)
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
                this.status(session.phase)
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
      }.width('100%').height('100%').padding({ left: 16, right: 16 })

      if (this.sidebar) {
        Button({ type: ButtonType.Normal }) {}
          .width('100%').height('100%').borderRadius(0).backgroundColor('#99000000')
          .accessibilityText('关闭会话列表').onClick(() => { this.sidebar = false })
        this.drawer()
      }
    }.width('100%').height('100%').backgroundColor(BACKGROUND)
  }
}

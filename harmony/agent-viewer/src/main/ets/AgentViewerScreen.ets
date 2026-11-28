import { AnimatorResult } from '@kit.ArkUI'
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
const COMPOSER_GAP: number = 16
const HORIZONTAL_SWIPE_DISTANCE: number = 48

@Component
struct ImageDownloadProgress {
  @State private segmentOffset: number = 0
  private segmentAnimator?: AnimatorResult

  aboutToAppear(): void {
    const animation = this.getUIContext().createAnimator({
      duration: 1200, easing: 'linear', delay: 0, fill: 'none', direction: 'normal',
      iterations: -1, begin: -32, end: 120
    })
    animation.onFrame = (value: number) => { this.segmentOffset = value }
    this.segmentAnimator = animation
    animation.play()
  }

  aboutToDisappear(): void {
    this.segmentAnimator?.cancel()
    this.segmentAnimator = undefined
  }

  build() {
    Column({ space: 5 }) {
      Text('正在下载图片').fontSize(12).fontColor(SECONDARY)
      Stack({ alignContent: Alignment.Start }) {
        Row().width(32).height(3).borderRadius(2).backgroundColor(ACCENT)
          .translate({ x: this.segmentOffset })
      }.width(120).height(3).backgroundColor(BORDER).borderRadius(2).clip(true)
        .accessibilityLevel('no-hide-descendants')
    }.width('100%').alignItems(HorizontalAlign.Start)
  }
}

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

@Observed
class MessageRow {
  readonly key: string
  content: ConversationRow

  constructor(conversationKey: number, content: ConversationRow) {
    this.key = `${conversationKey}/${content.id}`
    this.content = content
  }

  update(next: ConversationRow): boolean {
    const current = this.content
    if (current.kind !== next.kind || current.title !== next.title || current.text !== next.text ||
        current.depth !== next.depth || current.foldable !== next.foldable || current.collapsed !== next.collapsed) {
      this.content = next
      return true
    }
    return false
  }
}

@Component
struct ConversationMessage {
  @ObjectLink row: MessageRow
  @Prop imageStatus: boolean | undefined = undefined
  readImage: (source: string) => Promise<string> = () => Promise.reject(new Error('图片不可用'))
  parseDocument: (source: string, format: string) => string = () => ''
  onToggle: (row: ConversationRow) => void = () => {}
  onOpenImage: (rowId: string, source: string) => void = () => {}
  onContentReady: () => void = () => {}

  build() {
    Row() {
      Column({ space: 6 }) {
        if (this.row.content.foldable) {
          Button({ type: ButtonType.Normal }) {
            Row({ space: 8 }) {
              SymbolGlyph(this.row.content.collapsed ? $r('sys.symbol.chevron_right') : $r('sys.symbol.chevron_down'))
                .fontSize(10).fontColor([DETAIL])
              Text(this.row.content.title || blockLabel(this.row.content.kind)).layoutWeight(1)
                .fontSize(12).fontColor(this.row.content.kind === 'permission' ? AMBER : DETAIL)
                .maxLines(this.row.content.collapsed ? 1 : 2).textOverflow({ overflow: TextOverflow.Ellipsis })
            }.width('100%')
          }.width('100%').constraintSize({ minHeight: 32 }).padding(0).backgroundColor(Color.Transparent)
            .accessibilityGroup(true)
            .accessibilityText((this.row.content.title || blockLabel(this.row.content.kind)) + (this.row.content.collapsed ? '，已折叠' : '，已展开'))
            .onClick(() => { this.onToggle(this.row.content) })
        } else if (this.row.content.title || blockLabel(this.row.content.kind)) {
          Text(this.row.content.title || blockLabel(this.row.content.kind)).width('100%').fontSize(13)
            .fontWeight(FontWeight.Medium).fontColor(this.row.content.kind === 'user' ? ACCENT :
              this.row.content.kind === 'assistant' ? GREEN : this.row.content.kind === 'permission' ? AMBER : SECONDARY)
        }
        if (this.row.content.text && !this.row.content.collapsed) {
          if (this.row.content.kind === 'assistant' || this.row.content.kind === 'thought' ||
            this.row.content.kind === 'tool' ||
            (this.row.content.kind === 'user' && this.row.content.text.includes('![图片]('))) {
            OrgRenderer({ source: this.row.content.text, format: this.row.content.kind === 'user' ? 'markdown' : 'auto',
              subdued: this.row.content.foldable, parse: this.parseDocument, readImage: this.readImage,
              onOpenImage: (source: string): void => { this.onOpenImage(this.row.content.id, source) },
              onContentReady: (): void => { this.onContentReady() } })
            if (this.imageStatus === true) {
              ImageDownloadProgress()
            } else if (this.imageStatus === false) {
              Text('图片下载失败，重新点击可重试').fontSize(12).fontColor(RED)
            }
          } else {
            Text(this.row.content.text).width('100%').fontSize(15).fontColor(TEXT)
              .lineHeight(24).wordBreak(WordBreak.BREAK_ALL).copyOption(CopyOptions.LocalDevice)
          }
        }
      }.width('100%').alignItems(HorizontalAlign.Start)
        .padding(this.row.content.foldable ? { bottom: this.row.content.collapsed || !this.row.content.text ? 0 : 8, left: 8, right: 8 } :
          this.row.content.kind === 'user' ? { top: 10, bottom: 10, left: 12, right: 12 } : { top: 6, bottom: 6 })
        .backgroundColor(this.row.content.kind === 'user' ? SELECTED : Color.Transparent)
        .borderRadius(12)
    }.width('100%').padding({ left: this.row.content.depth * 12 })
      .border({ width: { left: this.row.content.depth > 0 ? 1 : 0 }, color: BORDER })
  }
}

@Component
export struct AgentViewerScreen {
  @Prop title: ResourceStr
  @Prop @Watch('sessionChanged') sessionId: string = ''
  @Prop @Watch('resetConversation') conversationKey: number = 0
  @Prop sessions: ViewerSession[] = []
  @Prop @Watch('refreshRows') blocks: ViewerBlock[] = []
  @Prop notice: string = ''
  @Prop connected: boolean = false
  @Prop connectionColor: string = SECONDARY
  @Prop connectionLabel: string = '离线'
  @Link preview: string
  @BuilderParam sessionActions?: () => void
  onSelect: (id: string) => void = () => {}
  onRetry: () => void = () => {}
  onSend: (text: string) => Promise<string> = () => Promise.resolve('无法发送消息')
  readImage: (source: string) => Promise<string> = () => Promise.reject(new Error('图片不可用'))
  parseDocument: (source: string, format: string) => string = () => ''
  @State private rows: MessageRow[] = []
  private readonly folds: Map<string, boolean> = new Map()
  @State private draft: string = ''
  @State private pendingMessages: string[] = []
  @State private sendError: string = ''
  @State private imageDownloads: Map<string, boolean> = new Map()
  @State private sessionOffset: number = 0
  @State private switchingTo: string = ''
  private sessionWidth: number = 0
  private slideFrame: number = -1
  private followTimer: number = -1
  private following: boolean = true
  private readonly history: Scroller = new Scroller()

  aboutToAppear(): void { this.refreshRows() }

  aboutToDisappear(): void {
    clearTimeout(this.followTimer)
    this.followTimer = -1
    clearTimeout(this.slideFrame)
    this.switchingTo = ''
  }

  private resetConversation(): void {
    this.folds.clear()
    this.rows = []
    this.following = true
    this.draft = ''
    this.pendingMessages = []
    this.sendError = ''
    this.imageDownloads = new Map()
    this.preview = ''
    this.refreshRows()
  }

  private refreshRows(): void {
    const existing = new Map<string, MessageRow>()
    this.rows.forEach(row => existing.set(row.content.id, row))
    let contentChanged = false
    const rows = conversationRows(this.blocks, this.folds).map((content: ConversationRow): MessageRow => {
      const row = existing.get(content.id)
      if (!row) return new MessageRow(this.conversationKey, content)
      if (row.update(content)) contentChanged = true
      return row
    })
    if (rows.length !== this.rows.length || rows.some((row, index) => row !== this.rows[index])) {
      this.rows = rows
      contentChanged = true
    }
    if (contentChanged) this.followLatest()
  }

  private followLatest(): void {
    if (!this.following || this.followTimer !== -1 || this.rows.length === 0) return
    this.followTimer = setTimeout(() => {
      this.followTimer = -1
      if (this.following && this.rows.length > 0) {
        this.history.scrollToIndex(this.rows.length - 1, false, ScrollAlign.END)
      }
    }, 16)
  }

  private async send(): Promise<void> {
    const text = this.draft
    if (!this.sessionId || !text.trim() || this.pendingMessages.includes(text)) return
    const key = this.conversationKey
    this.pendingMessages = [...this.pendingMessages, text]
    this.sendError = ''
    try {
      const error = await this.onSend(text)
      if (this.conversationKey !== key) return
      if (error) {
        this.sendError = error
      } else {
        if (this.draft === text) this.draft = ''
        this.following = true
        this.followLatest()
      }
    } catch (_) {
      if (this.conversationKey === key) this.sendError = '无法发送消息'
    } finally {
      if (this.conversationKey === key) {
        this.pendingMessages = this.pendingMessages.filter(message => message !== text)
      }
    }
  }

  private async openImage(rowId: string, source: string): Promise<void> {
    if (this.imageDownloads.get(rowId) === true) return
    const key = this.conversationKey
    this.imageDownloads.set(rowId, true)
    this.followLatest()
    try {
      const path = await this.readImage(source)
      if (key !== this.conversationKey) return
      this.imageDownloads.delete(rowId)
      this.preview = path
    } catch (_) {
      if (key === this.conversationKey) this.imageDownloads.set(rowId, false)
    } finally {
      if (key === this.conversationKey) this.followLatest()
    }
  }

  private toggleFold(row: ConversationRow): void {
    this.folds.set(row.id, !row.collapsed)
    if (!row.collapsed) this.following = true
    this.refreshRows()
  }

  private sessionIndex(): number {
    return this.sessions.findIndex(session => session.id === this.sessionId)
  }

  private selected(): ViewerSession | undefined {
    return this.sessions[this.sessionIndex()]
  }

  private adjacentSession(direction: number): ViewerSession | undefined {
    const current = this.sessionIndex()
    return current < 0 ? undefined : this.sessions[current + direction]
  }

  private resetSessionOffset(): void {
    if (this.sessionOffset === 0 || this.switchingTo) return
    this.getUIContext().animateTo({ duration: 160, curve: Curve.EaseOut }, () => {
      this.sessionOffset = 0
    })
  }

  private dragSession(event: GestureEvent): void {
    if (this.preview || this.switchingTo || this.sessions.length < 2 || this.sessionWidth <= 0) return
    const next = this.adjacentSession(event.offsetX < 0 ? 1 : -1)
    const limit = next ? this.sessionWidth * 0.62 : 26
    const offset = next ? event.offsetX : event.offsetX * 0.2
    this.sessionOffset = Math.min(limit, Math.max(-limit, offset))
  }

  private swipeSession(event: GestureEvent): void {
    if (Math.abs(event.offsetX) >= HORIZONTAL_SWIPE_DISTANCE && Math.abs(event.offsetX) >= Math.abs(event.offsetY)) {
      this.switchSession(event.offsetX < 0 ? 1 : -1)
    } else {
      this.resetSessionOffset()
    }
  }

  private switchSession(direction: number): void {
    if (this.preview || this.switchingTo) return
    const next = this.adjacentSession(direction)
    if (!next) {
      this.resetSessionOffset()
      return
    }
    if (this.sessionWidth <= 0) {
      this.onSelect(next.id)
      return
    }
    const target = next.id
    const outgoing = -direction * this.sessionWidth
    this.switchingTo = target
    this.getUIContext().animateTo({
      duration: 145, curve: Curve.EaseIn, onFinish: () => {
        if (this.switchingTo !== target) return
        if (this.preview || !this.sessions.some(session => session.id === target)) {
          this.switchingTo = ''
          this.resetSessionOffset()
          return
        }
        this.sessionOffset = -outgoing
        this.onSelect(target)
      }
    }, () => { this.sessionOffset = outgoing })
  }

  private sessionChanged(): void {
    clearTimeout(this.slideFrame)
    const target = this.switchingTo
    if (!target || target !== this.sessionId) {
      this.switchingTo = ''
      this.sessionOffset = 0
      return
    }
    // 先让新会话在另一侧完成排版，再滑入可视区域。
    this.slideFrame = setTimeout(() => {
      if (this.switchingTo !== target) return
      this.getUIContext().animateTo({
        duration: 205, curve: Curve.EaseOut, onFinish: () => {
          if (this.switchingTo === target) this.switchingTo = ''
        }
      }, () => { this.sessionOffset = 0 })
    }, 32)
  }

  @Builder
  private sessionArrow(direction: number) {
    Button({ type: ButtonType.Normal }) {
      SymbolGlyph(direction < 0 ? $r('sys.symbol.chevron_left') : $r('sys.symbol.chevron_right'))
        .fontSize(15).fontColor([this.adjacentSession(direction) ? ACCENT : DETAIL])
    }.width(38).height(44).padding(0).borderRadius(12)
      .backgroundColor(this.adjacentSession(direction) ? PANEL : Color.Transparent)
      .enabled(!!this.adjacentSession(direction) && !this.switchingTo)
      .accessibilityText(direction < 0 ? '上一会话' : '下一会话')
      .onClick(() => { this.switchSession(direction) })
  }

  @Builder
  private chatHeader() {
    Row({ space: 10 }) {
      Column({ space: 2 }) {
        Row({ space: 6 }) {
          Circle().width(6).height(6).fill(this.connectionColor).flexShrink(0)
            .accessibilityText('设备' + this.connectionLabel)
          Text(this.selected()?.title || this.title).fontSize(18).fontWeight(FontWeight.Medium)
            .fontColor(TEXT).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis }).flexShrink(1)
        }.width('100%')
        if (this.selected()) {
          Row({ space: 5 }) {
            AgentShellStatusIcon({ phase: this.connected ? this.selected()!.phase : 'disconnected', iconSize: 12 })
            Text(this.connected ? agentShellAppearance(this.selected()!.phase).label : '电脑离线')
              .fontSize(12).fontColor(agentShellAppearance(this.connected ? this.selected()!.phase : 'disconnected').color)
          }
        }
      }.layoutWeight(1).alignItems(HorizontalAlign.Start)
      if (this.sessionIndex() >= 0 && this.sessions.length > 1) {
        Column({ space: 2 }) {
          Row({ space: 5 }) {
            this.sessionArrow(-1)
            Text(`${this.sessionIndex() + 1} / ${this.sessions.length}`).fontSize(12).fontColor(SECONDARY)
            this.sessionArrow(1)
          }
          Text('左右滑动切换').fontSize(11).fontColor(DETAIL)
        }.flexShrink(0)
      }
      if (this.sessionId && this.sessionActions) {
        this.sessionActions()
      }
    }.width('100%').padding({ top: 5, bottom: 10 })
  }

  @Builder
  private chat() {
    List({ scroller: this.history, space: 8 }) {
      ForEach(this.rows, (row: MessageRow) => {
        ListItem() {
          ConversationMessage({ row: row, imageStatus: this.imageDownloads.get(row.content.id),
            readImage: this.readImage, parseDocument: this.parseDocument,
            onToggle: (item: ConversationRow): void => { this.toggleFold(item) },
            onOpenImage: (rowId: string, source: string): void => { this.openImage(rowId, source) },
            onContentReady: (): void => { this.followLatest() } })
        }
      }, (row: MessageRow) => row.key)
    }.width('100%').layoutWeight(1)
      .contentStartOffset(12).contentEndOffset(COMPOSER_GAP).stackFromEnd(true)
      .edgeEffect(EdgeEffect.None).scrollBar(BarState.Auto)
      .onAreaChange((_oldArea: Area, _newArea: Area) => { this.followLatest() })
      .onWillScroll((offset: number, _state: ScrollState, source: ScrollSource) => {
        if (offset !== 0 && (source === ScrollSource.DRAG || source === ScrollSource.OTHER_USER_INPUT ||
          source === ScrollSource.SCROLL_BAR)) this.following = false
      })
      .onScrollStop(() => {
        if (this.history.isAtEnd()) this.following = true
      })
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
          .onFocus(() => { this.following = true; this.followLatest() })
          .onChange((text: string) => { this.draft = text })
          .onSubmit((_key: EnterKeyType, event: SubmitEvent) => {
            event.keepEditableState()
            this.draft = event.text
            this.send()
          })
        Button(this.pendingMessages.includes(this.draft) ? '发送中' : '发送',
          { type: ButtonType.Normal }).height(44)
          .fontSize(14).fontColor('#182143').backgroundColor(ACCENT)
          .enabled(!!this.sessionId && !!this.draft.trim() && !this.pendingMessages.includes(this.draft))
          .onClick(() => { this.send() })
      }.width('100%')
    }.width('100%').padding({ top: 4, bottom: 8 })
  }

  build() {
    Stack({ alignContent: Alignment.TopStart }) {
      Column() {
        Column() {
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
          }.width('100%').height('100%').translate({ x: this.sessionOffset })
        }.width('100%').layoutWeight(1)
          .clip(true)
          .onSizeChange((_old: SizeOptions, current: SizeOptions) => {
            this.sessionWidth = Number(current.width)
          })
          .gesture(PanGesture({ direction: PanDirection.Horizontal })
            .onActionUpdate((event: GestureEvent) => { this.dragSession(event) })
            .onActionEnd((event: GestureEvent) => { this.swipeSession(event) })
            .onActionCancel(() => { this.resetSessionOffset() }))
        this.composer()
      }.width('100%').height('100%').padding({ left: 16, right: 16 })
        .accessibilityLevel(this.preview ? 'no-hide-descendants' : 'auto')

      if (this.preview) {
        Column() {
          Row() {
            Blank()
            Button('关闭', { type: ButtonType.Normal }).height(44).fontColor(TEXT)
              .backgroundColor(PANEL).onClick(() => { this.preview = '' })
          }.width('100%').padding(12)
          Image('file://' + this.preview).width('100%').layoutWeight(1).objectFit(ImageFit.Contain)
        }.width('100%').height('100%').backgroundColor('#F0000000')
          .gesture(PanGesture({ direction: PanDirection.Left })
            .onActionEnd((event: GestureEvent) => {
              if (event.offsetX <= -HORIZONTAL_SWIPE_DISTANCE && -event.offsetX >= Math.abs(event.offsetY)) {
                this.preview = ''
              }
            }))
      }
    }.width('100%').height('100%').backgroundColor(BACKGROUND)
  }
}

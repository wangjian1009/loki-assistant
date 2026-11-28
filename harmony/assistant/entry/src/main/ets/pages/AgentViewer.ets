import { KeyboardAvoidMode } from '@kit.ArkUI'
import { AgentAvatar, AgentViewerScreen, ViewerAgent, ViewerBlock, ViewerSession, ViewerSessionPreview } from 'agent_viewer'
import { AgentShell, AgentShellAgent, AgentShellBlock, AgentShellHistorySession, AgentShellProject, AgentShellSession, AgentShellConversationView,
  ConversationSubscription, ResponsError, renderDocument } from 'liblokiworkspace.so'
import { Application, ComputerDevice } from '../app/Application'
import { ACCENT, ACCENT_TEXT, AppPage, BODY_SIZE, BORDER, ERROR_TEXT, META_SIZE, PAGE_BACKGROUND, PAGE_MAX_WIDTH,
  PANEL_RADIUS, PRIMARY_TEXT, SECONDARY_TEXT, SURFACE, TOUCH_SIZE } from '../components/AppPage'
import { deviceConnectionAppearance } from '../components/DeviceConnection'
import { PopupMenu } from '../components/PopupMenu'
import { StatusSubscription as DeviceSubscription } from '../workspace/ServiceStatus'

type SessionAction = 'restart' | 'clear-buffer'
type CreationStep = 1 | 2 | 3

const RECENT_PROJECT_COUNT: number = 5

interface ConversationNotice {
  text: string
  canRetry?: boolean
}

function viewerBlock(block: AgentShellBlock): ViewerBlock {
  return { id: block.id, kind: block.kind, title: block.title ?? '', text: block.text,
    parentId: block.parent_id ?? '', collapsed: block.collapsed }
}

@Entry
@Component
struct AgentViewer {
  @StorageLink('lokiAgentViewerForeground') @Watch('foregroundChanged') private foreground: boolean = true
  @StorageLink('lokiAgentSessionRequest') @Watch('applySessionRequest') private sessionRequest: string = ''
  @State private sessions: ViewerSession[] = []
  @State private agents: ViewerAgent[] = []
  @State private blocks: ViewerBlock[] = []
  @State private adjacentViews: ViewerSessionPreview[] = []
  @State private selected: string = ''
  @State private notice: ConversationNotice = { text: '正在读取会话' }
  @State private conversationKey: number = 0
  @State private deviceStatus: string = 'inactive'
  @State private deviceReconnecting: boolean = false
  @State private imagePreview: string = ''
  @State private pendingAction: SessionAction | '' = ''
  @State private creationOpen: boolean = false
  @State private creationLoading: boolean = false
  @State private creating: boolean = false
  @State private creationLoadError: string = ''
  @State private creationError: string = ''
  @State @Watch('resetCreationScroll') private creationStep: CreationStep = 1
  @State private showAllProjects: boolean = false
  @State private projects: AgentShellProject[] = []
  @State private creationAgents: ViewerAgent[] = []
  @State private history: AgentShellHistorySession[] = []
  @State private historyLoading: boolean = false
  @State private historyError: string = ''
  @State private historyCanRetry: boolean = false
  @State private selectedHistoryId: string | null = null
  @State private projectIndex: number = -1
  @State private agentIndex: number = -1
  private creationVersion: number = 0
  private historyVersion: number = 0
  private historyRequested: boolean = false
  private readonly creationScroller: Scroller = new Scroller()
  private pendingSessionId: string = ''
  private visible: boolean = false
  private active: boolean = false
  private lifetime: number = 0
  private streamVersion: number = 0
  private agentRequestVersion: number = 0
  private agentsRequested: boolean = false
  private endpointId: string = ''
  private shell?: AgentShell
  private devices?: DeviceSubscription
  private conversation?: ConversationSubscription
  private readonly previewsLoading: Set<string> = new Set()
  private previewVersion: number = 0

  private deviceOnline(): boolean { return this.deviceStatus === 'online' }

  onPageShow(): void {
    this.getUIContext().setKeyboardAvoidMode(KeyboardAvoidMode.RESIZE)
    this.visible = true
    this.applySessionRequest()
    this.foregroundChanged()
  }

  private applySessionRequest(): void {
    if (!this.sessionRequest) return
    try {
      const request = JSON.parse(this.sessionRequest) as [string, number]
      this.pendingSessionId = request[0]
      if (this.sessions.some(session => session.id === this.pendingSessionId)) {
        const id = this.pendingSessionId
        this.pendingSessionId = ''
        this.select(id)
      }
    } catch (_) {
      this.pendingSessionId = ''
    } finally {
      this.sessionRequest = ''
    }
  }

  onPageHide(): void {
    this.visible = false
    this.foregroundChanged()
  }

  onBackPress(): boolean {
    if (this.creationOpen) {
      this.backInCreation()
      return true
    }
    if (!this.imagePreview) return false
    this.imagePreview = ''
    return true
  }

  aboutToDisappear(): void {
    this.visible = false
    this.foregroundChanged()
    this.shell = undefined
  }

  private foregroundChanged(): void {
    const shouldWatch = this.visible && this.foreground
    if (shouldWatch === this.active) return
    this.active = shouldWatch
    const lifetime = ++this.lifetime
    if (shouldWatch) {
      this.watch(lifetime)
    } else {
      this.resetAgentRequest()
      this.detachConversation()
      this.devices?.cancel()
      this.devices = undefined
      this.resetAdjacentViews()
    }
  }

  private async watch(lifetime: number): Promise<void> {
    const application = AppStorage.get<Application>('lokiApplication')
    if (!application) {
      this.notice = { text: '无法读取设备配置' }
      return
    }
    try {
      await application.prepare()
      const workspace = await application.workspace.open()
      if (lifetime !== this.lifetime) return
      if (!this.shell) this.shell = workspace.openAgentShell()
      const subscription = await application.watchDevices((devices: ComputerDevice[]) => {
        if (lifetime !== this.lifetime) return
        const current = devices.find(device => device.active)
        const endpointId = current?.endpoint_id ?? ''
        if (endpointId !== this.endpointId) {
          this.resetAgentRequest()
          this.detachConversation()
          this.resetAdjacentViews()
          this.closeSessionCreation()
          if (this.endpointId) this.pendingSessionId = ''
          this.endpointId = endpointId
          this.sessions = []
          this.agents = []
          this.selected = ''
          this.blocks = []
          this.conversationKey += 1
        }
        this.deviceStatus = current?.status ?? 'inactive'
        this.deviceReconnecting = current?.reconnecting ?? false
        if (!current || !this.deviceOnline()) {
          this.resetAgentRequest()
          if (current && this.selected) {
            if (!this.conversation) this.openConversation()
          } else {
            this.notice = { text: current ? deviceConnectionAppearance(current.status).label : '请在设备管理中选择一台电脑' }
          }
        } else {
          this.loadAgents(endpointId)
          this.updateSessions(current.dashboard?.agent_shell_sessions ?? null)
        }
      }, (message: string) => {
        if (lifetime === this.lifetime) this.notice = { text: message }
      })
      if (lifetime !== this.lifetime) {
        subscription.cancel()
        return
      }
      this.devices = subscription
    } catch (_) {
      if (lifetime === this.lifetime) this.notice = { text: '无法读取会话状态' }
    }
  }

  private resetAgentRequest(): void {
    this.agentRequestVersion += 1
    this.agentsRequested = false
  }

  private async loadAgents(endpointId: string): Promise<void> {
    const shell = this.shell
    if (!shell || this.agentsRequested) return
    this.agentsRequested = true
    const version = this.agentRequestVersion
    try {
      const agents = await shell.listAgents(endpointId)
      if (version !== this.agentRequestVersion) return
      this.agents = agents.map((agent: AgentShellAgent): ViewerAgent => ({
        id: agent.id, name: agent.name, icon: agent.icon ?? ''
      }))
    } catch (_) {
      // 保留当前电脑上一次成功返回的列表，下次上线或回到前台时刷新。
    }
  }

  private updateSessions(sessions: AgentShellSession[] | null): void {
    if (sessions === null) {
      if (this.deviceOnline() && !this.selected) this.notice = { text: '尚未收到会话状态' }
      return
    }
    this.sessions = sessions.map((session, index): ViewerSession => ({
      id: session.id, agentId: session.agent_id ?? '', title: session.title || '会话 ' + (index + 1), phase: session.phase
    }))
    if (this.sessions.length === 0) {
      this.detachConversation()
      this.resetAdjacentViews()
      if (this.selected) this.conversationKey += 1
      this.selected = ''
      this.blocks = []
      this.notice = { text: '电脑上暂无打开的会话' }
    } else if (this.pendingSessionId && this.sessions.some(session => session.id === this.pendingSessionId)) {
      const id = this.pendingSessionId
      this.pendingSessionId = ''
      this.select(id)
    } else if (!this.sessions.some(session => session.id === this.selected)) {
      const first = this.sessions.find(session => session.phase === 'running' || session.phase === 'waiting_input')
        ?? this.sessions[0]
      this.select(first.id)
    } else if (!this.conversation) {
      this.openConversation()
    }
    this.loadAdjacentViews()
  }

  private detachConversation(): void {
    this.streamVersion += 1
    this.conversation?.cancel()
    this.conversation = undefined
  }

  private select(sessionId: string): void {
    if (this.selected === sessionId) return
    const incoming = this.adjacentViews.find(view => view.sessionId === sessionId)
    const outgoing = this.selected && this.blocks.length > 0
      ? { sessionId: this.selected, blocks: this.blocks } as ViewerSessionPreview : undefined
    this.detachConversation()
    this.selected = sessionId
    this.blocks = incoming?.blocks ?? []
    this.notice = { text: incoming ? (this.blocks.length ? '' : '暂无会话内容') : '正在读取会话' }
    if (outgoing) {
      this.adjacentViews = [...this.adjacentViews.filter(view => view.sessionId !== outgoing.sessionId), outgoing]
    }
    this.conversationKey += 1
    this.openConversation()
    this.loadAdjacentViews()
  }

  private resetAdjacentViews(): void {
    this.previewVersion += 1
    this.previewsLoading.clear()
    this.adjacentViews = []
  }

  private adjacentIds(): string[] {
    const position = this.sessions.findIndex(session => session.id === this.selected)
    if (position < 0) return []
    return this.sessions.slice(Math.max(0, position - 1), position + 2)
      .filter(session => session.id !== this.selected).map(session => session.id)
  }

  private loadAdjacentViews(): void {
    const shell = this.shell
    if (!shell || !this.active || !this.deviceOnline()) return
    const ids = this.adjacentIds()
    this.adjacentViews = this.adjacentViews.filter(view => ids.includes(view.sessionId))
    for (const id of ids) {
      if (this.previewsLoading.has(id) || this.adjacentViews.some(view => view.sessionId === id)) continue
      this.previewsLoading.add(id)
      const version = this.previewVersion
      shell.previewConversation(id).then((blocks: AgentShellBlock[]) => {
        if (version === this.previewVersion && this.adjacentIds().includes(id)) {
          this.adjacentViews = [...this.adjacentViews.filter(view => view.sessionId !== id),
            { sessionId: id, blocks: blocks.map(viewerBlock) }]
        }
      }).catch(() => {
        // 会话切换后仍使用常规订阅获取正文。
      }).finally(() => {
        if (version === this.previewVersion) this.previewsLoading.delete(id)
      })
    }
  }

  private async submit(text: string): Promise<string> {
    const shell = this.shell
    const sessionId = this.selected
    if (!sessionId) return '请先选择会话'
    if (!shell) return '无法连接电脑'
    try {
      const error = await shell.sendMessage(sessionId, text)
      return error?.message ?? ''
    } catch (_) {
      return '无法发送消息'
    }
  }

  private async performSessionAction(action: SessionAction): Promise<void> {
    const shell = this.shell
    const session = this.selected
    if (!shell || !session || !this.deviceOnline() || this.pendingAction) return
    const endpoint = this.endpointId
    this.pendingAction = action
    let message = action === 'restart' ? '无法重启会话' : '无法清空缓冲区'
    try {
      const error = action === 'restart' ? await shell.restartSession(session) : await shell.clearBuffer(session)
      message = error?.message ?? (action === 'restart' ? '已请求重启会话' : '已清空缓冲区')
    } catch (_) {
      // 保留当前操作的失败提示。
    } finally {
      this.pendingAction = ''
    }
    if (this.visible && endpoint === this.endpointId && session === this.selected) {
      try { this.getUIContext().getPromptAction().showToast({ message }) }
      catch (_) { console.error('无法显示会话操作结果') }
    }
  }

  private closeSessionCreation(): void {
    this.creationVersion += 1
    this.creationOpen = false
    this.creating = false
  }

  private backInCreation(): void {
    if (this.creating) return
    this.creationError = ''
    if (this.creationStep === 3) {
      this.creationStep = 2
    } else if (this.creationStep === 2) {
      this.creationStep = 1
    } else {
      this.closeSessionCreation()
    }
  }

  private editCreationStep(step: CreationStep): void {
    if (this.creating || step >= this.creationStep) return
    this.creationError = ''
    this.creationStep = step
  }

  private resetHistory(): void {
    this.historyVersion += 1
    this.historyRequested = false
    this.historyLoading = false
    this.historyError = ''
    this.historyCanRetry = false
    this.selectedHistoryId = null
    this.history = []
  }

  private resetCreationScroll(): void {
    if (this.creationOpen) this.creationScroller.scrollEdge(Edge.Top)
  }

  private async openSessionCreation(): Promise<void> {
    const shell = this.shell
    if (!shell || !this.deviceOnline() || this.creating) return
    const endpoint = this.endpointId
    const version = ++this.creationVersion
    this.creationLoadError = ''
    this.creationError = ''
    this.creationStep = 1
    this.showAllProjects = false
    this.projectIndex = -1
    this.agentIndex = -1
    this.resetHistory()
    this.creationLoading = true
    this.getUIContext().getFocusController().clearFocus()
    this.creationOpen = true
    try {
      const projects = new Promise<AgentShellProject[]>((resolve, reject) => {
        shell.listProjects(endpoint, resolve, (error: ResponsError) => { reject(error) })
      })
      const result = await Promise.all([projects, shell.listAgents(endpoint)])
      if (version !== this.creationVersion) return
      this.projects = result[0]
      this.creationAgents = result[1].map((agent: AgentShellAgent): ViewerAgent => ({
        id: agent.id, name: agent.name, icon: agent.icon ?? ''
      }))
      this.agents = this.creationAgents
    } catch (_) {
      if (version === this.creationVersion) this.creationLoadError = '无法读取项目与 Agent'
    } finally {
      if (version === this.creationVersion) this.creationLoading = false
    }
  }

  private advanceCreation(): void {
    if (this.creating || !this.deviceOnline() || this.projectIndex < 0) return
    if (this.creationStep === 1) {
      this.creationStep = 2
    } else if (this.agentIndex >= 0) {
      if (this.creationStep === 2) {
        this.creationStep = 3
        if (!this.historyRequested) this.loadHistory()
      } else {
        this.createSession(this.selectedHistoryId)
      }
    }
  }

  private loadHistory(): void {
    const shell = this.shell
    const project = this.projects[this.projectIndex]
    const agent = this.creationAgents[this.agentIndex]
    if (!shell || !project || !agent || !this.deviceOnline()) return
    const version = ++this.historyVersion
    const creation = this.creationVersion
    this.historyRequested = true
    this.historyError = ''
    this.historyCanRetry = false
    this.historyLoading = true
    try {
      shell.listHistory(this.endpointId, project.id, agent.id, (sessions: AgentShellHistorySession[]) => {
        if (version !== this.historyVersion || creation !== this.creationVersion) return
        this.history = sessions
        this.historyLoading = false
      }, (error: ResponsError) => {
        if (version !== this.historyVersion || creation !== this.creationVersion) return
        this.historyError = error.message
        this.historyCanRetry = error.kind !== 'services' || error.code !== 'agent-shell-history-unsupported'
        this.historyLoading = false
      })
    } catch (_) {
      this.historyError = '无法读取历史会话'
      this.historyCanRetry = true
      this.historyLoading = false
    }
  }

  private createSession(historyId: string | null): void {
    const shell = this.shell
    const project = this.projects[this.projectIndex]
    const agent = this.creationAgents[this.agentIndex]
    if (!shell || !project || !agent || !this.deviceOnline() || this.creating) return
    const version = this.creationVersion
    this.creating = true
    this.creationError = ''
    try {
      shell.createSession(this.endpointId, project.id, agent.id, historyId, (id: string) => {
        if (version !== this.creationVersion) return
        this.closeSessionCreation()
        if (this.sessions.some(session => session.id === id)) {
          this.pendingSessionId = ''
          this.select(id)
        } else {
          this.pendingSessionId = id
        }
      }, (error: ResponsError) => {
        if (version !== this.creationVersion) return
        this.creating = false
        this.creationError = error.outcome === 'unknown' ? '操作结果未知，请先在会话列表中确认' : error.message
      })
    } catch (_) {
      this.creating = false
      this.creationError = historyId ? '无法提交恢复请求' : '无法提交创建请求'
    }
  }

  private historyDate(value: string): string {
    const date = new Date(value)
    return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN')
  }

  @Builder
  private choiceCheck() {
    SymbolGlyph($r('sys.symbol.checkmark')).fontSize(16).fontColor([ACCENT]).accessibilityLevel('no')
  }

  @Builder
  private historyRow(session: AgentShellHistorySession) {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 10 }) {
        Column({ space: 2 }) {
          Text(session.title?.trim() || session.id).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
            .maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
          if (session.updated_at) {
            Text(this.historyDate(session.updated_at)).fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
          }
        }.layoutWeight(1).alignItems(HorizontalAlign.Start)
        if (this.selectedHistoryId === session.id) {
          this.choiceCheck()
        }
      }.width('100%').padding({ top: 8, bottom: 8 })
    }.width('100%').constraintSize({ minHeight: TOUCH_SIZE }).padding(0)
      .backgroundColor(Color.Transparent)
      .accessibilitySelected(this.selectedHistoryId === session.id)
      .enabled(!this.creating).onClick(() => { this.selectedHistoryId = session.id; this.creationError = '' })
  }

  @Builder
  private projectRow(project: AgentShellProject, index: number) {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 10 }) {
        Column({ space: 2 }) {
          Text(project.name).fontSize(BODY_SIZE).fontWeight(FontWeight.Medium)
            .fontColor(this.projectIndex === index ? ACCENT : PRIMARY_TEXT)
          if (project.path) {
            Text(project.path).fontSize(META_SIZE).fontColor(SECONDARY_TEXT).wordBreak(WordBreak.BREAK_ALL)
          }
        }.layoutWeight(1).alignItems(HorizontalAlign.Start)
        if (this.projectIndex === index) {
          this.choiceCheck()
        }
      }.width('100%').padding({ top: 8, bottom: 8 })
    }.width('100%').constraintSize({ minHeight: TOUCH_SIZE }).padding(0)
      .backgroundColor(Color.Transparent)
      .accessibilityText(project.path ? `${project.name}，${project.path}` : project.name)
      .accessibilitySelected(this.projectIndex === index)
      .onClick(() => {
        if (this.projectIndex !== index) {
          this.projectIndex = index
          this.resetHistory()
        }
      })
  }

  @Builder
  private agentRow(agent: ViewerAgent, index: number) {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 10 }) {
        AgentAvatar({ name: agent.name, source: agent.icon })
        Text(agent.name).fontSize(BODY_SIZE).fontColor(this.agentIndex === index ? ACCENT : PRIMARY_TEXT).layoutWeight(1)
        if (this.agentIndex === index) {
          this.choiceCheck()
        }
      }.width('100%')
    }.width('100%').constraintSize({ minHeight: TOUCH_SIZE }).padding(0)
      .backgroundColor(Color.Transparent).accessibilitySelected(this.agentIndex === index)
      .onClick(() => {
        if (this.agentIndex !== index) {
          this.agentIndex = index
          this.resetHistory()
        }
      })
  }

  @Builder
  private projectChoices() {
    Row() {
      Text(this.showAllProjects ? '全部项目' : '最近项目').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).layoutWeight(1)
      if (this.projects.length > RECENT_PROJECT_COUNT) {
        Button(this.showAllProjects ? '仅看最近' : '查看全部', { type: ButtonType.Normal })
          .height(TOUCH_SIZE).padding({ left: 10, right: 0 }).fontSize(BODY_SIZE)
          .fontColor(ACCENT).backgroundColor(Color.Transparent)
          .onClick(() => { this.showAllProjects = !this.showAllProjects })
      }
    }.width('100%').constraintSize({ minHeight: 28 })
    ForEach(this.showAllProjects ? this.projects : this.projects.slice(0, RECENT_PROJECT_COUNT),
      (project: AgentShellProject, index: number) => { this.projectRow(project, index) },
      (project: AgentShellProject) => project.id)
    if (!this.showAllProjects && this.projectIndex >= RECENT_PROJECT_COUNT) {
      this.projectRow(this.projects[this.projectIndex], this.projectIndex)
    }
  }

  @Builder
  private creationStepNumber(step: CreationStep) {
    Text(`${step}`).fontSize(META_SIZE).fontWeight(FontWeight.Medium).textAlign(TextAlign.Center)
      .width(24).height(24).borderRadius(12).flexShrink(0)
      .fontColor(this.creationStep === step ? ACCENT_TEXT : this.creationStep > step ? ACCENT : SECONDARY_TEXT)
      .backgroundColor(this.creationStep === step ? ACCENT : PAGE_BACKGROUND)
      .border({ width: 1, color: this.creationStep === step ? ACCENT : BORDER })
      .accessibilityLevel('no')
  }

  @Builder
  private creationGuide(step: CreationStep) {
    Column() {
      if (step === 1) {
        Blank().height(24)
      }
      if (step === 3) {
        Row().width(1).height(24).backgroundColor(BORDER)
      } else {
        Row().width(1).layoutWeight(1).backgroundColor(BORDER)
      }
    }.width(24).height('100%').alignItems(HorizontalAlign.Center)
  }

  @Builder
  private completedCreationStep(step: CreationStep, label: string, value: string, detail: string = '') {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 10 }) {
        this.creationStepNumber(step)
        Column({ space: 3 }) {
          Text(label).fontSize(META_SIZE).fontColor(SECONDARY_TEXT)
          Text(value).fontSize(BODY_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
          if (detail) {
            Text(detail).fontSize(META_SIZE).fontColor(SECONDARY_TEXT).wordBreak(WordBreak.BREAK_ALL)
          }
        }.layoutWeight(1).alignItems(HorizontalAlign.Start)
        Text('修改').fontSize(META_SIZE).fontColor(ACCENT).flexShrink(0)
      }.width('100%').alignItems(VerticalAlign.Top).padding({ top: 12, bottom: 14 })
    }.width('100%').constraintSize({ minHeight: 48 }).padding(0)
      .backgroundColor(Color.Transparent).background(this.creationGuide(step), { align: Alignment.TopStart })
      .enabled(!this.creating)
      .accessibilityText(`第 ${step} 步，${label}，${value}${detail ? `，${detail}` : ''}，修改`)
      .onClick(() => { this.editCreationStep(step) })
  }

  @Builder
  private pendingCreationStep(step: CreationStep, label: string) {
    Row({ space: 10 }) {
      this.creationStepNumber(step)
      Text(label).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
    }.width('100%').padding({ top: 12, bottom: 12 })
      .background(this.creationGuide(step), { align: Alignment.TopStart })
      .accessibilityText(`第 ${step} 步，${label}`)
  }

  @Builder
  private sessionChoices() {
    Button({ type: ButtonType.Normal }) {
      Row({ space: 10 }) {
        SymbolGlyph($r('sys.symbol.plus')).fontSize(18).fontColor([ACCENT]).accessibilityLevel('no')
        Text('新会话').fontSize(BODY_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT).layoutWeight(1)
        if (this.selectedHistoryId === null) {
          this.choiceCheck()
        }
      }.width('100%')
    }.width('100%').height(TOUCH_SIZE).padding(0).backgroundColor(Color.Transparent)
      .accessibilitySelected(this.selectedHistoryId === null).enabled(!this.creating)
      .onClick(() => { this.selectedHistoryId = null; this.creationError = '' })

    Text('历史会话').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).margin({ top: 14, bottom: 2 })
    if (this.historyLoading) {
      Text('正在读取历史会话…').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).width('100%')
    } else if (this.historyError) {
      Text(this.historyError).fontSize(BODY_SIZE).fontColor(this.historyCanRetry ? ERROR_TEXT : SECONDARY_TEXT)
        .width('100%')
      if (this.historyCanRetry) {
        Button('重试', { type: ButtonType.Normal }).height(TOUCH_SIZE).fontColor(ACCENT)
          .backgroundColor(SURFACE).enabled(this.deviceOnline() && !this.creating).onClick(() => { this.loadHistory() })
      }
    } else if (!this.history.length) {
      Text('所选项目暂无可恢复的历史会话').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT).width('100%')
    } else {
      ForEach(this.history, (session: AgentShellHistorySession) => { this.historyRow(session) },
        (session: AgentShellHistorySession) => session.id)
    }
  }

  @Builder
  private currentCreationStep(step: CreationStep, label: string) {
    Column() {
      Row({ space: 10 }) {
        this.creationStepNumber(step)
        Text(label).fontSize(22).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      }.width('100%').constraintSize({ minHeight: 24 }).alignItems(VerticalAlign.Top)

      Column({ space: 4 }) {
        if (this.creationLoading) {
          Text('正在读取项目与 Agent…').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
        } else if (this.creationLoadError) {
          Text(this.creationLoadError).fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
          Button('重新读取', { type: ButtonType.Normal }).height(TOUCH_SIZE).fontColor(ACCENT)
            .backgroundColor(SURFACE).enabled(this.deviceOnline()).onClick(() => { this.openSessionCreation() })
        } else if (step === 1) {
          if (this.projects.length) {
            this.projectChoices()
          } else {
            Text('电脑上暂无包含 .agent-shell 目录的项目').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
          }
        } else if (step === 2) {
          if (this.creationAgents.length) {
            ForEach(this.creationAgents, (agent: ViewerAgent, index: number) => { this.agentRow(agent, index) },
              (agent: ViewerAgent) => agent.id)
          } else {
            Text('电脑上暂无可用的 Agent').fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
          }
        } else {
          this.sessionChoices()
        }
      }.width('100%').alignItems(HorizontalAlign.Start).padding({ left: 34, top: 8 })
    }.width('100%').padding({ top: 12, bottom: 14 })
      .background(this.creationGuide(step), { align: Alignment.TopStart })
  }

  @Builder
  private creationContent() {
    Column() {
      if (this.creationStep > 1) {
        Column() {
          this.completedCreationStep(1, '项目', this.projects[this.projectIndex]?.name ?? '',
            this.projects[this.projectIndex]?.path ?? '')
          if (this.creationStep === 3) {
            this.completedCreationStep(2, 'Agent', this.creationAgents[this.agentIndex]?.name ?? '')
          }
        }.width('100%').padding({ left: 16, right: 16, top: 6 })
      }

      Scroll(this.creationScroller) {
        Column() {
          if (this.creationStep === 1) {
            this.currentCreationStep(1, '选择项目')
            this.pendingCreationStep(2, '选择 Agent')
            this.pendingCreationStep(3, '选择会话')
          } else if (this.creationStep === 2) {
            this.currentCreationStep(2, '选择 Agent')
            this.pendingCreationStep(3, '选择会话')
          } else {
            this.currentCreationStep(3, '选择会话')
          }
        }.width('100%').padding({ left: 16, right: 16, top: this.creationStep === 1 ? 6 : 0, bottom: 16 })
      }.layoutWeight(1).width('100%').align(Alignment.Top).scrollBar(BarState.Auto)

      Column({ space: 8 }) {
        if (this.creationError) {
          Text(this.creationError).fontSize(BODY_SIZE).fontColor(ERROR_TEXT).width('100%')
        } else if (!this.deviceOnline()) {
          Text(deviceConnectionAppearance(this.deviceStatus).label).fontSize(BODY_SIZE).fontColor(SECONDARY_TEXT)
            .width('100%')
        }
        Button(this.creationActionLabel(), { type: ButtonType.Normal }).height(48).width('100%')
          .fontColor(ACCENT_TEXT).fontWeight(FontWeight.Medium).backgroundColor(ACCENT).borderRadius(PANEL_RADIUS)
          .enabled(this.deviceOnline() && this.projectIndex >= 0 && !this.creating &&
            (this.creationStep === 1 || this.agentIndex >= 0))
          .onClick(() => { this.advanceCreation() })
      }.width('100%').padding({ left: 16, right: 16, top: 10, bottom: 16 })
    }.layoutWeight(1).width('100%').constraintSize({ maxWidth: PAGE_MAX_WIDTH })
  }

  private creationActionLabel(): string {
    if (this.creationStep < 3) return '下一步'
    if (this.selectedHistoryId === null) {
      return this.creating ? '正在创建…' : '开启新会话'
    }
    return this.creating ? '正在恢复…' : '恢复会话'
  }

  @Builder
  private creationPage() {
    AppPage({ title: '新建会话', onBack: () => { this.backInCreation() }, content: () => { this.creationContent() } })
  }

  @Builder
  private sessionMenu() {
    PopupMenu({
      accessibilityLabel: '更多',
      items: [{
        id: 'new-session', label: '新建会话', icon: $r('sys.symbol.plus'),
        enabled: this.deviceOnline() && !this.creating,
        action: () => { this.openSessionCreation() }
      }, {
        id: 'restart-session', label: this.pendingAction === 'restart' ? '正在重启…' : '重启会话',
        icon: $r('sys.symbol.arrow_clockwise'), enabled: this.deviceOnline() && !!this.selected && !this.pendingAction,
        action: () => { this.performSessionAction('restart') }
      }, {
        id: 'clear-buffer', label: this.pendingAction === 'clear-buffer' ? '正在清空…' : '清空缓冲区',
        icon: $r('sys.symbol.trash'), enabled: this.deviceOnline() && !!this.selected && !this.pendingAction,
        action: () => { this.performSessionAction('clear-buffer') }
      }],
      trigger: () => { this.moreIcon() }
    })
  }

  @Builder
  private moreIcon() {
    Row({ space: 3 }) {
      Circle().width(3).height(3).fill(ACCENT)
      Circle().width(3).height(3).fill(ACCENT)
      Circle().width(3).height(3).fill(ACCENT)
    }.accessibilityLevel('no')
  }

  private readImage(source: string, session: string = this.selected): Promise<string> {
    const directory = AppStorage.get<string>('lokiAgentImageCache')
    const shell = this.shell
    if (!directory || !shell || !session || !this.deviceOnline()) {
      return Promise.reject(new Error('电脑未连接'))
    }
    return shell.cacheImage(directory, session, source)
  }

  private openConversation(): void {
    const shell = this.shell
    const session = this.selected
    if (!shell || !session || !this.active) return
    this.detachConversation()
    if (!this.blocks.length || this.notice.canRetry) {
      this.notice = { text: this.blocks.length ? '' : '正在读取会话' }
    }
    const version = this.streamVersion
    try {
      this.conversation = shell.observeConversation(session, (view: AgentShellConversationView) => {
        if (version === this.streamVersion && session === this.selected) this.receive(view, session)
      })
    } catch (_) {
      this.notice = { text: '无法打开会话', canRetry: true }
    }
  }

  private receive(view: AgentShellConversationView, session: string): void {
    if (view.phase !== 'loading' || !this.blocks.length) {
      this.blocks = view.blocks.map(viewerBlock)
    }
    switch (view.phase) {
      case 'loading':
        if (!this.blocks.length) this.notice = { text: '正在读取会话' }
        break
      case 'ready':
        this.notice = { text: this.blocks.length ? '' : '暂无会话内容' }
        break
      case 'reconnecting':
        this.notice = { text: '会话连接中断，正在重连' }
        break
      case 'closed': {
        const directory = AppStorage.get<string>('lokiAgentImageCache')
        if (directory) {
          this.shell?.clearSessionImages(directory, this.endpointId, session).catch(() => {})
        }
        this.conversationKey += 1
        this.notice = { text: '会话已关闭' }
        break
      }
      case 'error':
        this.notice = { text: view.message || '无法读取会话', canRetry: true }
        break
    }
  }

  build() {
    Stack() {
      Column()
        .width('100%').height('100%').backgroundColor(PAGE_BACKGROUND)
        .expandSafeArea([SafeAreaType.SYSTEM, SafeAreaType.CUTOUT], [SafeAreaEdge.TOP, SafeAreaEdge.BOTTOM])
        .hitTestBehavior(HitTestMode.None)
      AgentViewerScreen({ title: $r('app.string.agent_viewer_title'), sessionId: this.selected,
        sessions: this.sessions, agents: this.agents, adjacentViews: this.adjacentViews, blocks: this.blocks,
        notice: this.notice.text,
        canRetry: this.notice.canRetry ?? false,
        connected: this.deviceOnline(),
        preview: $imagePreview,
        sessionActions: () => { this.sessionMenu() },
        connectionColor: deviceConnectionAppearance(this.deviceStatus).color,
        connectionLabel: deviceConnectionAppearance(this.deviceStatus).label,
        reconnecting: this.deviceReconnecting,
        parseDocument: renderDocument,
        conversationKey: this.conversationKey,
        onSelect: (id: string): void => { this.select(id) },
        onSend: (value: string): Promise<string> => this.submit(value),
        readImage: (source: string, session?: string): Promise<string> => this.readImage(source, session),
        onRetry: (): void => { this.openConversation() } })
        .visibility(this.creationOpen ? Visibility.None : Visibility.Visible)
      if (this.creationOpen) {
        this.creationPage()
      }
    }.width('100%').height('100%')
  }
}

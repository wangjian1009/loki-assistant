import { KeyboardAvoidMode } from '@kit.ArkUI'
import { AgentViewerScreen, ViewerBlock, ViewerSession } from 'agent_viewer'
import { AgentShell, AgentShellBlock, AgentShellSession, AgentShellConversationView, ConversationSubscription,
  renderDocument, ResponsError } from 'liblokiworkspace.so'
import { Application, ComputerDevice } from '../app/Application'
import { PAGE_BACKGROUND } from '../components/AppPage'
import { deviceConnectionAppearance } from '../components/DeviceConnection'
import { StatusSubscription as DeviceSubscription } from '../workspace/ServiceStatus'

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
  @State private blocks: ViewerBlock[] = []
  @State private selected: string = ''
  @State private notice: string = '正在读取会话'
  @State private conversationKey: number = 0
  @State private deviceStatus: string = 'offline'
  @State private imagePreview: string = ''
  private pendingSessionId: string = ''
  private visible: boolean = false
  private active: boolean = false
  private lifetime: number = 0
  private streamVersion: number = 0
  private endpointId: string = ''
  private shell?: AgentShell
  private devices?: DeviceSubscription
  private conversation?: ConversationSubscription

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
      this.detachConversation()
      this.devices?.cancel()
      this.devices = undefined
    }
  }

  private async watch(lifetime: number): Promise<void> {
    const application = AppStorage.get<Application>('lokiApplication')
    if (!application) {
      this.notice = '无法读取设备配置'
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
          this.detachConversation()
          this.endpointId = endpointId
          this.sessions = []
          this.selected = ''
          this.blocks = []
          this.conversationKey += 1
        }
        this.deviceStatus = current?.status ?? 'offline'
        if (!current || !this.deviceOnline()) {
          if (current && this.selected) {
            if (!this.conversation) this.openConversation()
          } else {
            this.notice = current ? '电脑上线后显示会话' : '请在设备管理中选择一台电脑'
          }
        } else {
          this.updateSessions(current.dashboard?.agent_shell_sessions ?? null)
        }
      }, (message: string) => {
        if (lifetime === this.lifetime) this.notice = message
      })
      if (lifetime !== this.lifetime) {
        subscription.cancel()
        return
      }
      this.devices = subscription
    } catch (_) {
      if (lifetime === this.lifetime) this.notice = '无法读取会话状态'
    }
  }

  private updateSessions(sessions: AgentShellSession[] | null): void {
    if (sessions === null) {
      if (this.deviceOnline() && !this.selected) this.notice = '尚未收到会话状态'
      return
    }
    this.sessions = sessions.map((session, index): ViewerSession => ({
      id: session.id, title: session.title || '会话 ' + (index + 1), phase: session.phase
    }))
    if (this.sessions.length === 0) {
      this.detachConversation()
      if (this.selected) this.conversationKey += 1
      this.selected = ''
      this.blocks = []
      this.notice = '电脑上暂无打开的会话'
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
  }

  private detachConversation(): void {
    this.streamVersion += 1
    this.conversation?.cancel()
    this.conversation = undefined
  }

  private select(sessionId: string): void {
    if (this.selected === sessionId) return
    this.detachConversation()
    this.selected = sessionId
    this.blocks = []
    this.conversationKey += 1
    this.openConversation()
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

  private readImage(source: string): Promise<string> {
    const directory = AppStorage.get<string>('lokiAgentImageCache')
    const shell = this.shell
    if (!directory || !shell || !this.selected || !this.deviceOnline()) {
      return Promise.reject(new Error('电脑未连接'))
    }
    return shell.cacheImage(directory, this.selected, source)
  }

  private openConversation(): void {
    const shell = this.shell
    const session = this.selected
    if (!shell || !session || !this.active) return
    this.detachConversation()
    this.notice = '正在读取会话'
    const version = this.streamVersion
    try {
      this.conversation = shell.observeConversation(session, (view: AgentShellConversationView) => {
        if (version === this.streamVersion && session === this.selected) this.receive(view, session)
      })
    } catch (_) {
      this.notice = '无法打开会话'
    }
  }

  private receive(view: AgentShellConversationView, session: string): void {
    this.blocks = view.blocks.map(viewerBlock)
    switch (view.phase) {
      case 'loading':
        this.notice = '正在读取会话'
        break
      case 'ready':
        this.notice = this.blocks.length ? '' : '暂无会话内容'
        break
      case 'reconnecting':
        this.notice = '会话连接中断，正在重连'
        break
      case 'closed': {
        const directory = AppStorage.get<string>('lokiAgentImageCache')
        if (directory) {
          this.shell?.clearSessionImages(directory, this.endpointId, session).catch(() => {})
        }
        this.conversationKey += 1
        this.notice = '会话已关闭'
        break
      }
      case 'error':
        this.notice = view.message || '无法读取会话'
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
        sessions: this.sessions, blocks: this.blocks, notice: this.notice, connected: this.deviceOnline(),
        preview: $imagePreview,
        connectionColor: deviceConnectionAppearance(this.deviceStatus).color,
        connectionLabel: deviceConnectionAppearance(this.deviceStatus).label,
        parseDocument: renderDocument,
        conversationKey: this.conversationKey,
        onSelect: (id: string): void => { this.select(id) },
        onSend: (value: string): Promise<string> => this.submit(value),
        readImage: (source: string): Promise<string> => this.readImage(source),
        onRetry: (): void => { this.openConversation() } })
    }.width('100%').height('100%')
  }
}

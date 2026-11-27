import { KeyboardAvoidMode } from '@kit.ArkUI'
import { AgentViewerScreen, ViewerBlock, ViewerSession } from 'agent_viewer'
import { AgentShell, AgentShellBlock, AgentShellSession, AgentShellConversationEvent, RequestHandle,
  renderDocument, ResponsError } from 'liblokiworkspace.so'
import { Application, ComputerDevice } from '../app/Application'
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
  @State private deviceName: string = ''
  @State private selected: string = ''
  @State private notice: string = '正在读取会话'
  @State private conversationKey: number = 0
  @State private full: boolean = false
  @State private deviceOnline: boolean = false
  private pendingSessionId: string = ''
  private visible: boolean = false
  private active: boolean = false
  private lifetime: number = 0
  private streamVersion: number = 0
  private revision: number = 0
  private hasSnapshot: boolean = false
  private streamEnded: boolean = false
  private retryTimer: number = -1
  private retryDelay: number = 500
  private endpointId: string = ''
  private shell?: AgentShell
  private devices?: DeviceSubscription
  private conversation?: RequestHandle

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

  aboutToDisappear(): void {
    this.visible = false
    this.foregroundChanged()
  }

  private foregroundChanged(): void {
    const shouldWatch = this.visible && this.foreground
    if (shouldWatch === this.active) return
    this.active = shouldWatch
    const lifetime = ++this.lifetime
    if (shouldWatch) {
      this.watch(lifetime)
    } else {
      this.stopConversation()
      this.devices?.cancel()
      this.devices = undefined
      this.shell = undefined
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
      this.shell = workspace.openAgentShell()
      const subscription = await application.watchDevices((devices: ComputerDevice[]) => {
        if (lifetime !== this.lifetime) return
        const current = devices.find(device => device.active)
        const endpointId = current?.endpoint_id ?? ''
        if (endpointId !== this.endpointId) {
          this.stopConversation()
          this.endpointId = endpointId
          this.sessions = []
          this.selected = ''
          this.blocks = []
          this.conversationKey += 1
        }
        this.deviceName = current?.name ?? ''
        this.deviceOnline = current?.status === 'online'
        if (!current || !this.deviceOnline) {
          this.stopConversation()
          this.streamEnded = false
          this.notice = current ? '电脑上线后显示会话' : '请在当前设备页选择一台电脑'
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
      if (this.deviceOnline && !this.selected) this.notice = '尚未收到会话状态'
      return
    }
    this.sessions = sessions.map((session, index): ViewerSession => ({
      id: session.id, title: session.title || '会话 ' + (index + 1), phase: session.phase
    }))
    if (this.sessions.length === 0) {
      this.stopConversation()
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
    } else if (!this.conversation && !this.streamEnded && this.retryTimer === -1) {
      this.openConversation()
    }
  }

  private stopConversation(): void {
    this.streamVersion += 1
    if (this.retryTimer !== -1) {
      clearTimeout(this.retryTimer)
      this.retryTimer = -1
    }
    this.conversation?.cancel()
    this.conversation = undefined
    this.revision = 0
    this.hasSnapshot = false
  }

  private select(sessionId: string): void {
    if (this.selected === sessionId) return
    this.stopConversation()
    this.selected = sessionId
    this.full = false
    this.blocks = []
    this.conversationKey += 1
    this.streamEnded = false
    this.retryDelay = 500
    this.openConversation()
  }

  private loadFull(): void {
    this.full = true
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
    if (!directory || !shell || !this.selected || !this.deviceOnline) {
      return Promise.reject(new Error('电脑未连接'))
    }
    return shell.cacheImage(directory, this.selected, source)
  }

  private openConversation(): void {
    const shell = this.shell
    if (!shell || !this.selected || !this.active || !this.deviceOnline) return
    this.stopConversation()
    this.streamEnded = false
    this.notice = '正在读取会话'
    const version = this.streamVersion
    try {
      this.conversation = shell.watchConversation(this.selected, this.full, (event: AgentShellConversationEvent) => {
        if (version === this.streamVersion && event.session_id === this.selected) this.receive(event)
      }, (error: ResponsError | null) => {
        if (version !== this.streamVersion) return
        this.conversation = undefined
        if (!this.streamEnded) this.reconnect(error)
      })
    } catch (_) {
      this.notice = '无法打开会话'
    }
  }

  private receive(event: AgentShellConversationEvent): void {
    if (event.kind === 'closed' || event.kind === 'error') {
      if (event.kind === 'closed') {
        const directory = AppStorage.get<string>('lokiAgentImageCache')
        if (directory) {
          this.shell?.clearSessionImages(directory, this.endpointId, event.session_id).catch(() => {})
        }
        this.blocks = []
        this.conversationKey += 1
      }
      this.streamEnded = true
      this.notice = event.kind === 'closed' ? '会话已关闭' : event.message || '无法读取会话'
      return
    }
    if (event.kind === 'snapshot') {
      this.revision = event.revision
      this.hasSnapshot = true
      this.retryDelay = 500
      this.blocks = event.blocks.map(viewerBlock)
      this.notice = this.blocks.length ? '' : '暂无会话内容'
      return
    }
    if (!this.hasSnapshot || event.revision !== this.revision + 1) {
      this.stopConversation()
      this.reconnect(null)
      return
    }
    const next = this.blocks.slice()
    const indices = new Map<string, number>()
    next.forEach((block, index) => { indices.set(block.id, index) })
    for (const block of event.blocks) {
      const item = viewerBlock(block)
      const index = indices.get(block.id)
      if (index === undefined) {
        indices.set(block.id, next.length)
        next.push(item)
      } else {
        next[index] = item
      }
    }
    this.revision = event.revision
    this.blocks = next
    this.notice = next.length ? '' : '暂无会话内容'
  }

  private reconnect(error: ResponsError | null): void {
    if (error?.kind === 'services' && (error.status === 403 || error.status === 404)) {
      this.notice = error.status === 404 ? '电脑上找不到此会话' : '电脑拒绝读取会话'
      this.streamEnded = true
      return
    }
    this.notice = '会话连接中断，正在重连'
    if (!this.active || !this.deviceOnline) return
    this.retryTimer = setTimeout(() => {
      this.retryTimer = -1
      this.openConversation()
    }, this.retryDelay)
    this.retryDelay = Math.min(this.retryDelay * 2, 5000)
  }

  build() {
    Column() {
      AgentViewerScreen({ title: $r('app.string.agent_viewer_title'), sessionId: this.selected,
        sessions: this.sessions, blocks: this.blocks, deviceName: this.deviceName, notice: this.notice,
        full: this.full, connected: this.deviceOnline,
        parseDocument: renderDocument,
        conversationKey: this.conversationKey,
        onSelect: (id: string): void => { this.select(id) },
        onFull: (): void => { this.loadFull() },
        onSend: (value: string): Promise<string> => this.submit(value),
        readImage: (source: string): Promise<string> => this.readImage(source),
        onRetry: (): void => { this.openConversation() } })
    }.width('100%').height('100%')
  }
}

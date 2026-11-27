import { AgentViewerScreen, ViewerBlock, ViewerSession } from 'agent_viewer'
import { AgentShell, AgentShellSession, AgentShellConversationEvent, RequestHandle,
  ResponsError } from 'liblokiworkspace.so'
import { Application, ComputerDevice } from '../app/Application'
import { StatusSubscription as DeviceSubscription } from '../workspace/ServiceStatus'

@Entry
@Component
struct AgentViewer {
  @StorageLink('lokiAgentViewerForeground') @Watch('foregroundChanged') private foreground: boolean = true
  @State private sessions: ViewerSession[] = []
  @State private blocks: ViewerBlock[] = []
  @State private deviceName: string = ''
  @State private selected: string = ''
  @State private notice: string = '正在读取会话'
  @State private conversationKey: number = 0
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
  private deviceOnline: boolean = false
  private shell?: AgentShell
  private devices?: DeviceSubscription
  private conversation?: RequestHandle

  onPageShow(): void {
    this.visible = true
    this.foregroundChanged()
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
      this.selected = ''
      this.blocks = []
      this.notice = '电脑上暂无打开的会话'
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
    this.blocks = []
    this.conversationKey += 1
    this.streamEnded = false
    this.retryDelay = 500
    this.openConversation()
  }

  private openConversation(): void {
    const shell = this.shell
    if (!shell || !this.selected || !this.active || !this.deviceOnline) return
    this.stopConversation()
    this.streamEnded = false
    this.notice = '正在读取会话'
    const version = this.streamVersion
    try {
      this.conversation = shell.watchConversation(this.selected, (event: AgentShellConversationEvent) => {
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
      this.streamEnded = true
      this.notice = event.kind === 'closed' ? '会话已关闭' : event.message || '无法读取会话'
      return
    }
    if (event.kind === 'snapshot') {
      this.revision = event.revision
      this.hasSnapshot = true
      this.retryDelay = 500
      this.blocks = event.blocks.map((block): ViewerBlock => ({ id: block.id, kind: block.kind, title: block.title ?? '', text: block.text }))
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
      const item: ViewerBlock = { id: block.id, kind: block.kind, title: block.title ?? '', text: block.text }
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
        conversationKey: this.conversationKey,
        onSelect: (id: string): void => { this.select(id) },
        onRetry: (): void => { this.openConversation() } })
    }.width('100%').height('100%')
  }
}

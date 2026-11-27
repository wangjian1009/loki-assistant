import { AgentShellPhase, AgentShellSession, StatusSubscription } from 'liblokiworkspace.so'
import { CurrentDeviceState } from '../../app/CurrentDeviceState'
import { ACCENT, BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, PAGE_INSET, PANEL_RADIUS,
  PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SURFACE } from '../../components/AppPage'
import { Workspace } from '../../workspace/Workspace'

interface PhaseAppearance {
  icon: Resource
  label: string
  color: string
}

function phaseAppearance(phase: AgentShellPhase): PhaseAppearance {
  switch (phase) {
    case 'idle': return { icon: $r('sys.symbol.circle'), label: '空闲', color: SECONDARY_TEXT }
    case 'starting': return { icon: $r('sys.symbol.hourglass'), label: '启动中', color: ACCENT }
    case 'running': return { icon: $r('sys.symbol.play_circle'), label: '执行中', color: ACCENT }
    case 'waiting_input': return { icon: $r('sys.symbol.ellipsis_bubble'), label: '等待输入', color: '#F5CF92' }
    case 'completed': return { icon: $r('sys.symbol.checkmark_circle'), label: '已完成', color: '#9AE2B9' }
    case 'cancelled': return { icon: $r('sys.symbol.stop_circle'), label: '已取消', color: SECONDARY_TEXT }
    case 'failed': return { icon: $r('sys.symbol.exclamationmark_circle'), label: '执行失败', color: ERROR_TEXT }
    case 'disconnected': return { icon: $r('sys.symbol.wifi_slash'), label: '已断开', color: SECONDARY_TEXT }
    default: return { icon: $r('sys.symbol.questionmark_circle'), label: '状态未知', color: SECONDARY_TEXT }
  }
}

@Component
export struct AgentShellPanel {
  @ObjectLink @Watch('updateSubscription') device: CurrentDeviceState
  @State private sessions: AgentShellSession[] | null = null
  @State private errorMessage: string = ''
  private subscription?: StatusSubscription
  private endpointId?: string
  private version: number = 0

  aboutToAppear(): void { this.updateSubscription() }

  aboutToDisappear(): void {
    this.resetSubscription()
  }

  private resetSubscription(): void {
    this.version += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.endpointId = undefined
    this.sessions = null
    this.errorMessage = ''
  }

  private updateSubscription(): void {
    const current = this.device.current
    const workspace = this.device.active && this.device.hasStatus && current?.status === 'online'
      ? this.device.workspace : undefined
    const endpointId = workspace ? current?.endpoint_id : undefined
    if (endpointId === this.endpointId) return
    this.resetSubscription()
    if (workspace && endpointId) {
      this.endpointId = endpointId
      this.subscribe(workspace, this.version)
    }
  }

  private async subscribe(workspace: Workspace, version: number): Promise<void> {
    try {
      const native = await workspace.open()
      if (version !== this.version) return
      this.subscription = native.openAgentShell().watchStatus((sessions: AgentShellSession[] | null) => {
        if (version === this.version) this.sessions = sessions
      })
    } catch (_) {
      if (version === this.version) this.errorMessage = '无法读取 Agent Shell 状态'
    }
  }

  private emptyMessage(): string {
    if (!this.device.current) return this.device.loaded ? '请选择一台电脑' : '正在读取设备状态'
    if (!this.device.hasStatus) return '正在读取设备状态'
    if (this.device.current.status !== 'online') return '电脑上线后显示会话状态'
    return this.errorMessage || (this.sessions ? '暂无会话' : '尚未收到会话状态')
  }

  @Builder
  private phaseIcon(appearance: PhaseAppearance) {
    SymbolGlyph(appearance.icon).fontSize(18).fontColor([appearance.color])
      .flexShrink(0).accessibilityLevel('yes').accessibilityText(appearance.label)
  }

  build() {
    Column({ space: CONTENT_GAP }) {
      Text('Agent Shell').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      if (this.sessions?.length) {
        ForEach(this.sessions, (session: AgentShellSession, index: number) => {
          Row({ space: CONTENT_GAP }) {
            Text(session.title || `会话 ${index + 1}`).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
              .layoutWeight(1).maxLines(2).textOverflow({ overflow: TextOverflow.Ellipsis })
            this.phaseIcon(phaseAppearance(session.phase))
          }.width('100%')
        }, (session: AgentShellSession, index: number) => JSON.stringify([session.id, session.phase, session.title, index]))
      } else {
        Text(this.emptyMessage())
          .fontSize(BODY_SIZE).fontColor(this.errorMessage ? ERROR_TEXT : SECONDARY_TEXT)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(PAGE_INSET)
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }
}

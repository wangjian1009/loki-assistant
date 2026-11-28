import { AgentShellSession, StatusSubscription } from 'liblokiworkspace.so'
import { common } from '@kit.AbilityKit'
import { AgentShellStatusIcon } from 'agent_viewer'
import { CurrentDeviceState } from '../../app/CurrentDeviceState'
import { deviceConnectionAppearance } from '../../components/DeviceConnection'
import { BODY_SIZE, BORDER, CONTENT_GAP, ERROR_TEXT, PAGE_INSET, PANEL_RADIUS,
  PRIMARY_TEXT, SECONDARY_TEXT, SECTION_SIZE, SURFACE } from '../../components/AppPage'
import { Workspace } from '../../workspace/Workspace'

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
    if (this.device.current.status !== 'online') return deviceConnectionAppearance(this.device.current.status).label
    return this.errorMessage || (this.sessions ? '暂无会话' : '尚未收到会话状态')
  }

  private openSession(id: string): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext
    context.startAbility({ bundleName: context.abilityInfo.bundleName, moduleName: 'entry',
      abilityName: 'AgentViewerAbility', parameters: { sessionId: id } }).catch(() => {
      this.errorMessage = '无法打开 AI 远程对话'
    })
  }

  build() {
    Column({ space: CONTENT_GAP }) {
      Text('Agent Shell').fontSize(SECTION_SIZE).fontWeight(FontWeight.Medium).fontColor(PRIMARY_TEXT)
      if (this.sessions?.length) {
        ForEach(this.sessions, (session: AgentShellSession, index: number) => {
          Button({ type: ButtonType.Normal }) {
            Row({ space: CONTENT_GAP }) {
              Text(session.title || `会话 ${index + 1}`).fontSize(BODY_SIZE).fontColor(PRIMARY_TEXT)
                .layoutWeight(1).maxLines(1).textOverflow({ overflow: TextOverflow.Ellipsis })
              AgentShellStatusIcon({ phase: session.phase })
            }.width('100%')
          }.width('100%').constraintSize({ minHeight: 44 }).padding(0)
            .backgroundColor(Color.Transparent).onClick(() => { this.openSession(session.id) })
        }, (session: AgentShellSession, index: number) => JSON.stringify([session.id, session.phase, session.title, index]))
        if (this.errorMessage) {
          Text(this.errorMessage).fontSize(BODY_SIZE).fontColor(ERROR_TEXT)
        }
      } else {
        Text(this.emptyMessage())
          .fontSize(BODY_SIZE).fontColor(this.errorMessage ? ERROR_TEXT : SECONDARY_TEXT)
      }
    }.width('100%').alignItems(HorizontalAlign.Start).padding(PAGE_INSET)
      .backgroundColor(SURFACE).borderRadius(PANEL_RADIUS).border({ width: 0.5, color: BORDER })
  }
}

import { CurrentDeviceState } from './CurrentDeviceState'

export interface CurrentDevicePanelContext {
  device: CurrentDeviceState
  editing: boolean
}

export interface CurrentDevicePanel {
  id: string
  title: string
  supportsEditing: boolean
  isAvailable?: (device: CurrentDeviceState) => boolean
  render: WrappedBuilder<[CurrentDevicePanelContext]>
}

export class CurrentDevicePanelRegistry {
  private readonly panels: Map<string, CurrentDevicePanel> = new Map()

  register(panel: CurrentDevicePanel): void {
    if (!panel.id || this.panels.has(panel.id)) {
      throw new Error('面板 ID 为空或已注册：' + panel.id)
    }
    this.panels.set(panel.id, panel)
  }

  ids(): string[] {
    return Array.from(this.panels.keys())
  }

  get(id: string): CurrentDevicePanel {
    const panel = this.panels.get(id)
    if (!panel) throw new Error('面板未注册：' + id)
    return panel
  }
}

import { Workspace } from '../workspace/Workspace'

export interface ComputerDevice {
  endpoint_id: string
  name: string
  bound_at: number | null
  status: string
  detail: string
  checked_at: number
  active: boolean
}

interface DeviceResult {
  ok: boolean
  error?: string
  devices?: ComputerDevice[]
}

export class DeviceManager {
  private workspace: Workspace

  constructor(workspace: Workspace) { this.workspace = workspace }

  private async request(operation: string, payload: string = ''): Promise<DeviceResult> {
    const result: DeviceResult = JSON.parse(await this.workspace.execute(operation, payload)) as DeviceResult
    if (!result.ok) { throw new Error(result.error ?? '设备管理暂时不可用') }
    return result
  }

  async list(): Promise<ComputerDevice[]> {
    return (await this.request('list')).devices ?? []
  }

  async bind(payload: string): Promise<void> {
    await this.request('bind', payload)
  }

  async select(endpointId: string): Promise<void> {
    await this.request('select', endpointId)
  }

  async refresh(): Promise<ComputerDevice[]> {
    return (await this.request('refresh')).devices ?? []
  }
}

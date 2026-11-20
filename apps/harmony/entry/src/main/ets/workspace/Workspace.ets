import native, { WorkspaceHandle } from 'liblokidevices.so'
import { DeviceManager } from '../devices/DeviceManager'

export interface WorkspaceResult {
  ok: boolean
  error?: string
  network_error?: NetworkErrorData
}

export interface NetworkErrorData {
  kind: string
  code: string
  message: string
  request_id: string | null
  outcome: string
  status: number | null
  source: string | null
}

export class NetworkError extends Error {
  readonly details: NetworkErrorData
  constructor(details: NetworkErrorData) {
    super(details.message)
    this.details = details
  }
}

export interface ServiceResponse {
  request_id: string
  status: number
  body: Object | null
}

export interface ServiceRequest {
  method: string
  path: string
  query?: QueryParameter[]
  body?: Object | null
  timeout_ms?: number
}

export interface QueryParameter { name: string; value: string }

export class Network {
  private workspace: Workspace
  constructor(workspace: Workspace) { this.workspace = workspace }

  async request(request: ServiceRequest): Promise<ServiceResponse> {
    return JSON.parse(await this.workspace.execute('request', JSON.stringify(request))) as ServiceResponse
  }
}

export class Workspace {
  private handle?: Promise<WorkspaceHandle>
  private stopped: boolean = false
  private stopping?: Promise<void>
  private directory: string
  readonly network: Network
  readonly devices: DeviceManager

  constructor(directory: string) {
    this.directory = directory
    this.network = new Network(this)
    this.devices = new DeviceManager(this)
  }

  async execute(operation: string, payload: string = ''): Promise<string> {
    if (this.stopped && operation !== 'stop') { throw new Error('工作空间已停止') }
    if (!this.handle) { this.handle = native.open(this.directory) }
    let handle: WorkspaceHandle
    try {
      handle = await this.handle
    } catch (error) {
      this.handle = undefined
      throw new Error((error as Error).message)
    }
    const text = await native.execute(handle, operation, payload)
    const result = JSON.parse(text) as WorkspaceResult
    if (!result.ok) {
      if (result.network_error) { throw new NetworkError(result.network_error) }
      throw new Error(result.error ?? '工作空间暂时不可用')
    }
    return text
  }

  stop(): Promise<void> {
    if (!this.stopping) {
      this.stopped = true
      this.stopping = this.finishStop()
    }
    return this.stopping
  }

  private async finishStop(): Promise<void> {
    if (this.handle) { await this.execute('stop') }
  }

  async close(): Promise<void> { await this.stop() }
}

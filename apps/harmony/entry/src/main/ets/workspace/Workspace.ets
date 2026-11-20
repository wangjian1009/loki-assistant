import native, { WorkspaceHandle } from 'liblokiworkspace.so'
import { InvalidRequestError, ResponsError, responsError } from './ResponsError'

interface WorkspaceResult {
  ok: boolean
  error?: string
  respons_error?: ResponsError
}
export interface ServiceResponse {
  request_id: string
  status: number
  body: Object | null
  generation: number
}
export interface ServiceRequest {
  method: string
  path: string
  query?: QueryParameter[]
  body?: Object | null
  timeout_ms?: number
}
interface NativeRequest extends ServiceRequest { operation_id: string }
export interface QueryParameter { name: string; value: string }
export interface BindingCredential { token: string; bound_at: number }
export interface Unbound { status: 'unbound' }
export interface Bound { status: 'bound'; credential: BindingCredential }
export type BindingState = Unbound | Bound
export type DeviceType = 'computer' | 'mobile'
export interface Service {
  endpoint_id: string
  relay_url: string
  relay_token: string
  binding: BindingState
}
export interface ServiceState extends Service {
  generation: number
}
export interface Binding { endpoint_id: string; credential: BindingCredential }
export interface Invitation {
  version: number
  pairing_id: string
  endpoint_id: string
  relay_url: string
  relay_token: string
  alpn: string
  pairing_token: string
  expires_at: number
}
interface BindRequest { invitation: string; device_name: string }
interface InvitationResult { invitation: Invitation }
interface StateResult { service: ServiceState | null }
export interface ApplicationData { state: Object; endpoint_id: string }

export class RequestHandle {
  private cancelRequest: () => void
  constructor(cancelRequest: () => void) { this.cancelRequest = cancelRequest }
  cancel(): void { this.cancelRequest() }
}

export class Network {
  private workspace: Workspace
  private sequence: number = 0
  constructor(workspace: Workspace) { this.workspace = workspace }

  async setService(service: Service): Promise<void> {
    await this.workspace.execute('set_service', JSON.stringify(service))
  }
  async clearService(): Promise<void> { await this.workspace.execute('clear_service') }
  async service(): Promise<ServiceState | null> {
    return (JSON.parse(await this.workspace.execute('service')) as StateResult).service
  }
  async parseInvitation(payload: string): Promise<Invitation> {
    return (JSON.parse(await this.workspace.execute('parse_invitation', payload)) as InvitationResult).invitation
  }
  async bind(payload: string, deviceName: string): Promise<Binding> {
    const request: BindRequest = { invitation: payload, device_name: deviceName }
    return (JSON.parse(await this.workspace.execute('bind', JSON.stringify(request))) as ServiceResponse).body as Binding
  }
  request(request: ServiceRequest, onSuccess: (response: ServiceResponse) => void,
    onError: (error: ResponsError) => void): RequestHandle {
    const operationId = `${++this.sequence}`
    let finished = false
    let cancelled = false
    const payload: NativeRequest = {
      method: request.method, path: request.path, query: request.query, body: request.body,
      timeout_ms: request.timeout_ms, operation_id: operationId
    }
    Promise.resolve().then(() => {
      if (cancelled) {
        const failure: ResponsError = {
          kind: 'cancelled', reason: 'user_requested', message: '请求已取消',
          request_id: null, outcome: 'not_sent'
        }
        return Promise.reject(failure)
      }
      let text: string
      try { text = JSON.stringify(payload) } catch (_) {
        const failure: InvalidRequestError = {
          kind: 'invalid_request', reason: 'invalid_body', message: '请求体不能编码为 JSON',
          request_id: null, outcome: 'not_sent'
        }
        return Promise.reject(failure)
      }
      return this.workspace.execute('request', text)
    }).then((text: string) => JSON.parse(text) as ServiceResponse).then((response: ServiceResponse) => {
      finished = true
      onSuccess(response)
    }, (error: Object) => {
      finished = true
      onError(responsError(error))
    }).catch(() => {})
    return new RequestHandle(() => {
      if (!finished) { cancelled = true; this.workspace.cancelRequest(operationId).catch(() => {}) }
    })
  }
}

export class Workspace {
  private handle?: Promise<WorkspaceHandle>
  private stopped: boolean = false
  private stopping?: Promise<void>
  private directory: string
  readonly network: Network

  constructor(directory: string) {
    this.directory = directory
    this.network = new Network(this)
  }
  async execute(operation: string, payload: string = ''): Promise<string> {
    if (this.stopped && operation !== 'stop') {
      const error: ResponsError = {
        kind: 'cancelled', reason: 'network_closed', message: '工作空间已停止',
        request_id: null, outcome: 'not_sent'
      }
      return Promise.reject(error)
    }
    if (!this.handle) { this.handle = native.open(this.directory) }
    let handle: WorkspaceHandle
    try { handle = await this.handle } catch (error) {
      this.handle = undefined
      return Promise.reject(error)
    }
    const text = await native.execute(handle, operation, payload)
    const result = JSON.parse(text) as WorkspaceResult
    if (!result.ok) {
      if (result.respons_error) { return Promise.reject(result.respons_error) }
      throw new Error(result.error ?? '工作空间暂时不可用')
    }
    return text
  }
  async cancelRequest(operationId: string): Promise<void> {
    if (this.handle && !this.stopped) { native.cancel(await this.handle, operationId) }
  }
  async loadState(): Promise<ApplicationData> {
    return JSON.parse(await this.execute('load_state')) as ApplicationData
  }
  async saveState(state: Object): Promise<void> { await this.execute('save_state', JSON.stringify(state)) }
  stop(): Promise<void> {
    if (!this.stopping) {
      this.stopped = true
      this.stopping = this.finishStop()
    }
    return this.stopping
  }
  private async finishStop(): Promise<void> {
    // 已经开始 open 的操作也必须经过停止标记，避免停止之后再提交网络请求。
    if (this.handle) { await this.execute('stop') }
  }
  async close(): Promise<void> { await this.stop() }
}

// 原生方法由 Rust NAPI 宏生成。
export type HttpDispatch = (id: string, method: string, body: string) => void

export interface ApplicationData {
  state: Object
  endpoint_id: string
}

export type Outcome = 'not_sent' | 'rejected' | 'unknown'
interface Failure {
  message: string
  request_id: string | null
  outcome: Outcome
}
export interface NotConfiguredError extends Failure {
  kind: 'not_configured'
  reason: 'service_not_set'
}
export interface NotBoundError extends Failure {
  kind: 'not_bound'
  reason: 'binding_required' | 'binding_revoked' | 'invalid_credential'
}
export interface NetworkFailure extends Failure {
  kind: 'network'
  reason: 'connect_failed' | 'connect_timeout' | 'send_failed' | 'receive_failed' |
    'request_timeout' | 'invalid_response' | 'response_too_large'
}
export interface CancelledError extends Failure {
  kind: 'cancelled'
  reason: 'user_requested' | 'service_changed' | 'network_closed'
}
export interface ServicesError extends Failure {
  kind: 'services'
  reason: 'rejected'
  status: number
  source: string
  code: string
}
export interface InvalidRequestError extends Failure {
  kind: 'invalid_request'
  reason: 'invalid_method' | 'invalid_body' | 'invalid_query' | 'invalid_path' | 'invalid_timeout' | 'request_too_large' |
    'invalid_service' | 'invalid_device_name' | 'invalid_invitation' | 'expired_invitation' | 'invitation_target_mismatch'
}
export type ResponsError = NotConfiguredError | NotBoundError | NetworkFailure |
  CancelledError | ServicesError | InvalidRequestError

export interface BindingCredential { token: string; bound_at: number }
export interface Unbound { status: 'unbound' }
export interface BindingInProgress { status: 'binding' }
export interface BindingFailed { status: 'failed'; error: ResponsError }
export interface Bound { status: 'bound'; credential: BindingCredential }
export type BindingState = Unbound | BindingInProgress | BindingFailed | Bound
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
export interface AutoBinding { invitation: Invitation; device_name: string }
export interface ServiceConfiguration {
  endpoint_id: string
  relay_url: string
  relay_token: string
  binding: BindingState
  auto_binding?: AutoBinding
}
export interface ServiceState extends ServiceConfiguration { generation: number }
export interface ServiceRequest {
  method: string
  path: string
  query: QueryParameter[]
  body?: Object | null
}

export interface ServiceResponse {
  request_id: string
  status: number
  data: Object | null
  generation: number
}

export interface ServiceDeviceStatus { name: string; device_type: 'computer' }
export interface StatusSnapshot {
  generation: number
  endpoint_id: string | null
  checked_at: number
  error: ResponsError | null
  status: Object | null
  service: ServiceDeviceStatus | null
}
export type DeviceRefreshStep = 'registering' | 'querying'
export interface DeviceRefreshResult { error: ResponsError | null }
export type MailSyncPhase = 'queued' | 'downloading' | 'indexing' | 'refreshing' | 'completed' | 'failed' | 'unknown'
export interface MailSyncProgress { id: string; phase: MailSyncPhase }
export interface MailSyncSnapshot extends MailSyncProgress { messages: string[]; error: string | null }
export interface MailStatus {
  unread_count: number | null
  unread_subjects: string[]
  sync_runs: MailSyncProgress[]
}
export interface MailActivity { sync: MailSyncSnapshot | null; receiving: boolean; error: ResponsError | null }
export interface MailStatusChanged {
  endpoint_id: string | null
  generation: number
  status: MailStatus | null
  activity: MailActivity | null
}
export interface SlackPreview {
  id: string
  title: string
  team_name: string
  unread_count: number
  preview: string
}
export interface SlackStatus { unread_previews: SlackPreview[] }
export interface SlackStatusChanged { generation: number; status: SlackStatus | null }

export interface WorkspaceOptions {
  directory: string
}

export class Workspace {
  private constructor()
  static open(options: WorkspaceOptions): Promise<Workspace>
  network(): Network
  openMail(): Mail
  openSlack(): Slack
  loadState(): ApplicationData
  saveState(state: Object): Promise<void>
  refreshDevice(endpoint: string, progress: (step: DeviceRefreshStep) => void): Promise<DeviceRefreshResult>
  watchStatus(changed: (snapshot: StatusSnapshot) => void): StatusSubscription
  startHttpServer(port: number, dispatch: HttpDispatch): string
  respondHttp(id: string, status: number, body: string): void
  stopHttpServer(): Promise<void>
  setService(configuration: ServiceConfiguration): void
  clearService(): void
  service(): ServiceState | null
  parseInvitation(payload: string): Invitation
  shutdown(): Promise<void>
}

export class Network {
  private constructor()
  request(request: ServiceRequest, options: RequestOptions, onSuccess: (response: ServiceResponse) => void, onError: (error: ResponsError) => void): RequestHandle
}

export interface QueryParameter {
  name: string
  value: string
}

export interface RequestOptions {
  timeout_ms: number
}

export class RequestHandle {
  private constructor()
  cancel(): void
}

export class Mail {
  private constructor()
  startSync(endpoint: string): void
  watchStatus(changed: (snapshot: MailStatusChanged) => void): StatusSubscription
}

export class Slack {
  private constructor()
  watchStatus(changed: (snapshot: SlackStatusChanged) => void): StatusSubscription
}

export class StatusSubscription {
  private constructor()
  cancel(): void
}

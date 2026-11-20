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

export function responsError(value: Object): ResponsError {
  const error = value as ResponsError
  if (error && ['not_configured', 'not_bound', 'network', 'cancelled', 'services', 'invalid_request'].includes(error.kind)) {
    return error
  }
  const invalid: NetworkFailure = {
    kind: 'network', reason: 'invalid_response', message: '原生网络调用未返回有效结果',
    request_id: null, outcome: 'unknown'
  }
  return invalid
}

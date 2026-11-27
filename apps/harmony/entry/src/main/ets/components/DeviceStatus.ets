import { MailSyncProgress } from 'liblokiworkspace.so'
import { mailSyncPhaseText } from '../features/mail/MailModel'

export interface StatusDetail { path: string; label: string; value: string }
interface RelayStatus { status?: string }

function relayStatus(value: string): string {
  switch (value) {
    case 'connected': return '已连接'
    case 'connecting': return '连接中'
    case 'disconnected': return '未连接'
    case 'failed': return '连接失败'
    case 'offline': return '未配置'
    default: return value
  }
}

export function statusDetails(snapshot: Object): StatusDetail[] {
  const details: StatusDetail[] = []
  for (const moduleEntry of Object.entries<Object>(snapshot as Record<string, Object>)) {
    const moduleId = moduleEntry[0]
    const moduleValues = moduleEntry[1]
    if (!moduleValues || typeof moduleValues !== 'object') { continue }
    for (const item of Object.entries<Object>(moduleValues as Record<string, Object>)) {
      const key = item[0]
      const value = item[1]
      const path = `${moduleId}/${key}`
      switch (path) {
        case 'service/name':
        case 'service/device_type':
        case 'mail/unread_count':
        case 'mail/unread_subjects':
        case 'slack/unread_previews':
          continue
        case 'service/start_at':
          if (typeof value === 'number') {
            details.push({ path: path, label: '服务启动时间', value: new Date(value * 1000).toLocaleString() })
            continue
          }
          break
        case 'network/my': {
          const relay = value as RelayStatus | null
          if (relay && typeof relay.status === 'string' && Object.keys(relay).length === 1) {
            details.push({ path: path, label: '中转连接', value: relayStatus(relay.status) })
            continue
          }
          break
        }
        case 'mail/sync_runs':
          if (Array.isArray(value)) {
            const runs = value as MailSyncProgress[]
            details.push({ path: path, label: '最近的邮件同步', value: runs.length ?
              runs.map((run: MailSyncProgress) => mailSyncPhaseText(run.phase) ?? run.phase).join('、') : '暂无记录' })
            continue
          }
          break
      }
      details.push({ path: path, label: path, value: typeof value === 'string' ? value : JSON.stringify(value, null, 2) })
    }
  }
  return details
}

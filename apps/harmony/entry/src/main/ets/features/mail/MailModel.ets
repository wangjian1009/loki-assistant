import { MailSyncPhase } from 'liblokiworkspace.so'

export function mailSyncPhaseText(phase: MailSyncPhase): string | undefined {
  switch (phase) {
    case 'queued': return '等待同步'
    case 'downloading': return '正在下载邮件…'
    case 'indexing': return '正在更新索引…'
    case 'refreshing': return '正在刷新未读邮件…'
    case 'completed': return '邮件同步已完成'
    case 'failed': return '邮件同步失败'
    default: return undefined
  }
}

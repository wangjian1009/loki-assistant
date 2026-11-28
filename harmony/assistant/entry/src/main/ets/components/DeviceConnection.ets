export interface DeviceConnectionAppearance {
  label: string
  color: string
}

export function deviceConnectionAppearance(status: string): DeviceConnectionAppearance {
  switch (status) {
    case 'online': return { label: '在线', color: '#9AE2B9' }
    case 'checking': return { label: '正在连接', color: '#A8B1C4' }
    case 'local_network_unavailable': return { label: '本机无网络', color: '#FFB4B1' }
    case 'relay_unavailable': return { label: '无法连接服务器', color: '#FFB4B1' }
    case 'computer_offline': return { label: '电脑不在线', color: '#A8B1C4' }
    case 'binding': return { label: '绑定中', color: '#F5CF92' }
    case 'binding_failed': return { label: '绑定失败', color: '#FFB4B1' }
    default: return { label: '未连接', color: '#A8B1C4' }
  }
}

export interface DeviceConnectionAppearance {
  label: string
  color: string
}

export function deviceConnectionAppearance(status: string): DeviceConnectionAppearance {
  switch (status) {
    case 'online': return { label: '在线', color: '#9AE2B9' }
    case 'binding': return { label: '绑定中', color: '#F5CF92' }
    case 'binding_failed': return { label: '绑定失败', color: '#FFB4B1' }
    default: return { label: '离线', color: '#A8B1C4' }
  }
}

export enum CurrentDeviceWidget {
  Device = 'device',
  Mail = 'mail',
  Slack = 'slack'
}

const DEFAULT_ORDER: CurrentDeviceWidget[] = [
  CurrentDeviceWidget.Device,
  CurrentDeviceWidget.Mail,
  CurrentDeviceWidget.Slack
]

export const WIDGET_TITLES: Record<CurrentDeviceWidget, string> = {
  [CurrentDeviceWidget.Device]: '设备信息',
  [CurrentDeviceWidget.Mail]: '邮件收件箱',
  [CurrentDeviceWidget.Slack]: 'Slack 新消息'
}

export function currentDeviceOrder(saved: string = ''): CurrentDeviceWidget[] {
  const order: CurrentDeviceWidget[] = []
  for (const id of saved.split(',')) {
    const widget = DEFAULT_ORDER.find((item: CurrentDeviceWidget) => item === id)
    if (widget && !order.includes(widget)) { order.push(widget) }
  }
  // 旧版将设备信息固定在首位，升级时保留其余控件的相对位置。
  if (!order.includes(CurrentDeviceWidget.Device)) { order.unshift(CurrentDeviceWidget.Device) }
  return order.concat(DEFAULT_ORDER.filter((item: CurrentDeviceWidget) => !order.includes(item)))
}

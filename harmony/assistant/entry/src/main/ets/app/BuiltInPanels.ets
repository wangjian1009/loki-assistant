import { AppLauncherPanel } from '../panels/launcher/AppLauncherPanel'
import { AppLauncherRegistry } from '../panels/launcher/AppLaunchers'
import { DeviceStatus } from '../panels/device/DeviceStatus'
import { MailCard } from '../panels/mail/MailCard'
import { SlackCard } from '../panels/slack/SlackCard'
import { CurrentDevicePanelContext, CurrentDevicePanelRegistry } from './CurrentDevicePanel'
import { CurrentDeviceState } from './CurrentDeviceState'

function createAppLaunchers(): AppLauncherRegistry {
  const apps = new AppLauncherRegistry()
  apps.register({ id: 'health', ability: 'HealthAbility', labelResource: 'health_title', iconResource: 'health_icon',
    label: $r('app.string.health_title'), icon: $r('app.media.health_icon') })
  apps.register({ id: 'coffee', ability: 'CoffeeAbility', labelResource: 'coffee_title', iconResource: 'coffee_icon',
    label: $r('app.string.coffee_title'), icon: $r('app.media.coffee_icon') })
  apps.register({ id: 'agent-viewer', ability: 'AgentViewerAbility', labelResource: 'agent_viewer_title',
    iconResource: 'agent_viewer_icon', label: $r('app.string.agent_viewer_title'), icon: $r('app.media.agent_viewer_icon') })
  return apps
}

const apps = createAppLaunchers()

@Builder
function launcherPanel(context: CurrentDevicePanelContext) {
  AppLauncherPanel({ registry: apps, editing: context.editing })
}

@Builder
function devicePanel(context: CurrentDevicePanelContext) {
  DeviceStatus({ device: context.device })
}

@Builder
function mailPanel(context: CurrentDevicePanelContext) {
  MailCard({ workspace: context.device.workspace })
}

@Builder
function slackPanel(context: CurrentDevicePanelContext) {
  SlackCard({ workspace: context.device.workspace })
}

function isOnline(device: CurrentDeviceState): boolean {
  return device.hasStatus && device.current?.status === 'online' && !!device.workspace
}

export function createCurrentDevicePanels(): CurrentDevicePanelRegistry {
  const panels = new CurrentDevicePanelRegistry()
  panels.register({ id: 'launcher', title: '应用启动器', supportsEditing: true, render: wrapBuilder(launcherPanel) })
  panels.register({ id: 'device', title: '设备信息', supportsEditing: false, render: wrapBuilder(devicePanel) })
  panels.register({ id: 'mail', title: '邮件收件箱', supportsEditing: false,
    isAvailable: isOnline, render: wrapBuilder(mailPanel) })
  panels.register({ id: 'slack', title: 'Slack 新消息', supportsEditing: false,
    isAvailable: isOnline, render: wrapBuilder(slackPanel) })
  return panels
}

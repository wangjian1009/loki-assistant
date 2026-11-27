import { Application, ComputerDevice } from './Application'
import { StatusSubscription } from '../workspace/ServiceStatus'
import { Workspace } from '../workspace/Workspace'

@Observed
export class CurrentDeviceState {
  current?: ComputerDevice
  active: boolean = false
  hasStatus: boolean = false
  loading: boolean = false
  loaded: boolean = false
  errorMessage: string = ''
  detailsOpen: boolean = false
  private application?: Application
  private subscription?: StatusSubscription
  private version: number = 0

  get workspace(): Workspace | undefined {
    return this.application?.workspace
  }

  activate(application: Application | undefined): void {
    this.application = application
    this.active = true
    this.load()
  }

  deactivate(): void {
    this.active = false
    this.hasStatus = false
    this.version += 1
    this.subscription?.cancel()
    this.subscription = undefined
    this.loading = false
  }

  async refresh(): Promise<void> {
    const endpoint = this.current?.endpoint_id
    const workspace = this.workspace
    if (!this.active || !endpoint || !workspace) { return }
    try {
      const native = await workspace.open()
      if (this.active && this.current?.endpoint_id === endpoint) {
        native.refreshDevice(endpoint)
      }
    } catch (_) {
      console.error('无法发起当前设备刷新')
    }
  }

  collapseDetails(): boolean {
    if (!this.detailsOpen) { return false }
    this.detailsOpen = false
    return true
  }

  async load(): Promise<void> {
    const application = this.application
    if (!application || !this.active || this.loading || this.subscription) { return }
    const version = ++this.version
    this.loading = true
    try {
      const subscription = await application.watchDevices((devices: ComputerDevice[]) => {
        if (version !== this.version) { return }
        const current = devices.find((computer: ComputerDevice) => computer.active)
        if (this.current?.endpoint_id !== current?.endpoint_id) { this.collapseDetails() }
        this.current = current
        this.hasStatus = true
        this.loaded = true
        this.errorMessage = ''
      }, (message: string) => {
        if (version === this.version) { this.errorMessage = message }
      })
      if (version === this.version) {
        this.subscription = subscription
      } else {
        subscription.cancel()
      }
    } catch (error) {
      if (version === this.version) { this.errorMessage = (error as Error).message }
    } finally {
      if (version === this.version) { this.loading = false }
    }
  }
}

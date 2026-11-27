import { Network as RustNetwork, ServiceConfiguration, Workspace as RustWorkspace } from 'liblokiworkspace.so'

export type DeviceType = 'computer' | 'mobile'
export type Service = ServiceConfiguration

// 应用启动时延迟打开本地工作空间，所有调用方复用同一个 Rust 实例。
export class Workspace {
  private handle?: Promise<RustWorkspace>
  private stopping?: Promise<void>
  private directory: string

  constructor(directory: string) { this.directory = directory }

  async open(): Promise<RustWorkspace> {
    if (this.stopping) { throw new Error('工作空间已停止') }
    if (!this.handle) { this.handle = RustWorkspace.open({ directory: this.directory }) }
    const opening = this.handle
    try { return await opening } catch (error) {
      if (this.handle === opening) this.handle = undefined
      throw error as Error
    }
  }

  async network(): Promise<RustNetwork> { return (await this.open()).network() }

  stop(): Promise<void> {
    if (!this.stopping) {
      const handle = this.handle
      this.stopping = handle ? handle.then((workspace: RustWorkspace) => workspace.shutdown()) : Promise.resolve()
    }
    return this.stopping
  }
}

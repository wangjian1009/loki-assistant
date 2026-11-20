import { AutoBinding, BindingState, DeviceType, Service, ServiceResponse, Workspace } from '../workspace/Workspace'
import { NetworkFailure, ResponsError } from '../workspace/ResponsError'

export interface ComputerDevice {
  endpoint_id: string
  name: string
  device_type: DeviceType
  bound_at: number | null
  status: string
  detail: string
  checked_at: number
  active: boolean
  binding_error?: ResponsError
}
interface ComputerRecord extends Service {
  name: string
  device_type: DeviceType
}
interface StoredComputer {
  endpoint_id: string
  relay_url: string
  relay_token: string
  binding?: BindingState
  auto_binding?: AutoBinding
  name?: string
  device_type?: DeviceType
}
interface StoredState { computers?: StoredComputer[]; selected_endpoint_id?: string | null; device_name?: string }
interface ApplicationState { computers: ComputerRecord[]; selected_endpoint_id: string | null; device_name: string }
interface ComputerSession {
  api_version: string
  endpoint_id: string
  status: string
  bound_at: number
  name: string
  device_type: DeviceType
}
function now(): number { return Math.floor(Date.now() / 1000) }
function copy(state: ApplicationState): ApplicationState { return JSON.parse(JSON.stringify(state)) as ApplicationState }
function validName(name: string): boolean {
  return typeof name === 'string' && name.trim().length > 0 &&
    encodeURIComponent(name).replace(/%[0-9A-F]{2}/g, 'x').length <= 128 && !/[\x00-\x1f\x7f]/.test(name)
}
const BINDING_FAILED_DETAIL: string = '请删除此设备后重新扫码绑定。'

// 应用保存多设备配置；自动绑定、协议状态和绑定错误均由 Network 持有。
export class Application {
  readonly workspace: Workspace
  private state: ApplicationState
  private endpointId: string = ''
  private initialized: boolean = false
  private stopped: boolean = false
  private operations: Promise<void> = Promise.resolve()

  constructor(workspace: Workspace, deviceName: string = 'HarmonyOS 手机') {
    this.workspace = workspace
    this.state = { computers: [], selected_endpoint_id: null, device_name: deviceName }
  }
  private run<T>(operation: () => Promise<T>): Promise<T> {
    const task = this.operations.then(async () => {
      if (this.stopped) { throw new Error('应用已停止') }
      await this.initialize()
      return operation()
    })
    this.operations = task.then(() => {}, () => {})
    return task
  }
  private async initialize(): Promise<void> {
    if (this.initialized) { return }
    const stored = await this.workspace.loadState()
    const input = stored.state as StoredState
    const computers = input.computers === undefined ? [] : input.computers
    if (!Array.isArray(computers) || computers.length > 64) { throw new Error('电脑记录已损坏') }
    const state: ApplicationState = {
      computers: [], selected_endpoint_id: input.selected_endpoint_id ?? null,
      device_name: input.device_name ?? this.state.device_name
    }
    if (!validName(state.device_name)) { throw new Error('手机名称已损坏') }
    const ids: string[] = []
    for (const computer of computers) {
      if (!computer || !/^[0-9a-f]{64}$/.test(computer.endpoint_id) || computer.endpoint_id === stored.endpoint_id ||
        ids.includes(computer.endpoint_id) || !this.validRelay(computer.relay_url, computer.relay_token)) {
        throw new Error('电脑记录已损坏')
      }
      const binding: BindingState = computer.binding === undefined ? { status: 'unbound' } : computer.binding
      if (!binding) { throw new Error('绑定状态已损坏') }
      if (binding.status === 'bound') {
        if (!binding.credential || !/^[0-9a-f]{64}$/.test(binding.credential.token) ||
          !Number.isSafeInteger(binding.credential.bound_at) || binding.credential.bound_at <= 0) { throw new Error('绑定凭据已损坏') }
      } else if (binding.status === 'failed') {
        if (!binding.error || typeof binding.error.kind !== 'string' || typeof binding.error.reason !== 'string' ||
          typeof binding.error.message !== 'string') { throw new Error('绑定错误信息已损坏') }
      } else if (binding.status !== 'unbound' && binding.status !== 'binding') { throw new Error('绑定状态已损坏') }
      const name = computer.name ?? '电脑'
      const deviceType = computer.device_type ?? 'computer'
      if (!validName(name) || (deviceType !== 'computer' && deviceType !== 'mobile')) { throw new Error('设备展示信息已损坏') }
      state.computers.push({
        endpoint_id: computer.endpoint_id, relay_url: computer.relay_url, relay_token: computer.relay_token,
        binding: binding, auto_binding: computer.auto_binding, name: name, device_type: deviceType
      })
      ids.push(computer.endpoint_id)
    }
    if (state.selected_endpoint_id && !ids.includes(state.selected_endpoint_id)) { throw new Error('选中的电脑记录不存在') }
    if (!state.selected_endpoint_id && state.computers.length === 1) { state.selected_endpoint_id = state.computers[0].endpoint_id }
    const selected = state.computers.find((computer: ComputerRecord) => computer.endpoint_id === state.selected_endpoint_id)
    if (selected) { await this.workspace.network.setService(this.service(selected)) }
    this.endpointId = stored.endpoint_id
    this.state = state
    this.initialized = true
  }
  private validRelay(address: string, token: string): boolean {
    return typeof address === 'string' && /^https?:\/\/[^/?#@\\\s]+(?:\/[^?#\\\s]*)?$/.test(address) &&
      typeof token === 'string' && token.trim().length > 0 && token.length <= 4096
  }
  private service(computer: ComputerRecord): Service {
    return {
      endpoint_id: computer.endpoint_id, relay_url: computer.relay_url,
      relay_token: computer.relay_token, binding: computer.binding, auto_binding: computer.auto_binding
    }
  }
  private async save(state: ApplicationState): Promise<void> {
    await this.workspace.saveState(state)
    this.state = state
  }
  private views(): ComputerDevice[] {
    return this.state.computers.map((computer: ComputerRecord): ComputerDevice => {
      const active = computer.endpoint_id === this.state.selected_endpoint_id
      const failed = computer.binding.status === 'failed'
      const bindingError = computer.binding.status === 'failed' ? computer.binding.error : undefined
      return {
        endpoint_id: computer.endpoint_id, name: computer.name, device_type: computer.device_type,
        bound_at: computer.binding.status === 'bound' ? computer.binding.credential.bound_at : null,
        status: failed ? 'binding_failed' : active && computer.binding.status === 'binding' ? 'binding' : 'offline',
        detail: failed ? `${bindingError?.message}；${BINDING_FAILED_DETAIL}` : active && computer.binding.status === 'binding' ?
          '正在与电脑建立绑定' : active ? '等待检查电脑状态' : '左右滑动到此设备以连接',
        checked_at: 0, active: active, binding_error: bindingError
      }
    })
  }
  prepare(): Promise<void> {
    return this.run(async (): Promise<void> => {})
  }
  list(): Promise<ComputerDevice[]> {
    return this.run(async () => { await this.captureService(); return this.views() })
  }
  deviceName(): Promise<string> { return this.run(async () => this.state.device_name) }
  setDeviceName(name: string): Promise<void> {
    return this.run(async () => {
      if (!validName(name)) { throw new Error('请输入不含控制字符的有效设备名称') }
      const state = copy(this.state)
      state.device_name = name.trim()
      await this.save(state)
    })
  }
  select(endpointId: string): Promise<void> {
    return this.run(async () => {
      if (!this.state.computers.some((item: ComputerRecord) => item.endpoint_id === endpointId)) { throw new Error('电脑记录不存在') }
      // 切换前先保存旧目标的自动绑定结果，避免成功凭据遗留在运行时。
      await this.captureService()
      const latest = copy(this.state)
      latest.selected_endpoint_id = endpointId
      const computer = latest.computers.find((item: ComputerRecord) => item.endpoint_id === endpointId)
      if (!computer) { throw new Error('电脑记录不存在') }
      await this.save(latest)
      await this.workspace.network.setService(this.service(computer))
      await this.captureService()
    })
  }
  remove(endpointId: string): Promise<void> {
    return this.run(async () => {
      await this.captureService()
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === endpointId)
      if (!computer) { throw new Error('电脑记录不存在') }
      if (computer.binding.status === 'bound' || computer.binding.status === 'binding') {
        throw new Error('请先解除绑定或等待绑定结束后再删除设备')
      }
      const active = state.selected_endpoint_id === endpointId
      state.computers = state.computers.filter((item: ComputerRecord) => item.endpoint_id !== endpointId)
      if (active) { state.selected_endpoint_id = null }
      await this.save(state)
      if (active) { await this.workspace.network.clearService() }
    })
  }
  addService(payload: string): Promise<void> {
    return this.run(async () => {
      const invitation = await this.workspace.network.parseInvitation(payload)
      if (invitation.endpoint_id === this.endpointId) { throw new Error('不能绑定手机自身') }
      await this.captureService()
      const state = copy(this.state)
      if (state.computers.some((item: ComputerRecord) => item.endpoint_id === invitation.endpoint_id)) {
        throw new Error('此电脑已在设备列表中，请删除后重新扫码添加')
      }
      if (state.computers.length >= 64) { throw new Error('最多可保存 64 台电脑') }
      const computer: ComputerRecord = {
        endpoint_id: invitation.endpoint_id, relay_url: invitation.relay_url, relay_token: invitation.relay_token,
        binding: { status: 'unbound' }, auto_binding: { invitation: invitation, device_name: state.device_name },
        name: '电脑', device_type: 'computer'
      }
      state.computers.push(computer)
      state.selected_endpoint_id = computer.endpoint_id
      await this.save(state)
      // 添加目标即触发自动绑定，应用不调用独立绑定接口。
      await this.workspace.network.setService(this.service(computer))
      await this.captureService()
    })
  }
  private async captureService(): Promise<void> {
    const service = await this.workspace.network.service()
    if (!service) { return }
    if (service.endpoint_id !== this.state.selected_endpoint_id) { throw new Error('电脑服务已切换') }
    const state = copy(this.state)
    const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === service.endpoint_id)
    if (!computer) { throw new Error('电脑记录不存在') }
    computer.binding = service.binding
    computer.auto_binding = service.auto_binding
    if (JSON.stringify(state) !== JSON.stringify(this.state)) { await this.save(state) }
  }
  private session(): Promise<ComputerSession> {
    return new Promise((resolve, reject) => {
      this.workspace.network.request({ method: 'GET', path: '/network/session' }, (response: ServiceResponse) => {
        const session = response.body as ComputerSession
        if (!session || session.api_version !== '1' || session.endpoint_id !== this.state.selected_endpoint_id ||
          session.status !== 'online' || !Number.isSafeInteger(session.bound_at) || session.bound_at <= 0 ||
          !validName(session.name) || (session.device_type !== 'computer' && session.device_type !== 'mobile')) {
          const failure: NetworkFailure = {
            kind: 'network', reason: 'invalid_response', message: '电脑返回的状态无效',
            request_id: response.request_id, outcome: 'unknown'
          }
          reject(failure)
        } else { resolve(session) }
      }, (error: ResponsError) => { reject(error) })
    })
  }
  refresh(): Promise<ComputerDevice[]> {
    return this.run(async () => {
      await this.captureService()
      const selected = this.state.computers.find((item: ComputerRecord) => item.endpoint_id === this.state.selected_endpoint_id)
      if (!selected || selected.binding.status !== 'bound') { return this.views() }
      let session: ComputerSession
      try { session = await this.session() } catch (error) {
        await this.captureService()
        const failure = error as ResponsError
        const views = this.views()
        return views.map((view: ComputerDevice): ComputerDevice => {
          if (view.active) {
            if (view.status !== 'binding_failed') { view.status = 'offline'; view.detail = failure.message }
            view.checked_at = now()
          }
          return view
        })
      }
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === session.endpoint_id)
      if (!computer) { throw new Error('电脑服务已切换') }
      computer.name = session.name
      computer.device_type = session.device_type
      await this.save(state)
      return this.views().map((view: ComputerDevice): ComputerDevice => {
        if (view.active) { view.status = 'online'; view.detail = '电脑服务可用'; view.checked_at = now() }
        return view
      })
    })
  }
  async stop(): Promise<void> {
    this.stopped = true
    await this.workspace.stop()
    await this.operations
  }
}

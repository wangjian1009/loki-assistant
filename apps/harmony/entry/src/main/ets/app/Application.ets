import { BindingState, DeviceType, Invitation, Service, ServiceResponse, Workspace } from '../workspace/Workspace'
import { NetworkFailure, ResponsError, responsError } from '../workspace/ResponsError'

export interface ComputerDevice {
  endpoint_id: string
  name: string
  device_type: DeviceType
  bound_at: number | null
  status: string
  detail: string
  checked_at: number
  active: boolean
}
interface ComputerRecord extends Service {
  name: string
  device_type: DeviceType
  pending: Invitation | null
}
interface StoredComputer {
  endpoint_id: string
  relay_url: string
  relay_token: string
  binding?: BindingState
  name?: string
  device_type?: DeviceType
  pending?: Invitation | null
  bound_at?: number | null
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
const UNBOUND_DETAIL: string = '此设备已解除绑定，请删除后重新扫码添加。'
function validName(name: string): boolean {
  // UTF-8 长度最终由 Network / services 校验，应用限制输入并拒绝控制字符。
  return typeof name === 'string' && name.trim().length > 0 && encodeURIComponent(name).replace(/%[0-9A-F]{2}/g, 'x').length <= 128 && !/[\x00-\x1f\x7f]/.test(name)
}

// 应用负责历史设备、名称、选择和持久化；Network 只接收当前唯一 Service。
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
      // 旧版本只有 bound_at，缺少协议凭据，必须保持未绑定并重新扫码。
      const binding: BindingState = computer.binding === undefined ? { status: 'unbound' } : computer.binding
      if (!binding) { throw new Error('绑定状态已损坏') }
      if (binding.status === 'bound') {
        if (!binding.credential || !/^[0-9a-f]{64}$/.test(binding.credential.token) ||
          !Number.isSafeInteger(binding.credential.bound_at) || binding.credential.bound_at <= 0) { throw new Error('绑定凭据已损坏') }
      } else if (binding.status !== 'unbound') { throw new Error('绑定状态已损坏') }
      if (computer.bound_at !== undefined && computer.bound_at !== null &&
        (!Number.isSafeInteger(computer.bound_at) || computer.bound_at <= 0)) { throw new Error('旧绑定记录已损坏') }
      if (computer.pending && (computer.pending.endpoint_id !== computer.endpoint_id ||
        computer.pending.relay_url !== computer.relay_url || computer.pending.relay_token !== computer.relay_token ||
        !/^([0-9a-f]{8}-)([0-9a-f]{4}-){3}[0-9a-f]{12}$/.test(computer.pending.pairing_id) ||
        computer.pending.version !== 1 || computer.pending.alpn !== 'loki-assistant/pair/1' ||
        !/^[0-9a-f]{64}$/.test(computer.pending.pairing_token) ||
        !Number.isSafeInteger(computer.pending.expires_at) || computer.pending.expires_at <= 0)) { throw new Error('待确认绑定记录已损坏') }
      const name = computer.name ?? '已绑定电脑'
      const deviceType = computer.device_type ?? 'computer'
      if (!validName(name) || (deviceType !== 'computer' && deviceType !== 'mobile')) { throw new Error('设备展示信息已损坏') }
      state.computers.push({
        endpoint_id: computer.endpoint_id, relay_url: computer.relay_url, relay_token: computer.relay_token,
        binding: binding, pending: computer.pending ?? null, name: name, device_type: deviceType
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
      relay_token: computer.relay_token, binding: computer.binding
    }
  }
  private async save(state: ApplicationState): Promise<void> {
    await this.workspace.saveState(state)
    this.state = state
  }
  private views(): ComputerDevice[] {
    return this.state.computers.map((computer: ComputerRecord): ComputerDevice => {
      const active = computer.endpoint_id === this.state.selected_endpoint_id
      const unbound = computer.binding.status === 'unbound' && !computer.pending
      return {
        endpoint_id: computer.endpoint_id, name: computer.name, device_type: computer.device_type,
        bound_at: computer.binding.status === 'bound' ? computer.binding.credential.bound_at : null,
        status: !active ? 'inactive' : computer.pending ? 'pending' : computer.binding.status === 'unbound' ? 'unbound' : 'unknown',
        detail: unbound ? UNBOUND_DETAIL : active ? '等待检查电脑状态' : '选择此电脑后检查状态',
        checked_at: 0, active: active
      }
    })
  }
  list(): Promise<ComputerDevice[]> { return this.run(async () => this.views()) }
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
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === endpointId)
      if (!computer) { throw new Error('电脑记录不存在') }
      // 保存成功后才切换 Network，磁盘失败保留原目标。
      state.selected_endpoint_id = endpointId
      await this.save(state)
      await this.workspace.network.setService(this.service(computer))
    })
  }
  remove(endpointId: string): Promise<void> {
    return this.run(async () => {
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === endpointId)
      if (!computer) { throw new Error('电脑记录不存在') }
      if (computer.binding.status !== 'unbound' || computer.pending) {
        throw new Error('请先在电脑端解除绑定后再删除设备')
      }
      const active = state.selected_endpoint_id === endpointId
      state.computers = state.computers.filter((item: ComputerRecord) => item.endpoint_id !== endpointId)
      if (active) { state.selected_endpoint_id = null }
      // 持久化成功后再清除当前目标，保存失败时保留记录与连接选择。
      await this.save(state)
      if (active) { await this.workspace.network.clearService() }
    })
  }
  bind(payload: string): Promise<void> {
    return this.run(async () => {
      const invitation = await this.workspace.network.parseInvitation(payload)
      if (invitation.endpoint_id === this.endpointId) { throw new Error('不能绑定手机自身') }
      const state = copy(this.state)
      const index = state.computers.findIndex((item: ComputerRecord) => item.endpoint_id === invitation.endpoint_id)
      if (index < 0 && state.computers.length >= 64) { throw new Error('最多可保存 64 台电脑') }
      const old = index < 0 ? undefined : state.computers[index]
      const binding: BindingState = { status: 'unbound' }
      const computer: ComputerRecord = {
        endpoint_id: invitation.endpoint_id, relay_url: invitation.relay_url, relay_token: invitation.relay_token,
        binding: old?.binding ?? binding, pending: invitation,
        name: old?.name ?? '待绑定电脑', device_type: old?.device_type ?? 'computer'
      }
      if (index < 0) { state.computers.push(computer) } else { state.computers[index] = computer }
      state.selected_endpoint_id = computer.endpoint_id
      await this.save(state)
      await this.workspace.network.setService(this.service(computer))
      await this.workspace.network.bind(payload, state.device_name)
      await this.captureService()
    })
  }
  private async captureService(): Promise<void> {
    const service = await this.workspace.network.service()
    if (!service || service.endpoint_id !== this.state.selected_endpoint_id) { throw new Error('电脑服务已切换') }
    const state = copy(this.state)
    const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === service.endpoint_id)
    if (!computer) { throw new Error('电脑记录不存在') }
    computer.binding = service.binding
    if (service.binding.status === 'bound') { computer.pending = null }
    await this.save(state)
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
      const selected = this.state.computers.find((item: ComputerRecord) => item.endpoint_id === this.state.selected_endpoint_id)
      if (!selected) { return this.views() }
      let session: ComputerSession
      try { session = await this.session() } catch (error) {
        const failure = responsError(error)
        if (failure.kind !== 'not_bound' || !selected.pending || selected.pending.expires_at <= now()) {
          return this.failedRefresh(selected, failure)
        }
        try { await this.workspace.network.bind(JSON.stringify(selected.pending), this.state.device_name) }
        catch (error) { return this.failedRefresh(selected, responsError(error)) }
        await this.captureService()
        try { session = await this.session() }
        catch (error) { return this.failedRefresh(selected, responsError(error)) }
      }
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === session?.endpoint_id)
      if (!computer) { throw new Error('电脑服务已切换') }
      computer.name = session.name
      computer.device_type = session.device_type
      await this.save(state)
      return this.currentView('online', '电脑服务可用')
    })
  }
  private async failedRefresh(selected: ComputerRecord, failure: ResponsError): Promise<ComputerDevice[]> {
    if (failure.kind === 'not_bound') {
      const state = copy(this.state)
      const computer = state.computers.find((item: ComputerRecord) => item.endpoint_id === selected.endpoint_id)
      if (computer) { computer.binding = { status: 'unbound' }; await this.save(state) }
    }
    const unbound = failure.kind === 'not_bound' && !selected.pending
    return this.currentView(selected.pending ? 'pending' : unbound ? 'unbound' : 'unreachable',
      unbound ? UNBOUND_DETAIL : failure.message)
  }
  private currentView(status: string, detail: string): ComputerDevice[] {
    return this.views().map((view: ComputerDevice): ComputerDevice => {
      if (view.active) { view.status = status; view.detail = detail; view.checked_at = now() }
      return view
    })
  }
  async stop(): Promise<void> {
    this.stopped = true
    await this.workspace.stop()
    await this.operations
  }
}

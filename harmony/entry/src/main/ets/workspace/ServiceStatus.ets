// 页面取消订阅时同时阻止已排队的 UI 更新。
export class StatusSubscription {
  private unsubscribe: () => void
  constructor(unsubscribe: () => void) { this.unsubscribe = unsubscribe }
  cancel(): void { this.unsubscribe() }
}

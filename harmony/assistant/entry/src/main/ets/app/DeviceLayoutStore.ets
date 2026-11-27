import { common } from '@kit.AbilityKit'
import { preferences } from '@kit.ArkData'
import { ItemLayout, readItemLayout, readLegacyLauncherLayout, readLegacyPanelLayout } from './CurrentDeviceLayout'

export class DeviceLayoutStore {
  private readonly store: preferences.Preferences
  private readonly key: string

  constructor(context: common.Context, key: string) {
    this.store = preferences.getPreferencesSync(context, { name: 'current_device_layout' })
    this.key = key
  }

  load(ids: string[]): ItemLayout {
    const legacy = this.read('widget_layout')
    const fallback = this.key === 'panel_layout'
      ? readLegacyPanelLayout(ids, legacy, this.read('widget_order'))
      : readLegacyLauncherLayout(ids, legacy)
    return readItemLayout(ids, this.read(this.key), fallback)
  }

  save(next: ItemLayout): void {
    const previous = this.read(this.key)
    try {
      this.store.putSync(this.key, JSON.stringify(next))
      this.store.flushSync()
    } catch (error) {
      try { this.store.putSync(this.key, previous) }
      catch (_) { console.error('无法还原布局缓存') }
      throw error as Error
    }
  }

  private read(key: string): string {
    try {
      const value = this.store.getSync(key, '')
      return typeof value === 'string' ? value : ''
    } catch (_) {
      throw new Error('无法读取布局存储：' + key)
    }
  }
}

export interface ItemLayout {
  order: string[]
  hidden: string[]
}

interface LegacyLayout {
  panelOrder?: string[]
  hiddenPanels?: string[]
  appOrder?: string[]
  hiddenApps?: string[]
}

function knownItems(items: string[] | undefined, ids: string[]): string[] {
  if (!Array.isArray(items)) return []
  return items.filter((id: string, index: number) => ids.includes(id) && items.indexOf(id) === index)
}

function completeLayout(ids: string[], order?: string[], hidden?: string[]): ItemLayout {
  const known = knownItems(order, ids)
  return {
    order: known.concat(ids.filter((id: string) => !known.includes(id))),
    hidden: knownItems(hidden, ids)
  }
}

export function readItemLayout(ids: string[], saved: string, fallback: ItemLayout): ItemLayout {
  if (!saved) return fallback
  try {
    const stored = JSON.parse(saved) as ItemLayout
    return completeLayout(ids, stored.order, stored.hidden)
  } catch (_) {
    return fallback
  }
}

export function readLegacyPanelLayout(ids: string[], saved: string, legacyOrder: string): ItemLayout {
  if (saved) {
    try {
      const stored = JSON.parse(saved) as LegacyLayout
      if (Array.isArray(stored.panelOrder)) return completeLayout(ids, stored.panelOrder, stored.hiddenPanels)
    } catch (_) {
      // 旧版布局损坏时尝试更早的顺序记录。
    }
  }
  const order = knownItems(legacyOrder.split(','), ids)
  // 早期版本分别将设备信息和启动器插在首页最前。
  const leading = ['launcher', 'device'].filter((id: string) => ids.includes(id) && !order.includes(id))
  return completeLayout(ids, leading.concat(order))
}

export function readLegacyLauncherLayout(ids: string[], saved: string): ItemLayout {
  if (saved) {
    try {
      const stored = JSON.parse(saved) as LegacyLayout
      return completeLayout(ids, stored.appOrder, stored.hiddenApps)
    } catch (_) {
      // 旧版布局损坏时使用注册顺序。
    }
  }
  return completeLayout(ids)
}

export function moveVisibleItem(order: string[], hidden: string[], item: string, target: string): string[] {
  const visible = order.filter((id: string) => !hidden.includes(id))
  const from = visible.indexOf(item)
  const to = visible.indexOf(target)
  if (from < 0 || to < 0 || from === to) return order
  visible.splice(from, 1)
  visible.splice(to, 0, item)
  let index = 0
  return order.map((id: string) => hidden.includes(id) ? id : visible[index++])
}

export interface ViewerBlock {
  id: string
  kind: string
  title: string
  text: string
  parentId: string
  collapsed: boolean | null
}

export interface ConversationRow {
  id: string
  kind: string
  title: string
  text: string
  depth: number
  foldable: boolean
  collapsed: boolean
}

export function conversationRows(blocks: ViewerBlock[], folds: Map<string, boolean>): ConversationRow[] {
  const byId = new Map<string, ViewerBlock>()
  const continuation = new Map<string, string>()
  for (const block of blocks) {
    if (block.kind === 'continuation') {
      continuation.set(block.parentId, (continuation.get(block.parentId) ?? '') + block.text)
    } else {
      byId.set(block.id, block)
    }
  }
  const rows: ConversationRow[] = []
  for (const block of blocks) {
    if (block.kind === 'continuation' && byId.has(block.parentId)) continue
    const parent = byId.get(block.parentId)
    if (parent?.kind === 'group' && (folds.get(parent.id) ?? parent.collapsed ?? true)) continue
    const kind = block.kind === 'continuation' ? 'system' : block.kind
    const body = block.text + (continuation.get(block.id) ?? '')
    const foldable = kind === 'group' || (body.length > 0 &&
      (block.collapsed != null || kind === 'tool' || kind === 'thought'))
    const collapsed = folds.get(block.id) ??
      (kind === 'tool' || kind === 'thought' ? true : block.collapsed ?? true)
    rows.push({
      id: block.id, kind, title: block.title, text: body, depth: parent?.kind === 'group' ? 1 : 0,
      foldable, collapsed: foldable && collapsed
    })
  }
  return rows
}

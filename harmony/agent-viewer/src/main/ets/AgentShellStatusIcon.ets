import { accessibility } from '@kit.AccessibilityKit'

interface AgentShellAppearance {
  icon: Resource
  label: string
  color: string
}

export function agentShellAppearance(phase: string): AgentShellAppearance {
  switch (phase) {
    case 'idle': return { icon: $r('sys.symbol.circle'), label: '空闲', color: '#A8B1C4' }
    case 'starting': return { icon: $r('sys.symbol.hourglass'), label: '启动中', color: '#C2CCFF' }
    case 'running': return { icon: $r('sys.symbol.play_circle'), label: '执行中', color: '#C2CCFF' }
    case 'waiting_input': return { icon: $r('sys.symbol.ellipsis_bubble'), label: '等待输入', color: '#F5CF92' }
    case 'completed': return { icon: $r('sys.symbol.checkmark_circle'), label: '已完成', color: '#9AE2B9' }
    case 'cancelled': return { icon: $r('sys.symbol.stop_circle'), label: '已取消', color: '#A8B1C4' }
    case 'failed': return { icon: $r('sys.symbol.exclamationmark_circle'), label: '执行失败', color: '#FFB4B1' }
    case 'disconnected': return { icon: $r('sys.symbol.wifi_slash'), label: '已断开', color: '#A8B1C4' }
    default: return { icon: $r('sys.symbol.questionmark_circle'), label: '状态未知', color: '#A8B1C4' }
  }
}

@Component
export struct AgentShellStatusIcon {
  @Prop phase: string = 'unknown'
  @Prop iconSize: number = 18

  build() {
    Stack() {
      if (this.phase === 'running' && !accessibility.isAnimationReduceEnabledSync()) {
        LoadingProgress().width(this.iconSize).height(this.iconSize).color(agentShellAppearance(this.phase).color)
      } else {
        SymbolGlyph(agentShellAppearance(this.phase).icon).fontSize(this.iconSize)
          .fontColor([agentShellAppearance(this.phase).color])
      }
    }.width(this.iconSize).height(this.iconSize).flexShrink(0)
      .accessibilityGroup(true).accessibilityText(agentShellAppearance(this.phase).label)
  }
}

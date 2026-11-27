import { AgentViewerScreen } from 'agent_viewer'

@Entry
@Component
struct AgentViewer {
  build() {
    Column() {
      AgentViewerScreen({ title: $r('app.string.agent_viewer_title') })
    }
  }
}

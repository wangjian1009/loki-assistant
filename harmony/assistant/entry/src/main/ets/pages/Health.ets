import { HealthScreen } from 'health'

@Entry
@Component
struct Health {
  build() {
    Column() {
      HealthScreen({ title: $r('app.string.health_title') })
    }
  }
}

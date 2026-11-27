import { CoffeeScreen } from 'coffee'

@Entry
@Component
struct Coffee {
  build() {
    Column() {
      CoffeeScreen({ title: $r('app.string.coffee_title') })
    }
  }
}

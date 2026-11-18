@Entry
@Component
struct Index {
  build() {
    Column({ space: 16 }) {
      Text('Loki 执行中心')
        .fontSize(28)
        .fontWeight(FontWeight.Bold)
      Text('欢迎使用 Loki Assistant')
        .fontSize(18)
        .fontColor('#176443')
      Text('手机辅助能力正在开发中。')
        .fontSize(16)
        .fontColor('#666666')
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
    .padding(24)
  }
}

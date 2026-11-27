@Component
export struct CoffeeScreen {
  @Prop title: ResourceStr

  build() {
    Column() {
      Text(this.title).width('100%').fontSize(18).fontWeight(FontWeight.Medium)
        .fontColor('#F1F3FA').padding({ left: 16, right: 16, top: 16, bottom: 16 })
    }.width('100%').height('100%').backgroundColor('#111318')
  }
}

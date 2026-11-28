@Component
export struct AgentAvatar {
  @Prop name: string = ''
  @Prop source: string = ''
  @Prop avatarSize: number = 24
  @State private failedSource: string = ''

  build() {
    Stack() {
      if (this.source && this.source !== this.failedSource) {
        Image(this.source).width(this.avatarSize).height(this.avatarSize)
          .objectFit(ImageFit.Contain)
          .onError(() => { this.failedSource = this.source })
      } else {
        Text(Array.from(this.name.trim().toUpperCase())[0] ?? 'A')
          .fontSize(this.avatarSize * 0.55).fontWeight(FontWeight.Medium).fontColor('#C2CCFF')
          .width(this.avatarSize).height(this.avatarSize).textAlign(TextAlign.Center)
          .backgroundColor('#394462').borderRadius(this.avatarSize * 0.25)
      }
    }.width(this.avatarSize).height(this.avatarSize).flexShrink(0).accessibilityLevel('no-hide-descendants')
  }
}

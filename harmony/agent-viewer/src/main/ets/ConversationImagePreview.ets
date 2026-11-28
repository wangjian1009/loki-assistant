const FIT_SCALE: number = 1
const MAX_SCALE: number = 5
const SCALE_TOLERANCE: number = 0.001

@Component
export struct ConversationImagePreview {
  @Prop @Watch('resetZoom') source: string = ''
  onClose: () => void = () => {}
  @State private zoom: number = FIT_SCALE
  @State private viewportWidth: number = 1
  @State private viewportHeight: number = 1
  @State private imageWidth: number = 0
  @State private imageHeight: number = 0
  private swipeFromFit: boolean = false

  private resetZoom(): void {
    this.zoom = FIT_SCALE
    this.imageWidth = 0
    this.imageHeight = 0
  }

  private fitRatio(): number {
    if (!this.imageWidth || !this.imageHeight) return FIT_SCALE
    return Math.min(this.viewportWidth / this.imageWidth, this.viewportHeight / this.imageHeight)
  }

  private actualSizeZoom(): number {
    return 1 / this.fitRatio()
  }

  private atActualSize(): boolean {
    return Math.abs(this.zoom - this.actualSizeZoom()) < SCALE_TOLERANCE
  }

  private atFitSize(): boolean {
    return Math.abs(this.zoom - FIT_SCALE) < SCALE_TOLERANCE
  }

  private toggleZoom(): void {
    if (!this.imageWidth || !this.imageHeight) return
    this.zoom = this.atActualSize() ? FIT_SCALE : this.actualSizeZoom()
  }

  build() {
    Column() {
      Row() {
        Button(this.atActualSize() ? '适应屏幕' : '1:1', { type: ButtonType.Normal })
          .height(44).fontColor('#F1F3FA').backgroundColor('#1B1F28').onClick(() => { this.toggleZoom() })
          .enabled(this.imageWidth > 0 && this.imageHeight > 0)
        Blank()
        Button('关闭', { type: ButtonType.Normal })
          .height(44).fontColor('#F1F3FA').backgroundColor('#1B1F28').onClick(() => { this.onClose() })
      }.width('100%').padding(12)

      Scroll() {
        Image('file://' + this.source)
          .width(this.imageWidth ? this.imageWidth * this.fitRatio() : this.viewportWidth)
          .height(this.imageHeight ? this.imageHeight * this.fitRatio() : this.viewportHeight)
          .objectFit(ImageFit.Contain).autoResize(false)
          .onComplete(event => {
            if (event && event.width > 0 && event.height > 0) {
              const context = this.getUIContext()
              this.imageWidth = context.px2vp(event.width)
              this.imageHeight = context.px2vp(event.height)
            }
          })
      }.width('100%').layoutWeight(1).align(Alignment.Center).scrollable(ScrollDirection.FREE).scrollBar(BarState.Off)
        .minZoomScale(Math.min(FIT_SCALE, this.actualSizeZoom()))
        .maxZoomScale(Math.max(MAX_SCALE, this.actualSizeZoom())).zoomScale(this.zoom).enableBouncesZoom(false)
        .edgeEffect(EdgeEffect.None).clip(true)
        .onSizeChange((_old: SizeOptions, current: SizeOptions) => {
          this.viewportWidth = Number(current.width)
          this.viewportHeight = Number(current.height)
        })
        .onDidZoom((scale: number) => { this.zoom = scale })
        .gesture(TapGesture({ count: 2 }).onAction(() => { this.toggleZoom() }))
        .parallelGesture(PanGesture({ fingers: 1, direction: PanDirection.Horizontal })
          .onActionStart(() => { this.swipeFromFit = this.atFitSize() })
          .onActionEnd((event: GestureEvent) => {
            if (this.swipeFromFit && this.atFitSize() && event.offsetX <= -48 && -event.offsetX >= Math.abs(event.offsetY)) {
              this.onClose()
            }
          }))
    }.width('100%').height('100%').backgroundColor('#F0000000')
  }
}

import { webview } from '@kit.ArkWeb'
import { fileIo } from '@kit.CoreFileKit'

const IMAGE_PREFIX: string = 'https://loki-image.invalid/'

const STYLES: string = `
  :root { color-scheme: dark; }
  html, body { margin: 0; padding: 0; background: transparent; }
  body { color: #f1f3fa; font: 15px/1.58 sans-serif; overflow-wrap: anywhere; }
  p, ul, ol, blockquote, pre, table, dl { margin: 0 0 .75em; }
  h1, h2, h3, h4, h5, h6 { margin: .8em 0 .35em; line-height: 1.35; font-weight: 650; }
  h1 { font-size: 1.5em; } h2 { font-size: 1.32em; } h3 { font-size: 1.18em; }
  h4, h5, h6 { font-size: 1.05em; }
  body > :first-child, main > :first-child, section > :first-child { margin-top: 0; }
  body > :last-child, main > :last-child, section > :last-child { margin-bottom: 0; }
  ul, ol { padding-inline-start: 1.4em; }
  li > p { margin: 0; } li > ul, li > ol { margin: .2em 0; }
  blockquote { border-left: 3px solid #57668f; padding-left: 10px; color: #c6d0e4; }
  code, pre { font-family: monospace; font-size: .92em; }
  code { background: #282f42; color: #d3dcff; border-radius: 4px; padding: 1px 4px; }
  pre { margin-block: .55em .75em; padding: 10px 12px; border: 1px solid #303744;
    border-radius: 9px; background: #121720; color: #d5ece1; white-space: pre-wrap;
    overflow-wrap: anywhere; tab-size: 4; }
  pre code { font-size: inherit; padding: 0; background: transparent; color: inherit; }
  table { border-collapse: collapse; width: 100%; font-size: .94em; }
  td, th { border: 1px solid #41495b; padding: 5px 7px; text-align: start; }
  th, thead { background: #282f42; font-weight: 650; }
  a { color: #c2ccff; }
  img { max-width: 100%; height: auto; display: block; margin-block: .5em; border-radius: 8px; }
  hr { border: 0; border-top: 1px solid #41495b; }
  .detail { color: #a8b1c4; font-size: 13px; }
  .detail h1, .detail h2, .detail h3 { font-size: 1.12em; }
  .detail pre { background: #171b23; color: #a8b1c4; }
`

@Component
export struct OrgRenderer {
  @Prop @Watch('loadContent') source: string = ''
  @Prop @Watch('loadContent') format: string = 'auto'
  @Prop @Watch('loadContent') subdued: boolean = false
  parse: (source: string, format: string) => string = () => ''
  readImage: (source: string) => Promise<string> = () => Promise.reject(new Error('图片不可用'))
  onOpenImage: (source: string) => void = () => {}
  onContentReady: () => void = () => {}
  @State private failed: boolean = false
  private readonly controller: webview.WebviewController = new webview.WebviewController()
  private attached: boolean = false

  private interceptImage(url: string): WebResourceResponse | null {
    const prefix = IMAGE_PREFIX + 'inline/'
    if (!url.startsWith(prefix)) return null
    const response = new WebResourceResponse()
    response.setResponseIsReady(false)
    try {
      const source = decodeURIComponent(url.substring(prefix.length))
      this.readImage(source).then(path => {
        const file = fileIo.openSync(path, fileIo.OpenMode.READ_ONLY)
        try {
          const data = new ArrayBuffer(fileIo.statSync(path).size)
          fileIo.readSync(file.fd, data)
          const extension = path.substring(path.lastIndexOf('.') + 1)
          response.setResponseMimeType('image/' + (extension === 'jpg' ? 'jpeg' : extension))
          response.setResponseCode(200)
          response.setResponseData(data)
        } finally {
          fileIo.closeSync(file)
        }
        response.setResponseIsReady(true)
      }).catch(() => {
        response.setResponseCode(404)
        response.setResponseData('')
        response.setResponseIsReady(true)
      })
    } catch (_) {
      response.setResponseCode(404)
      response.setResponseData('')
      response.setResponseIsReady(true)
    }
    return response
  }

  private loadContent(): void {
    if (!this.attached) return
    try {
      const content = this.parse(this.source, this.format)
      if (!content && this.source) {
        this.failed = true
        return
      }
      this.controller.loadData(
        '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">' +
        '<style>' + STYLES + '</style></head><body' + (this.subdued ? ' class="detail"' : '') +
        '>' + content + '</body></html>',
        'text/html', 'UTF-8', 'https://localhost/')
    } catch (_) {
      this.failed = true
    }
  }

  build() {
    if (this.failed) {
      Text(this.source).width('100%').fontSize(15).lineHeight(24)
        .fontColor('#F1F3FA').copyOption(CopyOptions.LocalDevice)
    } else {
      Web({ src: '', controller: this.controller })
        .width('100%').backgroundColor(Color.Transparent).layoutMode(WebLayoutMode.FIT_CONTENT)
        .javaScriptAccess(false).domStorageAccess(false).fileAccess(false).blockNetwork(true)
        .onInterceptRequest(event => this.interceptImage(event?.request.getRequestUrl() ?? '') as WebResourceResponse)
        .onLoadIntercept(event => {
          const url = event?.data.getRequestUrl() ?? ''
          if (url.startsWith(IMAGE_PREFIX)) {
            const parts = url.substring(IMAGE_PREFIX.length).split('/')
            if (parts.length === 2) {
              try { this.onOpenImage(decodeURIComponent(parts[1])) } catch (_) {}
            }
            return true
          }
          return url !== 'about:blank' && url !== 'https://localhost/' && !url.startsWith('data:text/html')
        })
        .onPageEnd(() => { this.onContentReady() })
        .onControllerAttached(() => {
          this.attached = true
          this.loadContent()
        })
    }
  }
}

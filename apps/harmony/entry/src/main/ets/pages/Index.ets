import { hilog } from '@kit.PerformanceAnalysisKit';
import lokiProbe from 'liblokiprobe.so';

interface ProbeReport {
  ok: boolean;
  error?: string;
  services_id?: string;
  mobile_id?: string;
  transport?: string;
  relay?: string;
  mobile_to_services?: string;
  services_to_mobile?: string;
}

@Entry
@Component
struct Index {
  @State private running: boolean = false;
  @State private status: string = '等待连接验证';
  @State private details: string = '';

  aboutToAppear(): void {
    this.verify();
  }

  private async verify(): Promise<void> {
    if (this.running) return;
    this.running = true;
    this.status = '正在通过中继验证双向消息…';
    this.details = '';
    try {
      const raw = await lokiProbe.runProbe();
      hilog.info(0xFF00, 'LokiNetworkProbe', '%{public}s', raw);
      const report = JSON.parse(raw) as ProbeReport;
      if (report.ok) {
        this.status = '双向消息验证通过';
        this.details = `中继入口\n${report.relay}\n\n手机身份\n${report.mobile_id}\n\nservices 身份\n${report.services_id}` +
          `\n\n手机 → services\n${report.mobile_to_services}\n\nservices → 手机\n${report.services_to_mobile}`;
      } else {
        this.status = '验证失败';
        this.details = report.error ?? '未返回错误原因';
      }
    } catch (error) {
      this.status = '验证失败';
      this.details = String(error);
      const report: ProbeReport = { ok: false, error: this.details };
      hilog.error(0xFF00, 'LokiNetworkProbe', '%{public}s', JSON.stringify(report));
    } finally {
      this.running = false;
    }
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        Text('身份组网 · 连接验证').fontSize(24).fontWeight(FontWeight.Bold)
        Text(this.status).fontSize(20)
          .fontColor(this.running ? '#666666' : (this.status === '验证失败' ? '#B3261E' : '#176443'))
          .id('probe-status')
        Text('通过 iroh-relay 转发，已关闭节点间 IP 直连。').fontSize(15)
        Text(this.details).fontSize(14).width('100%').id('probe-details')
        Button(this.running ? '验证中…' : '重新验证')
          .enabled(!this.running)
          .onClick(() => { this.verify(); })
      }.width('100%').padding(24).alignItems(HorizontalAlign.Start)
    }.width('100%').height('100%')
  }
}

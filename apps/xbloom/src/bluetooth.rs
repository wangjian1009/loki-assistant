use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, ValueNotification,
    WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::{Stream, StreamExt};
use tokio::time::{sleep, timeout};
use uuid::Uuid;
use xbloom_device::{BleTransport, NOTIFY_UUID, SERVICE_UUID, WRITE_UUID};

const SCAN_TIME: Duration = Duration::from_secs(5);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Device {
    peripheral: Peripheral,
    pub name: String,
}

impl Device {
    pub fn id(&self) -> String {
        self.peripheral.id().to_string()
    }
}

pub async fn scan() -> Result<Vec<Device>> {
    let manager = timeout(OPERATION_TIMEOUT, Manager::new())
        .await
        .context("初始化蓝牙服务超时")??;
    let adapter = timeout(OPERATION_TIMEOUT, manager.adapters())
        .await
        .context("等待系统蓝牙适配器超时")??
        .into_iter()
        .next()
        .context("找不到可用的蓝牙适配器")?;
    timeout(OPERATION_TIMEOUT, adapter.start_scan(ScanFilter::default()))
        .await
        .context("启动蓝牙扫描超时")??;
    sleep(SCAN_TIME).await;
    timeout(OPERATION_TIMEOUT, adapter.stop_scan())
        .await
        .context("停止蓝牙扫描超时")??;
    let peripherals = timeout(OPERATION_TIMEOUT, adapter.peripherals())
        .await
        .context("读取蓝牙扫描结果超时")??;
    let service = Uuid::parse_str(SERVICE_UUID)?;
    let mut devices = Vec::new();
    for peripheral in peripherals {
        let Some(properties) = timeout(OPERATION_TIMEOUT, peripheral.properties())
            .await
            .context("读取蓝牙设备属性超时")??
        else {
            continue;
        };
        let name = properties.local_name.unwrap_or_default();
        if properties.services.contains(&service)
            || name.to_ascii_lowercase().starts_with("xbloom ")
        {
            devices.push(Device { peripheral, name });
        }
    }
    devices.sort_by_key(Device::id);
    Ok(devices)
}

pub struct Connection {
    peripheral: Peripheral,
    writer: Characteristic,
    notify_uuid: Uuid,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
}

impl Connection {
    pub async fn open(selector: Option<&str>) -> Result<Self> {
        let mut devices = scan().await?;
        if let Some(selector) = selector {
            devices.retain(|device| {
                device.id().eq_ignore_ascii_case(selector)
                    || device.name.eq_ignore_ascii_case(selector)
            });
        }
        let device = match devices.len() {
            0 => bail!("未发现匹配的 Studio，请确认机器已开机并释放其他蓝牙连接"),
            1 => devices.pop().unwrap(),
            _ => bail!("发现多台设备，请用 scan 查询并通过 --device 选择"),
        };
        let peripheral = device.peripheral;
        if let Err(error) = timeout(OPERATION_TIMEOUT, peripheral.connect())
            .await
            .context("蓝牙连接超时")
            .and_then(|result| result.context("蓝牙连接失败"))
        {
            let _ = timeout(OPERATION_TIMEOUT, peripheral.disconnect()).await;
            return Err(error);
        }
        let result = timeout(OPERATION_TIMEOUT, Self::configure(&peripheral))
            .await
            .context("蓝牙特征发现或订阅超时")
            .and_then(|result| result);
        match result {
            Ok((writer, notify_uuid, notifications)) => Ok(Self {
                peripheral,
                writer,
                notify_uuid,
                notifications,
            }),
            Err(error) => {
                let _ = timeout(OPERATION_TIMEOUT, peripheral.disconnect()).await;
                Err(error)
            }
        }
    }

    async fn configure(
        peripheral: &Peripheral,
    ) -> Result<(
        Characteristic,
        Uuid,
        Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
    )> {
        peripheral.discover_services().await?;
        let service = Uuid::parse_str(SERVICE_UUID)?;
        let write = Uuid::parse_str(WRITE_UUID)?;
        let notify_uuid = Uuid::parse_str(NOTIFY_UUID)?;
        let characteristics = peripheral.characteristics();
        let writer = characteristics
            .iter()
            .find(|item| item.service_uuid == service && item.uuid == write)
            .cloned()
            .context("设备缺少 Studio 写入特征 FFE1")?;
        let reader = characteristics
            .iter()
            .find(|item| item.service_uuid == service && item.uuid == notify_uuid)
            .context("设备缺少 Studio 通知特征 FFE2")?;
        let notifications = peripheral.notifications().await?;
        peripheral.subscribe(reader).await?;
        Ok((writer, notify_uuid, notifications))
    }

    pub async fn disconnect(self) -> Result<()> {
        timeout(OPERATION_TIMEOUT, self.peripheral.disconnect()).await??;
        Ok(())
    }
}

impl BleTransport for Connection {
    async fn delay(&mut self, duration: Duration) {
        sleep(duration).await;
    }

    async fn write(&mut self, frame: &[u8]) -> Result<(), String> {
        timeout(
            OPERATION_TIMEOUT,
            self.peripheral
                .write(&self.writer, frame, WriteType::WithoutResponse),
        )
        .await
        .map_err(|_| "蓝牙写入超时".to_owned())?
        .map_err(|error| error.to_string())
    }

    async fn notification(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, String> {
        let result = timeout(wait, async {
            while let Some(notification) = self.notifications.next().await {
                if notification.uuid == self.notify_uuid {
                    return Ok(Some(notification.value));
                }
            }
            Err("设备通知流已断开".to_owned())
        })
        .await;
        result.unwrap_or(Ok(None))
    }
}

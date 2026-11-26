use std::future::Future;
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;
use xbloom_recipes::Recipe;

pub const SERVICE_UUID: &str = "0000e0ff-3c17-d293-8e48-14fe2e4da212";
pub const WRITE_UUID: &str = "0000ffe1-0000-1000-8000-00805f9b34fb";
pub const NOTIFY_UUID: &str = "0000ffe2-0000-1000-8000-00805f9b34fb";

const HANDSHAKE: u16 = 8100;
const BACK_TO_HOME: u16 = 8022;
const BYPASS_AND_DOSE: u16 = 8102;
const CUP: u16 = 8104;
const COFFEE_WITH_GRIND: u16 = 8001;
const COFFEE_WITHOUT_GRIND: u16 = 8004;
const COMMIT: u16 = 8002;
const START: u16 = 40518;
const STATUS: u16 = 8023;
const CANCEL: u16 = 40519;
const IDLE: u8 = 0x01;
const ARMED: u8 = 0x1f;
const AWAITING_CONFIRM: u8 = 0x1e;
const GRINDING: u8 = 0x22;
const BREWING: u8 = 0x3b;
const NO_WATER: u8 = 0x0c;
const NO_BEANS: u8 = 0x0f;
const TIMEOUT: Duration = Duration::from_secs(12);

/// 连接、发现特征和启用通知后，将 BLE 连接交给冲煮库。
pub trait BleTransport {
    fn write(&mut self, frame: &[u8]) -> impl Future<Output = Result<(), String>> + Send;
    fn delay(&mut self, duration: Duration) -> impl Future<Output = ()> + Send;

    /// 等待下一条通知。超时返回 None，断开连接返回 Err。
    fn notification(
        &mut self,
        timeout: Duration,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("只支持 Studio 的咖啡配方")]
    UnsupportedRecipe,
    #[error("配方缺少有效字段或数值超出范围：{0}")]
    RecipeField(&'static str),
    #[error("蓝牙通信失败：{0}")]
    Bluetooth(String),
    #[error("等待设备{0}超时")]
    Timeout(&'static str),
    #[error("当前连接中尚未成功加载配方")]
    NotLoaded,
    #[error("设备报告缺水")]
    NoWater,
    #[error("设备报告缺豆")]
    NoBeans,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Coffee {
    dose: f64,
    grinder_size: Option<f64>,
    is_set_grinder_size: Option<u8>,
    rpm: Option<u8>,
    cup_type: u8,
    is_enable_bypass_water: Option<u8>,
    bypass_volume: Option<f32>,
    bypass_temp: Option<f32>,
    pour_list: Vec<Pour>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pour {
    volume: f64,
    temperature: f64,
    pattern: u8,
    pausing: u8,
    flow_rate: f64,
    is_enable_vibration_before: Option<u8>,
    is_enable_vibration_after: Option<u8>,
}

/// 已校验并编码的 Studio 咖啡配方。
pub struct StudioRecipe {
    dose: u8,
    bypass: (f32, f32),
    pours: Vec<u8>,
    grinder: bool,
}

impl TryFrom<&Recipe> for StudioRecipe {
    type Error = Error;

    fn try_from(recipe: &Recipe) -> Result<Self, Error> {
        if recipe.model() != Some(1) {
            return Err(Error::UnsupportedRecipe);
        }
        let coffee: Coffee = serde_json::from_value(serde_json::Value::Object(recipe.0.clone()))
            .map_err(|_| Error::RecipeField("dose / cupType / pourList"))?;
        if !(1..=3).contains(&coffee.cup_type) {
            return Err(Error::UnsupportedRecipe);
        }
        let dose = bounded_byte(coffee.dose, 1, 18, "dose")?;
        let grinder = match coffee.is_set_grinder_size {
            Some(1) => true,
            Some(2) => false,
            None => coffee.grinder_size.is_some_and(|size| size > 0.0),
            _ => return Err(Error::RecipeField("isSetGrinderSize")),
        };
        let grinder_size = if grinder {
            bounded_byte(
                coffee.grinder_size.unwrap_or_default(),
                1,
                80,
                "grinderSize",
            )?
        } else {
            0xfe
        };
        let rpm = coffee.rpm.unwrap_or(120);
        if rpm != 0 && !(60..=120).contains(&rpm) {
            return Err(Error::RecipeField("rpm"));
        }
        if coffee.pour_list.len() < 2 {
            return Err(Error::RecipeField("pourList"));
        }
        let bypass = match coffee.is_enable_bypass_water {
            Some(1) => {
                let volume = coffee
                    .bypass_volume
                    .filter(|v| (1.0..=240.0).contains(v))
                    .ok_or(Error::RecipeField("bypassVolume"))?;
                let temp = coffee
                    .bypass_temp
                    .filter(|t| (40.0..=95.0).contains(t))
                    .ok_or(Error::RecipeField("bypassTemp"))?;
                (volume, temp)
            }
            None | Some(2) => (0.0, 0.0),
            _ => return Err(Error::RecipeField("isEnableBypassWater")),
        };
        let mut segments = Vec::new();
        let mut total = 0_u32;
        for (index, pour) in coffee.pour_list.iter().enumerate() {
            let volume = bounded_byte(pour.volume, 1, 240, "pourList.volume")?;
            let temperature = bounded_byte(pour.temperature, 40, 95, "pourList.temperature")?;
            let flow = bounded_byte(pour.flow_rate * 10.0, 30, 35, "pourList.flowRate")?;
            let pattern = match pour.pattern {
                1 => 0,
                2 => 2,
                3 => 1,
                _ => return Err(Error::RecipeField("pourList.pattern")),
            };
            let vibration = vibration(pour.is_enable_vibration_before)?
                | (vibration(pour.is_enable_vibration_after)? << 1);
            total += u32::from(volume);
            let mut remaining = volume;
            while remaining > 127 {
                segments.extend([127, temperature, pattern, vibration]);
                remaining -= 127;
            }
            segments.extend([
                remaining,
                temperature,
                pattern,
                vibration,
                0_u8.wrapping_sub(pour.pausing),
                0,
                if index == 0 { rpm } else { 0 },
                flow,
            ]);
        }
        let segment_count =
            u8::try_from(segments.len()).map_err(|_| Error::RecipeField("pourList"))?;
        // 固件对比率下舍入敏感。旁路水独立下发，此处只累计注水段。
        let ratio = u8::try_from((total * 10).div_ceil(u32::from(dose)))
            .map_err(|_| Error::RecipeField("粉水比"))?;
        let mut pours = Vec::with_capacity(segments.len() + 3);
        pours.push(segment_count);
        pours.extend(segments);
        pours.extend([grinder_size, ratio]);
        Ok(Self {
            dose,
            bypass,
            pours,
            grinder,
        })
    }
}

fn bounded_byte(value: f64, min: u8, max: u8, field: &'static str) -> Result<u8, Error> {
    let rounded = value.round();
    if value < f64::from(min)
        || value > f64::from(max)
        || !value.is_finite()
        || (rounded - value).abs() > 1e-8
    {
        return Err(Error::RecipeField(field));
    }
    Ok(rounded as u8)
}

fn vibration(flag: Option<u8>) -> Result<u8, Error> {
    match flag {
        Some(1) => Ok(1),
        None | Some(2) => Ok(0),
        _ => Err(Error::RecipeField("pourList.isEnableVibration")),
    }
}

/// Studio 协议会话。每次操作独占连接，避免不同配方的指令交错。
pub struct Studio<T> {
    transport: T,
    frames: Notifications,
    connected: bool,
    loaded: bool,
    state: Option<u8>,
}

impl<T: BleTransport> Studio<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            frames: Notifications::default(),
            connected: false,
            loaded: false,
            state: None,
        }
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    /// 建立设备协议会话并等待机器回执。
    pub async fn connect(&mut self) -> Result<(), Error> {
        if !self.connected {
            self.send(HANDSHAKE, &encode_u32s(&[185, 1])).await?;
            self.wait_for("握手确认", |code, _| {
                code == HANDSHAKE || code == STATUS
            })
            .await?;
            self.connected = true;
            self.transport.delay(Duration::from_millis(500)).await;
        }
        Ok(())
    }

    /// 加载并等待机器报告待确认状态。用户可在机身上启动。
    pub async fn load(&mut self, recipe: &StudioRecipe) -> Result<(), Error> {
        self.loaded = false;
        self.connect().await?;
        if self.state != Some(IDLE) {
            self.wait_for("就绪", |code, payload| {
                code == STATUS && payload.first() == Some(&IDLE)
            })
            .await?;
        }
        self.send(BACK_TO_HOME, &[]).await?;
        // 实测固件需要先完成连接后的状态切换，再下发配方。
        self.transport.delay(Duration::from_secs(2)).await;
        let (volume, temperature) = recipe.bypass;
        self.send(
            BYPASS_AND_DOSE,
            &encode_u32s(&[
                volume.to_bits(),
                (temperature * 10.0).to_bits(),
                u32::from(recipe.dose),
            ]),
        )
        .await?;
        self.transport.delay(Duration::from_millis(400)).await;
        self.send(
            CUP,
            &encode_u32s(&[110.0_f32.to_bits(), 90.0_f32.to_bits()]),
        )
        .await?;
        self.transport.delay(Duration::from_millis(400)).await;
        let command = if recipe.grinder {
            COFFEE_WITH_GRIND
        } else {
            COFFEE_WITHOUT_GRIND
        };
        self.send(command, &recipe.pours).await?;
        self.wait_for("配方就绪", |code, payload| {
            code == STATUS && payload.first() == Some(&ARMED)
        })
        .await?;
        self.loaded = true;
        Ok(())
    }

    /// 显式启动当前连接加载的配方，并等待设备进入冲煮状态。
    pub async fn start(&mut self) -> Result<(), Error> {
        if !std::mem::take(&mut self.loaded) {
            return Err(Error::NotLoaded);
        }
        self.send(COMMIT, &[]).await?;
        let mut awaiting_confirmation = false;
        let settle = Instant::now() + Duration::from_secs(8);
        while let Some(state) = self.next_status(settle).await? {
            match state {
                GRINDING | BREWING => return Ok(()),
                NO_WATER => return Err(Error::NoWater),
                NO_BEANS => return Err(Error::NoBeans),
                _ => awaiting_confirmation = state == AWAITING_CONFIRM,
            }
        }
        // 部分机器会自动启动。只在设备仍处于待确认时发送下一条指令。
        if !awaiting_confirmation {
            return Err(Error::Timeout("等待启动响应"));
        }
        self.send(START, &[]).await?;
        self.wait_for_start(TIMEOUT).await
    }

    /// 等待机器进入研磨或注水状态，可用于机身确认路径。
    pub async fn wait_for_start(&mut self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        while let Some(state) = self.next_status(deadline).await? {
            match state {
                GRINDING | BREWING => {
                    self.loaded = false;
                    return Ok(());
                }
                NO_WATER => return Err(Error::NoWater),
                NO_BEANS => return Err(Error::NoBeans),
                _ => {}
            }
        }
        Err(Error::Timeout("启动冲煮"))
    }

    pub async fn cancel(&mut self) -> Result<(), Error> {
        self.loaded = false;
        self.connect().await?;
        self.send(CANCEL, &[]).await?;
        self.wait_for("取消确认", |code, _| code == CANCEL).await
    }

    async fn send(&mut self, command: u16, payload: &[u8]) -> Result<(), Error> {
        self.transport
            .write(&frame(command, payload))
            .await
            .map_err(Error::Bluetooth)
    }

    async fn wait_for(
        &mut self,
        action: &'static str,
        predicate: impl Fn(u16, &[u8]) -> bool + Send,
    ) -> Result<(), Error> {
        let until = Instant::now() + TIMEOUT;
        while let Some((command, payload)) = self.next_frame(until).await? {
            if predicate(command, &payload) {
                return Ok(());
            }
        }
        Err(Error::Timeout(action))
    }

    async fn next_status(&mut self, until: Instant) -> Result<Option<u8>, Error> {
        while let Some((command, payload)) = self.next_frame(until).await? {
            if command == STATUS
                && let Some(&status) = payload.first()
            {
                return Ok(Some(status));
            }
        }
        Ok(None)
    }

    async fn next_frame(&mut self, until: Instant) -> Result<Option<(u16, Vec<u8>)>, Error> {
        loop {
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if let Some(frame) = self.frames.next() {
                if frame.0 == STATUS {
                    self.state = frame.1.first().copied();
                }
                return Ok(Some(frame));
            }
            let Some(data) = self
                .transport
                .notification(remaining)
                .await
                .map_err(Error::Bluetooth)?
            else {
                return Ok(None);
            };
            self.frames.0.extend(data);
        }
    }
}

fn encode_u32s(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn frame(command: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len() + 12);
    bytes.extend([0x58, 0x01, 0x01]);
    bytes.extend(command.to_le_bytes());
    bytes.extend(((payload.len() + 12) as u32).to_le_bytes());
    bytes.push(0x01);
    bytes.extend(payload);
    bytes.extend(crc(&bytes).to_le_bytes());
    bytes
}

fn crc(bytes: &[u8]) -> u16 {
    let mut crc = 0_u16;
    for byte in bytes {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x8408 } else { 0 };
        }
    }
    crc
}

#[derive(Default)]
struct Notifications(Vec<u8>);

impl Notifications {
    fn next(&mut self) -> Option<(u16, Vec<u8>)> {
        loop {
            let Some(start) = self.0.iter().position(|&byte| byte == 0x58) else {
                self.0.clear();
                return None;
            };
            self.0.drain(..start);
            if self.0.len() < 12 {
                return None;
            }
            if self.0[1..3] != [0x02, 0x07] {
                self.0.remove(0);
                continue;
            }
            let length = u32::from_le_bytes(self.0[5..9].try_into().unwrap()) as usize;
            if !(12..=4096).contains(&length) {
                self.0.remove(0);
                continue;
            }
            if self.0.len() < length {
                return None;
            }
            if self.0[length - 2..length] != crc(&self.0[..length - 2]).to_le_bytes() {
                self.0.remove(0);
                continue;
            }
            let command = u16::from_le_bytes([self.0[3], self.0[4]]);
            let payload = self.0[10..length - 2].to_vec();
            self.0.drain(..length);
            return Some((command, payload));
        }
    }
}

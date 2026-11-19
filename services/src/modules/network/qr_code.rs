use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{Color, EcLevel, QrCode};

use super::protocol::{NetworkError, QrPayload};

/// 生成带四个 module 白边的 PNG；CPU 编码不占用异步执行线程。
pub(super) async fn render(payload: &QrPayload) -> Result<String, NetworkError> {
    let data = serde_json::to_vec(payload).map_err(|_| NetworkError::QrCode)?;
    tokio::task::spawn_blocking(move || {
        let code = QrCode::with_error_correction_level(data, EcLevel::M)
            .map_err(|_| NetworkError::QrCode)?;
        let scale = 6;
        let width = (code.width() + 8) * scale;
        let mut pixels = vec![255u8; width * width];
        for y in 0..code.width() {
            for x in 0..code.width() {
                if code[(x, y)] == Color::Dark {
                    for row in (y + 4) * scale..(y + 5) * scale {
                        pixels[row * width + (x + 4) * scale..row * width + (x + 5) * scale]
                            .fill(0);
                    }
                }
            }
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width as u32, width as u32);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|_| NetworkError::QrCode)?;
            writer
                .write_image_data(&pixels)
                .map_err(|_| NetworkError::QrCode)?;
            writer.finish().map_err(|_| NetworkError::QrCode)?;
        }
        Ok(STANDARD.encode(bytes))
    })
    .await
    .map_err(|_| NetworkError::QrCode)?
}

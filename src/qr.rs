use eframe::egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use qrcode::QrCode;
use std::fs::File;
use std::io::Read;

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Build the random service name and password used by the Android wireless debugging QR flow.
pub fn credentials() -> (String, String) {
    let mut bytes = [0u8; 64];
    let filled = File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();

    if !filled {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut state = seed | 1;
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
    }

    let token = |len: usize| -> String {
        bytes
            .iter()
            .take(len)
            .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
            .collect()
    };

    (format!("ADB_WIFI_{}", token(10)), token(12))
}

pub fn payload(name: &str, password: &str) -> String {
    format!("WIFI:T:ADB;S:{name};P:{password};;")
}

/// Render the payload into a nearest-neighbour texture so the QR stays crisp at any size.
pub fn texture(ctx: &Context, name: &str, payload: &str) -> Result<TextureHandle, String> {
    let code = QrCode::new(payload.as_bytes()).map_err(|e| format!("QR code error: {e}"))?;
    let width = code.width();
    let quiet = 4usize;
    let size = width + quiet * 2;
    let colors = code.to_colors();

    let mut pixels = vec![Color32::WHITE; size * size];
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                pixels[(y + quiet) * size + x + quiet] = Color32::BLACK;
            }
        }
    }

    let image = ColorImage::new([size, size], pixels);
    Ok(ctx.load_texture(name, image, TextureOptions::NEAREST))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_uses_android_adb_qr_format() {
        assert_eq!(
            payload("ADB_WIFI_test", "secret"),
            "WIFI:T:ADB;S:ADB_WIFI_test;P:secret;;"
        );
    }

    #[test]
    fn credentials_have_expected_lengths_and_are_alphanumeric() {
        let (name, password) = credentials();
        assert!(name.starts_with("ADB_WIFI_"));
        assert_eq!(name.len(), "ADB_WIFI_".len() + 10);
        assert_eq!(password.len(), 12);
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        assert!(password.bytes().all(|b| b.is_ascii_alphanumeric()));
    }
}

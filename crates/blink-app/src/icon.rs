//! The blink icon, drawn in code so the binary carries no image file or decoder.
//!
//! The tray draws it at runtime. `assets/blink.ico`, which `build.rs` embeds
//! for Explorer and the installer, holds the same drawing at several sizes; a
//! test fails when it is stale, and `just icon` regenerates it.

/// Width and height of the tray icon in pixels.
pub(crate) const ICON_SIZE: u32 = 32;

/// Fill color of the record dot.
const RED: [u8; 3] = [0xE5, 0x39, 0x35];

/// A red record dot on a transparent background, `size` pixels square, as
/// row-major RGBA. The dot spans three quarters of the icon, and its edge fades
/// to transparent over one pixel.
pub(crate) fn rgba(size: u32) -> Vec<u8> {
    let size = i64::from(size);
    // Radii in half pixels: fully opaque inside `inner`, transparent from `outer`.
    let radius = 3 * size / 4;
    let (inner, outer) = ((radius - 1).pow(2), (radius + 1).pow(2));
    let mut pixels = Vec::with_capacity(usize::try_from(size * size * 4).unwrap_or(0));
    for y in 0..size {
        for x in 0..size {
            // Distance from the center in half pixels, measured at the pixel center.
            let (dx, dy) = (2 * x + 1 - size, 2 * y + 1 - size);
            let distance = dx * dx + dy * dy;
            let alpha = if distance <= inner {
                255
            } else if distance >= outer {
                0
            } else {
                255 * (outer - distance) / (outer - inner)
            };
            let [r, g, b] = RED;
            pixels.extend([r, g, b, u8::try_from(alpha).unwrap_or(u8::MAX)]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizes in `assets/blink.ico`: the Windows shell's standard icon sizes.
    const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

    const ICO_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/blink.ico");

    fn alpha(pixels: &[u8], x: u32, y: u32) -> u8 {
        let index = usize::try_from((y * ICON_SIZE + x) * 4 + 3).unwrap();
        pixels[index]
    }

    /// An ICO file holding the drawing at each of `ICO_SIZES` as a PNG image.
    fn ico() -> Vec<u8> {
        let images: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|&size| (size, png(size))).collect();
        let count = u16::try_from(images.len()).unwrap();
        let header_len = 6 + 16 * u32::from(count);

        let mut file = Vec::new();
        // ICONDIR: reserved, type 1 (icon), image count.
        file.extend(0_u16.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(count.to_le_bytes());
        let mut offset = header_len;
        for (size, data) in &images {
            let len = u32::try_from(data.len()).unwrap();
            // ICONDIRENTRY: a width and height of 0 mean 256.
            let side = u8::try_from(*size).unwrap_or(0);
            file.extend([side, side, 0, 0]);
            file.extend(1_u16.to_le_bytes());
            file.extend(32_u16.to_le_bytes());
            file.extend(len.to_le_bytes());
            file.extend(offset.to_le_bytes());
            offset += len;
        }
        for (_, data) in images {
            file.extend(data);
        }
        file
    }

    fn png(size: u32) -> Vec<u8> {
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgba(size)).unwrap();
        writer.finish().unwrap();
        data
    }

    #[test]
    fn is_a_dot_on_a_transparent_square() {
        let pixels = rgba(ICON_SIZE);
        let side = usize::try_from(ICON_SIZE).unwrap();
        assert_eq!(pixels.len(), side * side * 4);
        assert_eq!(alpha(&pixels, 16, 16), 255);
        assert_eq!(alpha(&pixels, 0, 0), 0);
        assert_eq!(alpha(&pixels, 31, 31), 0);
        assert_eq!(alpha(&pixels, 16, 0), 0);
        let edge = alpha(&pixels, 4, 16);
        assert!(edge > 0 && edge < 255, "antialiased edge, got {edge}");
    }

    #[test]
    fn ico_is_current() {
        let expected = ico();
        if std::env::var_os("BLINK_WRITE_ICON").is_some() {
            std::fs::write(ICO_PATH, &expected).unwrap();
        }
        let actual = std::fs::read(ICO_PATH).unwrap();
        assert!(
            actual == expected,
            "crates/blink-app/assets/blink.ico is out of date; run `just icon`"
        );
    }
}

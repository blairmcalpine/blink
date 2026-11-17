//! The tray icon, drawn in code so the binary carries no image file or decoder.

/// Width and height of the icon in pixels.
pub(crate) const ICON_SIZE: u32 = 32;

/// Fill color of the record dot.
const RED: [u8; 3] = [0xE5, 0x39, 0x35];

/// Radii of the dot in half pixels: fully opaque inside `INNER`, fading to
/// transparent at `OUTER` for a smooth edge.
const INNER: i64 = 23;
const OUTER: i64 = 25;

/// A red record dot on a transparent background, as row-major RGBA.
pub(crate) fn rgba() -> Vec<u8> {
    let size = i64::from(ICON_SIZE);
    let (inner, outer) = (INNER * INNER, OUTER * OUTER);
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

    fn alpha(pixels: &[u8], x: u32, y: u32) -> u8 {
        let index = usize::try_from((y * ICON_SIZE + x) * 4 + 3).unwrap();
        pixels[index]
    }

    #[test]
    fn is_a_dot_on_a_transparent_square() {
        let pixels = rgba();
        let side = usize::try_from(ICON_SIZE).unwrap();
        assert_eq!(pixels.len(), side * side * 4);
        assert_eq!(alpha(&pixels, 16, 16), 255);
        assert_eq!(alpha(&pixels, 0, 0), 0);
        assert_eq!(alpha(&pixels, 31, 31), 0);
        assert_eq!(alpha(&pixels, 16, 0), 0);
        let edge = alpha(&pixels, 4, 16);
        assert!(edge > 0 && edge < 255, "antialiased edge, got {edge}");
    }
}

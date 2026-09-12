//! Converting a core's framebuffer to tightly packed BGRA8, the layout the
//! render module uploads. Pure; the video-refresh callback calls it.

use super::sys::retro_pixel_format;
use std::ffi::c_uint;

/// The framebuffer formats a core may select with `SET_PIXEL_FORMAT`, the
/// ones [`to_bgra`] reads. libretro.h's enum also has an UNKNOWN sentinel,
/// which is refused at selection, so it never reaches here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// `RETRO_PIXEL_FORMAT_0RGB1555`, libretro's default when a core sets none.
    Xrgb1555,
    /// `RETRO_PIXEL_FORMAT_XRGB8888`.
    Xrgb8888,
    /// `RETRO_PIXEL_FORMAT_RGB565`.
    Rgb565,
}

impl PixelFormat {
    /// The format libretro.h numbers `raw`, or `None` for one this tool
    /// cannot read (including `RETRO_PIXEL_FORMAT_UNKNOWN`).
    pub fn from_raw(raw: c_uint) -> Option<PixelFormat> {
        const XRGB1555: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_0RGB1555 as c_uint;
        const XRGB8888: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_XRGB8888 as c_uint;
        const RGB565: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_RGB565 as c_uint;
        match raw {
            XRGB1555 => Some(PixelFormat::Xrgb1555),
            XRGB8888 => Some(PixelFormat::Xrgb8888),
            RGB565 => Some(PixelFormat::Rgb565),
            _ => None,
        }
    }
}

fn expand5(v: u16) -> u8 {
    let v = (v & 0x1f) as u8;
    v << 3 | v >> 2
}

fn expand6(v: u16) -> u8 {
    let v = (v & 0x3f) as u8;
    v << 2 | v >> 4
}

/// `width * height * 4` bytes of BGRA8 from `height` rows of `pitch` bytes.
/// XRGB8888 rows are copied with the X byte forced opaque; the 16-bit
/// formats expand each channel by bit replication.
///
/// # Panics
///
/// If `data` is shorter than `(height - 1) * pitch + width * bpp` bytes or
/// `width * bpp` exceeds `pitch`; the video callback checks both first.
pub fn to_bgra(format: PixelFormat, data: &[u8], width: u32, height: u32, pitch: usize) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * pitch..];
        match format {
            PixelFormat::Xrgb8888 => {
                for px in row[..w * 4].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
                }
            }
            PixelFormat::Rgb565 => {
                for px in row[..w * 2].as_chunks::<2>().0 {
                    let v = u16::from_le_bytes(*px);
                    out.extend_from_slice(&[expand5(v), expand6(v >> 5), expand5(v >> 11), 0xff]);
                }
            }
            PixelFormat::Xrgb1555 => {
                for px in row[..w * 2].as_chunks::<2>().0 {
                    let v = u16::from_le_bytes(*px);
                    out.extend_from_slice(&[expand5(v), expand5(v >> 5), expand5(v >> 10), 0xff]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrgb8888_copies_bgr_and_forces_alpha() {
        // Two rows of one pixel, pitch 8 (4 bytes of padding per row).
        let data = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        assert_eq!(
            to_bgra(PixelFormat::Xrgb8888, &data, 1, 2, 8),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn rgb565_expands_channels() {
        // Pure red 0xF800, pure green 0x07E0, pure blue 0x001F.
        let data = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00];
        assert_eq!(
            to_bgra(PixelFormat::Rgb565, &data, 3, 1, 6),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn argb1555_expands_channels() {
        // Pure red 0x7C00, pure green 0x03E0, pure blue 0x001F.
        let data = [0x00, 0x7C, 0xE0, 0x03, 0x1F, 0x00];
        assert_eq!(
            to_bgra(PixelFormat::Xrgb1555, &data, 3, 1, 6),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }
}

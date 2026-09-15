//! Converting a core's framebuffer to a [`Frame`] the render module
//! uploads: BGRA8 for the 16-bit and 8-bit formats, a straight row copy
//! for the packed 10-bit ones. Pure; the video-refresh callback calls it.

use super::sys::retro_pixel_format;
use crate::config::Size;
use crate::hosted::render::{Frame, FrameFormat};
use std::ffi::c_uint;

/// The framebuffer formats a core may select with `SET_PIXEL_FORMAT`, the
/// ones [`to_frame`] reads. libretro.h's enum also has an UNKNOWN
/// sentinel, which is refused at selection, so it never reaches here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// `RETRO_PIXEL_FORMAT_0RGB1555`, libretro's default when a core sets none.
    Xrgb1555,
    /// `RETRO_PIXEL_FORMAT_XRGB8888`.
    Xrgb8888,
    /// `RETRO_PIXEL_FORMAT_RGB565`.
    Rgb565,
    /// `RETRO_PIXEL_FORMAT_XRGB2101010`: 10-bit SDR, packed 2:10:10:10
    /// with red in bits 29:20.
    Xrgb2101010,
    /// `RETRO_PIXEL_FORMAT_HDR10_2101010`: the same packing carrying
    /// PQ-encoded Rec.2020 samples. Accepted only under `--hdr hdr10`
    /// (the environment callback gates it); this module treats the bytes
    /// exactly as `Xrgb2101010`, since only their interpretation differs.
    Hdr10,
}

impl PixelFormat {
    /// The format libretro.h numbers `raw`, or `None` for one this tool
    /// cannot read (including `RETRO_PIXEL_FORMAT_UNKNOWN`).
    pub fn from_raw(raw: c_uint) -> Option<PixelFormat> {
        const XRGB1555: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_0RGB1555 as c_uint;
        const XRGB8888: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_XRGB8888 as c_uint;
        const RGB565: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_RGB565 as c_uint;
        const XRGB2101010: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_XRGB2101010 as c_uint;
        const HDR10: c_uint = retro_pixel_format::RETRO_PIXEL_FORMAT_HDR10_2101010 as c_uint;
        match raw {
            XRGB1555 => Some(PixelFormat::Xrgb1555),
            XRGB8888 => Some(PixelFormat::Xrgb8888),
            RGB565 => Some(PixelFormat::Rgb565),
            XRGB2101010 => Some(PixelFormat::Xrgb2101010),
            HDR10 => Some(PixelFormat::Hdr10),
            _ => None,
        }
    }

    /// Bytes per pixel in a core's framebuffer.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Xrgb8888 | PixelFormat::Xrgb2101010 | PixelFormat::Hdr10 => 4,
            PixelFormat::Rgb565 | PixelFormat::Xrgb1555 => 2,
        }
    }

    /// The layout [`to_frame`] delivers this format in.
    pub fn frame_format(self) -> FrameFormat {
        match self {
            PixelFormat::Xrgb2101010 | PixelFormat::Hdr10 => FrameFormat::Bgr10a2,
            PixelFormat::Xrgb8888 | PixelFormat::Rgb565 | PixelFormat::Xrgb1555 => {
                FrameFormat::Bgra8
            }
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

/// A frame of `width * height * 4` bytes from `height` rows of `pitch`
/// bytes. XRGB8888 rows are copied with the X byte forced opaque; the
/// 16-bit formats expand each channel by bit replication; the 10-bit
/// formats are copied as they are with the two high bits forced to 1,
/// so alpha samples as 1.0, the treatment the X byte gets.
///
/// # Panics
///
/// If `data` is shorter than `(height - 1) * pitch + width * bpp` bytes or
/// `width * bpp` exceeds `pitch`; the video callback checks both first.
pub fn to_frame(format: PixelFormat, data: &[u8], width: u32, height: u32, pitch: usize) -> Frame {
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
            PixelFormat::Xrgb2101010 | PixelFormat::Hdr10 => {
                for px in row[..w * 4].as_chunks::<4>().0 {
                    let v = u32::from_le_bytes(*px) | 0xC000_0000;
                    out.extend_from_slice(&v.to_le_bytes());
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
    Frame {
        size: Size { width, height },
        format: format.frame_format(),
        pixels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_maps_libretros_five_and_refuses_the_rest() {
        assert_eq!(PixelFormat::from_raw(0), Some(PixelFormat::Xrgb1555));
        assert_eq!(PixelFormat::from_raw(1), Some(PixelFormat::Xrgb8888));
        assert_eq!(PixelFormat::from_raw(2), Some(PixelFormat::Rgb565));
        assert_eq!(PixelFormat::from_raw(3), Some(PixelFormat::Xrgb2101010));
        assert_eq!(PixelFormat::from_raw(4), Some(PixelFormat::Hdr10));
        assert_eq!(PixelFormat::from_raw(5), None);
        // RETRO_PIXEL_FORMAT_UNKNOWN is INT_MAX.
        assert_eq!(PixelFormat::from_raw(2147483647), None);
    }

    #[test]
    fn bytes_per_pixel_and_frame_format_per_format() {
        for (format, bytes, layout) in [
            (PixelFormat::Xrgb1555, 2, FrameFormat::Bgra8),
            (PixelFormat::Rgb565, 2, FrameFormat::Bgra8),
            (PixelFormat::Xrgb8888, 4, FrameFormat::Bgra8),
            (PixelFormat::Xrgb2101010, 4, FrameFormat::Bgr10a2),
            (PixelFormat::Hdr10, 4, FrameFormat::Bgr10a2),
        ] {
            assert_eq!(format.bytes_per_pixel(), bytes, "{format:?}");
            assert_eq!(format.frame_format(), layout, "{format:?}");
        }
    }

    #[test]
    fn xrgb8888_copies_bgr_and_forces_alpha() {
        // Two rows of one pixel, pitch 8 (4 bytes of padding per row).
        let data = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        let frame = to_frame(PixelFormat::Xrgb8888, &data, 1, 2, 8);
        assert_eq!(
            frame.size,
            Size {
                width: 1,
                height: 2
            }
        );
        assert_eq!(frame.format, FrameFormat::Bgra8);
        assert_eq!(frame.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn rgb565_expands_channels() {
        // Pure red 0xF800, pure green 0x07E0, pure blue 0x001F.
        let data = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00];
        assert_eq!(
            to_frame(PixelFormat::Rgb565, &data, 3, 1, 6).pixels,
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn argb1555_expands_channels() {
        // Pure red 0x7C00, pure green 0x03E0, pure blue 0x001F.
        let data = [0x00, 0x7C, 0xE0, 0x03, 0x1F, 0x00];
        assert_eq!(
            to_frame(PixelFormat::Xrgb1555, &data, 3, 1, 6).pixels,
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn ten_bit_formats_copy_rows_and_force_the_high_bits() {
        // 2x1, pitch 12: pure red (bits 29:20) then pure blue (bits 9:0),
        // little-endian, then 4 bytes of padding.
        let data = [0x00, 0x00, 0xF0, 0x3F, 0xFF, 0x03, 0x00, 0x00, 9, 9, 9, 9];
        for format in [PixelFormat::Xrgb2101010, PixelFormat::Hdr10] {
            let frame = to_frame(format, &data, 2, 1, 12);
            assert_eq!(frame.format, FrameFormat::Bgr10a2, "{format:?}");
            assert_eq!(
                frame.pixels,
                [0x00, 0x00, 0xF0, 0xFF, 0xFF, 0x03, 0x00, 0xC0],
                "{format:?}"
            );
        }
    }
}

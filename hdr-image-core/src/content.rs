//! Decoding a content file and encoding it for the format the frontend
//! accepted, through the main crate's `hdr` module. Pure Rust, no unsafe.

use ra_metal_capture::config::Hdr;
use ra_metal_capture::hdr::{
    encode_hdr10, encode_sdr_8, encode_sdr_10, sdr_to_linear, srgb_encode,
};
use std::path::Path;

/// The formats the core asks for, in the order it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `RETRO_PIXEL_FORMAT_HDR10_2101010`: PQ Rec.2020, 10 bits.
    Hdr10,
    /// `RETRO_PIXEL_FORMAT_XRGB2101010`: sRGB, 10 bits.
    Xrgb2101010,
    /// `RETRO_PIXEL_FORMAT_XRGB8888`: sRGB, 8 bits.
    Xrgb8888,
}

/// A decoded image. SDR content keeps its nonlinear sRGB values in 0..1
/// so an 8-bit file comes out of `XRGB8888` unchanged; HDR content is
/// linear radiance with 1.0 meaning paper white.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Sdr {
        width: u32,
        height: u32,
        srgb: Vec<[f32; 3]>,
    },
    Hdr {
        width: u32,
        height: u32,
        linear: Vec<[f32; 3]>,
    },
}

impl Content {
    /// Decode the file at `path`: a Radiance image (which the `image`
    /// crate decodes to 32-bit floats) is HDR content, anything else SDR.
    pub fn open(path: &Path) -> Result<Content, String> {
        let img = image::open(path).map_err(|e| format!("decoding {}: {e}", path.display()))?;
        let (width, height) = (img.width(), img.height());
        Ok(match img {
            image::DynamicImage::ImageRgb32F(f) => Content::Hdr {
                width,
                height,
                linear: f.pixels().map(|p| p.0).collect(),
            },
            other => Content::Sdr {
                width,
                height,
                srgb: other
                    .into_rgb8()
                    .pixels()
                    .map(|p| p.0.map(|c| f32::from(c) / 255.0))
                    .collect(),
            },
        })
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            Content::Sdr { width, height, .. } | Content::Hdr { width, height, .. } => {
                (*width, *height)
            }
        }
    }
}

/// The whole image as `format`, one `u32` per pixel, rows top first.
pub fn encode(content: &Content, format: Format, hdr: &Hdr) -> Vec<u32> {
    match (content, format) {
        (Content::Sdr { srgb, .. }, Format::Xrgb8888) => {
            srgb.iter().map(|p| encode_sdr_8(*p)).collect()
        }
        (Content::Sdr { srgb, .. }, Format::Xrgb2101010) => {
            srgb.iter().map(|p| encode_sdr_10(*p)).collect()
        }
        (Content::Sdr { srgb, .. }, Format::Hdr10) => srgb
            .iter()
            .map(|p| encode_hdr10(p.map(sdr_to_linear), hdr))
            .collect(),
        (Content::Hdr { linear, .. }, Format::Hdr10) => {
            linear.iter().map(|p| encode_hdr10(*p, hdr)).collect()
        }
        (Content::Hdr { linear, .. }, Format::Xrgb2101010) => linear
            .iter()
            .map(|p| encode_sdr_10(p.map(srgb_encode)))
            .collect(),
        (Content::Hdr { linear, .. }, Format::Xrgb8888) => linear
            .iter()
            .map(|p| encode_sdr_8(p.map(srgb_encode)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_metal_capture::hdr::pack_2101010;

    #[test]
    fn content_open_reads_the_fixtures() {
        let png = Content::open(Path::new("../fixtures/sample.png")).unwrap();
        assert_eq!(png.size(), (160, 144));
        assert!(matches!(png, Content::Sdr { .. }));

        let hdr = Content::open(Path::new("../fixtures/probe.hdr")).unwrap();
        assert_eq!(hdr.size(), (4, 4));
        let Content::Hdr { linear, .. } = &hdr else {
            panic!("a Radiance file is HDR content");
        };
        // Radiance stores a mantissa and a power of two, so these are exact.
        assert_eq!(linear[0], [1.0, 1.0, 1.0]);
        assert_eq!(linear[1], [4.0, 4.0, 4.0]);
        assert_eq!(linear[2], [0.0, 0.0, 0.0]);
        assert_eq!(linear[4], [1.0, 0.0, 0.0]);
        assert_eq!(linear[6], [0.0, 0.0, 1.0]);
        assert_eq!(linear[8], [0.25, 0.25, 0.25]);

        assert!(Content::open(Path::new("../Cargo.toml")).is_err());

        let scene = Content::open(Path::new("../fixtures/sample.hdr")).unwrap();
        assert_eq!(scene.size(), (160, 144));
        let Content::Hdr { linear, .. } = &scene else {
            panic!("the scene is HDR content");
        };
        // The sun's centre: sRGB (255, 239, 49) linearised, times 8.
        let sun = linear[13 * 160 + 140];
        assert_eq!(sun[0], 8.0);
        assert!(sun[1] > 6.0 && sun[1] < 7.5, "{}", sun[1]);
        assert!(sun[2] < 0.5, "{}", sun[2]);
    }

    #[test]
    fn sdr_content_round_trips_under_the_sdr_formats() {
        let c = Content::Sdr {
            width: 2,
            height: 1,
            srgb: vec![[1.0, 0.0, 0.0], [18.0 / 255.0, 52.0 / 255.0, 86.0 / 255.0]],
        };
        let s = Hdr::default();
        assert_eq!(encode(&c, Format::Xrgb8888, &s), [0x00FF_0000, 0x0012_3456]);
        assert_eq!(
            encode(&c, Format::Xrgb2101010, &s),
            [0x3FF0_0000, pack_2101010(72, 209, 345)]
        );
        // Under HDR10 the same pixels are linearised and PQ-encoded, so
        // red stays red-dominant and the dark pixel stays dark.
        let hdr = encode(&c, Format::Hdr10, &s);
        let (red, blue) = (hdr[0] >> 20, hdr[0] & 0x3FF);
        assert!(red > 0 && blue < red, "{red} {blue}");
        assert!(hdr[1] >> 20 < red);
    }

    #[test]
    fn hdr_content_under_sdr_formats_clamps_and_srgb_encodes() {
        let c = Content::Hdr {
            width: 4,
            height: 1,
            linear: vec![
                [1.0, 1.0, 1.0],
                [4.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [0.5, 0.5, 0.5],
            ],
        };
        let s = Hdr::default();
        assert_eq!(
            encode(&c, Format::Xrgb8888, &s),
            [0x00FF_FFFF, 0x00FF_0000, 0, 0x00BC_BCBC]
        );
        assert_eq!(
            encode(&c, Format::Xrgb2101010, &s)[0..3],
            [0x3FFF_FFFF, 0x3FF0_0000, 0]
        );
        let hdr = encode(&c, Format::Hdr10, &s);
        let (white, highlight) = (hdr[0] >> 20, hdr[1] >> 20);
        assert!(
            highlight > white,
            "the 4x highlight is brighter than white: {highlight} vs {white}"
        );
        assert_eq!(hdr[2], 0);
    }
}

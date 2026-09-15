//! The encoding both HDR sources share: image mode's Radiance files
//! and the `hdr-image-core` crate. It mirrors RetroArch's HDR shader
//! (`gfx/drivers/vulkan_shaders/hdr_common.glsl` and `hdr.frag`) so a
//! trace of either source under HDR10 matches what RetroArch's own HDR
//! path produces for the same SDR image. Pure; no `image` dependency,
//! no feature gate, no unsafe.

// The shader's constants carry more digits than f32 holds; they are kept as written there.
#![allow(clippy::excessive_precision)]

use crate::config::{Gamut, Hdr};

// ST.2084's constants, as RetroArch's `LinearToST2084` writes them.
const PQ_M1: f32 = 0.1593017578;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

/// The PQ signal in 0..1 for `nits`: the ST.2084 curve over 0..10000
/// nits, clamped at both ends.
pub fn pq_encode(nits: f32) -> f32 {
    let y = (nits / 10000.0).clamp(0.0, 1.0);
    let ym = y.powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym)).powf(PQ_M2)
}

// hdr_common.glsl's matrices with the rows as written there. GLSL
// multiplies a row vector by the column-major literal, which is this
// row-major product.
const K709_TO_2020: [[f32; 3]; 3] = [
    [0.6274040, 0.3292820, 0.0433136],
    [0.0690970, 0.9195400, 0.0113612],
    [0.0163916, 0.0880132, 0.8955950],
];
const K_EXPANDED709_TO_2020: [[f32; 3]; 3] = [
    [0.6274040, 0.3292820, 0.0433136],
    [0.0457456, 0.941777, 0.0124772],
    [-0.00121055, 0.0176041, 0.983607],
];
const K_P3_TO_2020: [[f32; 3]; 3] = [
    [0.753833, 0.198597, 0.047570],
    [0.045744, 0.941777, 0.012479],
    [-0.001210, 0.017602, 0.983609],
];

/// Linear Rec.709 into the Rec.2020 container for RetroArch's gamut
/// setting (`To2020` in hdr_common.glsl). `Super` rotates nothing.
/// Negative results clamp to 0.
pub fn to_2020(rgb: [f32; 3], gamut: Gamut) -> [f32; 3] {
    let m: [[f32; 3]; 3] = match gamut {
        Gamut::Accurate => K709_TO_2020,
        Gamut::Expanded => K_EXPANDED709_TO_2020,
        Gamut::Wide => K_P3_TO_2020,
        Gamut::Super => return rgb.map(|v| v.max(0.0)),
    };
    m.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).max(0.0))
}

/// RetroArch's linearisation of SDR content (`Sample` in hdr.frag): a
/// plain 2.4 power, not the sRGB curve.
pub fn sdr_to_linear(v: f32) -> f32 {
    v.abs().powf(2.4)
}

/// sRGB's nonlinear encoding of a linear value, clamped to 0..1.
pub fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Three 10-bit channels as libretro's XRGB2101010: R in bits 29:20, G
/// in 19:10, B in 9:0, the top two bits clear.
pub fn pack_2101010(r: u16, g: u16, b: u16) -> u32 {
    ((u32::from(r) & 0x3FF) << 20) | ((u32::from(g) & 0x3FF) << 10) | (u32::from(b) & 0x3FF)
}

/// Three 8-bit channels as libretro's XRGB8888, the X byte clear.
pub fn pack_8888(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// `v` clamped to 0..1, scaled to `max`, rounded to the nearest code.
pub fn quantise(v: f32, max: f32) -> u32 {
    (v.clamp(0.0, 1.0) * max).round() as u32
}

/// Roll a linear pixel (1.0 at paper white) off toward the peak, given
/// as a ratio to paper white: values up to paper white are untouched,
/// and the excess above it is compressed so the brightest channel
/// approaches the peak without reaching it, every channel scaled by the
/// same factor so hue holds. libretro.h calls the peak "a ceiling to
/// roll off toward rather than a level to target". RetroArch's own HDR10
/// path applies no roll-off (its shader's `Tonemap` also darkens paper
/// white, so the shader never uses it there); this knee is this tool's
/// choice, not the shader's. A peak at or below paper white is zero
/// headroom: everything above paper white lands exactly at it.
pub fn roll_off(rgb: [f32; 3], peak_ratio: f32) -> [f32; 3] {
    let brightest = rgb[0].max(rgb[1]).max(rgb[2]);
    if brightest <= 1.0 {
        return rgb;
    }
    let headroom = (peak_ratio - 1.0).max(0.0);
    let excess = brightest - 1.0;
    let rolled = 1.0 + headroom * excess / (excess + headroom);
    rgb.map(|c| c * rolled / brightest)
}

/// One HDR10 pixel from linear Rec.709 with 1.0 at paper white: rolled
/// off toward the peak ([`roll_off`]), rotated per the gamut setting,
/// scaled to nits, PQ-encoded, and quantised to 10 bits.
pub fn encode_hdr10(linear: [f32; 3], hdr: &Hdr) -> u32 {
    let peak_ratio = if hdr.paper_white_nits > 0.0 {
        hdr.max_nits / hdr.paper_white_nits
    } else {
        1.0
    };
    let [r, g, b] = to_2020(roll_off(linear, peak_ratio), hdr.expand_gamut)
        .map(|v| quantise(pq_encode(v * hdr.paper_white_nits), 1023.0) as u16);
    pack_2101010(r, g, b)
}

/// One XRGB2101010 pixel from nonlinear sRGB values in 0..1.
pub fn encode_sdr_10(srgb: [f32; 3]) -> u32 {
    let [r, g, b] = srgb.map(|c| quantise(c, 1023.0) as u16);
    pack_2101010(r, g, b)
}

/// One XRGB8888 pixel from nonlinear sRGB values in 0..1.
pub fn encode_sdr_8(srgb: [f32; 3]) -> u32 {
    let [r, g, b] = srgb.map(|c| quantise(c, 255.0) as u8);
    pack_8888(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HdrMode;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn pq_reference_code_values() {
        // ST.2084 at 10 bits: black, 100 nits, 1000 nits (the HDR10
        // reference peak), 10000 nits (the encoding's ceiling).
        let code = |nits| quantise(pq_encode(nits), 1023.0);
        assert_eq!(code(0.0), 0);
        assert_eq!(code(100.0), 520);
        assert_eq!(code(1000.0), 769);
        assert_eq!(code(10000.0), 1023);
        assert_eq!(code(20000.0), 1023, "clamped, not wrapped");
    }

    #[test]
    fn packing_puts_red_in_the_high_bits() {
        assert_eq!(pack_2101010(0x3FF, 0, 0), 0x3FF0_0000);
        assert_eq!(pack_2101010(0, 0x3FF, 0), 0x000F_FC00);
        assert_eq!(pack_2101010(0, 0, 0x3FF), 0x0000_03FF);
        assert_eq!(pack_8888(0xFF, 0, 0), 0x00FF_0000);
        assert_eq!(pack_8888(0x12, 0x34, 0x56), 0x0012_3456);
    }

    #[test]
    fn gamut_matrices_on_pure_red() {
        let red = [1.0, 0.0, 0.0];
        assert!(near(
            to_2020(red, Gamut::Accurate),
            [0.6274040, 0.0690970, 0.0163916]
        ));
        // The expanded and P3 rows give a negative blue, clamped to 0.
        assert!(near(
            to_2020(red, Gamut::Expanded),
            [0.6274040, 0.0457456, 0.0]
        ));
        assert!(near(to_2020(red, Gamut::Wide), [0.753833, 0.045744, 0.0]));
        assert_eq!(to_2020(red, Gamut::Super), red);
        assert_eq!(
            to_2020([0.5, 0.25, 0.125], Gamut::Super),
            [0.5, 0.25, 0.125]
        );
    }

    #[test]
    fn srgb_and_the_shaders_linearisation() {
        assert_eq!(quantise(srgb_encode(0.0), 255.0), 0);
        assert_eq!(quantise(srgb_encode(1.0), 255.0), 255);
        assert_eq!(quantise(srgb_encode(0.5), 255.0), 188);
        assert_eq!(quantise(srgb_encode(2.0), 255.0), 255, "clamped");
        assert!((sdr_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((sdr_to_linear(0.5) - 0.5f32.powf(2.4)).abs() < 1e-6);
    }

    #[test]
    fn roll_off_keeps_sdr_and_compresses_highlights_toward_the_peak() {
        assert_eq!(roll_off([0.5, 0.25, 1.0], 5.0), [0.5, 0.25, 1.0]);
        // 8x paper white under a 10x peak: 1 + 9 * 7 / 16 on the brightest
        // channel, the others scaled with it.
        let [r, g, b] = roll_off([8.0, 4.0, 0.0], 10.0);
        assert_eq!(r, 4.9375);
        assert_eq!(g, 4.9375 / 2.0);
        assert_eq!(b, 0.0);
        // Far above the peak the brightest channel approaches the peak.
        let [r, _, _] = roll_off([1.0e6, 1.0e6, 1.0e6], 5.0);
        assert!(r > 4.99 && r < 5.0, "{r}");
        // Zero headroom: a peak at or below paper white pins to paper white.
        assert_eq!(roll_off([10.0, 10.0, 10.0], 1.0), [1.0, 1.0, 1.0]);
        assert_eq!(roll_off([10.0, 5.0, 10.0], 0.5), [1.0, 0.5, 1.0]);
    }

    #[test]
    fn hdr10_white_sits_at_paper_white_and_highlights_roll_off_toward_the_peak() {
        let s = Hdr {
            mode: HdrMode::Hdr10,
            paper_white_nits: 200.0,
            max_nits: 1000.0,
            expand_gamut: Gamut::Super,
        };
        let code = quantise(pq_encode(200.0), 1023.0) as u16;
        assert_eq!(
            encode_hdr10([1.0, 1.0, 1.0], &s),
            pack_2101010(code, code, code)
        );
        // 100x paper white rolls off below the 1000 nit peak (code 769)
        // and stays above paper white; a value far beyond the peak reaches
        // the peak's code without passing it.
        let hundred = (encode_hdr10([100.0, 100.0, 100.0], &s) >> 20) & 0x3FF;
        assert!(hundred > u32::from(code) && hundred < 769, "{hundred}");
        assert_eq!(
            encode_hdr10([1.0e6, 1.0e6, 1.0e6], &s),
            pack_2101010(769, 769, 769)
        );
        // A peak below paper white is zero headroom, not a negative range.
        let low = Hdr {
            max_nits: 50.0,
            ..s
        };
        assert_eq!(
            encode_hdr10([10.0, 10.0, 10.0], &low),
            encode_hdr10([1.0, 1.0, 1.0], &low)
        );
        // Rec.709 white is Rec.2020 white under the accurate rotation.
        let v = encode_hdr10([1.0, 1.0, 1.0], &Hdr::default());
        assert_eq!((v >> 20) & 0x3FF, (v >> 10) & 0x3FF);
        assert_eq!((v >> 10) & 0x3FF, v & 0x3FF);
    }

    #[test]
    fn sdr_packings_quantise_srgb_values() {
        assert_eq!(encode_sdr_8([1.0, 0.0, 0.0]), 0x00FF_0000);
        assert_eq!(
            encode_sdr_8([18.0 / 255.0, 52.0 / 255.0, 86.0 / 255.0]),
            0x0012_3456
        );
        assert_eq!(encode_sdr_10([1.0, 1.0, 1.0]), 0x3FFF_FFFF);
        assert_eq!(
            encode_sdr_10([18.0 / 255.0, 52.0 / 255.0, 86.0 / 255.0]),
            pack_2101010(72, 209, 345)
        );
        assert_eq!(encode_sdr_10([4.0, -1.0, 0.0]), 0x3FF0_0000, "clamped");
    }
}

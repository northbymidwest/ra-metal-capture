//! The librashader backend: render an image through a preset inside this
//! process and write the `.gputrace` with Metal's capture API. Sizing is
//! pure and tested; everything that touches Metal lives in [`run`] and
//! [`Trace`], and is exercised only by a real run. The input texture takes
//! the frame's own format and the output the HDR mode's; this and
//! `hosted::libretro` are the two modules that allow `unsafe`; here it is
//! for the Metal calls objc2 cannot prove safe (descriptor construction
//! and the byte upload).
//!
//! Under `MTL_CAPTURE_ENABLED=1` the device Metal hands back is a capture
//! proxy class that does not statically declare every `MTLDevice` selector.
//! objc2's debug-build method verification looks each selector up and
//! panics on one it cannot find, so any new call against `device` in this
//! module needs a real debug-build run to confirm it is actually there,
//! not just a clean `cargo build`.
#![allow(unsafe_code)]

mod trace;
pub use trace::{CAPTURE_ENV, Trace};

use crate::bundle::discard_partial;
use crate::config::{Aspect, Hdr, HdrMode, Size, WindowMode};
use crate::display::Screen;
use crate::hdr::{encode_hdr10, encode_sdr_10, srgb_encode};
use crate::interrupt;
use anyhow::{Context, Result, anyhow, bail};
use librashader::presets::ShaderFeatures;
use librashader::runtime::mtl::{FilterChain, FrameOptions};
use librashader::runtime::{ColorSpace, Viewport};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLGPUFamily,
    MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage,
};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

/// Warm-up counts at or above this print a line first, so a long, silent
/// warm-up (the default settle on a core is about 300 frames) is not
/// mistaken for a hang.
const WARMUP_PROGRESS_THRESHOLD: u32 = 60;

/// Everything [`run`] needs: the frame source, the preset, how to size the
/// output, how many frames to record, and where the bundle goes.
pub struct RenderOptions {
    /// Where frames come from; image mode uses `ImageSource`, core hosting a
    /// `libretro::Core`.
    pub source: Box<dyn FrameSource>,
    /// The `.slangp` preset.
    pub preset: PathBuf,
    /// Window mode from the command line, mapped to pixels by [`output_size`].
    pub window: WindowMode,
    /// The viewport aspect; `Native` is the source's own.
    pub aspect: Aspect,
    /// HDR output mode and the preset's HDR uniforms.
    pub hdr: Hdr,
    /// The main display, for `Fill` and `Fullscreen`.
    pub screen: Screen,
    /// Frames rendered through the chain before the capture starts, so
    /// history passes see real prior frames in the first recorded one.
    /// Image mode uses 0.
    pub warmup: u32,
    /// Number of frames to render and record; the frame count advances by one each.
    pub frames: u32,
    /// Absolute path of the `.gputrace` to write. The caller clears it
    /// first with [`crate::bundle::prepare_output`]; a failed capture
    /// removes what it left there.
    pub output: PathBuf,
    /// Print the decoded and output sizes to stderr.
    pub verbose: bool,
}

/// The layout of a frame's bytes, which is the format of the input
/// texture they are uploaded into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// 8 bits per channel, blue first: every SDR source after conversion.
    Bgra8,
    /// Packed 2:10:10:10 with red in bits 29:20, which is what libretro's
    /// XRGB2101010 and HDR10_2101010 are in memory; uploaded unconverted,
    /// as RetroArch's Metal driver does.
    Bgr10a2,
}

impl FrameFormat {
    /// The name printed by `-v`, Metal's for the texture format.
    pub fn name(self) -> &'static str {
        match self {
            FrameFormat::Bgra8 => "BGRA8",
            FrameFormat::Bgr10a2 => "BGR10A2",
        }
    }

    /// The Metal texture format that reads these bytes as they are.
    pub fn metal(self) -> MTLPixelFormat {
        match self {
            FrameFormat::Bgra8 => MTLPixelFormat::BGRA8Unorm,
            FrameFormat::Bgr10a2 => MTLPixelFormat::BGR10A2Unorm,
        }
    }
}

/// One frame: tightly packed rows in `format`, top row first, exactly
/// `size.width * size.height * 4` bytes (both formats are 4 bytes per
/// pixel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub size: Size,
    pub format: FrameFormat,
    pub pixels: Vec<u8>,
}

/// Where frames come from: a decoded image (the same bytes forever) or a
/// running libretro core (one emulated frame per call).
pub trait FrameSource {
    /// The source's nominal size: an image's, or a core's base geometry.
    /// It sizes the output; a core's frames may come at another size
    /// (SNES hi-res modes do), which [`run`] follows by re-creating its
    /// input texture.
    fn size(&self) -> Size;
    /// The source's own aspect ratio: a core's reported one, or the pixel
    /// aspect for an image or a core that reports none.
    fn aspect_ratio(&self) -> f64;
    /// The layout every frame of this source comes in: an image's is
    /// always `Bgra8`; a core's is fixed by the pixel format it selected
    /// while loading. It sizes and types the input texture.
    fn format(&self) -> FrameFormat;
    /// The next frame, valid until the next call.
    fn next(&mut self) -> Result<&Frame>;
}

/// A static image, decoded once.
pub struct ImageSource {
    frame: Frame,
}

impl ImageSource {
    /// File extensions the `image` crate decodes with the features this
    /// crate enables (see `Cargo.toml`).
    pub const EXTENSIONS: [&str; 12] = [
        "png", "jpg", "jpeg", "bmp", "gif", "tga", "pbm", "pgm", "ppm", "pam", "pnm", "hdr",
    ];

    /// Whether `path` has an extension in [`ImageSource::EXTENSIONS`],
    /// case-insensitively.
    pub fn accepts(path: &Path) -> bool {
        crate::image_file::has_extension(path, &Self::EXTENSIONS)
    }

    /// Take an image already in memory, converting RGBA8 to BGRA8.
    pub fn from_image(img: image::RgbaImage) -> ImageSource {
        let (width, height) = img.dimensions();
        let mut bgra = img.into_raw();
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        ImageSource {
            frame: Frame {
                size: Size { width, height },
                format: FrameFormat::Bgra8,
                pixels: bgra,
            },
        }
    }

    /// Take linear Rec.709 content with 1.0 at paper white (a decoded
    /// Radiance file) as a packed 10-bit frame: PQ under `HdrMode::Hdr10`,
    /// which is what an HDR10 core delivers, sRGB otherwise, which is what
    /// a 10-bit SDR core delivers. The two high bits are set, as
    /// `pixels::to_frame` sets them, so alpha samples as 1.0.
    pub fn from_linear(width: u32, height: u32, linear: Vec<[f32; 3]>, hdr: &Hdr) -> ImageSource {
        let mut pixels = Vec::with_capacity(linear.len() * 4);
        for rgb in linear {
            let packed = match hdr.mode {
                HdrMode::Hdr10 => encode_hdr10(rgb, hdr),
                HdrMode::Off => encode_sdr_10(rgb.map(srgb_encode)),
            };
            pixels.extend_from_slice(&(packed | 0xC000_0000).to_le_bytes());
        }
        ImageSource {
            frame: Frame {
                size: Size { width, height },
                format: FrameFormat::Bgr10a2,
                pixels,
            },
        }
    }

    /// Decode the file at `path` with the `image` crate. A Radiance file
    /// (the one format it decodes to floats) is linear HDR content and
    /// becomes a 10-bit frame; everything else is 8-bit BGRA8.
    pub fn open(path: &Path, hdr: &Hdr) -> Result<ImageSource> {
        let img = image::open(path).with_context(|| format!("decoding {}", path.display()))?;
        let (width, height) = (img.width(), img.height());
        Ok(match img {
            image::DynamicImage::ImageRgb32F(f) => {
                ImageSource::from_linear(width, height, f.pixels().map(|p| p.0).collect(), hdr)
            }
            other => ImageSource::from_image(other.into_rgba8()),
        })
    }
}

impl FrameSource for ImageSource {
    fn size(&self) -> Size {
        self.frame.size
    }
    fn aspect_ratio(&self) -> f64 {
        pixel_aspect(self.frame.size)
    }
    fn format(&self) -> FrameFormat {
        self.frame.format
    }
    fn next(&mut self) -> Result<&Frame> {
        Ok(&self.frame)
    }
}

/// A 2D texture of `size` in `format` with `usage`, in shared memory on
/// an Apple-family GPU (Apple silicon, natively or under Rosetta), which
/// has no managed storage, and managed memory otherwise.
///
/// The family is asked rather than `hasUnifiedMemory`, which would be the
/// direct question: `supportsFamily:` is real on the capture proxy device
/// class where `hasUnifiedMemory` is not. See the module doc for why that
/// distinction matters under capture.
fn new_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    size: Size,
    format: MTLPixelFormat,
    usage: MTLTextureUsage,
    what: &str,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    // SAFETY: plain descriptor construction with a pixel format and a size.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            format,
            size.width as usize,
            size.height as usize,
            false,
        )
    };
    desc.setStorageMode(if device.supportsFamily(MTLGPUFamily::Apple1) {
        MTLStorageMode::Shared
    } else {
        MTLStorageMode::Managed
    });
    desc.setUsage(usage);
    device.newTextureWithDescriptor(&desc).ok_or_else(|| {
        anyhow!(
            "creating the {what} texture ({}x{} {})",
            size.width,
            size.height,
            pixel_format_name(format)
        )
    })
}

/// Render `opts.warmup` uncaptured frames followed by `opts.frames`
/// recorded frames from `opts.source` through the preset, writing the
/// latter to `opts.output` as a `.gputrace`. The frame count passed to the
/// filter chain runs continuously across both phases.
pub fn run(mut opts: RenderOptions) -> Result<()> {
    let mut image_size = opts.source.size();
    check_texture_size(image_size).context("the source frame")?;
    let aspect = opts.aspect.ratio_or(opts.source.aspect_ratio());
    let size = output_size(&opts.window, image_size, aspect, &opts.screen);
    check_texture_size(size).context("the output")?;
    let mut image_format = opts.source.format();
    let output_format = output_pixel_format(opts.hdr.mode, image_format);
    if opts.verbose {
        eprintln!(
            "source {}x{} {} at aspect {aspect:.4} -> output {}x{} {} px, {} warm-up + {} recorded frame(s)",
            image_size.width,
            image_size.height,
            image_format.name(),
            size.width,
            size.height,
            pixel_format_name(output_format),
            opts.warmup,
            opts.frames
        );
    }

    let device = MTLCreateSystemDefaultDevice().context("no Metal device is available")?;
    let queue = device
        .newCommandQueue()
        .context("creating a Metal command queue")?;

    let mut input = new_texture(
        &device,
        image_size,
        image_format.metal(),
        MTLTextureUsage::ShaderRead,
        "input",
    )?;
    let output = new_texture(
        &device,
        size,
        output_format,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        "output",
    )?;

    let mut chain = FilterChain::load_from_path(&opts.preset, ShaderFeatures::NONE, &queue, None)
        .with_context(|| format!("loading preset {}", opts.preset.display()))?;
    let viewport = Viewport::new_render_target_sized_origin(&*output, None)
        .context("sizing the viewport to the output texture")?;
    let per_frame = frame_options(&opts.hdr);

    let upload = |input: &ProtocolObject<dyn MTLTexture>, frame: &Frame| -> Result<()> {
        let Size { width, height } = frame.size;
        if frame.pixels.len() != width as usize * height as usize * 4 {
            bail!(
                "frame source yielded {} bytes for {width}x{height}",
                frame.pixels.len()
            );
        }
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: width as usize,
                height: height as usize,
                depth: 1,
            },
        };
        let pixels = NonNull::new(frame.pixels.as_ptr().cast_mut().cast())
            .context("frame buffer is null")?;
        // SAFETY: `frame.pixels` holds exactly width * height * 4 bytes of
        // the frame's format with rows of width * 4 bytes (checked above),
        // matching `region` and the row stride, and Metal copies them
        // before returning. `input` is a texture of exactly that size,
        // created for it by the loop below.
        unsafe {
            input.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                pixels,
                width as usize * 4,
            )
        };
        Ok(())
    };
    let mut render_one =
        |input: &ProtocolObject<dyn MTLTexture>, frame_count: usize| -> Result<()> {
            let cmd = queue
                .commandBuffer()
                .context("creating a Metal command buffer")?;
            chain
                .frame(input, &viewport, &cmd, frame_count, Some(&per_frame))
                .with_context(|| format!("rendering frame {frame_count}"))?;
            cmd.commit();
            cmd.waitUntilCompleted();
            Ok(())
        };

    // Both phases: a frame per call, the count running on across them.
    // Ctrl-C is polled between frames (see `interrupt`). A frame at a new
    // size (a core switching resolution) gets a new input texture; the
    // output keeps the size the boot geometry gave it, as RetroArch's
    // window does.
    let mut count = 0usize;
    let mut render_frames = |n: u32| -> Result<()> {
        for _ in 0..n {
            interrupt::check()?;
            let frame = opts.source.next()?;
            if frame.size != image_size || frame.format != image_format {
                check_texture_size(frame.size).context("the source's new frame size")?;
                input = new_texture(
                    &device,
                    frame.size,
                    frame.format.metal(),
                    MTLTextureUsage::ShaderRead,
                    "input",
                )?;
                if opts.verbose {
                    eprintln!(
                        "frame changed from {}x{} {} to {}x{} {}",
                        image_size.width,
                        image_size.height,
                        image_format.name(),
                        frame.size.width,
                        frame.size.height,
                        frame.format.name()
                    );
                }
                image_size = frame.size;
                image_format = frame.format;
            }
            upload(&input, frame)?;
            render_one(&input, count)?;
            count += 1;
        }
        Ok(())
    };
    if opts.warmup >= WARMUP_PROGRESS_THRESHOLD {
        eprintln!(
            "rendering {} warm-up frame(s) before the capture starts",
            opts.warmup
        );
    }
    render_frames(opts.warmup)?;
    let trace = Trace::start(&device, &opts.output)?;
    if let Err(e) = render_frames(opts.frames) {
        // Stop Metal writing before removing what it wrote, so the next
        // run to this path is not refused for a bundle we left behind.
        drop(trace);
        discard_partial(&opts.output);
        return Err(e);
    }
    trace.finish();

    if !opts.output.join("index").exists() {
        discard_partial(&opts.output);
        bail!(
            "the Metal capture finished but {} has no `index` entry; the bundle looks incomplete and was removed",
            opts.output.display()
        );
    }
    Ok(())
}

/// Multiply a size in points by the backing scale, flooring to whole pixels.
fn to_pixels(size: Size, backing_scale: f64) -> Size {
    Size {
        width: (size.width as f64 * backing_scale) as u32,
        height: (size.height as f64 * backing_scale) as u32,
    }
}

/// Width over height of `size` in pixels; 0 for an empty size.
pub fn pixel_aspect(size: Size) -> f64 {
    if size.height == 0 {
        0.0
    } else {
        size.width as f64 / size.height as f64
    }
}

/// The shape a source is shown at: its own height, and the width that
/// height needs at `aspect`, rounded. This is what RetroArch sizes a
/// window from, so both backends start from the same shape.
pub fn display_size(image: Size, aspect: f64) -> Size {
    if aspect <= 0.0 || !aspect.is_finite() {
        return image;
    }
    Size {
        width: (image.height as f64 * aspect).round() as u32,
        height: image.height,
    }
}

/// Scale `image` by one factor so it fits inside `max`, keeping its aspect
/// ratio and flooring to whole pixels, the way RetroArch's fill mode clamps
/// a scaled window and its viewport letterboxes inside a fixed one. Falls
/// back to the image's own size when the factor would be zero or produce
/// an empty texture (a zero `max`, or an image with a zero dimension).
fn fit(image: Size, max: Size) -> Size {
    if image.width == 0 || image.height == 0 {
        return image;
    }
    let factor = f64::min(
        max.width as f64 / image.width as f64,
        max.height as f64 / image.height as f64,
    );
    let fitted = Size {
        width: (image.width as f64 * factor) as u32,
        height: (image.height as f64 * factor) as u32,
    };
    if fitted.width == 0 || fitted.height == 0 {
        image
    } else {
        fitted
    }
}

/// The largest texture dimension Metal allows on every Apple GPU family
/// this tool runs on; a descriptor above it fails an assertion inside
/// Metal rather than returning an error, so it is checked here first.
pub const MAX_TEXTURE_DIM: u32 = 16384;

/// Refuse a texture size Metal would reject: a zero dimension, or one
/// above [`MAX_TEXTURE_DIM`]. Both the source frame and the output are
/// checked, since Metal asserts rather than erroring on either.
pub fn check_texture_size(size: Size) -> Result<()> {
    if size.width == 0 || size.height == 0 {
        bail!(
            "size is {}x{}; refusing to create a zero-sized texture",
            size.width,
            size.height
        );
    }
    if size.width > MAX_TEXTURE_DIM || size.height > MAX_TEXTURE_DIM {
        bail!(
            "size is {}x{}; Metal textures cannot exceed {MAX_TEXTURE_DIM} on a side (for the output, use a smaller --size or --scale)",
            size.width,
            size.height
        );
    }
    Ok(())
}

/// The output texture size, in pixels, for a window mode, given the
/// source and the aspect it is shown at. The texture is the viewport
/// RetroArch would draw into for the same request: `Scale` multiplies the
/// [`display_size`] in points (RetroArch's window scale); `Exact`,
/// `Fullscreen`, and `Fill` hold the largest box of that aspect inside the
/// size, the display, or the visible area, which is where RetroArch
/// letterboxes. RetroArch's modes are in points; each is mapped to pixels
/// on `screen`, except `Exact`, which is taken as pixels as given.
pub fn output_size(mode: &WindowMode, image: Size, aspect: f64, screen: &Screen) -> Size {
    let display = display_size(image, aspect);
    match mode {
        WindowMode::Exact(size) => fit(display, *size),
        WindowMode::Scale(n) => to_pixels(
            Size {
                width: display.width.saturating_mul(*n),
                height: display.height.saturating_mul(*n),
            },
            screen.backing_scale,
        ),
        WindowMode::Fullscreen => fit(display, to_pixels(screen.full, screen.backing_scale)),
        WindowMode::Fill { max } => fit(display, to_pixels(*max, screen.backing_scale)),
    }
}

/// The output texture's format. An SDR source without HDR renders into
/// BGRA8, RetroArch's default swapchain. A 10-bit source keeps 10 bits
/// even without HDR, so the precision a core paid for reaches the final
/// pass and the answer to `GET_SCREEN_10BPC_CAPABLE` is honest; RetroArch
/// offers the same as its opt-in 10-bit SDR swapchain. Under HDR10 the
/// output is 10-bit PQ, RetroArch's HDR10 swapchain format, which the
/// preset's final pass renders straight into.
pub fn output_pixel_format(mode: HdrMode, source: FrameFormat) -> MTLPixelFormat {
    match (mode, source) {
        (HdrMode::Off, FrameFormat::Bgra8) => MTLPixelFormat::BGRA8Unorm,
        (HdrMode::Off, FrameFormat::Bgr10a2) | (HdrMode::Hdr10, _) => MTLPixelFormat::RGB10A2Unorm,
    }
}

/// Metal's name for a format this module uses, for `-v`.
fn pixel_format_name(format: MTLPixelFormat) -> &'static str {
    if format == MTLPixelFormat::BGRA8Unorm {
        "BGRA8"
    } else if format == MTLPixelFormat::BGR10A2Unorm {
        "BGR10A2"
    } else if format == MTLPixelFormat::RGB10A2Unorm {
        "RGB10A2"
    } else {
        "other"
    }
}

/// The per-frame options every frame is rendered with: librashader's
/// defaults, which is what passing none gives, plus the HDR uniforms
/// (`HDRMode`, `BrightnessNits`, `ExpandGamut`) from the run's settings.
/// RetroArch's own `InverseTonemap` and `HDR10` uniforms stay at zero,
/// the passthrough state its Vulkan driver uses for an HDR10 source.
pub fn frame_options(hdr: &Hdr) -> FrameOptions {
    FrameOptions {
        color_space: match hdr.mode {
            HdrMode::Off => ColorSpace::Sdr,
            HdrMode::Hdr10 => ColorSpace::Hdr10,
        },
        brightness_nits: hdr.paper_white_nits,
        expand_gamut: hdr.expand_gamut.as_u32(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Gamut, Hdr, HdrMode};
    use crate::hdr::{pack_2101010, pq_encode, quantise, srgb_encode};
    use std::path::Path;

    #[test]
    fn image_source_is_bgra8_swapped_from_rgba_and_repeats() {
        // A 2x1 image: red then blue, both opaque.
        let img = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let mut src = ImageSource::from_image(img);
        assert_eq!(
            src.size(),
            Size {
                width: 2,
                height: 1
            }
        );
        assert_eq!(src.format(), FrameFormat::Bgra8);
        let first = src.next().unwrap();
        assert_eq!(first.format, FrameFormat::Bgra8);
        let a = first.pixels.to_vec();
        assert_eq!(a, [0, 0, 255, 255, 255, 0, 0, 255]);
        assert_eq!(src.next().unwrap().pixels, a);
    }

    #[test]
    fn image_source_open_decodes_the_fixture() {
        let src = ImageSource::open(Path::new("fixtures/sample.png"), &Hdr::default()).unwrap();
        assert_eq!(
            src.size(),
            Size {
                width: 160,
                height: 144
            }
        );
        assert!(ImageSource::open(Path::new("Cargo.toml"), &Hdr::default()).is_err());
    }

    /// The little-endian bytes of a packed 10-bit pixel with the two
    /// high bits set, as every `Bgr10a2` frame carries them.
    fn px10(r: u16, g: u16, b: u16) -> [u8; 4] {
        (pack_2101010(r, g, b) | 0xC000_0000).to_le_bytes()
    }

    #[test]
    fn radiance_image_is_pq_ten_bit_under_hdr10() {
        let hdr = Hdr {
            mode: HdrMode::Hdr10,
            expand_gamut: Gamut::Super,
            ..Hdr::default()
        };
        let mut src = ImageSource::open(Path::new("fixtures/probe.hdr"), &hdr).unwrap();
        assert_eq!(
            src.size(),
            Size {
                width: 4,
                height: 4
            }
        );
        assert_eq!(src.format(), FrameFormat::Bgr10a2);
        let frame = src.next().unwrap();
        assert_eq!(frame.pixels.len(), 4 * 4 * 4);
        let px = |i: usize| -> [u8; 4] { frame.pixels[i * 4..i * 4 + 4].try_into().unwrap() };
        let white = quantise(pq_encode(200.0), 1023.0) as u16;
        // 4x paper white under the default 1000 nit peak (5x, 4x of
        // headroom) rolls off to 1 + 4 * 3 / 7 of paper white.
        let bright = quantise(pq_encode(200.0 * (1.0 + 4.0 * 3.0 / 7.0)), 1023.0) as u16;
        assert_eq!(px(0), px10(white, white, white), "paper white");
        assert_eq!(
            px(1),
            px10(bright, bright, bright),
            "4x paper white, rolled off"
        );
        assert_eq!(px(2), px10(0, 0, 0), "black");
        assert_eq!(px(4), px10(white, 0, 0), "red, no rotation under super");
    }

    #[test]
    fn radiance_image_is_srgb_ten_bit_without_hdr() {
        let hdr = Hdr::default();
        let mut src = ImageSource::open(Path::new("fixtures/probe.hdr"), &hdr).unwrap();
        assert_eq!(src.format(), FrameFormat::Bgr10a2);
        let frame = src.next().unwrap();
        let px = |i: usize| -> [u8; 4] { frame.pixels[i * 4..i * 4 + 4].try_into().unwrap() };
        assert_eq!(px(0), px10(1023, 1023, 1023), "white");
        assert_eq!(px(1), px10(1023, 1023, 1023), "the highlight clips");
        assert_eq!(px(2), px10(0, 0, 0), "black");
        let grey = quantise(srgb_encode(0.5), 1023.0) as u16;
        assert_eq!(px(3), px10(grey, grey, grey), "mid grey is sRGB-encoded");
        assert_eq!(px(4), px10(1023, 0, 0), "red");
    }

    #[test]
    fn png_stays_bgra8_whatever_the_hdr_mode() {
        let hdr = Hdr {
            mode: HdrMode::Hdr10,
            ..Hdr::default()
        };
        let src = ImageSource::open(Path::new("fixtures/sample.png"), &hdr).unwrap();
        assert_eq!(src.format(), FrameFormat::Bgra8);
    }

    const GB: Size = Size {
        width: 160,
        height: 144,
    };

    fn screen(backing_scale: f64) -> Screen {
        Screen {
            visible: Size {
                width: 2488,
                height: 1410,
            },
            full: Size {
                width: 2560,
                height: 1440,
            },
            backing_scale,
        }
    }

    #[test]
    fn check_texture_size_rejects_zero_and_oversize() {
        assert!(
            check_texture_size(Size {
                width: 0,
                height: 5
            })
            .is_err()
        );
        let err = check_texture_size(Size {
            width: 40000,
            height: 10,
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("16384"), "{err}");
        assert!(
            check_texture_size(Size {
                width: 16384,
                height: 16384
            })
            .is_ok()
        );
    }

    #[test]
    fn pixel_aspect_is_width_over_height() {
        assert_eq!(pixel_aspect(GB), 160.0 / 144.0);
        assert_eq!(
            pixel_aspect(Size {
                width: 5,
                height: 0
            }),
            0.0
        );
    }

    /// A 4:3 console with non-square pixels: 256x224 shown at 4:3.
    const SNES: Size = Size {
        width: 256,
        height: 224,
    };
    const FOUR_THREE: f64 = 4.0 / 3.0;
    /// GB's own aspect is its pixel aspect.
    const GB_ASPECT: f64 = 160.0 / 144.0;

    #[test]
    fn display_size_widens_to_the_aspect_at_the_source_height() {
        assert_eq!(display_size(GB, GB_ASPECT), GB);
        assert_eq!(
            display_size(SNES, FOUR_THREE),
            Size {
                width: 299,
                height: 224
            }
        );
    }

    #[test]
    fn exact_is_the_aspect_box_inside_the_size() {
        let size = Size {
            width: 1600,
            height: 1440,
        };
        assert_eq!(
            output_size(&WindowMode::Exact(size), GB, GB_ASPECT, &screen(2.0)),
            Size {
                width: 1600,
                height: 1440
            }
        );
        assert_eq!(
            output_size(&WindowMode::Exact(size), SNES, FOUR_THREE, &screen(2.0)),
            Size {
                width: 1600,
                height: 1198
            }
        );
    }

    #[test]
    fn scale_multiplies_the_display_size_in_points() {
        assert_eq!(
            output_size(&WindowMode::Scale(4), GB, GB_ASPECT, &screen(2.0)),
            Size {
                width: 1280,
                height: 1152
            }
        );
        assert_eq!(
            output_size(&WindowMode::Scale(4), GB, GB_ASPECT, &screen(1.0)),
            Size {
                width: 640,
                height: 576
            }
        );
        assert_eq!(
            output_size(&WindowMode::Scale(2), SNES, FOUR_THREE, &screen(1.0)),
            Size {
                width: 598,
                height: 448
            }
        );
    }

    #[test]
    fn fullscreen_is_the_aspect_box_inside_the_full_frame() {
        assert_eq!(
            output_size(&WindowMode::Fullscreen, SNES, FOUR_THREE, &screen(2.0)),
            Size {
                width: 3844,
                height: 2880
            }
        );
        assert_eq!(
            output_size(&WindowMode::Fullscreen, SNES, FOUR_THREE, &screen(1.0)),
            Size {
                width: 1922,
                height: 1440
            }
        );
    }

    #[test]
    fn fill_is_height_limited_for_a_wide_display() {
        // 2488x1382 points at 2x is 4976x2764 px; 2764/144 = 19.19 < 4976/160.
        let max = Size {
            width: 2488,
            height: 1382,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, GB, GB_ASPECT, &screen(2.0)),
            Size {
                width: 3071,
                height: 2764
            }
        );
    }

    #[test]
    fn fill_uses_the_aspect_not_the_pixels() {
        let max = Size {
            width: 1000,
            height: 1000,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, SNES, FOUR_THREE, &screen(1.0)),
            Size {
                width: 1000,
                height: 749
            }
        );
    }

    #[test]
    fn fill_is_width_limited_for_a_wide_image() {
        let max = Size {
            width: 1000,
            height: 1000,
        };
        let wide = Size {
            width: 400,
            height: 100,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, wide, 4.0, &screen(1.0)),
            Size {
                width: 1000,
                height: 250
            }
        );
    }

    #[test]
    fn fill_shrinks_an_image_larger_than_the_display() {
        let max = Size {
            width: 1000,
            height: 1000,
        };
        let huge = Size {
            width: 4000,
            height: 1000,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, huge, 4.0, &screen(1.0)),
            Size {
                width: 1000,
                height: 250
            }
        );
    }

    #[test]
    fn fill_falls_back_to_the_display_size_when_max_is_degenerate() {
        let zero = Size {
            width: 0,
            height: 0,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max: zero }, GB, GB_ASPECT, &screen(2.0)),
            GB
        );
        let sliver = Size {
            width: 100,
            height: 0,
        };
        assert_eq!(
            output_size(
                &WindowMode::Fill { max: sliver },
                SNES,
                FOUR_THREE,
                &screen(1.0)
            ),
            Size {
                width: 299,
                height: 224
            }
        );
    }

    #[test]
    fn frame_formats_map_to_metals_packed_formats() {
        assert_eq!(FrameFormat::Bgra8.metal(), MTLPixelFormat::BGRA8Unorm);
        assert_eq!(FrameFormat::Bgr10a2.metal(), MTLPixelFormat::BGR10A2Unorm);
        assert_eq!(pixel_format_name(MTLPixelFormat::BGRA8Unorm), "BGRA8");
        assert_eq!(pixel_format_name(MTLPixelFormat::BGR10A2Unorm), "BGR10A2");
        assert_eq!(pixel_format_name(MTLPixelFormat::RGB10A2Unorm), "RGB10A2");
    }

    #[test]
    fn output_is_bgra8_only_for_an_sdr_source_without_hdr() {
        assert_eq!(
            output_pixel_format(HdrMode::Off, FrameFormat::Bgra8),
            MTLPixelFormat::BGRA8Unorm
        );
        assert_eq!(
            output_pixel_format(HdrMode::Off, FrameFormat::Bgr10a2),
            MTLPixelFormat::RGB10A2Unorm
        );
        assert_eq!(
            output_pixel_format(HdrMode::Hdr10, FrameFormat::Bgra8),
            MTLPixelFormat::RGB10A2Unorm
        );
        assert_eq!(
            output_pixel_format(HdrMode::Hdr10, FrameFormat::Bgr10a2),
            MTLPixelFormat::RGB10A2Unorm
        );
    }

    #[test]
    fn frame_options_carry_the_hdr_uniforms_over_the_defaults() {
        let off = frame_options(&Hdr::default());
        assert!(matches!(off.color_space, ColorSpace::Sdr));
        assert_eq!(off.brightness_nits, 200.0);
        assert_eq!(off.expand_gamut, 0);
        let on = frame_options(&Hdr {
            mode: HdrMode::Hdr10,
            paper_white_nits: 300.0,
            max_nits: 800.0,
            expand_gamut: Gamut::Wide,
        });
        assert!(matches!(on.color_space, ColorSpace::Hdr10));
        assert_eq!(on.brightness_nits, 300.0);
        assert_eq!(on.expand_gamut, 2);
        // Everything else is librashader's default, which `None` gave.
        let d = FrameOptions::default();
        assert_eq!(on.frame_direction, d.frame_direction);
        assert_eq!(on.rotation, d.rotation);
        assert_eq!(on.total_subframes, d.total_subframes);
        assert_eq!(on.aspect_ratio, d.aspect_ratio);
        assert_eq!(on.frames_per_second, d.frames_per_second);
    }

    #[test]
    fn sample_hdr_is_the_scene_with_the_sun_above_paper_white() {
        let hdr = Hdr {
            mode: HdrMode::Hdr10,
            max_nits: 2000.0,
            expand_gamut: Gamut::Super,
            ..Hdr::default()
        };
        let mut src = ImageSource::open(Path::new("fixtures/sample.hdr"), &hdr).unwrap();
        assert_eq!(
            src.size(),
            Size {
                width: 160,
                height: 144
            }
        );
        assert_eq!(src.format(), FrameFormat::Bgr10a2);
        let frame = src.next().unwrap();
        fn red(frame: &Frame, x: usize, y: usize) -> u32 {
            let i = (y * 160 + x) * 4;
            (u32::from_le_bytes(frame.pixels[i..i + 4].try_into().unwrap()) >> 20) & 0x3FF
        }
        // With a 2000 nit peak (10x paper white, 9x of headroom) the sun's
        // centre (140, 13), pure red at 8x paper white, rolls off to
        // 1 + 9 * 7 / 16 = 4.9375x, 987.5 nits; the sky at (0, 0) is well
        // below paper white in red.
        assert_eq!(red(frame, 140, 13), quantise(pq_encode(987.5), 1023.0));
        assert!(red(frame, 0, 0) < quantise(pq_encode(200.0), 1023.0));

        // At the default 1000 nit peak (5x, 4x of headroom) the same sun
        // rolls off to 1 + 4 * 7 / 11 of paper white.
        let mut src = ImageSource::open(
            Path::new("fixtures/sample.hdr"),
            &Hdr {
                mode: HdrMode::Hdr10,
                expand_gamut: Gamut::Super,
                ..Hdr::default()
            },
        )
        .unwrap();
        let frame = src.next().unwrap();
        assert_eq!(
            red(frame, 140, 13),
            quantise(pq_encode(200.0 * (1.0 + 4.0 * 7.0 / 11.0)), 1023.0)
        );

        // Without HDR the sun clips to white and the sky keeps its sRGB value.
        let mut src = ImageSource::open(Path::new("fixtures/sample.hdr"), &Hdr::default()).unwrap();
        let frame = src.next().unwrap();
        assert_eq!(red(frame, 140, 13), 1023);
        // Sky red is 107/255 in sRGB, 429 of 1023, but Radiance's shared
        // exponent follows the pixel's largest channel (blue, at 1.0), so
        // red keeps 7 mantissa bits and lands a few codes low.
        assert!(
            (red(frame, 0, 0) as i64 - 429).abs() <= 12,
            "{}",
            red(frame, 0, 0)
        );
    }
}

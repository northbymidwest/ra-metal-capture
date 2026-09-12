//! The librashader backend: render an image through a preset inside this
//! process and write the `.gputrace` with Metal's capture API. Sizing is
//! pure and tested; everything that touches Metal lives in [`run`] and
//! [`Trace`], and is exercised only by a real run. This is the one module
//! in the crate that allows `unsafe`, for the Metal calls objc2 cannot
//! prove safe (descriptor construction and the byte upload).
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
use crate::config::{Aspect, Size, WindowMode};
use crate::display::Screen;
use crate::interrupt;
use anyhow::{Context, Result, anyhow, bail};
use librashader::presets::ShaderFeatures;
use librashader::runtime::Viewport;
use librashader::runtime::mtl::FilterChain;
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

/// Where frames come from: a decoded image (the same bytes forever) or a
/// running libretro core (one emulated frame per call).
pub trait FrameSource {
    /// Size of every frame this source yields.
    fn size(&self) -> Size;
    /// The source's own aspect ratio: a core's reported one, or the pixel
    /// aspect for an image or a core that reports none.
    fn aspect_ratio(&self) -> f64;
    /// The next frame as tightly packed BGRA8 rows, top row first,
    /// exactly `size().width * size().height * 4` bytes, valid until the
    /// next call.
    fn next(&mut self) -> Result<&[u8]>;
}

/// A static image, decoded once.
pub struct ImageSource {
    size: Size,
    bgra: Vec<u8>,
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
            size: Size { width, height },
            bgra,
        }
    }

    /// Decode the file at `path` with the `image` crate.
    pub fn open(path: &Path) -> Result<ImageSource> {
        let img = image::open(path)
            .with_context(|| format!("decoding {}", path.display()))?
            .into_rgba8();
        Ok(ImageSource::from_image(img))
    }
}

impl FrameSource for ImageSource {
    fn size(&self) -> Size {
        self.size
    }
    fn aspect_ratio(&self) -> f64 {
        pixel_aspect(self.size)
    }
    fn next(&mut self) -> Result<&[u8]> {
        Ok(&self.bgra)
    }
}

/// A BGRA8 2D texture of `size` with `usage`, in shared memory on an
/// Apple-family GPU (Apple silicon, natively or under Rosetta), which has no
/// managed storage, and managed memory otherwise.
///
/// The family is asked rather than `hasUnifiedMemory`, which would be the
/// direct question: `supportsFamily:` is real on the capture proxy device
/// class where `hasUnifiedMemory` is not. See the module doc for why that
/// distinction matters under capture.
fn new_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    size: Size,
    usage: MTLTextureUsage,
    what: &str,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    // SAFETY: plain descriptor construction with a pixel format and a size.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
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
            "creating the {what} texture ({}x{})",
            size.width,
            size.height
        )
    })
}

/// Render `opts.warmup` uncaptured frames followed by `opts.frames`
/// recorded frames from `opts.source` through the preset, writing the
/// latter to `opts.output` as a `.gputrace`. The frame count passed to the
/// filter chain runs continuously across both phases.
pub fn run(mut opts: RenderOptions) -> Result<()> {
    let image_size = opts.source.size();
    check_texture_size(image_size).context("the source frame")?;
    let aspect = opts.aspect.ratio_or(opts.source.aspect_ratio());
    let size = output_size(&opts.window, image_size, aspect, &opts.screen);
    check_texture_size(size).context("the output")?;
    if opts.verbose {
        eprintln!(
            "source {}x{} at aspect {aspect:.4} -> output {}x{} px, {} warm-up + {} recorded frame(s)",
            image_size.width, image_size.height, size.width, size.height, opts.warmup, opts.frames
        );
    }

    let device = MTLCreateSystemDefaultDevice().context("no Metal device is available")?;
    let queue = device
        .newCommandQueue()
        .context("creating a Metal command queue")?;

    let input = new_texture(&device, image_size, MTLTextureUsage::ShaderRead, "input")?;
    let output = new_texture(
        &device,
        size,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        "output",
    )?;

    let mut chain = FilterChain::load_from_path(&opts.preset, ShaderFeatures::NONE, &queue, None)
        .with_context(|| format!("loading preset {}", opts.preset.display()))?;
    let viewport = Viewport::new_render_target_sized_origin(&*output, None)
        .context("sizing the viewport to the output texture")?;

    let upload = |bytes: &[u8]| -> Result<()> {
        if bytes.len() != image_size.width as usize * image_size.height as usize * 4 {
            bail!(
                "frame source yielded {} bytes for {}x{}",
                bytes.len(),
                image_size.width,
                image_size.height
            );
        }
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: image_size.width as usize,
                height: image_size.height as usize,
                depth: 1,
            },
        };
        let pixels =
            NonNull::new(bytes.as_ptr().cast_mut().cast()).context("frame buffer is null")?;
        // SAFETY: `bytes` holds exactly width * height * 4 bytes of BGRA8
        // with rows of width * 4 bytes (checked above), matching `region`
        // and the row stride, and Metal copies them before returning.
        unsafe {
            input.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                pixels,
                image_size.width as usize * 4,
            )
        };
        Ok(())
    };
    let mut render_one = |frame_count: usize| -> Result<()> {
        let cmd = queue
            .commandBuffer()
            .context("creating a Metal command buffer")?;
        chain
            .frame(&input, &viewport, &cmd, frame_count, None)
            .with_context(|| format!("rendering frame {frame_count}"))?;
        cmd.commit();
        cmd.waitUntilCompleted();
        Ok(())
    };

    // Both phases: a frame per call, the count running on across them.
    // Ctrl-C is polled between frames (see `interrupt`).
    let mut count = 0usize;
    let mut render_frames = |n: u32| -> Result<()> {
        for _ in 0..n {
            interrupt::check()?;
            upload(opts.source.next()?)?;
            render_one(count)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn image_source_swaps_rgba_to_bgra_and_repeats() {
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
        let a = src.next().unwrap().to_vec();
        assert_eq!(a, [0, 0, 255, 255, 255, 0, 0, 255]);
        assert_eq!(src.next().unwrap(), a);
    }

    #[test]
    fn image_source_open_decodes_the_fixture() {
        let src = ImageSource::open(Path::new("fixtures/sample.png")).unwrap();
        assert_eq!(
            src.size(),
            Size {
                width: 160,
                height: 144
            }
        );
        assert!(ImageSource::open(Path::new("Cargo.toml")).is_err());
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
}

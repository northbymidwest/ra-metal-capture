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

use crate::capture::prepare_output;
use crate::config::{Size, WindowMode};
use crate::display::Screen;
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
    /// The main display, for `Fill` and `Fullscreen`.
    pub screen: Screen,
    /// Frames rendered through the chain before the capture starts, so
    /// history passes see real prior frames in the first recorded one.
    /// Image mode uses 0.
    pub warmup: u32,
    /// Number of frames to render and record; the frame count advances by one each.
    pub frames: u32,
    /// Absolute path of the `.gputrace` to write.
    pub output: PathBuf,
    /// Print the decoded and output sizes to stderr.
    pub verbose: bool,
}

/// Where frames come from: a decoded image (the same bytes forever) or a
/// running libretro core (one emulated frame per call).
pub trait FrameSource {
    /// Size of every frame this source yields.
    fn size(&self) -> Size;
    /// The next frame as tightly packed BGRA8 rows, top row first,
    /// exactly `size().width * size().height * 4` bytes.
    fn next(&mut self) -> Result<Vec<u8>>;
}

/// A static image, decoded once.
pub struct ImageSource {
    size: Size,
    bgra: Vec<u8>,
}

impl ImageSource {
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
    fn next(&mut self) -> Result<Vec<u8>> {
        Ok(self.bgra.clone())
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
    prepare_output(&opts.output)?;
    let image_size = opts.source.size();
    let size = output_size(&opts.window, image_size, &opts.screen);
    if size.width == 0 || size.height == 0 {
        bail!(
            "the output size is {}x{}; refusing to create a zero-sized texture",
            size.width,
            size.height
        );
    }
    if opts.verbose {
        eprintln!(
            "source {}x{} -> output {}x{} px, {} warm-up + {} recorded frame(s)",
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

    let mut count = 0usize;
    for _ in 0..opts.warmup {
        upload(&opts.source.next()?)?;
        render_one(count)?;
        count += 1;
    }
    let trace = Trace::start(&device, &opts.output)?;
    for _ in 0..opts.frames {
        upload(&opts.source.next()?)?;
        render_one(count)?;
        count += 1;
    }
    trace.finish();

    if !opts.output.join("index").exists() {
        bail!(
            "the Metal capture finished but {} has no `index` entry; the bundle looks incomplete",
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

/// Scale `image` by one factor so it fits inside `max`, keeping its aspect
/// ratio and flooring to whole pixels, the way RetroArch's fill mode clamps
/// a scaled window. Falls back to the image's own size when the factor
/// would be zero or produce an empty texture (a zero `max`, or an image
/// with a zero dimension).
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

/// The output texture size, in pixels, for a window mode. RetroArch's modes
/// are in points; this maps each to pixels on `screen` so both backends
/// produce a similarly sized frame. `Exact` is taken as pixels as given.
pub fn output_size(mode: &WindowMode, image: Size, screen: &Screen) -> Size {
    match mode {
        WindowMode::Exact(size) => *size,
        WindowMode::Scale(n) => Size {
            width: image.width.saturating_mul(*n),
            height: image.height.saturating_mul(*n),
        },
        WindowMode::Fullscreen => to_pixels(screen.full, screen.backing_scale),
        WindowMode::Fill { max } => fit(image, to_pixels(*max, screen.backing_scale)),
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
        let a = src.next().unwrap();
        assert_eq!(a, [0, 0, 255, 255, 255, 0, 0, 255]);
        assert_eq!(src.next().unwrap(), a);
    }

    #[test]
    fn image_source_open_decodes_the_fixture() {
        let src = ImageSource::open(Path::new("sample.png")).unwrap();
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
    fn exact_is_pixels_as_given() {
        let size = Size {
            width: 1600,
            height: 1440,
        };
        assert_eq!(
            output_size(&WindowMode::Exact(size), GB, &screen(2.0)),
            size
        );
    }

    #[test]
    fn scale_multiplies_the_image() {
        assert_eq!(
            output_size(&WindowMode::Scale(4), GB, &screen(2.0)),
            Size {
                width: 640,
                height: 576
            }
        );
    }

    #[test]
    fn fullscreen_is_the_full_frame_in_pixels() {
        assert_eq!(
            output_size(&WindowMode::Fullscreen, GB, &screen(2.0)),
            Size {
                width: 5120,
                height: 2880
            }
        );
        assert_eq!(
            output_size(&WindowMode::Fullscreen, GB, &screen(1.0)),
            Size {
                width: 2560,
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
            output_size(&WindowMode::Fill { max }, GB, &screen(2.0)),
            Size {
                width: 3071,
                height: 2764
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
            output_size(&WindowMode::Fill { max }, wide, &screen(1.0)),
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
            output_size(&WindowMode::Fill { max }, huge, &screen(1.0)),
            Size {
                width: 1000,
                height: 250
            }
        );
    }

    #[test]
    fn fill_falls_back_to_the_image_when_max_is_degenerate() {
        let zero = Size {
            width: 0,
            height: 0,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max: zero }, GB, &screen(2.0)),
            GB
        );
        let sliver = Size {
            width: 100,
            height: 0,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max: sliver }, GB, &screen(1.0)),
            GB
        );
    }
}

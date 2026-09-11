//! The librashader backend: render an image through a preset inside this
//! process and write the `.gputrace` with Metal's capture API. Sizing is
//! pure and tested; everything that touches Metal lives in [`run`] and
//! [`trace`], and is exercised only by a real run.

use crate::config::{Size, WindowMode};
use crate::display::Screen;

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
            width: image.width * n,
            height: image.height * n,
        },
        WindowMode::Fullscreen => to_pixels(screen.full, screen.backing_scale),
        WindowMode::Fill { max } => fit(image, to_pixels(*max, screen.backing_scale)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

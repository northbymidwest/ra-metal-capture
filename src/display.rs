//! The main display's visible size, for the default fill-the-screen window
//! mode. Queries AppKit; falls back to 1920x1080 off the main thread.

use crate::config::{Size, WindowMode};
use objc2_app_kit::NSScreen;
use objc2_foundation::MainThreadMarker;

/// Allowance for the window title bar, which the visible frame does not exclude.
pub const TITLE_BAR_POINTS: u32 = 28;

const FALLBACK: Size = Size {
    width: 1920,
    height: 1080,
};

/// Width and height in points of the main display's visible frame
/// (excludes the menu bar and dock). Falls back to 1920x1080 with a
/// warning on stderr when no screen can be queried.
pub fn visible_size() -> Size {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("warning: not on the main thread; assuming a 1920x1080 display");
        return FALLBACK;
    };
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        eprintln!("warning: no main screen; assuming a 1920x1080 display");
        return FALLBACK;
    };
    let frame = screen.visibleFrame();
    Size {
        width: frame.size.width as u32,
        height: frame.size.height as u32,
    }
}

/// The fill-the-screen window mode for a given visible size.
pub fn fill_mode(visible: Size) -> WindowMode {
    WindowMode::Fill {
        max: Size {
            width: visible.width,
            height: visible.height.saturating_sub(TITLE_BAR_POINTS),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_mode_subtracts_title_bar() {
        assert_eq!(
            fill_mode(Size {
                width: 2488,
                height: 1410
            }),
            WindowMode::Fill {
                max: Size {
                    width: 2488,
                    height: 1382
                }
            }
        );
    }

    #[test]
    fn fill_mode_saturates_on_tiny_heights() {
        assert_eq!(
            fill_mode(Size {
                width: 100,
                height: 10
            }),
            WindowMode::Fill {
                max: Size {
                    width: 100,
                    height: 0
                }
            }
        );
    }
}

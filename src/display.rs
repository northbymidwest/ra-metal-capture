//! The main display: its visible and full frames in points and its
//! backing scale, for the default fill-the-screen window mode and for
//! sizing the librashader backend's output in pixels. Queries AppKit;
//! falls back to 1920x1080 at 1x off the main thread.

use crate::config::{Size, WindowMode};
use objc2_app_kit::NSScreen;
use objc2_foundation::MainThreadMarker;

/// Allowance for the window title bar, which the visible frame does not exclude.
pub const TITLE_BAR_POINTS: u32 = 28;

const FALLBACK: Size = Size {
    width: 1920,
    height: 1080,
};

/// The main display as the tool sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// Visible frame in points; excludes the menu bar and dock.
    pub visible: Size,
    /// The whole display in points.
    pub full: Size,
    /// Pixels per point, for example 2.0 on a Retina display.
    pub backing_scale: f64,
}

const FALLBACK_SCREEN: Screen = Screen {
    visible: FALLBACK,
    full: FALLBACK,
    backing_scale: 1.0,
};

/// The main display's visible frame, full frame, and backing scale. Falls
/// back to 1920x1080 at 1x with a warning on stderr when no screen can be
/// queried.
pub fn main_screen() -> Screen {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("warning: not on the main thread; assuming a 1920x1080 display at 1x");
        return FALLBACK_SCREEN;
    };
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        eprintln!("warning: no main screen; assuming a 1920x1080 display at 1x");
        return FALLBACK_SCREEN;
    };
    let visible = screen.visibleFrame();
    let full = screen.frame();
    Screen {
        visible: Size {
            width: visible.size.width as u32,
            height: visible.size.height as u32,
        },
        full: Size {
            width: full.size.width as u32,
            height: full.size.height as u32,
        },
        backing_scale: screen.backingScaleFactor(),
    }
}

/// Width and height in points of the main display's visible frame
/// (excludes the menu bar and dock). See [`main_screen`] for the fallback.
pub fn visible_size() -> Size {
    main_screen().visible
}

/// A window mode as RetroArch should see it: a `Fill` loses the title bar
/// allowance, since RetroArch's window has one and the visible frame does
/// not exclude it. Other modes pass through.
pub fn for_retroarch_window(mode: WindowMode) -> WindowMode {
    match mode {
        WindowMode::Fill { max } => fill_mode(max),
        other => other,
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
    fn for_retroarch_window_trims_only_fill() {
        let visible = Size {
            width: 2488,
            height: 1410,
        };
        assert_eq!(
            for_retroarch_window(WindowMode::Fill { max: visible }),
            fill_mode(visible)
        );
        assert_eq!(
            for_retroarch_window(WindowMode::Scale(3)),
            WindowMode::Scale(3)
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

    #[test]
    fn visible_size_matches_main_screen() {
        // Both are AppKit queries (or the same fallback off the main thread),
        // so they must agree; this pins visible_size to main_screen.
        assert_eq!(visible_size(), main_screen().visible);
    }

    #[test]
    fn fallback_screen_is_1080p_at_1x() {
        assert_eq!(FALLBACK_SCREEN.visible, FALLBACK);
        assert_eq!(FALLBACK_SCREEN.full, FALLBACK);
        assert_eq!(FALLBACK_SCREEN.backing_scale, 1.0);
    }
}

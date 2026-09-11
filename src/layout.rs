//! Where RetroArch keeps things on macOS, and how a hosted core's inferred
//! paths (a bare core name, a save-state slot, the system directory) are
//! resolved against that layout: the defaults first, `retroarch.cfg` only
//! when a default candidate is missing.

use crate::{config, state};
use std::path::{Path, PathBuf};

/// Where RetroArch keeps the things a hosted core needs. The values come
/// from RetroArch's Darwin platform driver: user documents under
/// `~/Documents/RetroArch`, hidden app data under
/// `~/Library/Application Support/RetroArch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetroArchDirs {
    pub libretro_dir: PathBuf,
    pub system_dir: PathBuf,
    pub states: state::StateDirs,
}

impl RetroArchDirs {
    /// RetroArch's macOS defaults, with states sorted into per-core folders.
    pub fn defaults() -> RetroArchDirs {
        RetroArchDirs {
            libretro_dir: config::expand_tilde("~/Library/Application Support/RetroArch/cores"),
            system_dir: config::expand_tilde("~/Documents/RetroArch/system"),
            states: state::StateDirs {
                savestate_directory: config::expand_tilde("~/Documents/RetroArch/states"),
                sort_by_core: true,
                sort_by_content: false,
                in_content_dir: false,
            },
        }
    }

    /// The directories a `retroarch.cfg` names, with the defaults filling
    /// any key it leaves out.
    pub fn from_config(text: &str) -> RetroArchDirs {
        let keys = config::read_all(text);
        let base = RetroArchDirs::defaults();
        let dir = |key: &str, default: PathBuf| {
            keys.get(key)
                .map(|s| config::expand_tilde(s))
                .unwrap_or(default)
        };
        let flag = |key: &str, default: bool| keys.get(key).map(|v| v == "true").unwrap_or(default);
        RetroArchDirs {
            libretro_dir: dir("libretro_directory", base.libretro_dir),
            system_dir: dir("system_directory", base.system_dir),
            states: state::StateDirs {
                savestate_directory: dir("savestate_directory", base.states.savestate_directory),
                sort_by_core: flag("sort_savestates_enable", base.states.sort_by_core),
                sort_by_content: flag(
                    "sort_savestates_by_content_enable",
                    base.states.sort_by_content,
                ),
                in_content_dir: flag("savestates_in_content_dir", base.states.in_content_dir),
            },
        }
    }
}

/// What [`DirResolver::locate`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// The first candidate that exists.
    Found(PathBuf),
    /// No candidate exists; every one tried, the default first.
    Missing(Vec<PathBuf>),
}

/// Resolves paths inferred from RetroArch's layout: the defaults are tried
/// first, and `retroarch.cfg` is parsed (once, lazily) only when a default
/// candidate does not exist. A machine without RetroArch therefore works
/// with the default layout and never needs a config file.
pub struct DirResolver<'a> {
    defaults: RetroArchDirs,
    /// Reads the config text, or `None` when there is no config file.
    load_config: Box<dyn FnOnce() -> Option<String> + 'a>,
    /// `None` until first needed; then the parsed config, or `None` if absent.
    config: Option<Option<RetroArchDirs>>,
    verbose: bool,
}

impl<'a> DirResolver<'a> {
    pub fn new(
        defaults: RetroArchDirs,
        load_config: impl FnOnce() -> Option<String> + 'a,
        verbose: bool,
    ) -> DirResolver<'a> {
        DirResolver {
            defaults,
            load_config: Box::new(load_config),
            config: None,
            verbose,
        }
    }

    /// The resolver for the `retroarch.cfg` at `path`, absent or not; with
    /// `None` the defaults are the only layout ever tried.
    pub fn for_config(path: Option<&'a Path>, verbose: bool) -> DirResolver<'a> {
        DirResolver::new(
            RetroArchDirs::defaults(),
            move || path.and_then(|p| std::fs::read_to_string(p).ok()),
            verbose,
        )
    }

    /// `pick` maps a layout to the candidate path; `exists` says whether a
    /// candidate is usable. The default layout is tried first, then the
    /// config's, parsed on first need.
    pub fn locate(
        &mut self,
        what: &str,
        pick: impl Fn(&RetroArchDirs) -> PathBuf,
        exists: impl Fn(&Path) -> bool,
    ) -> Located {
        let candidate = pick(&self.defaults);
        if exists(&candidate) {
            return Located::Found(candidate);
        }
        let mut tried = vec![candidate];
        if self.config.is_none() {
            let loader = std::mem::replace(&mut self.load_config, Box::new(|| None));
            self.config = Some(loader().map(|text| RetroArchDirs::from_config(&text)));
        }
        if let Some(Some(cfg)) = &self.config {
            let candidate = pick(cfg);
            if candidate != tried[0] && exists(&candidate) {
                if self.verbose {
                    eprintln!(
                        "{what}: not at the default {}; using {} from retroarch.cfg",
                        tried[0].display(),
                        candidate.display()
                    );
                }
                return Located::Found(candidate);
            }
            if candidate != tried[0] {
                tried.push(candidate);
            }
        }
        Located::Missing(tried)
    }
}

/// The candidates a [`Located::Missing`] tried, for an error message.
pub fn describe_tried(tried: &[PathBuf]) -> String {
    tried
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

/// Warm-up frames for a settle of `settle` seconds at `fps`, with 60 fps
/// substituted when a core reports none (RetroArch's own fallback).
/// Warm-up frames for `--settle` seconds at the core's own frame rate. A
/// core that reports no frame rate is run at 60 fps, RetroArch's fallback,
/// so the settle is a wait rather than nothing.
pub fn settle_frames(settle: f64, fps: f64) -> u32 {
    let fps = if fps > 0.0 { fps } else { 60.0 };
    (settle * fps).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retroarch_dirs_from_config_overrides_only_named_keys() {
        let text =
            "savestate_directory = \"/elsewhere/states\"\nsort_savestates_enable = \"false\"\n";
        let d = RetroArchDirs::from_config(text);
        let base = RetroArchDirs::defaults();
        assert_eq!(d.libretro_dir, base.libretro_dir);
        assert_eq!(d.system_dir, base.system_dir);
        assert_eq!(
            d.states.savestate_directory,
            PathBuf::from("/elsewhere/states")
        );
        assert!(!d.states.sort_by_core);
        assert!(!d.states.sort_by_content);
        assert!(!d.states.in_content_dir);
        assert!(base.states.sort_by_core);
    }

    fn fake_dirs(system: &str) -> RetroArchDirs {
        RetroArchDirs {
            libretro_dir: PathBuf::from("/d/cores"),
            system_dir: PathBuf::from(system),
            states: RetroArchDirs::defaults().states,
        }
    }

    #[test]
    fn resolver_takes_the_default_without_reading_the_config() {
        use std::cell::Cell;
        let loaded = Cell::new(false);
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || {
                loaded.set(true);
                Some("system_directory = \"/c/system\"".into())
            },
            false,
        );
        let hit = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/d/system"),
        );
        assert_eq!(hit, Located::Found(PathBuf::from("/d/system")));
        assert!(
            !loaded.get(),
            "config must not be read when the default exists"
        );
    }

    #[test]
    fn resolver_falls_back_to_the_config_and_reads_it_once() {
        use std::cell::Cell;
        let loads = Cell::new(0);
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || {
                loads.set(loads.get() + 1);
                Some("system_directory = \"/c/system\"".into())
            },
            false,
        );
        let hit = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/c/system"),
        );
        assert_eq!(hit, Located::Found(PathBuf::from("/c/system")));
        let again = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/c/system"),
        );
        assert_eq!(again, Located::Found(PathBuf::from("/c/system")));
        assert_eq!(loads.get(), 1, "config parsed once");
    }

    #[test]
    fn resolver_reports_every_candidate_when_nothing_exists() {
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || Some("system_directory = \"/c/system\"".into()),
            false,
        );
        let hit = r.locate("system", |d| d.system_dir.clone(), |_| false);
        let tried = vec![PathBuf::from("/d/system"), PathBuf::from("/c/system")];
        assert_eq!(hit, Located::Missing(tried.clone()));
        assert_eq!(describe_tried(&tried), "/d/system or /c/system");
    }

    #[test]
    fn for_config_none_never_consults_a_file() {
        let mut r = DirResolver::for_config(None, false);
        let hit = r.locate("core", |d| d.libretro_dir.clone(), |_| false);
        assert_eq!(
            hit,
            Located::Missing(vec![RetroArchDirs::defaults().libretro_dir])
        );
    }

    #[test]
    fn resolver_without_a_config_file_tries_the_default_only() {
        let mut r = DirResolver::new(fake_dirs("/d/system"), || None, false);
        let hit = r.locate("system", |d| d.system_dir.clone(), |_| false);
        assert_eq!(hit, Located::Missing(vec![PathBuf::from("/d/system")]));
    }

    #[test]
    fn settle_frames_uses_sixty_fps_when_the_core_reports_none() {
        assert_eq!(settle_frames(5.0, 59.728), 299);
        assert_eq!(settle_frames(5.0, 0.0), 300);
        assert_eq!(settle_frames(0.0, 59.728), 0);
    }
}

//! Assembling the RetroArch command line and environment from a
//! [`LaunchPlan`].

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Everything needed to assemble the RetroArch command line.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub binary: PathBuf,
    pub core: Option<PathBuf>,
    pub content: PathBuf,
    pub shader: PathBuf,
    /// The per-run `retroarch.cfg` RetroArch is launched on.
    pub config: PathBuf,
    pub fullscreen: bool,
    pub verbose: bool,
}

/// A fully assembled command: program, arguments in order, extra environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
}

/// Pure assembly of the RetroArch invocation.
/// Order: `[-L <core>] [-f] --set-shader <p> -c <cfg> --sram-mode
/// noload-nosave [-v] <content>`. The SRAM mode makes RetroArch neither
/// read nor write `.srm`/`.rtc` files for the run, so the user's saves
/// are never touched whatever directory the config names.
/// Env: `MTL_CAPTURE_ENABLED=1` so GPUToolsCapture loads into RetroArch.
pub fn build_command(plan: &LaunchPlan) -> LaunchCommand {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(core) = &plan.core {
        args.push("-L".into());
        args.push(core.as_os_str().into());
    }
    if plan.fullscreen {
        args.push("-f".into());
    }
    args.push("--set-shader".into());
    args.push(plan.shader.as_os_str().into());
    args.push("-c".into());
    args.push(plan.config.as_os_str().into());
    args.push("--sram-mode".into());
    args.push("noload-nosave".into());
    if plan.verbose {
        args.push("-v".into());
    }
    args.push(plan.content.as_os_str().into());
    LaunchCommand {
        program: plan.binary.clone(),
        args,
        env: vec![("MTL_CAPTURE_ENABLED".to_string(), "1".to_string())],
    }
}

impl LaunchCommand {
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).envs(self.env.iter().cloned());
        cmd
    }

    /// One-line rendering for `--verbose` output.
    pub fn display(&self) -> String {
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| format!("{:?}", a.to_string_lossy()))
            .collect();
        format!(
            "{} {} {}",
            env.join(" "),
            self.program.display(),
            args.join(" ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> LaunchPlan {
        LaunchPlan {
            binary: PathBuf::from("/Applications/RetroArch.app/Contents/MacOS/RetroArch"),
            core: Some(PathBuf::from("/cores/sameboy_libretro.dylib")),
            content: PathBuf::from("/roms/z.gb"),
            shader: PathBuf::from("/shaders/crt.slangp"),
            config: PathBuf::from("/tmp/run/retroarch.cfg"),
            fullscreen: false,
            verbose: false,
        }
    }

    fn strs(cmd: &LaunchCommand) -> Vec<String> {
        cmd.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn minimal_windowed_launch() {
        let cmd = build_command(&plan());
        assert_eq!(cmd.program, plan().binary);
        assert_eq!(
            strs(&cmd),
            [
                "-L",
                "/cores/sameboy_libretro.dylib",
                "--set-shader",
                "/shaders/crt.slangp",
                "-c",
                "/tmp/run/retroarch.cfg",
                "--sram-mode",
                "noload-nosave",
                "/roms/z.gb",
            ]
        );
        assert_eq!(
            cmd.env,
            vec![("MTL_CAPTURE_ENABLED".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn fullscreen_and_verbose_in_order() {
        let mut p = plan();
        p.fullscreen = true;
        p.verbose = true;
        let cmd = build_command(&p);
        assert_eq!(
            strs(&cmd),
            [
                "-L",
                "/cores/sameboy_libretro.dylib",
                "-f",
                "--set-shader",
                "/shaders/crt.slangp",
                "-c",
                "/tmp/run/retroarch.cfg",
                "--sram-mode",
                "noload-nosave",
                "-v",
                "/roms/z.gb",
            ]
        );
    }

    #[test]
    fn display_shows_env_program_and_quoted_args() {
        let s = build_command(&plan()).display();
        assert!(
            s.starts_with("MTL_CAPTURE_ENABLED=1 /Applications/RetroArch.app"),
            "{s}"
        );
        assert!(s.ends_with("\"/roms/z.gb\""), "{s}");
    }

    #[test]
    fn image_content_has_no_core_flag() {
        let mut p = plan();
        p.core = None;
        p.content = PathBuf::from("/img/sample.png");
        assert_eq!(
            strs(&build_command(&p)),
            [
                "--set-shader",
                "/shaders/crt.slangp",
                "-c",
                "/tmp/run/retroarch.cfg",
                "--sram-mode",
                "noload-nosave",
                "/img/sample.png"
            ]
        );
    }
}

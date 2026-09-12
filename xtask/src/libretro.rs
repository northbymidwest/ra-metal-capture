//! The vendored libretro.h and the bindings generated from it.
//!
//! `update` finds the newest RetroArch commit that touched
//! `libretro-common/include/libretro.h` (or takes one), downloads the
//! header at that commit, and writes it, its commit, and its README under
//! `third_party/libretro/`, then regenerates. `regen` regenerates from what
//! is vendored, for changes to the generator itself.
//!
//! Bindings are types and constants only: the functions a core exports are
//! resolved by name through libloading and the callbacks live in `env.rs`,
//! so no extern block is wanted. The header's per-item Doxygen comments are
//! turned into rustdoc by doxygen-bindgen, after `@code` blocks are fenced
//! with their language so rustdoc never runs one as a Rust doctest. The
//! header's leading file comment, which holds its license, is copied
//! verbatim since bindgen only carries comments attached to declarations.

use anyhow::{Context, Result, bail};
use bindgen::callbacks::ParseCallbacks;
use clap::Subcommand;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

#[derive(Debug)]
struct Doxygen;

impl ParseCallbacks for Doxygen {
    fn process_comment(&self, comment: &str) -> Option<String> {
        let prepared = join_dangling_see(&fence_code_blocks(comment));
        // A comment doxygen-bindgen refuses is kept as prepared (still
        // fenced, so never a doctest) and reported.
        match doxygen_bindgen::transform(&prepared) {
            Ok(rustdoc) => Some(move_punctuation_out_of_links(&rustdoc)),
            Err(e) => {
                eprintln!(
                    "doxygen-bindgen could not transform a comment ({e}); kept as is:\n{prepared}\n---"
                );
                Some(prepared)
            }
        }
    }
}

/// A `@see` or `@sa` alone on its line, with the reference on the next
/// one, is joined onto one line so the reference is what gets linked.
fn join_dangling_see(comment: &str) -> String {
    let lines: Vec<&str> = comment.lines().collect();
    let mut out = String::with_capacity(comment.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let bare = matches!(trimmed, "@see" | "@sa" | "\\see" | "\\sa");
        if bare && i + 1 < lines.len() {
            out.push_str(lines[i].trim_end());
            out.push(' ');
            out.push_str(lines[i + 1].trim());
            out.push('\n');
            i += 2;
        } else {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
        }
    }
    out
}

/// `@code{c}` / `@code{.c}` / `@code` ... `@endcode` (or the backslash
/// forms) become fenced blocks tagged with the language, `text` when none
/// is given, so rustdoc treats them as display, not doctests.
fn fence_code_blocks(comment: &str) -> String {
    let mut out = String::with_capacity(comment.len());
    for line in comment.lines() {
        let trimmed = line.trim_start();
        let is_code = trimmed.starts_with("@code") || trimmed.starts_with("\\code");
        let is_end = trimmed.starts_with("@endcode") || trimmed.starts_with("\\endcode");
        if is_code {
            let lang = trimmed
                .strip_prefix("@code")
                .or_else(|| trimmed.strip_prefix("\\code"))
                .unwrap_or("")
                .trim()
                .trim_start_matches('{')
                .trim_end_matches('}')
                .trim_start_matches('.')
                .trim();
            let lang = if lang.is_empty() { "text" } else { lang };
            let _ = writeln!(out, "```{lang}");
        } else if is_end {
            out.push_str("```\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// doxygen-bindgen takes the word after `@see` or `\ref` up to the next
/// space, so a sentence-ending mark rides along inside the link:
/// `` [`NAME.`] `` becomes `` [`NAME`]. ``.
fn move_punctuation_out_of_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[`") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("`]") {
            Some(end) => {
                let inner = &after[..end];
                let trimmed = inner.trim_end_matches(['.', ',', ';', ':']);
                out.push_str("[`");
                out.push_str(trimmed);
                out.push_str("`]");
                out.push_str(&inner[trimmed.len()..]);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("[`");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The header's leading block comment, verbatim, demoted from a doc
/// comment (`/*!`) to a plain one so rustdoc leaves it alone.
fn leading_comment(header: &str) -> String {
    let mut out = String::new();
    for line in header.lines() {
        out.push_str(line);
        out.push('\n');
        if line.contains("*/") {
            break;
        }
    }
    out.replacen("/*!", "/*", 1)
}

const REPO: &str = "libretro/RetroArch";
const HEADER_PATH: &str = "libretro-common/include/libretro.h";
const VENDOR_DIR: &str = "third_party/libretro";
const OUTPUT: &str = "src/hosted/libretro/sys.rs";
const USER_AGENT: &str = "ra-metal-capture-xtask";

#[derive(Subcommand)]
pub enum Command {
    /// Vendor the header at the newest RetroArch commit that changed it
    /// (or at --commit), then regenerate the bindings
    Update {
        /// A RetroArch commit to vendor instead of the newest
        #[arg(long)]
        commit: Option<String>,
    },
    /// Regenerate the bindings from the vendored header
    Regen,
}

pub fn run(command: Command) -> Result<()> {
    let root = repo_root()?;
    let vendor = root.join(VENDOR_DIR);
    match command {
        Command::Update { commit } => {
            let commit = match commit {
                Some(c) => c,
                None => newest_header_commit()?,
            };
            check_commit(&commit)?;
            let current = std::fs::read_to_string(vendor.join("COMMIT"))
                .map(|c| c.trim().to_string())
                .unwrap_or_default();
            if current == commit {
                println!("libretro.h is already at {commit}");
            } else {
                let header = download_header(&commit)?;
                std::fs::create_dir_all(&vendor)?;
                std::fs::write(vendor.join("libretro.h"), &header)?;
                std::fs::write(vendor.join("COMMIT"), format!("{commit}\n"))?;
                std::fs::write(vendor.join("README.md"), readme(&commit))?;
                println!("vendored libretro.h at {commit} (was {current})");
            }
        }
        Command::Regen => {}
    }
    let commit = std::fs::read_to_string(vendor.join("COMMIT"))
        .with_context(|| format!("reading {}/COMMIT", vendor.display()))?
        .trim()
        .to_string();
    check_commit(&commit)?;
    let output = root.join(OUTPUT);
    generate(&vendor.join("libretro.h"), &commit, &output)?;
    rustfmt(&output)?;
    println!(
        "wrote {}",
        output.strip_prefix(&root).unwrap_or(&output).display()
    );
    Ok(())
}

/// The repository root: the parent of this crate's manifest directory.
fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask has no parent directory")
}

fn check_commit(commit: &str) -> Result<()> {
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected a full 40-hex-digit RetroArch commit hash, got {commit:?}");
    }
    Ok(())
}

/// A GET with the GitHub-friendly headers; `GITHUB_TOKEN` raises the
/// unauthenticated rate limit when set. ureq connects to the first
/// address a host resolves to and does not fall back to another family,
/// so a host with IPv6 addresses fails on a machine without an IPv6
/// route; a transport error is retried over IPv4 only.
fn get(url: &str) -> Result<String> {
    use ureq::config::IpFamily;
    let attempt = |family: IpFamily| -> std::result::Result<String, ureq::Error> {
        let agent =
            ureq::Agent::new_with_config(ureq::Agent::config_builder().ip_family(family).build());
        let mut request = agent.get(url).header("User-Agent", USER_AGENT);
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
        {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        request.call()?.body_mut().read_to_string()
    };
    match attempt(IpFamily::Any) {
        Ok(body) => Ok(body),
        Err(ureq::Error::Io(_)) => {
            attempt(IpFamily::Ipv4Only).with_context(|| format!("GET {url}"))
        }
        Err(e) => Err(e).with_context(|| format!("GET {url}")),
    }
}

/// The newest commit on RetroArch's default branch that changed the
/// header: the point at which its current content was introduced.
fn newest_header_commit() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/commits?path={HEADER_PATH}&per_page=1");
    let body = get(&url)?;
    let json: serde_json::Value = serde_json::from_str(&body).context("parsing the commit list")?;
    json[0]["sha"]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("no commit in the response for {HEADER_PATH}: {body}"))
}

fn download_header(commit: &str) -> Result<String> {
    let text = get(&format!(
        "https://raw.githubusercontent.com/{REPO}/{commit}/{HEADER_PATH}"
    ))?;
    if let Some(bad) = text.bytes().find(|b| !b.is_ascii()) {
        bail!("libretro.h at {commit} contains a non-ASCII byte {bad:#x}; the tree is ASCII-only");
    }
    Ok(text)
}

fn header_url(commit: &str) -> String {
    format!("https://github.com/{REPO}/blob/{commit}/{HEADER_PATH}")
}

fn readme(commit: &str) -> String {
    format!(
        "# libretro.h\n\n\
         `libretro.h` is the libretro API header, copied verbatim from RetroArch at\n\
         commit [`{short}`]({url})\n\
         (`{HEADER_PATH}`; the full hash is in `COMMIT`). Its license is in the\n\
         comment at the top of the file and applies to that file only.\n\n\
         `{OUTPUT}` is generated from it. `cargo xtask libretro update` vendors the\n\
         header at the newest commit that changed it and regenerates; `cargo xtask\n\
         libretro regen` regenerates from what is here. This file is written by\n\
         `update`.\n",
        short = &commit[..12],
        url = header_url(commit),
    )
}

fn generate(header: &Path, commit: &str, output: &Path) -> Result<()> {
    let header_text =
        std::fs::read_to_string(header).with_context(|| format!("reading {}", header.display()))?;
    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .parse_callbacks(Box::new(Doxygen))
        .allowlist_type("retro_.*")
        .allowlist_var("RETRO_.*")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .use_core()
        .layout_tests(false)
        .ignore_functions()
        .clang_arg("-xc")
        .generate()
        .context("bindgen failed")?;
    let mut text = format!(
        "//! libretro's types and constants, generated from\n\
         //! `{VENDOR_DIR}/libretro.h` by `cargo xtask libretro regen`.\n\
         //! The header is RetroArch's `{HEADER_PATH}` at commit\n\
         //! {commit}:\n\
         //! <{url}>\n\
         //! Do not edit; `cargo xtask libretro update` replaces the header and this\n\
         //! file together. The header's leading comment, with its license, follows\n\
         //! verbatim.\n",
        url = header_url(commit),
    );
    text.push_str(&leading_comment(&header_text));
    text.push_str(
        "#![allow(\n    non_camel_case_types,\n    non_snake_case,\n    non_upper_case_globals,\n    dead_code,\n    clippy::all\n)]\n\n",
    );
    text.push_str(&bindings.to_string());
    if let Some(bad) = text.bytes().find(|b| !b.is_ascii()) {
        bail!("generated bindings contain a non-ASCII byte {bad:#x}");
    }
    std::fs::write(output, text).with_context(|| format!("writing {}", output.display()))
}

fn rustfmt(path: &Path) -> Result<()> {
    let status = Process::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(path)
        .status()
        .context("running rustfmt")?;
    if !status.success() {
        bail!("rustfmt failed with {status}");
    }
    Ok(())
}

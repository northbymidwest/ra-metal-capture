//! Generates the libretro bindings module. Usage: `<header> <output>`.
//!
//! Types and constants only: the functions a core exports are resolved by
//! name through libloading and the callbacks live in `env.rs`, so no
//! extern block is wanted. The header's per-item Doxygen comments are
//! turned into rustdoc by doxygen-bindgen, after `@code` blocks are fenced
//! with their language so rustdoc never runs one as a Rust doctest. The header's
//! leading file comment, which holds its license, is copied verbatim since
//! bindgen only carries comments attached to declarations.

use bindgen::callbacks::ParseCallbacks;
use std::fmt::Write as _;
use std::path::Path;

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
                eprintln!("doxygen-bindgen could not transform a comment ({e}); kept as is:\n{prepared}\n---");
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, header, output] = args.as_slice() else {
        eprintln!("usage: regen-libretro-bindings <libretro.h> <sys.rs>");
        std::process::exit(2);
    };
    let header_text = std::fs::read_to_string(header).expect("reading the header");
    let bindings = bindgen::Builder::default()
        .header(header)
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
        .expect("bindgen failed");
    let mut text = String::from(
        "//! libretro's types and constants, generated from\n\
         //! `third_party/libretro/libretro.h` by `scripts/regen-libretro-bindings.sh`.\n\
         //! Do not edit; rerun the script after replacing the header. The header's\n\
         //! leading comment, with its license, follows verbatim.\n",
    );
    text.push_str(&leading_comment(&header_text));
    text.push_str(
        "#![allow(\n    non_camel_case_types,\n    non_snake_case,\n    non_upper_case_globals,\n    dead_code,\n    clippy::all\n)]\n\n",
    );
    text.push_str(&bindings.to_string());
    if let Some(bad) = text.bytes().find(|b| !b.is_ascii()) {
        panic!("generated bindings contain a non-ASCII byte {bad:#x}");
    }
    std::fs::write(output, text).expect("writing the output");
    println!("wrote {}", Path::new(output).display());
}

//! `cargo xtask`: repository maintenance tasks. See each subcommand's help.

mod libretro;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Repository maintenance tasks for ra-metal-capture")]
struct Xtask {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The vendored libretro.h and the bindings generated from it
    Libretro {
        #[command(subcommand)]
        command: libretro::Command,
    },
}

fn main() -> Result<()> {
    match Xtask::parse().command {
        Command::Libretro { command } => libretro::run(command),
    }
}

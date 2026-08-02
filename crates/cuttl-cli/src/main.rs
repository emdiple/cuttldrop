//! `cuttl` — Cuttldrop CLI. The M0 observable is:
//!
//! ```sh
//! cuttl encode f.bin -o pulses/ && cuttl decode pulses/ -o out.bin && cmp f.bin out.bin
//! cuttl decode pulses/ --distort heavy --loss 0.6 -o out.bin   # still identical
//! ```

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "cuttl",
    version,
    about = "Cuttldrop optical file transfer — encode/decode pulse sequences"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode a file into a directory of pulse PNGs (the skin, offline)
    Encode {
        /// Input file
        input: PathBuf,
        /// Output directory for pulse PNGs
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Decode a directory of pulse PNGs back into a file (the eye, offline)
    Decode {
        /// Directory of pulse PNGs
        input: PathBuf,
        /// Reconstructed output file
        #[arg(short, long)]
        output: PathBuf,
        /// Apply the synthetic optical channel before decoding: none|light|heavy
        #[arg(long, default_value = "none")]
        distort: String,
        /// Fraction of pulses to drop, 0.0–1.0
        #[arg(long, default_value_t = 0.0)]
        loss: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Encode { .. } => anyhow::bail!("not implemented — lands in M0 (see DESIGN.md §5)"),
        Command::Decode { .. } => anyhow::bail!("not implemented — lands in M0 (see DESIGN.md §5)"),
    }
}

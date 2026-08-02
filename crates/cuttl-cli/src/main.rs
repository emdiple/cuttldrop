//! `cuttl` — Cuttldrop CLI. The M0 observable is:
//!
//! ```sh
//! cuttl encode f.bin -o pulses/ && cuttl decode pulses/ -o out.bin && cmp f.bin out.bin
//! cuttl decode pulses/ --distort heavy --loss 0.6 -o out.bin   # still identical
//! ```
//!
//! The first line works today. The second needs the synthetic channel and the
//! fountain layer, which are M0 step 2 — until then it exits with an error
//! rather than silently ignoring the flags.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use cuttl_codec::{Grid, Palette, Reassembler, stream};
use std::path::{Path, PathBuf};

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

/// Grid + palette pairing. The eye will learn this from the beacon once the
/// beacon exists (M1); until then both ends are told explicitly.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum Profile {
    /// 48×27 mono — the M1 air-gap profile
    #[default]
    M1,
    /// 96×54 eight-colour — the M3 profile
    M3,
}

impl Profile {
    fn parts(self) -> (Grid, Palette) {
        match self {
            Profile::M1 => (Grid::M1_MONO, Palette::Mono1),
            Profile::M3 => (Grid::M3_COLOR, Palette::Color3),
        }
    }
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
        /// Grid and palette profile
        #[arg(long, value_enum, default_value_t = Profile::M1)]
        profile: Profile,
        /// Pixels per chroma cell
        #[arg(long, default_value_t = cuttl_sim::DEFAULT_CELL_PX)]
        cell_px: u32,
        /// Stream id, so the eye can ignore a foreign transfer in view
        #[arg(long, default_value_t = 1)]
        stream_id: u32,
    },
    /// Decode a directory of pulse PNGs back into a file (the eye, offline)
    Decode {
        /// Directory of pulse PNGs
        input: PathBuf,
        /// Reconstructed output file
        #[arg(short, long)]
        output: PathBuf,
        /// Grid and palette profile
        #[arg(long, value_enum, default_value_t = Profile::M1)]
        profile: Profile,
        /// Apply the synthetic optical channel before decoding: none|light|heavy
        #[arg(long, default_value = "none")]
        distort: String,
        /// Fraction of pulses to drop, 0.0–1.0
        #[arg(long, default_value_t = 0.0)]
        loss: f64,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Encode {
            input,
            output,
            profile,
            cell_px,
            stream_id,
        } => encode(&input, &output, profile, cell_px, stream_id),
        Command::Decode {
            input,
            output,
            profile,
            distort,
            loss,
        } => {
            if distort != "none" || loss != 0.0 {
                bail!(
                    "--distort/--loss need the synthetic channel and the fountain layer \
                     (M0 step 2). The carousel in cuttl-codec::stream requires every \
                     pulse, so any loss is fatal by construction — see DESIGN.md §3c."
                );
            }
            decode(&input, &output, profile)
        }
    }
}

fn encode(
    input: &Path,
    output: &Path,
    profile: Profile,
    cell_px: u32,
    stream_id: u32,
) -> Result<()> {
    if cell_px == 0 {
        bail!("--cell-px must be at least 1");
    }
    let (grid, palette) = profile.parts();
    let object = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;

    let pulses = stream::encode(&object, grid, palette, stream_id)?;
    std::fs::create_dir_all(output).with_context(|| format!("creating {}", output.display()))?;

    for (i, pulse) in pulses.iter().enumerate() {
        let path = output.join(format!("pulse-{i:06}.png"));
        cuttl_sim::render(pulse, cell_px)
            .save(&path)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let chunk = stream::chunk_capacity(grid, palette)?;
    let payload = grid.payload_bytes(palette);
    println!(
        "skin: {} B -> {} pulses  [{}×{} {:?}, {} px/cell]",
        object.len(),
        pulses.len(),
        grid.cols,
        grid.rows,
        palette,
        cell_px
    );
    println!(
        "      {payload} B/pulse payload, {} B header, {chunk} B/pulse goodput",
        stream::HEADER_LEN
    );
    println!("      {}", output.display());
    Ok(())
}

fn decode(input: &Path, output: &Path, profile: Profile) -> Result<()> {
    let (grid, palette) = profile.parts();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(input)
        .with_context(|| format!("reading {}", input.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!("no PNG pulses found in {}", input.display());
    }

    let mut rx = Reassembler::new();
    let mut unreadable = 0u32;
    for path in &paths {
        let image = image::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .to_rgb8();
        match cuttl_sim::sample(&image, grid, palette) {
            Ok(pulse) => {
                rx.ingest(&pulse);
            }
            // A frame the eye cannot even geometrically resolve is an erasure,
            // not a failure — the same treatment a CRC reject gets (§1b).
            Err(_) => unreadable += 1,
        }
    }

    let (have, need) = rx.progress();
    println!(
        "eye:  {} pulses seen, {have}/{need} chunks held, {} rejected, {unreadable} unreadable",
        paths.len(),
        rx.rejected()
    );

    let object = rx.finish()?;
    std::fs::write(output, &object).with_context(|| format!("writing {}", output.display()))?;
    println!(
        "      {} B -> {}  (object CRC verified)",
        object.len(),
        output.display()
    );
    Ok(())
}

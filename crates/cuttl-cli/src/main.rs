//! `cuttl` — Cuttldrop CLI. The M0 observable:
//!
//! ```sh
//! cuttl encode f.bin -o pulses/ && cuttl decode pulses/ -o out.bin && cmp f.bin out.bin
//! cuttl decode pulses/ --distort heavy --loss 0.6 -o out.bin   # still identical
//! ```
//!
//! Both lines work. `--distort` is photometric only for now — perspective warp
//! and rolling-shutter tear arrive in M0 step 3, once the eye can locate the
//! grid instead of being told where it is.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use cuttl_codec::{Grid, Palette, Receiver, stream};
use cuttl_sim::{Channel, Preset};
use rand::{RngExt, SeedableRng, rngs::StdRng};
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
    /// Delegates to `cuttl_codec::Profile`; this enum exists only because clap
    /// needs a `ValueEnum` and the codec crate must not depend on clap.
    fn parts(self) -> (Grid, Palette) {
        match self {
            Profile::M1 => cuttl_codec::Profile::M1,
            Profile::M3 => cuttl_codec::Profile::M3,
        }
        .parts()
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum Distort {
    #[default]
    None,
    Light,
    Heavy,
    /// Past what the stack survives without the inner RS code — see
    /// `cuttl_sim::Preset::Brutal`. Expected to fail; that is what it is for.
    Brutal,
}

impl From<Distort> for Preset {
    fn from(distort: Distort) -> Self {
        match distort {
            Distort::None => Preset::None,
            Distort::Light => Preset::Light,
            Distort::Heavy => Preset::Heavy,
            Distort::Brutal => Preset::Brutal,
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
        /// Repair symbols as a ratio of source symbols. A real skin is rateless
        /// and loops forever; a finite directory has to stand in for that, so
        /// this is what decides how much loss the output can survive.
        #[arg(long, default_value_t = 2.0)]
        overhead: f32,
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
        /// Photometric distortion to apply before decoding
        #[arg(long, value_enum, default_value_t = Distort::None)]
        distort: Distort,
        /// Fraction of pulses to drop, 0.0–1.0
        #[arg(long, default_value_t = 0.0)]
        loss: f64,
        /// Seed for distortion and loss, so runs are reproducible
        #[arg(long, default_value_t = 0)]
        seed: u64,
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
            overhead,
        } => encode(&input, &output, profile, cell_px, stream_id, overhead),
        Command::Decode {
            input,
            output,
            profile,
            distort,
            loss,
            seed,
        } => decode(&input, &output, profile, distort, loss, seed),
    }
}

fn encode(
    input: &Path,
    output: &Path,
    profile: Profile,
    cell_px: u32,
    stream_id: u32,
    overhead: f32,
) -> Result<()> {
    if cell_px == 0 {
        bail!("--cell-px must be at least 1");
    }
    if !overhead.is_finite() || overhead < 0.0 {
        bail!("--overhead must be a non-negative number");
    }
    let (grid, palette) = profile.parts();
    let object = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;

    let pulses = stream::encode(&object, grid, palette, stream_id, overhead)?;
    std::fs::create_dir_all(output).with_context(|| format!("creating {}", output.display()))?;

    for (i, pulse) in pulses.iter().enumerate() {
        let path = output.join(format!("pulse-{i:06}.png"));
        cuttl_sim::render(pulse, cell_px)
            .save(&path)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let symbol = stream::symbol_capacity(grid, palette)?;
    println!(
        "skin: {} B -> {} pulses  [{}×{} {:?}, {} px/cell, overhead {overhead:.1}×]",
        object.len(),
        pulses.len(),
        grid.cols,
        grid.rows,
        palette,
        cell_px
    );
    println!(
        "      {} B/pulse payload, {} B header, <= {symbol} B/pulse symbol",
        grid.payload_bytes(palette),
        stream::HEADER_LEN
    );
    println!("      {}", output.display());
    Ok(())
}

fn decode(
    input: &Path,
    output: &Path,
    profile: Profile,
    distort: Distort,
    loss: f64,
    seed: u64,
) -> Result<()> {
    if !(0.0..1.0).contains(&loss) {
        bail!("--loss must be in [0.0, 1.0)");
    }
    let (grid, palette) = profile.parts();
    let channel = Channel::preset(distort.into());

    let mut paths: Vec<PathBuf> = std::fs::read_dir(input)
        .with_context(|| format!("reading {}", input.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!("no PNG pulses found in {}", input.display());
    }

    let load = |path: &Path| -> Result<image::RgbImage> {
        Ok(image::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .to_rgb8())
    };

    let mut rng = StdRng::seed_from_u64(seed);
    let mut rx = Receiver::new();
    let mut dropped = 0u32;
    let mut unreadable = 0u32;

    // Every capture sees two pulses: the skin loops forever, so the frame after
    // the last is the first again. Without a pair there is no tear and no
    // exposure straddle to simulate.
    let mut current = load(&paths[0])?;
    for index in 0..paths.len() {
        let next = load(&paths[(index + 1) % paths.len()])?;
        if rng.random::<f64>() < loss {
            dropped += 1;
            current = next;
            continue;
        }

        // Cell size is inferred from the rendered image, so the channel's blur
        // is scaled the same way the eye will see it.
        let cell_px = (current.width() / grid.cols as u32).max(1);
        let frame = cuttl_sim::channel::capture(&current, &next, &channel, cell_px, &mut rng);

        match cuttl_sim::read(&frame, grid, palette) {
            Ok(pulse) => {
                rx.ingest(&pulse);
            }
            // A frame whose finders the eye cannot find is an erasure, not a
            // failure — the same treatment a CRC reject gets (§1b).
            Err(_) => unreadable += 1,
        }
        current = next;
        if rx.is_complete() {
            break;
        }
    }

    let (have, need) = rx.progress();
    println!(
        "eye:  {} pulses on disk, {dropped} dropped, {} torn, {} rejected, {unreadable} unreadable",
        paths.len(),
        rx.torn(),
        rx.rejected()
    );
    println!("      {have}/{need} symbols absorbed  [distort {distort:?}, loss {loss:.2}]");

    let object = rx.finish()?;
    std::fs::write(output, &object).with_context(|| format!("writing {}", output.display()))?;
    println!(
        "      {} B -> {}  (object CRC verified)",
        object.len(),
        output.display()
    );
    Ok(())
}

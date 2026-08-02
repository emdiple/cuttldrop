# Cuttldrop

Optical air-gap file transfer: the **skin** (sender) strobes a file as dense colour
frames on its screen; the **eye** (receiver) reconstructs it through a camera. No
network path of any kind — no wifi, bluetooth, server, or pairing.

**`DESIGN.md` is the authoritative architecture document.** This file is the working
summary; when they disagree, DESIGN.md wins and this file has rotted — fix it.

## ⚠️ Path quirk

The parent directory is literally named `*mine` — the asterisk is part of the
directory name. **Always quote paths** in shell commands:
`'/Users/paullaidlaw/Workbench/Projects/*mine/cuttldrop'`. An unquoted path is a
glob and will expand to the wrong thing or nothing.

## Vocabulary (canonical — use consistently in code, comments, and docs)

| Term | Meaning |
|---|---|
| **skin** | the sender / rendering side |
| **eye** | the receiver / camera side |
| **pulse** | one rendered frame |
| **chroma cell** | one colour-carrying cell within a pulse |
| **band** | horizontal stripe of a pulse; one fountain symbol (from M2) |
| **beacon** | ECC-heavy fixed header at top *and* bottom of a pulse |
| **pilot** | known-value chroma cell used for colour calibration |
| **cal pulse** | payload-free pulse carrying a full-frame known calibration pattern |
| **stream** | one transfer session |

## Decisions (agreed 2026-08-03 — see DESIGN.md for full rationale)

1. **Concatenated FEC** (§1b) — inner Reed–Solomon (~10% parity, per band) + CRC-32
   gate + outer RaptorQ (`raptorq` crate, RFC 6330). Fountain codes fix *erasures*,
   not *errors*; the CRC gate converts surviving errors into erasures. Never feed
   an unverified symbol to the fountain decoder. M0/M1 may stub inner RS; the
   format reserves space for it from the start.
2. **RaptorQ over LT** (§1a) — lower implementation risk, not overhead. Behind a
   trait for later A/B.
3. **Colour is the third lever, worth ~1.7× not 3×** (§1c) — 8-colour RGB-cube
   palette (== JAB Code's 8), max 3 bits/cell. Never chase 64 colours. B/W until M3;
   colour lands behind an A/B toggle so we get a measurement.
4. **Per-band symbols from M2, whole-pulse in M1** — bands are the structural answer
   to rolling-shutter tear; M1 stays minimal.
5. **Dual beacons** (top+bottom pulse counter) turn torn frames into *detected*
   erasures. Boring geometry (QR-style finders); innovation budget goes to the
   temporal layer — see HCCB's cautionary tale (§9).
6. **Distributed pilots + periodic cal pulses** (§3b) — glare is spatially varying,
   so the colour transform is interpolated, not global (deliberate divergence from
   JAB — don't "fix" it back). CMI is attacked with equalisation, not ML.
7. **Human is the back channel** (§1e) — the eye shows `MOVE CLOSER` / `SLOW DOWN`
   hints; the skin has a manual pulse-rate control. No automatic ACK path exists.
8. **BLAKE3 verify is mandatory** — never hand back an unverified file.
9. **Bottleneck ranking** (§2): temporal (rolling shutter/exposure) ≫ spatial (MTF)
   ≫ colour accuracy ≫ decode CPU. Plan work in that order.
10. **Stack split** (§4): Rust owns the codec + simulator + CLI (+ WASM shim at M1);
    TypeScript owns the browser (camera, render, UI). The per-frame image pipeline
    starts in TS and moves to WASM only if a profiler demands it — expect it won't.
11. **iPhone-as-eye**: probe iOS Safari camera-control limits during M1, decide then.

## Layout

```
crates/cuttl-codec/   shared codec definition (native + wasm32) — geometry, palette,
                      pilots, framing, FEC, manifest
crates/cuttl-sim/     synthetic optical channel; native-only test harness
crates/cuttl-cli/     `cuttl` binary: encode/decode PNG pulse dirs (M0 observable)
crates/cuttl-wasm/    (M1) wasm-bindgen shim for the browser
web/                  (M1) Vite + vanilla TS — no framework
```

## Commands

```sh
cargo check --workspace     # fast validity check
cargo test --workspace      # sim-driven tests are the primary test surface
cargo fmt --all && cargo clippy --workspace --all-targets   # CI runs both with -D warnings

# The M0 observable, working today:
cargo run --release -p cuttl-cli -- encode FILE -o pulses/
cargo run --release -p cuttl-cli -- decode pulses/ -o out.bin
```

## Milestones (observables in DESIGN.md §5)

- **M0 step 1** ✅ lossless `cuttl encode | decode` round-trip — 85 B/pulse goodput at 48×27 mono
- **M0 step 2** synthetic channel (warp, blur, noise, crosstalk, tear, drops) + RaptorQ, so
  `--distort heavy --loss 0.6` decodes. Both flags currently exit with an error rather
  than pretending to work — the chunked carousel in `stream` needs every pulse.
- **M1** air gap crossed: B/W 48×27, two devices, real file, ~0.8 KB/s
- **M2** robustness: bands, tear detect, RS+CRC, manifest stream, BLAKE3, feedback overlay, capture corpus in CI
- **M3** colour: pilots, cal pulses, equalisation, A/B toggle → real colour-gain number
- **M4** density: smaller cells, faster pulses, glare masking; QDA + RS-ladder experiments
- **M5** product: multi-file, PWA, native eye shell only if iOS forces it

## Provisional / open

- `reed-solomon-32` chosen for the inner code (error+erasure, GF(256), no_std;
  32-ECC-byte cap → interleave 2–4 blocks per band). Validate against real M0
  error rates before committing.
- On-record prediction (§5 M3): measured colour gain lands near **1.7×**.
- RS strength laddering across pulses: speculative, M4 at the earliest (§5 M4).

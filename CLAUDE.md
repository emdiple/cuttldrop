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

This also bites tools that glob internally. **esbuild expands the asterisk when
it bundles a Vite config**, failing with `Must use "outdir" when there are
multiple input files` — which looks nothing like a path problem. Hence
`--configLoader runner` in every `web` script: it evaluates the config directly
and never invokes esbuild. Verified: the identical tree builds fine from a path
without an asterisk.

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
10. **Stack split** (§4): Rust owns the codec + simulator + CLI + WASM shim;
    TypeScript owns the browser (camera, canvas, UI, worker plumbing). **Revised at
    M1a**: the per-frame image pipeline (finder detection, homography, sampling) is
    Rust too, not TS. It already existed and was tested for the simulator, and a
    second implementation is exactly the skin/eye divergence the shared crate exists
    to prevent. It works on a borrowed `Raster` — no image crate — so it compiles to
    wasm32 unchanged.
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

# The browser. `dev` serves it; open /skin.html on one device, /eye.html on the
# other. Camera access needs https or localhost.
cd web && npm install && npm run dev
cd web && npm test        # JS boundary round trip, no browser needed
```

## Milestones (observables in DESIGN.md §5)

- **M0 step 1** ✅ lossless `cuttl encode | decode` round-trip
- **M0 step 2** ✅ RaptorQ fountain + photometric channel. `--distort heavy --loss 0.6`
  decodes byte-identical.
- **M0 step 3a** ✅ inner Reed–Solomon below the CRC gate.
- **M0 step 3b** ✅ perspective warp + finder detection + homography. The eye now
  *locates* the grid rather than being told where it is (`cuttl_sim::read`).
  **`M1_MONO` grew 48×27 → 64×36** and the finder 5×5 → 7×7 plus a separator ring:
  a 5-wide finder scans as 1:1:1:1:1, which random payload produces constantly, so
  detection needs QR's 1:1:3:1:1 and that needs 7 cells. Registration is a fixed
  ~256-cell cost, so the grid grew to amortise it — and more than paid for itself:
  goodput went 64 → 160 B/pulse at M1.
- **M0 step 3c** ✅ beacon + temporal distortion. `cuttl_sim::channel::capture` takes
  two consecutive pulses; tear stitches them at a sensor row, blend integrates both.
  The beacon (4 B: stream id + 24-bit counter, repetition-coded, duplicated top and
  bottom) makes tear *detected* rather than merely survived — `Ingest::Torn`.
  Measured at `heavy`: 80 torn, 18 CRC-rejected, transfer still byte-exact.
  **M0 is complete.**
- **M1a** ✅ eye pipeline moved into `cuttl-codec` behind a borrowed `Raster`, so the
  browser runs the same detection/homography/sampling code as the simulator.
- **M1b** ✅ `cuttl-wasm`: `Skin` (file → pulses as RGBA) and `Eye` (frames → file).
  334 KB wasm. CI builds the codec for wasm32 to keep native-only deps out.
- **M1c** ✅ browser app: skin paints via Canvas2D at integer scale, eye captures via
  `requestVideoFrameCallback` and shows `MOVE CLOSER` / `SLOW DOWN` hints (§1e).
  Builds and typechecks; the JS boundary is tested. **Not yet run against a real
  camera** — that is the remaining M1 observable and needs two physical devices.
- **M1 observable** ⬜ **the only thing blocking M1**: a real file across a real air
  gap, two physical devices. Cannot be done from a terminal — needs a camera.
- **M2** robustness. Three of the original six landed early and are struck out:
  ~~tear detect~~ (3c), ~~RS + CRC~~ (3a), ~~feedback overlay~~ (M1c). Left:
  ~~per-band symbols~~ (landed; see below), **manifest stream** (filename/size/mime so the eye can name the file and
  show it early), **BLAKE3** replacing the placeholder CRC-32 object check, **capture
  corpus** (recorded real camera frames replayed in CI), and **decode in a worker**
  (currently on the main thread — fine for M1, janky beyond it).
- **M3** colour: pilots, cal pulses, equalisation, A/B toggle → real colour-gain number
- **M4** density: smaller cells, faster pulses, glare masking; QDA + RS-ladder experiments
- **M5** product: multi-file, PWA, native eye shell only if iOS forces it

## Provisional / open

- ~~`reed-solomon-32` for the inner code~~ — **wrong crate, corrected in step 3a.**
  Despite the name it works in GF(2^5) with 31-*symbol*, 5-bit blocks, not GF(256)
  with a 32-byte ECC cap. Using it would have meant repacking every byte and running
  ~90 blocks per colour pulse. Now on `reed-solomon` 0.2 — the GF(2^8) original it
  was forked from, 255-byte blocks, byte symbols, no_std.
- **Corrected prediction, M0 step 3a** — `brutal` was expected to start decoding once
  RS landed. It did not, and the test that pinned that expectation now records why.
  Measured mean cells misread per pulse (photometric only, 4 px/cell): mono 107.9,
  colour 888.6. Correcting that needs more ECC than a pulse has bytes.
  **The inner code protects the sparse-error regime, not this one.**
- **Measured, per-band symbols** — §3a suggested 4–8 bands; the measured optimum is
  **2** for colour and **1** for mono, and at 7 bands banding is *worse than none*.
  Per-band ECC/framing is a fixed tax, and tear turns out to be partly self-mitigating:
  bands below a tear line hold the next pulse's symbols, which are valid. The remaining
  case for bands is **glare**, which is not self-mitigating and is not yet simulated —
  so `Grid::bands` should be revisited once it is.
- **Beacon is a diagnostic, not a guarantee** — a stitched frame fails the CRC gate
  anyway. The beacon earns its place by being cheap and *early* (skips RS + fountain
  work) and by being legible: "frames are tearing" is actionable by the human back
  channel (§1e); "CRC failed" is not. Tear detection is also incomplete by design —
  a tear line landing inside a beacon strip or in the dark surround leaves both
  strips agreeing, and the CRC catches those instead.
- **Open, M0 step 3b** — photometric distortion still shows no middle ground: mono
  gets 0 errors/pulse below the cliff and ~108 above it. The sparse-error regime RS
  protects comes from *sub-cell sampling error under perspective*, which now exists.
  Re-check `ECC_LEN` against warp-driven error rates before treating 16 as settled.
- **Measured, M0 step 2** — photometric distortion barely touches mono. At `Heavy` the
  cell error rate is ~3e-6 and zero pulses are lost; the same preset costs colour ~1.5%
  of pulses. Mono's decision margin is simply enormous. The photometric channel earns
  its keep at M3, not now — which is §1c's bottleneck ordering showing up as a number.
- On-record prediction (§5 M3): measured colour gain lands near **1.7×**.
- RS strength laddering across pulses: speculative, M4 at the earliest (§5 M4).

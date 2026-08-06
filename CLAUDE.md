# Cuttldrop

Optical air-gap file transfer: the **skin** (sender) strobes a file as dense colour
frames on its screen; the **eye** (receiver) reconstructs it through a camera. No
network path of any kind — no wifi, bluetooth, server, or pairing.

**`DESIGN.md` is the authoritative architecture document.** This file is the working
summary; when they disagree, DESIGN.md wins and this file has rotted — fix it.
**`ROADMAP.md` is the ordered work list** — what to do next and the evidence for the
order. DESIGN.md §5 still owns what the milestones *mean*.

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
| **profile** | a named grid + palette pairing (`m1`…`m4`); the skin picks, the eye detects |

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
# -o is optional: decode names the output from the manifest (and refuses to
# overwrite without an explicit -o). Decode replays the directory up to 10×,
# because the directory stands in for a skin that loops forever (§3d).

# The browser. `dev` serves it; open /skin.html on one device, /eye.html on the
# other. `cert` is mandatory whenever the eye is a phone: mediaDevices does not
# exist outside a secure context, and a LAN address is not one.
cd web && npm install && npm run cert && npm run dev
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
- **M2** robustness — **complete except the capture corpus**, which needs a camera.
  ~~tear detect~~ (3c), ~~RS + CRC~~ (3a), ~~feedback overlay~~ (M1c),
  ~~per-band symbols~~ (see below), ~~manifest stream~~ (name/mime + full BLAKE3,
  band 0's slot every 8th pulse, flagged in the v3 header; size rides in the OTI),
  ~~BLAKE3 verify~~ (completion now *requires* the manifest and `finish` checks the
  full hash — the CRC-32 object check is gone), ~~decode in a worker~~ (frames cross
  as transferred buffers; busy frames are dropped, not queued). Left: **capture
  corpus** (recorded real camera frames replayed in CI) — hardware-gated, like the
  M1 observable.
- **M3** colour: pilots, cal pulses, equalisation, A/B toggle → real colour-gain number
- **M4** density — **the grid half landed early**: `Profile` is now a four-rung ladder
  (m1 64×36 mono → m2 192×108 mono → m3 96×54 colour → m4 192×108 colour, 160 B →
  6336 B/pulse), the skin has a density menu, and the eye auto-detects by trial decode
  so only one device is ever set. All four deliver byte-exact through the full optical
  path in sim. **Interior alignment patterns landed** (see below) — the radial-term plan
  in §3a is superseded. Left: glare masking, QDA + RS-ladder experiments, adaptive
  compression + measured file-size ceiling (`COMPARISON-decimen.md` R3–R4)
- **M5** product: multi-file, PWA, native eye shell only if iOS forces it; optional
  passphrase encryption, standalone single-file builds (`COMPARISON-decimen.md` R2, R8)

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
- **Manifest period = 8 pulses** — the name arrives ≤ 0.8 s in at 10 Hz, costing
  1-in-8 of mono's symbol slots (12.5%) and 1-in-16 of colour's (6.25%). A knob,
  not a law: revisit if sustained mono goodput ever matters more than naming
  latency. The manifest is *not* fountain-coded (§3c corrected in place): it fits
  one symbol, and a fountain over one symbol is just repetition.
- On-record prediction (§5 M3): measured colour gain lands near **1.7×**.
- RS strength laddering across pulses: speculative, M4 at the earliest (§5 M4).
- **Measured, timed shutter model** — tear and blend now *emerge* from pulse rate vs
  `Shutter::PHONE` timing (`capture_timed`). Mono goodput is linear to **20 Hz**
  (2.74 KB/s at 30 fps capture — 2× the old default; skin slider now defaults to 20,
  max 30). Integer pulse:capture ratios freeze the phase and can starve a transfer
  outright (30:30 starved every seed); above ~43 Hz the shutter window exceeds the
  period and nothing captures clean. Findings pinned; sweeps re-runnable with
  `cargo test -p cuttl-sim --release -- --ignored --nocapture`.
- **Measured, density** — the cliff is between 3 and 2 px/cell (sampling error, not
  detection). At 4 px/cell, 192×108 reads with ~10 misread cells/frame; px/cell at the
  sensor is the binding constraint, so `WORK_WIDTH` in `eye.ts` is now 1280 (192 cols ×
  4 px ÷ a 70%-filled frame ≈ 1100).
- **Measured, density pays twice** — 24 KB through the full channel: m1 528 pulses,
  m2 **39**, m3 56, m4 **13**. Two findings. **m2 beats m3: density outruns colour**,
  which is §2's ordering as a number, and it is the safer lever besides. And density
  pays *more* than its cell count — registration is a fixed ~600 cells, so payload share
  climbs 73% → 83% → 91% and 9× the cells buys 13× the bytes. Both pinned
  (`the_density_ladder_delivers_end_to_end`, `density_amortises_the_registration_tax`).
- **Measured, short loops are the dense profiles' one hazard** — a small file on a dense
  grid makes a *short* loop (24 KB on m4 is 13 pulses), and below ~32 pulses a single
  pass at 20% loss is a lottery: too few distinct symbols to route around a bad frame.
  Not a colour problem. The fix already exists — the skin loops forever (§3d), the CLI
  replays 10× — and is pinned by `a_short_loop_needs_the_skin_to_repeat_itself`. The
  skin warns when a loop comes out under 32 pulses.
- **Eye telemetry landed** — capture fps, decode fps, goodput, elapsed, new/dup, ETA,
  and the camera mode the browser actually granted (R6's diagnostic). Goodput is
  `symbols × symbolBytes ÷ elapsed`, clocked from the first *accepted symbol* rather
  than page load, so aiming time is not charged against the rate. This is the instrument
  the M1 observable reports through: without it "a file crossed the gap" is a boolean.
- **Measured, wasm size vs decode speed — the release profile stays speed-tuned.**
  The 351 KB / 195 KB-gzip artifact is not wasm-opt's fault: `-O`/`-Os`/`-Oz`/`-O3`/`-O4`
  span 0.3%, so the level wasm-pack picks is irrelevant. `lto = "fat"` and `strip`
  buy nothing either (wasm-bindgen has already stripped). The *only* lever is
  `opt-level = "z"`, worth 195 → 178 KB gzip — and it costs **2.3×** on the eye
  pipeline (1.76 s → 4.08 s on `the_density_ladder_delivers_end_to_end`). That is a
  one-time 19 KB download against every frame of every transfer, on the one device
  where decode fps is already a measured constraint. Declined. Re-open only if a
  profile-guided or feature-trimmed build changes the trade, not by re-running these.
- **Four ways an iPhone eye fails silently**, all now closed. (1) `mediaDevices` is
  absent outside a secure context, so a LAN address gives no camera at all — hence
  `npm run cert`, and the page now *says* this instead of printing a raw `TypeError`.
  (2) `getUserMedia` and `play()` want a user gesture, so the camera starts on a tap
  rather than on the worker's `ready`. (3) an unhandled `play()` rejection left the
  page on its opening hint forever — indistinguishable from "no signal". (4) iOS can
  resolve `play()` before `videoWidth` is known, and the old `9/16` fallback stretched
  every frame: video visible, grid non-square, every frame failing the CRC gate. The
  work canvas is now sized lazily from the first frame that has real dimensions.
  Only (1) blocks bring-up; (4) is the one that would have wasted an afternoon.
- **Measured, interior alignment patterns — a homography is not enough, and "small
  radial distortion" was wrong by a lot.** Four corner finders give an *exact* fit for a
  planar target under a pinhole camera, so everything projective is free. Everything that
  is not projective is not: barrel distortion, and the pose drift of a handheld camera
  across a rolling shutter's readout. There is no gentle degradation — on 192×108 mono,
  barrel 0.015 costs 12 misread cells/frame and 0.020 costs **2208**, because past half a
  cell every sample lands in the neighbour. Tolerance was ~1.6% of the half-diagonal,
  i.e. *one cell*, and dense grids have small cells.
  The fix is QR's: a lattice of 5×5 alignment patterns (`align_period` 32 on m2/m4, 28 on
  m3, **0 on m1** — its 62×30 data region fits too few to interpolate between). **5, not
  the finder's 7, is a correctness point**: a 5-wide concentric square scans as
  1:1:1:1:1, exactly what finder detection rejects, so an alignment pattern can never be
  mistaken for a finder and wreck the corner fit. It needs no distinctive ratio because
  it is only ever searched for *locally*, near a position the homography already predicts.
  The eye interpolates the residuals with inverse-distance weighting rather than fitting a
  global term — the same interpolate-don't-fit argument §3b makes for colour.
  Measured: tolerance 0.016 → 0.024, and at barrel 0.02 misreads go **2208 → 2/frame**.
  Costs 2.2% of the grid (m2 goodput 2128 → 2064 B/pulse) and **+11% decode time**
  (7.66 → 8.48 ms; `locate` still dominates at 7.3). Lattice density has a knee: 24 and 20
  buy under 10% more tolerance for another 1.7–3.3% of the grid, and 16 measures *worse*
  than 20 in places — interpolation running out of signal, not points. Pinned by
  `alignment_patterns_earn_their_cells`, `correction_is_inert_when_the_homography_is_already_right`
  (it must change nothing under pure perspective, or it is fitting noise), and
  `patterns_only_ever_displace_data_cells` (a pattern may never overwrite a finder, beacon
  or timing track — those are what locates it in the first place).
  The channel gained `skew` and `barrel` to measure this. **Both are 0 in every preset on
  purpose**: folding a new distortion into `heavy` would silently move numbers cited as
  evidence elsewhere.
- **The skin's display size is adjustable while strobing**, counted in whole pixels per
  cell rather than a percentage — the canvas must be an integer multiple of the grid or
  nearest-neighbour upscaling gives some cells an extra pixel and the 1:1:3:1:1 ratios
  stop being clean. A percentage slider would spend most of its travel rounding to the
  same scale.
- **The grid is 16:9 and that is load-bearing.** Square was asked for and costs real
  throughput: at equal cell count a square grid inside a 16:9 frame is limited by height
  and yields **1.33× fewer px/cell**; at equal px/cell it carries **1.78× fewer cells**.
  Since the binding constraint is px/cell at the sensor, square pays twice. The genuine
  shape mismatch is a *portrait* phone giving a 9:16 frame — the answer is to hold the
  eye landscape, not to reshape the format.
- **Adopted from decimen's field notes** (`COMPARISON-decimen.md` R5–R7): `skin.ts`
  hardcodes `refreshRate = 60` — measure it from rAF timestamps (**still open**).
  ~~iOS delivers 30 fps when asked for `{ideal: 60}`~~ and ~~the capture loop needs a
  generation counter~~ both landed — see below.
- **iPhone camera handling, modelled on decimen's** (`web/src/platform.ts`, new). Six
  things, each a silent failure rather than an error:
  1. **`frameRate: {exact}` first, `{ideal}` as fallback.** iOS accepts `{ideal: 60}`,
     delivers 30, and reports success — an `exact` constraint is the only one that
     *rejects* rather than quietly substituting, so the fallback has to exist for
     cameras that genuinely cannot hit the number. We ask for **30**, not 60: the
     measured optimum is 20 Hz against 30 fps, and asking higher on a phone tends to
     buy a lower-resolution sensor mode instead of more frames.
  2. **Capture width matched to `WORK_WIDTH` (1280), not maximised.** Anything above it
     is downscaled away; 1920 selects a slower mode for no gain where it counts.
  3. **`getUserMedia` failures classified**, not printed raw — `NotAllowedError`
     (permission), `NotReadableError` (another app holds the camera), `NotFound`/
     `Overconstrained`. Each names the action that fixes it.
  4. **Retry releases the stream first.** A start that failed after `srcObject` was set
     left a live camera nobody read; on iOS a second `getUserMedia` over that is itself
     one of the ways `NotReadableError` happens.
  5. **Generation counter on the capture loop** (R7) — now genuinely live, because the
     retry path above makes a second successful `start()` reachable. Two loops racing
     the `busy` flag double captures and halve decode rate, which reads as a camera
     fault.
  6. **Wake locks at both ends**, re-acquired on `visibilitychange` because the lock is
     dropped when the page hides and *not* restored. On the skin this is not comfort:
     the screen **is** the transmitter, so a display timeout stops the send with the
     page still apparently running.
  Also: capabilities are **probed, never sniffed** — the one UA test (`isMobileWebKit`)
  is quarantined and used only for quirks with no observable signal. Continuous
  autofocus is applied where offered; a hunting lens is the top decode killer.
- **The two screens have opposite UI rules, and the skin's is optical, not aesthetic.**
  On the skin the screen *is* the transmitter, so anything lit that is not payload is
  emissive area inside the receiving camera's frame, competing with the pulse for
  auto-exposure. Chrome during a send is therefore **summoned, not resident** *on a
  narrow screen*: the controls appear on a tap and retire after a few seconds. When they
  do appear they *shrink* the pulse rather than covering it — the bottom rows are the
  second beacon strip, and occluding those turns detected tears back into silent CRC
  failures. **Above 62rem the rule lapses and the panel is resident**, because the
  argument was never tidiness: at that width the controls sit *outside* the pulse, on a
  surround the receiving camera is not pointed at, so residency costs no exposure. On the
  eye the screen is the only feedback channel that exists (§1e), so the rule inverts:
  one instruction in large type over a full-bleed preview, a 16:9 aim frame (the human's
  only lever on px/cell is how much of the frame the sender fills), and the telemetry
  folded into a disclosure — it is instrumentation for the M1 observable, not for someone
  moving a photo between two phones. Also: `[hidden]` needs `!important` in this
  codebase. The UA sheet's `display: none` loses to any author `display` rule, so
  `.panel { display: flex }` silently defeated `setup.hidden = true`.
- **Both screens are a two-column shell above 62rem and a single stack below, and neither
  page ever scrolls.** Body is `100svh` — the *small* viewport, so a mobile URL bar
  sliding in cannot make the page taller than the glass — with `overflow: hidden`; the
  panels scroll inside themselves. The pulse is fitted to a measured `#stage` element
  rather than to viewport-minus-chrome arithmetic: the stage is the whole screen on a
  phone and the column beside the panel on a laptop, so one `maxScale()` serves both and
  cannot disagree with what CSS actually did. The refit `ResizeObserver` watches the
  stage *and* the controls but is guarded on a size signature, because `resize()` is
  upstream of both (it publishes `--overlay-room`, which pads the body, which resizes the
  stage) — without the guard that path rings instead of settling.
- **"Can't see the app on the network" is almost never the network.** Two causes, both
  silent. The server is https-only, so a bare `172.20.10.3:5173` makes the browser try
  http, get an empty reply, and report a connection failure — the scheme has to be typed.
  And the cert is bound to *addresses*, which move: a phone hotspot re-leases on every
  reconnect, so a cert made an hour ago names a machine that has since become a different
  one. `vite.config.ts` now compares the cert's SANs against the live interfaces at
  listen time and warns; silent when they match. `--host` is redundant — the config has
  always set `host: true`.
- **The dev-server certificate warning is not a bug.** `npm run cert` now prefers
  `mkcert` when installed (signed by a local root → no warning once the root is on the
  phone) and falls back to openssl (signed by nobody → each device warns once, tap
  through). Both give a genuine secure context, which is all `mediaDevices` requires.

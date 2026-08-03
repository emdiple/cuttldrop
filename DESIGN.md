# Cuttldrop — Optical Air-Gap Transport: Architecture & Design Notes

**Status:** Draft v0 — proposed, not yet accepted. No code scaffolded.
**Date:** 2026-08-03
**Open questions blocking scaffold:** see [§6](#6-before-scaffolding--open-questions).

Cuttldrop transfers files between two devices with no network path between them — no
wifi, no bluetooth, no server, no pairing. The **skin** (sender) renders the file as a
rapidly refreshing sequence of high-density colour frames on its screen. The **eye**
(receiver) points its camera at that screen and reconstructs the file.

The name is cephalopod: cuttlefish strobe colour patterns across their skin via
chromatophores at roughly 10 Hz. Same mechanism — a dense colour display refreshing to
push data optically. As §2 shows, the physics lands us at almost exactly that rate.

---

## 1. Design disagreements and corrections

This section records where the initial framing was wrong or incomplete. It is first
because everything downstream depends on it.

### 1a. RaptorQ isn't overkill — it's *less* work than LT, and the overhead argument is a red herring

The original framing was "RaptorQ vs LT on overhead." That's the wrong axis. LT with a
well-tuned Robust Soliton runs ~5–30% reception overhead depending on K; RaptorQ is
~1e-2 failure at zero overhead, 1e-4 at +1 symbol, 1e-6 at +2. So call it 15% vs 2%.

But the optical channel will lose **40–70% of transmitted symbols** to blur, tear, and
glare. A 13-point difference in code overhead is noise against that. Pick on
implementation risk instead — and there, RaptorQ wins outright: the `raptorq` crate
(RFC 6330, `no_std`+alloc, compiles clean to wasm32) is `Encoder::with_defaults(&data,
mtu)` and an incremental `Decoder::add_new_packet()`. Rolling our own LT means owning
the degree-distribution tuning, and a mistuned LT is silently 2× worse with no error
message. RaptorQ is the lower-effort option.

**Decision:** RaptorQ via the `raptorq` crate, behind a trait so LT can be A/B'd later.
Do not spend M0 writing an LT decoder.

### 1b. The load-bearing correction: fountain codes fix *erasures*, not *errors* — and this channel produces both

A fountain decoder assumes every symbol it receives is *correct or absent*. Feed it one
silently-corrupted symbol and the corruption propagates through the XOR graph and
poisons the whole object — you get a "successful" decode of garbage. This channel is not
a clean erasure channel: colour misclassification produces **sparse random bit errors**
scattered through otherwise-fine frames.

Do the arithmetic on a single-layer design (fountain + CRC only). A 1650-byte symbol at
3 bits/cell is ~4400 cells. At a per-cell error rate `p`, P(symbol survives) =
(1−p)^4400. To get 90% of symbols through we need p ≤ 2.4e-5. That is a brutal
requirement for a handheld phone camera pointed at a screen under room light — we'd
spend the whole project chasing it and still fail on the first glare gradient.

**We need a concatenated code:**

| Layer | Job | Mechanism |
|---|---|---|
| Inner | mop up sparse cell errors within one symbol | Reed–Solomon over GF(256), ~8–10% parity, per band |
| Gate | convert surviving errors into clean erasures | CRC-32 per symbol — nothing enters the fountain decoder without it |
| Outer | recover from lost/torn/blurred symbols | RaptorQ across pulses |

With ~10% RS parity on a ~200-byte band symbol we tolerate p ≈ 3e-3 comfortably — two
orders of magnitude more slack. That is the difference between "works in my kitchen" and
"works."

**Free 2× on the inner code:** because we classify colours against calibrated centroids,
we get a confidence value per cell for free (distance to nearest centroid). Mark
low-confidence cells as *erasures* rather than guessing them. RS corrects 2× as many
erasures as errors. Costs nothing, roughly doubles inner code strength.

### 1c. Colour is a real lever but it's the *third* one, and it's worth ~1.7×, not 3×

Going 1 bit → 3 bits/cell does not triple throughput. The camera's chroma path is
spatially coarser than its luma path — Bayer gives green 2× the sample density of red
and blue, and demosaic smears chroma further. To classify 8 colours reliably we need
cells roughly 4–5 camera pixels across where B/W tolerated 3. That eats about half the
gain. Net: expect **1.5–2×**, and don't be shocked if the honest measured number is 1.6.

**The named mechanism behind that estimate is cross-module colour interference (CMI)** —
chroma from one cell bleeding into its neighbours (§9). It is *worse* for us than for the
print-and-scan literature where it was characterised, because we stack two mechanisms
print doesn't have: Bayer demosaic, and 4:2:0 chroma subsampling if any part of the
capture path touches a video codec — which literally halves chroma resolution in both
axes. The HiQ results (§9) show CMI is the *dominant* colour-mode failure mode, not a
rounding error. If anything the 1.7× estimate above is optimistic.

CMI is deterministic and neighbour-dependent, which makes it inter-symbol interference,
which means the right tool is **equalisation, not machine learning** — estimate the
crosstalk kernel from the pilots and deconvolve before classifying. See §3e.

Which means colour is not milestone 1. It is milestone 3, gated behind an A/B toggle so
we get a real number instead of an assumed one.

**Don't chase 6 bits/cell (64 colours).** That requires discriminating 4 amplitude levels
*per channel*, and amplitude is the single most fragile dimension available —
vignetting, backlight non-uniformity, specular glare gradients, and camera auto-exposure
all attack it directly. Spend those bits on more cells instead. Leave the palette
pluggable and let M4 disprove this with data.

Corroboration: ISO/IEC 23634:2022 (JAB Code) standardises 2-, 4-, and 8-colour modes and
stops there — a standards body with print's controlled geometry, static targets, and
flatbed-grade scanners available still capped at 3 bits/cell. We have a handheld camera
and motion blur.

### 1d. Don't inherit QR's structure either — not just its encoder

Stock QR encoders were already rejected (QR is binary by spec; no standard decoder reads
colour variants). Go further: QR's Reed–Solomon-inside-the-symbol plus masking plus
format info is tuned for a **single-shot static read** where the frame must self-correct
because there is no second chance. We have thousands of chances. Heavy in-frame ECC on
the payload is capacity burned duplicating what the fountain layer already does better.

The exception, and it's important: the tiny fixed header **cannot** be fountain-coded —
the receiver needs it to interpret anything at all. So the split is:

- **Beacon field** (~16 bytes: stream ID, RaptorQ OTI, pulse counter, grid geometry):
  heavy ECC, fixed position, present in *every* pulse. The only place QR-grade
  redundancy belongs.
- **Payload:** light inner RS + CRC only, as above.

### 1e. "No back channel" is true for the machines and false for the system

There is no *automatic* back channel — that's why fountain coding is right. But a human
is holding the eye, looking at its screen. That's a legitimate low-bandwidth,
high-latency control loop and we should exploit it hard: the eye displays `MOVE CLOSER`
/ `HOLD STILL` / `TOO BRIGHT` / `47% — SLOW DOWN`, and the human acts on it. This gets
most of the practical benefit of rate adaptation for near-zero complexity.

A genuine duplex mode — both devices strobing and watching each other — would let us cut
fountain overhead dramatically, but it reintroduces pairing, which is explicitly out of
scope. Noted as possible v2, not proposed.

---

## 2. Where the bottleneck actually is

Ranked, most binding first. This ordering drives the milestone plan.

1. **Temporal — rolling shutter and exposure straddle.** Dominant, by a lot. Phone
   sensors read out line-by-line over 10–33 ms. If the display flips mid-readout, the
   captured image contains a horizontal band of pulse N above a band of pulse N+1.
   Separately, a 1/60 s exposure at 60 Hz pulses will almost always straddle a
   transition and capture a blend of two pulses. Practical ceiling: **10–20 clean
   pulses/sec** from a 30–60 fps camera, no matter how fast the display is.

   That lands right on the cuttlefish's ~10 Hz. The name was better chosen than we knew.

2. **Spatial — camera MTF and focus.** Caps cells per pulse. We need ~3–5 camera pixels
   per cell; lens MTF at high spatial frequency on a phone is poor, and hand-held focus
   drifts. Realistically 5k–15k cells.

3. **Colour accuracy.** Caps bits/cell. Third, and the most improvable via calibration
   (§3b).

4. **Decode CPU — *not* a bottleneck**, if we don't write it stupidly. Per-pulse work is:
   locate markers *at quarter resolution*, solve an 8-DOF homography, bilinearly sample
   ~10k points, classify, RS-decode a few bands, XOR into fountain state. Low-single-digit
   milliseconds — achievable in plain TypeScript. What *will* melt a core is naive
   full-frame processing at 1080p×30fps (full-res connected components, per-pixel passes
   over 2M pixels). The fix is algorithmic, not linguistic.

**Practical risk to note now:** we want to lock exposure, focus, and white balance.
`MediaTrackConstraints` exposes `exposureMode` / `focusMode` / `whiteBalanceMode` /
`exposureTime` / `iso`, but support is uneven — Chrome on Android gives us some of it,
**Safari on iOS gives essentially none**. If iPhone-as-eye is a requirement, that may
eventually push the eye side into a Capacitor/Tauri shell to reach `AVCaptureDevice`.
Don't solve it now; do design so the capture layer is swappable.

---

## 3. Architecture

### 3a. Pulse format

```
┌────────────────────────────────────────────────────┐
│ ◰        [ BEACON: id·OTI·pulse#·geom ]         ◱  │  ← finder + beacon strip
├────────────────────────────────────────────────────┤
│ ▓ ░ ▓ ░  band 0   sym ESI·payload·RS·CRC   ░ ▓ ░ ▓ │  ← timing track both edges
│ ▓ ░ ▓ ░  band 1   sym ESI·payload·RS·CRC   ░ ▓ ░ ▓ │
│ ▓ ░ ▓ ░  band 2   ...          · pilots ·  ░ ▓ ░ ▓ │
│ ▓ ░ ▓ ░  band 3                            ░ ▓ ░ ▓ │
├────────────────────────────────────────────────────┤
│ ◲        [ BEACON: id·OTI·pulse#·geom ]         ◳  │  ← repeated, for tear detect
└────────────────────────────────────────────────────┘
```

**Geometry.** 16:9 grid to match both screens. Four corner finders (three identical
concentric-square, one distinct for orientation disambiguation — QR's trick, and it
works).

**Measured at M0 step 3b: the finder must be 7 cells, not 5.** Detection scans for the
run-length ratio through a finder's centre. A 7-wide concentric square gives QR's
1:1:3:1:1; a 5-wide one gives 1:1:1:1:1, which random payload produces constantly. Each
finder also needs a blank separator ring, or payload cells of the same polarity merge
with the outer ring and the ratio test fails. That is a fixed ~256-cell cost per pulse,
which is why the M1 grid grew from 48×27 to 64×36 — see §5. Alternating-cell timing tracks down both vertical edges and across the beacon
strips: these validate the homography and let us detect scale/skew drift cheaply.

**Perspective.** Four corner correspondences → 8-DOF homography via DLT. That is *exact*
for a planar target under a pinhole model, so perspective is a solved problem, not a hard
one. Radial lens distortion is small for a centred target at moderate FOV — ignore it in
M1, add a single radial term in M4 if corner cells show systematic error.

**Rolling-shutter skew — the key trick.** Put the beacon (including the pulse counter) at
*both* top and bottom. Then:

- top pulse# == bottom pulse# → clean frame, decode everything
- they differ → torn frame, and we know it

That converts an invisible silent corruption into a *detected erasure*, which is
precisely what the outer fountain code wants. Cheap, and it's the difference between
mysterious decode failures and clean accounting.

**Then take it further: make each horizontal band its own independent fountain symbol**
(own ESI, own RS parity, own CRC, own mini-beacon). A torn frame no longer costs the
whole pulse — it costs the one or two bands straddling the tear. Same for a glare blob:
we lose two bands, not 1650 bytes. This is the single highest-leverage structural
decision in the format, which is why it belongs in from M2 rather than retrofitted.

~~Suggested: 4–8 bands per pulse, ~200–400 payload bytes each.~~ **Measured at M2: two.**

Bands cost goodput — per-band ECC and framing are a fixed tax, and the smallest band sets
the symbol size for all of them — and buy damage granularity. Bytes delivered per frame
shown, under `Preset::Heavy`, averaged across seeds:

| bands | 1 | **2** | 3 | 4 | 5 | 7 |
|---|---|---|---|---|---|---|
| B/frame | 952 | **1014** | 978 | 985 | 870 | 800 |

The curve rises then falls, and by seven bands banding is *worse than not banding at
all*. Two reasons, and the second is the one this section got wrong:

1. Per-band overhead scales with the band count, unconditionally.
2. **Tear is partly self-mitigating.** Bands below the tear line carry the *next* pulse's
   symbols, which are perfectly valid — a torn frame loses only the band straddling the
   tear, not everything below it. So extra granularity buys far less against tear than
   this section assumed.

The stronger case for bands is **glare**, which ruins a fixed region of the frame and is
not self-mitigating. The channel does not model glare yet, so that case is currently
unmeasured — which means two bands is where the evidence points today, not necessarily
where it will settle.

The M1 mono profile stays at **one** band: at 211 B per pulse there is no room to pay for
a second copy of the inner code.

**Correction from M0, measured not estimated: the beacon cannot carry ~16 bytes.** §1d
assumed a beacon holding stream ID, RaptorQ OTI, pulse counter *and* grid geometry. On
the M1 profile the geometry says otherwise. Each beacon strip is 3 rows × 48 cells minus
the two finders, so 114 cells — 114 bits at 1 bit/cell. Apply the 3× repetition that
"heavy ECC" implies and the strip holds **≈4 bytes**, not 16.

So the beacon carries only what tear detection and stream discrimination actually need:

| Field | Bytes |
|---|---|
| stream id | 1 |
| pulse counter | 3 |

Everything else §1d put in the beacon — OTI, object length, grid geometry — moves into
the payload, repeated every pulse. That is affordable (a few percent) and it is *not*
a regression: those fields need to survive loss, not to be readable before the grid has
been located, which is the only thing the beacon is uniquely good for.

This does not weaken the tear detector. Duplicating a 3-byte counter top and bottom is
exactly as effective as duplicating 16 bytes — the mismatch is what carries the signal.

**Implemented at M0 step 3c**, with one addition the design did not anticipate: the
beacon is written **one bit per cell**, not in the pulse's palette. It has to be
readable before the pilots have been fitted, so it cannot depend on colour
classification at all. In colour mode that makes each beacon cell a majority vote over
three subchannels — free redundancy, and it falls straight out of refusing to use the
full alphabet.

### 3b. Colour scheme — and why per-pulse calibration is non-negotiable

**Palette: 8 colours, treating R/G/B as three independent binary subchannels** (each cell
is a corner of the RGB cube → 3 bits). Maximum inter-symbol distance in every channel, no
amplitude discrimination required. Be pessimistic and plan on 3 bits; treat 6 as a
hypothesis M4 tests.

Reassuringly, this lands on exactly the same eight colours as JAB Code's 8-colour mode
(CMY + RGB + K + W) — cyan is G+B, magenta R+B, yellow R+G. Two designs converging on an
identical palette from opposite directions, subtractive and additive, is about as good as
independent confirmation gets.

The reason this works at all across arbitrary display/camera/ambient combinations is that
display primaries and camera Bayer filters don't align. A "pure red" cell produces
meaningful green and blue response. That crosstalk is a roughly linear mixing we **can
invert — but only if we measure it in situ.**

So: **pilot cells.** Known-value cells at ~1-in-64 density scattered through the grid
(~1.5% capacity cost), plus reference patches at all four corners. The eye measures them
per-pulse and fits a 3×4 colour transform (3×3 mixing + offset). Critically, glare and
vignetting are *spatially varying*, so interpolate the transform across the frame rather
than fitting one global matrix.

This is straight out of OFDM pilot-tone practice, and that's the right mental model:
**this is a communications problem wearing a graphics costume.**

**Why we deliberately diverge from JAB Code here — don't "fix" this back.** JAB embeds
its reference palette as a *fixed block*, which can only support a single **global**
colour transform. That is correct for evenly-lit paper. It is wrong for a glossy emissive
screen, where specular glare makes the distortion strongly **spatially varying**.
Distributing pilots across the grid so the transform can be interpolated is not a
stylistic preference; it is forced by our medium. The idea of an in-symbol reference
palette is borrowed and is load-bearing — the *layout* is ours.

Two dividends fall out free: the per-cell distance to its nearest calibrated centroid
gives the confidence metric that drives erasure marking (§1b), and a region whose pilots
read *clipped/saturated* tells us exactly where the glare is so we can mask those bands.

#### Calibration pulses

One pulse in ~32 (~3% overhead) carries no payload — instead a full-frame known pattern:
all eight palette colours in a checkerboard, plus a resolution wedge. From one such pulse
the eye recovers, at full spatial resolution rather than at pilot density:

- a per-region colour transform
- a **CMI/crosstalk kernel** estimate, which is what the equaliser in §3e needs
- an MTF / focus / motion-blur metric, feeding the `ADJUST FOCUS` and `HOLD STILL` hints

This is only possible because we are a *dynamic* carrier, and it is the sharpest way to
convert that structural advantage into decode margin. A static symbology cannot do it at
any price. See §9.

#### Classifier

Start with nearest-centroid in the calibrated space — trivial, and adequate once
equalisation is in place. The upgrade path, if M3 data justifies it, is **QDA fitted
online from the pilots**: eight quadratic forms in three dimensions is ~100 flops/cell,
about 1 ms for 10k cells, so it fits the budget. That is effectively HiQ's model class
(§9) with our fitting regime — per-session and adaptive, requiring no pre-trained corpus
and no per-device-pair training data.

### 3c. FEC stack

```
file → BLAKE3 hash + manifest
     → RaptorQ encode (symbol ≈ one band payload)      [outer: erasures]
     → + CRC-32 per symbol                             [gate: errors → erasures]
     → + RS(GF256) ~10% parity per band                [inner: sparse cell errors]
     → cell packing → palette → pulse raster
```

**Symbol sizing:** one symbol per band, ~200–400 bytes. Smaller symbols mean finer
erasure granularity (good for tear/glare) at the cost of a larger K and slightly more
per-symbol header overhead. K in the low thousands is comfortable for RaptorQ.

**How the eye knows it has enough:** RaptorQ tells us — decode succeeds or it doesn't.
Attempt at K, then K+1, K+2, then back off to every +5, +10 (decode attempts aren't free
at large K). Then **verify BLAKE3 against the manifest** — mandatory, non-negotiable, our
only true correctness check.

Show progress as `symbols collected / K`. That's honest and monotonic. Don't show
"% decoded" — it isn't a real quantity.

**The manifest.** ~~Two multiplexed streams: a tiny separate manifest stream (filename,
size, mime, hash), fountain-coded on its own and interleaved ~1-in-16~~ — **landed
simpler at M2, deliberately.** The manifest fits in *one* symbol, and a fountain code
over a single-symbol object degenerates to repetition — there is nothing for a second
OTI and a second decoder to buy. What ships instead: every 8th pulse donates band 0's
symbol slot to the manifest (name, mime, and the full BLAKE3 hash), flagged in the
stream header and protected by the same RS + CRC path as any symbol. That is 1-in-8 of
mono's symbol slots and 1-in-16 of colour's — the colour figure landing exactly where
this paragraph guessed. Size is deliberately not carried: the OTI in every stream header
already holds the exact transfer length, and a second copy could only disagree. (The
plan to put the OTI in the beacon died at M0 step 3c, when the beacon measured 4 bytes;
it rides in the band-0 stream header.) The eye displays *"receiving cuttlefish.pdf —
2.4 MB"* within a second of looking, whenever it starts. The header also repeats the
hash's first four bytes in every pulse, binding symbols and manifest to each other;
completion requires both, and nothing is handed back until the reconstruction matches
the full hash.

### 3d. Skin (sender)

- Encode in Rust/WASM → hand out a byte-per-cell buffer.
- **Render: Canvas2D `putImageData` at grid resolution + nearest-neighbour upscale
  (`imageSmoothingEnabled = false`).** Roughly ten lines. Reaching for WebGL here is
  premature — this is not a GPU-bound problem, and the shader buys nothing until colour
  management becomes a measured issue. (It might: browsers can apply colour-profile
  conversion to canvas content and shift the palette. Set `{ colorSpace: 'srgb' }` on the
  context; if palette drift shows up, *then* go to WebGL2 for explicit control.)
- **Pacing: `requestAnimationFrame`, holding each pulse for an integer number of display
  refreshes.** Never attempt to change pulses faster than the display can commit them.
  3 refreshes at 60 Hz = 20 pulses/sec is a sane starting point.
- **Refresh-rate mismatch has no automatic solution** — no back channel means the skin
  cannot know how the eye is doing. So: expose a manual pulse-rate control, and let the
  human close the loop off the eye's on-screen feedback (§1e).
- Loop the fountain stream forever; a human stops it.

### 3e. Eye (receiver)

Pipeline, with where the realtime budget goes at 30 fps (33 ms/frame):

| Stage | Cost | Notes |
|---|---|---|
| Frame acquire | ~1 ms | `requestVideoFrameCallback` — the universal path (Chrome + Safari 15.4+). `MediaStreamTrackProcessor` is faster and zero-copy-ish but **Chrome-only**; abstract behind an interface |
| Downscale to ¼ grayscale | ~2 ms | everything below runs on this, not full res |
| Finder detection | ~3 ms | at ¼ res; this is the step that gets slow if done naively at 1080p |
| Homography (DLT) | <0.1 ms | 8×8 solve |
| Pilot fit + colour transform | ~1 ms | sparse — only pilot cells |
| Sample ~10k cells | ~2 ms | bilinear sample at full res, sparse gather |
| **CMI equalisation** | ~1 ms | deconvolve the crosstalk kernel measured from the last calibration pulse (§3b). Colour modes only — skipped in B/W |
| Classify | ~1 ms | nearest-centroid, or online QDA (§3b) |
| RS + CRC + fountain XOR | ~2 ms | |
| **Total** | **~13 ms** | comfortable inside 33 ms, in TypeScript |

Run it in a worker so the UI thread stays free for the feedback overlay. Amortised
RaptorQ decode attempts happen off the per-frame path. **Landed (M2):** the page keeps
the camera and the overlay; the worker owns the WASM eye. Frames cross as *transferred*
buffers, and a frame captured while the worker is busy is dropped rather than queued —
a decoder behind a live camera must shed load, and the skin repeats everything anyway.

### 3f. Failure modes and detection

| Failure | Detected by | Response |
|---|---|---|
| Torn frame (rolling shutter) | top/bottom beacon pulse# mismatch | salvage unaffected bands, erase the straddling ones |
| Motion blur | finder corner sharpness / edge gradient below threshold | drop frame; `HOLD STILL` |
| Glare / specular | pilot cells clipped in a region | mask those bands as erased; `TILT SCREEN` |
| Defocus | high variance in corner localisation; timing-track contrast low | `ADJUST FOCUS` |
| Too far (aliasing) | cells-per-camera-pixel from timing track < ~3 | `MOVE CLOSER` |
| Too close (clipped) | finder pattern off-frame | `MOVE BACK` |
| Ambient too bright / screen too dim | pilot dynamic range compressed | `DIM THE ROOM` |
| Colour misclassification | per-symbol CRC | erasure; if the rate is high, drop to B/W palette |
| Pulse rate too fast for this camera | clean-pulse yield vs. beacon counter gaps | `SLOW DOWN — 43%` |
| Corrupted reconstruction | **BLAKE3 vs. manifest** | fail loudly; never hand back an unverified file |
| Two transfers in view | stream ID in beacon | ignore foreign streams |

---

## 4. Stack — and where Rust earns its place

**Honest split: Rust owns the codec, TypeScript owns the browser. The image pipeline
starts in TS and moves only if a profiler says so.**

### Rust/WASM earns it — strongly

- **RaptorQ.** GF(256) matrix work over thousands of symbols. The `raptorq` crate is
  mature and WASM-clean. Writing this in TS would be painful *and* slow. Unambiguous win.
- **One codec definition shared by skin and eye.** Grid geometry, palette, pilot layout,
  symbol framing, CRC — defined once, used by both sides. Eliminates an entire bug class
  ("encoder and decoder disagree about cell ordering") that would otherwise cost days.
  This is the *underrated* reason and probably the strongest one.
- **The native simulator.** Because the codec is a Rust library, we can build a
  synthetic-channel harness — render pulse → warp → blur → noise → colour-mix → drop
  frames → decode — that runs at thousands of frames/sec in `cargo test`, with **no
  browser and no camera**. We can `proptest` "any distortion within envelope decodes
  correctly." That development-velocity win is worth more than all the runtime
  performance combined, and it's only available because we chose Rust.

### Rust/WASM is friction — skip it

- **The render path.** It's `putImageData`. Rust adds nothing.
- **Camera plumbing, `getUserMedia` constraints, permissions, UI overlay.** All TS, all
  DOM-shaped.
- ~~**The per-frame image pipeline — for now.**~~ **Reversed at M1a, and worth saying
  why.** The plan was to write finder detection, the homography and cell sampling in
  TypeScript and move them to WASM only under a profiler. But M0 built all three in
  Rust to drive the simulator, with tests — including two subtle bugs already found and
  fixed (single-linkage clustering, and a half-pixel coordinate convention). Rewriting
  that in TypeScript would mean re-deriving those fixes and maintaining two
  implementations of the one thing §4 says must never diverge: the definition skin and
  eye both agree on. So `geom` and `eye` moved into `cuttl-codec`, behind a borrowed
  `Raster` view with no image-library dependency, and the browser calls the same code.
  The boundary copy is real — 1080p RGB is ~6 MB/frame — but it is a memcpy at roughly
  1 ms, against re-implementing and re-debugging a working pipeline.

### Does the decode hot loop need WASM, or is WebCodecs + a worker enough?

**A worker is enough. WebCodecs is a nice-to-have, not a requirement.** The budget table
shows the headroom. The real wins are algorithmic — quarter-res marker search and sparse
gather instead of dense per-pixel passes. Reach for WASM in the image path only with a
profile showing otherwise, and expect that profile never to arrive.

### Repo layout

```
cuttl-codec/   Rust lib — geometry, palette, pilots, framing, RS, CRC,
               RaptorQ wrapper, manifest.  native + wasm32.
cuttl-sim/     Rust — synthetic optical channel. Native only. The test harness.
cuttl-cli/     Rust bin — encode/decode to PNG dirs. M0's deliverable.
cuttl-wasm/    wasm-bindgen shim: Encoder::next_pulse() -> &[u8],
               Decoder::ingest(...) -> Progress
web/           Vite + vanilla TS. No framework — this app doesn't need one.
               Canvas2D skin, getUserMedia + rVFC eye, decode worker.
```

Build: `wasm-pack` + `vite-plugin-wasm`. Tests: `cargo test` + `proptest` against the
sim; one Playwright smoke test for the browser shell. No server, ever — static hosting
only, which also makes it trivially a PWA later.

---

## 5. Milestones

Each ends with something observable.

**M0 — Simulator. No browser, no camera, no network.**
Rust CLI: encode a file to a directory of PNGs, decode it back. Then bolt on the
synthetic channel — perspective warp, blur, noise, colour mixing, frame drops — and prove
decode survives it.

> **Observable:** `cuttl encode f.bin -o pulses/ && cuttl decode pulses/ -o out.bin && cmp f.bin out.bin`
> Then the same with `--distort heavy` and 60% frame loss.

Highest-leverage milestone in the plan. Every subsequent optical bug becomes debuggable,
because we can ask "does the simulator reproduce it?" Skipping it means debugging codec
bugs through a camera lens. **Don't skip it.**

**M1 — Air gap crossed. Pathetic bitrate, real file.**
B/W only. 64×36 cells. ~6 pulses/sec. Two browser tabs, then two laptops.

> **Observable:** a real 20 KB file crosses the gap in ~20 s. **~1 KB/s.**

The grid was 48×27 in this plan until M0 step 3b measured what registration actually
costs. Finders plus separators are ~256 cells whatever the grid size, so on a 48×27
frame they were a quarter of everything. Enlarging amortises the fixed cost and more
than pays for itself: measured goodput went **64 → 160 B/pulse**.

**M2 — Robustness. The milestone where it stops being a demo.**
~~Per-band symbols~~, ~~dual beacons + tear detection~~, ~~inner RS~~, ~~CRC gating~~,
~~manifest stream~~, ~~BLAKE3 verify~~, ~~live eye feedback overlay~~ — all landed, and
the decode worker (§3e) landed with them. What remains of M2 is the capture corpus
below, which needs a real camera.

Also: **start the capture corpus.** Record a few hundred *real* camera frames of a known
stream across lighting, distance, angle, and motion conditions, and replay them through
the decoder in CI. The M0 simulator tests what we thought of; a capture corpus tests what
we didn't. This is our analogue of CUHK-CQRC (§9), and it only grows in value — every
field failure gets a recording added, and it never regresses again.

> **Observable:** handheld phone, normal room lighting, first try, no fiddling. That's
> the acceptance criterion — not a throughput number.
> Plus: `cargo test` replays the corpus and reports per-condition decode rates.

**M3 — Colour. With an A/B toggle so we get a measurement, not an assumption.**
Pilot cells, in-situ colour calibration, calibration pulses (§3b), CMI equalisation,
8-colour palette, confidence-based erasure marking. Nearest-centroid classifier only —
QDA is an M4 question, not an M3 one.

> **Observable:** a goodput meter and a real B/W-vs-colour number. **~13 KB/s ≈ 100
> kbit/s.** Prediction on record: the colour gain lands near 1.7×.

**M4 — Density.**
Shrink cells toward 96×54 then 160×90, push pulse rate, region-level glare masking,
optionally decode torn frames as two independent band sets. Radial distortion term if the
data demands it. Online QDA classifier (§3b) if M3's error rates justify it.

Speculative, flagged not decided: **ladder the inner RS strength across pulses** — some
light, some heavy — so that whatever the channel conditions are, a workable subset gets
through. Rateless-ish for the inner code, mirroring what the fountain layer does for the
outer, and a hedge against the skin having no way to learn conditions. It costs capacity
on the heavy pulses, so it needs M3 data before it earns a place.

> **Observable:** goodput-vs-distance and goodput-vs-lighting curves from a repeatable
> bench harness. **Stretch: ~45 KB/s ≈ 360 kbit/s**, tripod, large screen, short range.

**M5 — Product.** Multi-file manifests, drag-and-drop, PWA install, and the native shell
for the eye *if* iOS camera-control limits turn out to bind.

### Throughput summary — plan on the middle column

| | M1 | M3 (realistic) | M4 (stretch) |
|---|---|---|---|
| Grid | 64×36 | 96×54 | 160×90 |
| Bits/cell | 1 | 3 | 3 |
| Clean pulses/sec | 6 | 12 | 15 |
| **Goodput** | **~1 KB/s** | **13 KB/s** | **45 KB/s** |
| 1 MB file | ~17 min | ~80 s | ~23 s |

One free lever not in the table: **laptop→phone beats phone→phone substantially.** A 15"
screen at 40 cm fills the camera frame with far more resolvable cells than a 6" one.
Worth defaulting the docs to that direction.

---

## 6. Before scaffolding — open questions

1. **Accept the concatenated-code stack (§1b)?** The load-bearing disagreement.
   Everything downstream assumes inner RS + CRC gate + outer RaptorQ. If fountain-only
   is preferred first, better to argue it out now than in M3.

2. **Per-band symbols from M2, or from M1?** Band-symbols are the biggest structural win
   against rolling shutter, but they complicate M1's "get anything working" goal.
   Instinct: whole-pulse symbols in M1, bands in M2 — the codec crate is where that
   lives, so the churn is contained.

3. **Is iPhone-as-eye a hard requirement?** If yes, prototype the camera-control limits
   in M1 rather than discovering them at M4. If a laptop or Android can be the eye, the
   browser-only path holds all the way through.

4. **Vocabulary additions — need a ruling.** See §7.

---

## 7. Vocabulary

Locked:

| Term | Meaning |
|---|---|
| **skin** | the sender / rendering side |
| **eye** | the receiver / camera side |
| **pulse** | one rendered frame |
| **chroma cell** | one colour-carrying cell within a pulse |

Proposed, pending ruling:

| Term | Meaning |
|---|---|
| **band** | horizontal stripe of a pulse; one fountain symbol |
| **beacon** | the ECC-heavy fixed header at top and bottom of a pulse |
| **pilot** | a known-value chroma cell used for colour calibration |
| **cal pulse** | a payload-free pulse carrying a full-frame known calibration pattern |
| **stream** | one transfer session |

---

## 8. Prior art

- **txqr** (divan) — animated QR + fountain coding, Go. Closest conceptual relative, and
  the only one that shares our temporal dimension.
- **decimen-optical-transfer** (bashalarmistalt, 2026) — the txqr thesis rebuilt on the
  modern browser stack: animated B/W QR (up to v40, EC level L) + hand-rolled LT with
  robust soliton, zxing-cpp WASM decoding in workers fed by `requestVideoFrameCallback`,
  SHA-256 verify, self-describing per-frame header for mid-stream join. Claims
  **~129 KB/s handheld** on real phones. Two data points worth taking seriously: real
  cameras evidently resolve ~30k modules/frame at speed behind a mature detector — which
  de-risks our M4 density direction — and brute temporal redundancy (60 fps, fountain
  absorbs every straddled frame whole) works without any tear salvage when spatial
  density is that high. What it deliberately does not test is our layer: stock QR is
  binary by spec, whole-frame erasure granularity, and detection strength is rented from
  zxing rather than owned — the three constraints this project exists to remove.
- **Twibright Optar** — paper-based optical storage, Golay-based ECC on a plain
  black/white raster. The right reference for "how dense can a raster get before the
  optics give up."

See §9 for the polychrome symbology line — HCCB, HCC2D, JAB Code, HiQ — which is
adjacent but structurally different, and needs reading with care.

---

## 9. Related work: polychrome symbologies, and what transfers

The colour-barcode literature (Microsoft HCCB; HCC2D; JAB Code / ISO/IEC 23634:2022 from
Fraunhofer SIT; the HiQ decoding framework and its CUHK-CQRC dataset) is the closest body
of work on *colour* as a density lever. It is also the easiest thing on this page to
over-apply, because it solves a structurally different problem.

### The mismatch that governs everything else

| | JAB / HCC2D / HiQ | Cuttldrop |
|---|---|---|
| Reads per payload | exactly 1 | thousands |
| Medium | subtractive CMYK ink on paper | additive RGB, emissive |
| Illumination | ambient, reflective, uncontrolled | self-illuminated, we control it |
| Failure recovery | must live *inside* the symbol | across time, via fountain |
| Calibration | one shot, from a fixed patch | continuous, adaptive, per-session |
| Temporal artefacts | none | our **#1 bottleneck** (§2) |

Their whole ECC economics — JAB's eleven Reed–Solomon levels, HCC2D's structural
conservatism — exists to answer "there is no second chance." That is the *correct* answer
to their problem and the wrong one to ours. It is evidence **for** §1d, not against it:
they spend that capacity because they must, and we shouldn't.

Symmetrically, the literature is silent on rolling shutter, display persistence, and frame
tear — our top bottleneck. There is no prior art to crib there, which is a useful signal
about where the real engineering risk sits.

### What we take

| Idea | Where it lands |
|---|---|
| 8-colour palette = CMY+RGB+K+W | §3b — identical to our RGB-cube corners, reached from the opposite direction |
| ISO stopping at 8 colours | §1c — independent support for "3 bits/cell, don't chase 64" |
| In-symbol reference palette | §3b — our pilot cells; the field treats this as mandatory, so do we |
| Cross-module colour interference | §1c, §3e — the named mechanism behind our 1.7× estimate; attacked with equalisation |
| Reed–Solomon inner code | §1b — same mechanism, much lighter setting, because the fountain layer covers erasures |
| The CUHK-CQRC benchmark *idea* | M2 capture corpus — the dataset itself is print-scan and unusable |

### What we reject

- **CMYK primaries.** A printing constraint. We are emissive; use additive RGB corners.
- **Quiet-zone elimination, docked secondary symbols, irregular shapes.** Solutions to
  packaging-layout problems. We own the entire display — infinite quiet zone, free.
- **HiQ's pre-trained SVM/QDA as shipped.** It needs per-device-pair training data we
  cannot ship. Borrow the model class, not the fitting regime — fit online from pilots
  (§3b). We can do this precisely because we are dynamic; they cannot.
- **ML as the first tool against CMI.** CMI is deterministic and neighbour-dependent,
  i.e. inter-symbol interference. Equalise first (§3e). Reach for a learned classifier
  only if equalisation leaves error on the table.

### The cautionary tale, and where the innovation budget goes

HCCB did not die of colour. It died of **detection and alignment fragility** — triangles
are dense but poor for robust corner localisation, and the scanning hardware of the day
couldn't hold registration.

That is the most useful thing in this literature. Be boring in the geometry: QR-style
concentric-square finders, timing tracks, quiet zones we get for free (§3a). Spend the
innovation budget in the layer where we have a structural advantage the entire field
lacks — **time**.

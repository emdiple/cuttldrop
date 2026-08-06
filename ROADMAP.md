# Roadmap — the working improvement list

`DESIGN.md` §5 owns the **milestones** and stays authoritative on what M0–M5 mean.
This file is the ordered list of *work*, with the evidence for each item's position in
it. `COMPARISON-decimen.md` is where the R-numbered items came from.

Written 2026-08-04. Items marked **measured** have a number from that day's benchmarks,
reproduced at the bottom. Everything else is reasoned or adopted, and should be treated
as weaker until something measures it.

One caveat over the whole list: every performance figure in this repo, including the
new ones below, comes from a simulator or a Node benchmark. Nothing here has been
through a lens. Tier 3 is what changes that, and it outranks its position.

---

## Tier 0 — do before the hardware session, or it wastes the session

Each of these shows up in the first ten minutes of pointing a phone at a screen, and
each fails in a way that looks like something else.

### 1. On-demand pulse generation — **measured** · size M

`Skin::new` → `stream::encode_named` materialises **every pulse** into a `Vec<Pulse>`
before the first frame paints, synchronously, on the main thread.

| file | profile | pulses | encode | pulse RAM |
|---|---|---|---|---|
| 1 MB | m1 | 7,866 | 0.17 s | 18 MB |
| 5 MB | m1 | 39,323 | 0.70 s | 91 MB |
| 20 MB | m1 | 172,268 | **3.07 s** | **397 MB** |
| 20 MB | m2 | 11,039 | 1.94 s | 229 MB |
| 20 MB | m4 | 3,581 | 0.82 s | 74 MB |

Native release; wasm is slower. The page is frozen for the duration and the memory is
held for the whole session, so a large file on a sparse profile will lose a phone tab
outright. It also means our file-size ceiling is governed by RAM and undocumented —
see item 7.

The fix needs no wire-format change: retain the RaptorQ encoder and generate pulse *i*
on demand, as decimen's `LTEncoder.encode(seq)` does. The one-time intermediate-symbol
solve stays; per-pulse work leaves the critical path. That the timings scale with pulse
count (m1 3.07 s for 172k pulses vs m4 0.82 s for 3.6k) says per-pulse work dominates,
which is exactly the part that moves.

### 2. The eye cannot adopt a second stream — **verified defect** · size S

`Receiver::adopt` locks onto the first `(stream_id, hash_head)` it sees and returns
`false` for every disagreeing header thereafter. `Eye` has no reset and `eye.ts` never
rebuilds it.

So: aim at file A, don't let it finish, switch the skin to file B — or just change the
density menu, which re-encodes — and **every subsequent frame is rejected, permanently**.
The readout shows `0 new` over a perfectly good video feed, which is exactly what a
detection failure looks like.

decimen's `streamIdentity()` resets on *any* header-field disagreement, with the
reasoning that a stale decoder fails silently and only surfaces at the final checksum.
We are safer than them on collisions (u32 id **and** hash_head, against their 16-bit id)
and worse on the case that actually costs time.

Design call to make when implementing: reset on the first disagreeing header that passes
the CRC gate, or after two consecutive. Prefer **two** — a CRC-32 false accept is ~2⁻³²
per band, so the second frame is near-free insurance against discarding an in-flight
transfer on a fluke.

### 3. Bound the finder-candidate search — **measured** · size S

Cost of one frame through `Eye.ingest`, by what the camera is looking at:

| frame | locked (m2) | auto (4 grids) |
|---|---|---|
| aimed, decodes | 10 ms | 14 ms |
| too far / clipped / half off-screen | 5–6 ms | 9–24 ms |
| blank wall, flat grey, soft room detail | 3.5–4 ms | 15–18 ms |
| **dense high-frequency detail** | **204 ms** | **834 ms** |

Realistic mis-aiming is cheap, which is good news about the aiming experience. But an
image full of fine random detail produces a combinatorial explosion of false finder
candidates, and there is no bound on it. Reachable in practice: high-ISO sensor noise
in a dim room, foliage, carpet, a keyboard. At 834 ms the eye manages barely one frame
a second and the page looks hung.

Cap the candidate count, or early-out once it exceeds what any grid could consume. Note
that item 13 would remove most of this cost as a side effect. Worth adding a benchmark
alongside the fix: every existing test feeds the detector well-formed input, which is
why this went unnoticed.

### 4. Version and build stamp in the UI · size S

decimen shows `v0.2.0 · ed4cbcf` on the page. When the M1 session reports "38 KB/s,
12% torn", that number is worth far more attributed to a commit. Vite can inject
`git rev-parse --short HEAD` at build time; the eye already has a footnote line for it.

---

## Tier 1 — measured or adopted, not session-blocking

### 5. ~~iOS `frameRate: {exact}` first, `ideal` fallback (R6)~~ — **done 2026-08-05**
Landed with the rest of the camera work; see item 28. We ask for 30, not their 60.

### 6. Measure display refresh from rAF timestamps (R5) · size S
`skin.ts` still hardcodes `refreshRate = 60`. A 120 Hz ProMotion phone and a 60 Hz
laptop are both wrong in different directions.

### 7. Refuse oversized transfers before streaming (R4) · size M
decimen's `frame-capacity.ts` derives the true ceiling from header field widths — their
u16 `k` caps a stream at ~30 MB at 500 B/frame, not the advertised 64 MB — and refuses
up front, naming the setting that fixes it. We check nothing, and after item 1 our
ceiling becomes a real number worth stating rather than a memory cliff.

### 8. Adaptive compression (R3) · size M
Only when it shrinks the object; their `isPrecompressedType()` skip-list is liftable.
Worth more than when R3 was written: at m4's 131 KB/s the file itself becomes the
bottleneck, and source/logs/text compress 3–4×. Belongs in the codec so the CLI and the
browser agree — `flate2`'s pure-Rust backend compiles to wasm32.

### 9. Text snippet mode · size S
Send a block of text, not only a file. Arguably the *common* air-gap case: a key, a
config, a command, a wallet seed — and it saves the user creating a file first. The
manifest already carries name and mime, so this is a UI affordance over existing
machinery.

### 10. State the confidentiality trade-off (R1) · size S
decimen puts "anything on the sending screen is readable by any camera pointed at it"
on the landing page, not in a README footnote. That placement is correct. The property
this project gives you is **no network**, not privacy.

---

## Tier 2 — deferred, with the reason recorded

| # | Item | Why it waits |
|---|---|---|
| 11 | Decode worker pool | **Downgraded 2026-08-04.** A verified 8 ms/frame means one worker keeps up with a 30 fps camera. The earlier "several-fold multiplier" claim came from a native benchmark that was timing a *failing* decode. Revisit if a phone disagrees. |
| 12 | ~~Capture-loop generation counter (R7)~~ | **Done 2026-08-05**, and it stopped being theoretical on the way: `offerRetry` makes a second successful `start()` reachable, which is exactly the double-pump the note predicted. |
| 13 | Finder-geometry dimension estimate | `cols ≈ 7 × grid_px / finder_px` breaks the header/grid circularity properly and retires trial decode (`DESIGN.md` §3d). Also removes most of item 3's cost. QR gets this free from its format info; we chose to owe it. **Now cheaper than it was**: the interior alignment lattice gives a second, denser scale reference, so the estimate has more than four points to work from. |
| 14 | Worker count + capture width as bring-up knobs | Diagnostics, not configuration — deliberately unlike the density slider we refused. Only worth it if item 11 or the first session says so. |
| 15 | ~~Display-size slider~~ + restart button | **Slider done 2026-08-04** — in the strobing overlay, counted in whole px/cell because the canvas must stay an integer multiple of the grid. The restart button is still blocked on item 12. |

---

## Tier 3 — hardware-gated, and outranks everything above

### 16. The M1 observable
One real file, two physical devices, one real air gap. Still the highest-value step in
the project and the only one a terminal cannot do. The telemetry panel exists to make
its output a measurement rather than a boolean.

### 17. Capture corpus
Recorded real camera frames replayed in CI — M2's last outstanding item. Needs 16 first.

---

## Tier 4 — M5 and research

18. Passphrase encryption, XChaCha20-Poly1305, key never on the wire (R2)
19. Single-file standalone builds (R8)
20. PWA
21. Multi-file transfer
22. Glare masking — **then** revisit `Grid::bands`. Bands measured *worse* than none at
    7, and their remaining justification is glare, which is not yet simulated. The
    channel now has non-projective distortion (`skew`, `barrel`) but still no glare.
23. Re-check `ECC_LEN = 16` against warp-driven error rates now that sub-cell sampling
    error under perspective exists — and now that alignment correction has changed the
    residual error distribution it was last sized against
24. QDA and RS-strength laddering across pulses
25. Revisit `MANIFEST_PERIOD = 8` if sustained mono goodput ever outranks naming latency

---

## Landed since this list was written

27. **Interior alignment patterns** — 5×5 QR-style patterns on a lattice through the data
    region, with the eye interpolating their residuals over the corner homography. Closes
    a failure nothing in the stack detected: non-projective distortion (lens barrel,
    rolling-shutter pose drift) which a four-corner fit cannot represent at all. Measured
    2208 → 2 misread cells/frame at barrel 0.02, for 2.2% of the grid and +11% decode
    time. Full numbers in `DESIGN.md` §3a; sweeps re-runnable with
    `cargo test -p cuttl-sim --release -- --ignored --nocapture`.

    Two follow-ons this opens rather than closes: it makes item 13 cheaper (a denser scale
    reference than four corners), and the residual field it measures is exactly what a
    glare detector would want, since glare shows up as anchors that fail their contrast
    gate in a region.

28. **iPhone camera handling** — exact/ideal frame-rate negotiation, classified
    `getUserMedia` errors, retry that releases the stream, capture-loop generation
    counter, wake locks at both ends, probed (never sniffed) capabilities. Closes
    items 5 and 12 and the R6/R7 field notes. New shared module `web/src/platform.ts`.

    Still open from that cluster: **item 6**, measuring real display refresh from rAF
    timestamps. `skin.ts` still hardcodes 60, which is wrong in both directions — a
    120 Hz ProMotion phone and a 60 Hz laptop.

29. ~~Both screens were phone-shaped at every width~~ — **done 2026-08-06.** Above
    `62rem` each is now a two-column shell with a resident panel: skin controls left of
    the pulse, eye readout right of the preview. Below it, unchanged — one thing at a
    time, chrome floating. Neither page scrolls at any width (`100svh` +
    `overflow: hidden`, panels scroll internally).

    The load-bearing change is that the pulse is fitted to a measured `#stage` element
    instead of viewport-minus-chrome arithmetic, so one `maxScale()` serves both layouts
    and cannot disagree with what CSS did. Verified over CDP at 1440×900 and 390×844:
    pulse inside the stage, no overlap with the controls, no page scroll, 16 px/cell and
    6 px/cell respectively.

## Housekeeping

26. ~~No LICENSE file~~ — **done 2026-08-04.** Apache-2.0, and `Cargo.toml` narrowed
    from `MIT OR Apache-2.0` to match. The copyright appendix ships as the ASF
    distributes it, with the `[yyyy] [name of copyright owner]` template unfilled;
    naming a copyright holder is the owner's call, not a maintenance detail.

---

## Reproducing the measurements

The three tables above came from throwaway benchmarks rather than committed tests,
which is itself a gap — items 1 and 3 should land with permanent ones.

- **Encode latency / memory**: an example over `stream::encode_named` for 1/5/20 MB
  across `Profile::ALL`, timing the call and summing `pulses.len() × cols × rows`.
- **Frame cost**: `Eye.ingest` through the built `web/pkg`, on a nearest-neighbour
  upscale of `pulseRgba(0)` to the working raster, **checking the returned `Outcome`** —
  the first attempt at this measured a failing decode and reported it as a success,
  which is how the worker-pool item came to be over-ranked.
- Both ran native/Node on Apple silicon. A phone is slower by some factor nobody here
  has measured, which is item 16's job.

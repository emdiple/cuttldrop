# Cuttldrop

Move a file between two devices that have no network path between them. No wifi, no
bluetooth, no server, no pairing, no account.

One device — the **skin** — renders the file as a rapidly refreshing sequence of dense
colour frames. The other — the **eye** — points its camera at that screen and rebuilds
the file. The only thing crossing the gap is light.

The name is cephalopod. Cuttlefish strobe colour across their skin via chromatophores at
roughly 10 Hz; the physics here lands at almost exactly the same rate, for the same
reason — that is about as fast as you can change a surface and still have something read
it.

## Status

**Works end to end in simulation. Not yet run against a real camera.**

| | |
|---|---|
| Codec, fountain layer, FEC stack | done, 110 tests |
| Manifest + mandatory BLAKE3 verify | done — files arrive named, typed, and hash-checked |
| On-demand pulses + adaptive compression | done — three-frame lookahead; compress only when useful |
| Optical channel simulator | done — warp, tear, exposure blend, crosstalk, vignette, blur, noise |
| CLI (`cuttl encode` / `cuttl decode`) | done |
| Browser skin + eye | built and typechecked; decode runs in a worker; JS boundary tested |
| Product interface | responsive role flow, permanent desktop panels, drag/drop skin, live eye states |
| iOS camera handling | exact/ideal fps negotiation, classified errors, retry, wake lock |
| Dense/colour eye path | adaptive 1280→1920 capture, five-point cell sampling, pilot calibration |
| Skin display discipline | four-cell black quiet zone, physical-pixel fit, two-refresh pulse floor |
| QR Reference ladder | fixed standard QR v27/v35/v40-L writer + local ZXing reader; same RaptorQ, manifest, and BLAKE3 stream |
| QR RGB colour trial | three standard QR symbols multiplexed into R/G/B per frame, 3× payload; write→ZXing→ingest seam tested in Node |
| **A real file across a real air gap** | **not done** — needs two physical devices |

That last row is the honest headline. Everything upstream of the camera is verified;
the camera itself is not.

## Try it

### Offline, no camera

The CLI renders pulses to PNGs and reads them back, optionally through a synthetic
camera path. This is the fastest way to see the whole stack work.

```sh
cargo run --release -p cuttl-cli -- encode myfile.pdf -o pulses/
cargo run --release -p cuttl-cli -- decode pulses/ -o out.pdf
cmp myfile.pdf out.pdf

# Same thing, but throw away half the frames and mangle the rest.
cargo run --release -p cuttl-cli -- decode pulses/ -o out.pdf --distort heavy --loss 0.5
```

Both come back byte-identical, and the eye announces *"receiving myfile.pdf
(application/pdf) — N B expected"* within the first few frames: every 8th pulse carries
a manifest with the name, type, and BLAKE3 hash. Leave `-o` off and the output names
itself from the manifest. `--distort brutal` is deliberately past what the stack
survives, and fails loudly rather than returning a corrupt file.

### In a browser

```sh
cd web && npm install && npm run cert && npm run dev
```

Open `/skin.html` on the sending device and `/eye.html` on the receiving one.

For an optical A/B baseline, choose a **QR Reference** rung on the skin and **QR Reference ·
standard QR** in the eye's receiver mode before starting the camera. The ladder fixes each
symbol's version, ECC-L level and mask-4 pattern: v27 is 125 × 125 modules, v35 is
157 × 157, and v40 is 177 × 177, each plus a white four-module quiet zone. v40 carries
the most data (2,920 RaptorQ bytes per packet) but needs the largest, sharpest modules.
The QR layer alone changes; the file still goes through Cuttldrop's compression, RaptorQ,
manifest, CRC gate, and mandatory BLAKE3 verification. The ZXing reader WASM ships inside
the web build, so it does not fetch a decoder from a CDN while receiving.

Each rung also has a **QR RGB** variant (skin: *QR RGB v27/v35/v40*; eye: *QR RGB · one
QR per colour channel*): three standard symbols of the same version multiplexed into the
red, green and blue channels of a single frame, for three packets — up to 8.8 KB — per
refresh. Function patterns coincide across same-version symbols, so finders, timing and
alignment stay black and ZXing detects each separated channel as an ordinary QR code.
This is JAB Code's colour thesis on standard-QR geometry, and it sits deliberately
*between* the black-and-white control and the custom chroma cells: if b/w QR works where
QR RGB fails, the camera's colour handling is implicated before Cuttldrop's raster ever
enters the conversation.

`npm run cert` is not optional if either device is a phone. `navigator.mediaDevices`
does not exist outside a secure context — `localhost` counts, the `https://192.168.x.x`
a phone uses to reach your laptop does not — so without TLS the eye page loads,
looks fine, and has no camera.

Open the address Vite prints under `Network:`, **with the `https://` on the front**. A
bare `192.168.x.x:5173` makes the browser try http, which this server does not speak —
the empty reply looks exactly like "the server isn't on the network". The dev server
warns if the certificate no longer covers the current address, which happens whenever the
machine changes network: a phone hotspot hands out a fresh lease every reconnect.

**The certificate warning is expected.** A self-signed cert is signed by nobody, so the
browser correctly says so: *Show Details → visit this website* on Safari, *Advanced →
Proceed* on Chrome, once per device. It has no bearing on the camera — a secure context
is a secure context once the connection is accepted. If you have `mkcert` installed,
`npm run cert` uses it instead and prints how to install its root on the phone, after
which there is no warning at all.

### Without a second device

Three of the four test surfaces need no camera and no network at all.

```sh
cargo test --workspace                    # 108 tests, the full synthetic optical channel
cargo run --release -p cuttl-cli -- encode f.pdf -o pulses/ && \
cargo run --release -p cuttl-cli -- decode pulses/ -o out.pdf --distort heavy --loss 0.5
cd web && npm test                        # file -> WASM skin -> WASM eye -> file, in Node
```

The fourth is the browser, and it has a one-machine mode. Open `/skin.html` in its own
window, open `/eye.html` in another, and press **Read a window instead** — the eye takes
a `getDisplayMedia` stream in place of a camera and reads the skin's window directly.
Everything downstream is the real path: rVFC pacing, the transferred-buffer hop to the
worker, locate, homography, sampling, Reed-Solomon, the CRC gate, the fountain, BLAKE3.

What it does **not** exercise is the optics — no perspective, no rolling-shutter tear,
no glare, no lens blur, no auto-exposure fighting a strobing panel. A pass means the
software is right. It is not the M1 observable and a goodput figure from it must never
be quoted as one, which is why the readout says `screen` rather than `camera`.

## How it works

The interesting part is not the picture, it is what is underneath it. This is a
communications problem wearing a graphics costume.

**A fountain code repairs erasures, not errors** — and a camera pointed at a screen
produces both. One silently corrupted symbol propagates through the decoder and poisons
the whole file. So the stack is concatenated:

```
skin:  file → useful compression → fountain → framing → inner Reed-Solomon → cells → screen
eye:   camera → locate → sample → RS correct → CRC gate → fountain → restore → BLAKE3 → file
```

The **CRC gate** in the middle is the load-bearing piece: it converts errors into
erasures, which is the one thing the fountain layer can actually repair. Nothing
unverified ever reaches the decoder — and the **BLAKE3 check** at the very end is the
only statement about the *file*: the eye holds the expected hash (it rides in the
manifest, every 8th pulse, along with the filename and mime type) and refuses to hand
anything back until the reconstruction matches. The same manifest is why the eye can
say *"receiving cuttlefish.pdf — 2.4 MB"* a second after it starts looking.

The skin prepares RaptorQ once and generates pulses on demand behind a three-frame
lookahead; selecting a large file no longer rasterises and retains its entire repair
loop. Before fountain coding, v4 tries raw DEFLATE for plausibly compressible data and
keeps it only when at least 64 B are saved. Already-compressed media stays untouched;
the manifest carries the original length and the final BLAKE3 is always checked against
the restored original.

Rendering adds an opaque-black four-cell quiet zone without changing the pulse bytes.
Every chroma cell is an integer number of physical display pixels—even at fractional
device pixel ratios—and every pulse remains on the panel for at least two refreshes.
While sending, the optical stage is pure black: the desktop controls remain resident in
their side panel, but texture, glow, and illuminated edges are removed around the pulse.

There is also a deliberately conventional **QR Reference** ladder. It wraps those same
framed fountain packets in fixed standard QR v27-L, v35-L, or v40-L symbols, generated
by `qrcode` and decoded by the locally bundled `zxing-wasm` reader. Its white quiet zone,
standard finder/alignment patterns, and mature detector make it the control experiment for
the custom chroma-cell renderer—not a replacement for it. If the reference profile works
in the same physical setup and a custom profile does not, the evidence points at
Cuttldrop's optical raster rather than its transport or file-integrity layers.

The **QR RGB** variant multiplexes three of those symbols into one frame's colour
channels. Nothing changes in Rust — which packets share a frame is a rasterization
detail, and every packet still crosses the CRC gate alone, so a channel ruined by
Bayer-filter or subpixel crosstalk costs one symbol, never the frame. It extends the A/B
ladder into colour: b/w QR isolates the optics, QR RGB adds only colour separation on
top, and the custom profiles add Cuttldrop's own raster on top of that.

**The eye locates the grid rather than being told where it is.** Four QR-style
concentric-square finders, found by scanning for the 1:1:3:1:1 run-length ratio, then
four correspondences into a homography. Perspective is exact for a planar target under a
pinhole camera — it is the *easy* part of the problem.

What a homography *cannot* represent is anything non-projective: lens barrel distortion,
and the pose drift of a handheld camera across a rolling shutter's 10–30 ms readout. That
error is harmless until it exceeds half a cell and catastrophic immediately after, with
nothing in between — measured, 12 misread cells per frame becomes 2208 across one step of
the sweep. So the dense grids carry an interior lattice of QR-style **alignment
patterns**, 5×5 rather than the finder's 7 so they can never be mistaken for one. The eye
predicts each through the corner fit, finds where it actually landed, and interpolates
the difference. Costs 2.2% of the grid, buys a 1.5× wider distortion tolerance, and turns
that 2208 back into 2.

Dense profiles do not permanently pay the CPU and memory cost of full-resolution camera
frames. The eye starts at 1280 pixels wide for M1, asks the camera to preserve a
1920-wide source, and escalates only when a dense profile locks or eight frames cannot
be identified. It never upscales a lower-resolution camera mode. Each chroma cell is the
median of five interior samples rather than one fragile centre pixel. In colour mode,
64 spatially distributed pilot references teach the eye the observed low/high level of
each camera channel, replacing the old fixed RGB=128 threshold.

**The hard part is time.** Rolling shutter reads a sensor row by row over 10–30 ms, so a
capture that straddles a pulse flip is stitched from two different frames. Each pulse
carries a duplicated counter in strips top and bottom; if they disagree, the frame was
torn. That does not make the transfer correct — the CRC already does — but it makes the
failure *legible*, which is what lets the receiver tell a human to slow down.

**The human is the back channel.** Nothing adapts automatically, because nothing can. The
eye displays `FILL THE FRAME` / `SLOW THE SENDER DOWN` / `HOLD STILL`, and a person acts
on it. That is a real, if low-bandwidth, control loop.

**No network also means no confidentiality.** Whatever the skin shows, any camera with
line of sight can read. What this design buys is the absence of a network path — not
privacy. Optional passphrase encryption is planned for a later product milestone.

## Numbers

Measured, not estimated. Goodput is payload bytes per pulse after every layer of
overhead.

| Profile | Grid | Bits/cell | Payload cells | Goodput | At 20 Hz | At 25 Hz |
|---|---|---|---|---|---|---|
| M1 — safe | 64 × 36 | 1 | 73% | 160 B/pulse | 2.7 KB/s | 2.6 KB/s |
| M2 — dense | 192 × 108 | 1 | 89% | **2064 B/pulse** | 37.4 KB/s | **43.7 KB/s** |
| M3 — colour | 96 × 54 | 3 | 80% | 1360 B/pulse | 24.7 KB/s | 28.3 KB/s |
| M4 — dense colour | 192 × 108 | 3 | 89% | **6336 B/pulse** | 114.2 KB/s | **130.2 KB/s** |

The last two columns are measured *end to end* through the timed shutter model — a
30 fps camera with phone shutter timing watching a screen flipping at the stated rate,
with every torn and rejected frame charged against the total. They are not goodput
multiplied by pulse rate. That arithmetic runs 9–16% optimistic, because it assumes
every capture lands clean and none do. Reproduce with:

```sh
cargo test -p cuttl-sim --release -- --ignored --nocapture profile_sweep
```

The skin picks one; the eye works out which by trying each grid until one passes the
CRC gate, so density is a menu on one device only.

Two things that table is really saying. **M2 beats M3** — density is a bigger lever
than colour, and it is the safer one, since nothing about a mono grid depends on a
camera's white balance. That is the project's bottleneck ordering showing up as a
number. And **the payload column is why**: registration costs the same four finders
whatever the grid, so a 9× cell count buys 13× the bytes. Small grids do not merely
carry less, they spend a quarter of themselves saying where they are.

**25 Hz, not 20, is the peak for the dense profiles** — worth 15% on both M2 and M4, and
enough to clear what the old arithmetic promised. The 20 Hz figure was measured on M1
alone, where the two rates sit inside the noise of each other. Stay clear of 30: that is
the capture rate, the phase relationship freezes there, and goodput drops while torn
frames roughly triple. Past 35 the shutter window approaches the pulse period and the
whole thing collapses.

4 px/cell at the sensor is the measured floor — the cliff is between 3 and 2. Every
figure here comes out of the simulator. Nothing in this table has met a real camera yet,
which is exactly what the eye's goodput readout exists to settle.

## Layout

```
crates/cuttl-codec/   the shared definition — geometry, palette, framing, FEC, the eye
                      pipeline. Compiles native and to wasm32; both ends run this.
crates/cuttl-sim/     synthetic optical channel. Native only, and the primary test
                      surface: thousands of frames per second, no camera required.
crates/cuttl-cli/     `cuttl` — encode and decode PNG pulse directories.
crates/cuttl-wasm/    wasm-bindgen shim: custom Skin/Eye and QR Reference packet bridge.
web/                  Vite + TypeScript. No framework, no server, no build-time magic.
README.md             the tracked project documentation.
```

## Development

```sh
cargo test --workspace     # the simulator is the main test surface
cargo fmt --all && cargo clippy --workspace --all-targets
cd web && npm test         # JS boundary round trip, no browser needed
```

## Prior art

**txqr** (divan) — animated QR plus fountain coding; the closest relative, and the only
one that shares the temporal dimension. **decimen-optical-transfer** — the same thesis
on the modern browser stack (animated QR + LT codes + ZXing WASM), with a real-device
~129 KB/s claim; its `qrcode`/ZXing optical pairing is now Cuttldrop's QR Reference
control. **Twibright Optar** — paper-based optical
storage, the right reference for how dense a raster can get before the optics give up.
**JAB Code** (ISO/IEC 23634:2022) — the polychrome symbology line; adjacent but built for
a single static read, which changes the economics completely. Cuttldrop borrows the
registration lessons while keeping a dynamic, fountain-coded transport.

## License

Apache-2.0. See [LICENSE](LICENSE).

The copyright appendix ships as the ASF distributes it, with the template placeholders
unfilled — naming a copyright holder is a decision for whoever owns the work, not a
detail to be inherited from a build tool's warning.

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
| Codec, fountain layer, FEC stack | done, 89 tests |
| Manifest + mandatory BLAKE3 verify | done — files arrive named, typed, and hash-checked |
| Optical channel simulator | done — warp, tear, exposure blend, crosstalk, vignette, blur, noise |
| CLI (`cuttl encode` / `cuttl decode`) | done |
| Browser skin + eye | built and typechecked; decode runs in a worker; JS boundary tested |
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

`npm run cert` is not optional if either device is a phone. `navigator.mediaDevices`
does not exist outside a secure context — `localhost` counts, the `https://192.168.x.x`
a phone uses to reach your laptop does not — so without TLS the eye page loads,
looks fine, and has no camera. The cert is self-signed, so Safari warns once per
device: *Show Details → visit this website*. For no warning at all, use `mkcert` and
install its root on the phone.

## How it works

The interesting part is not the picture, it is what is underneath it. This is a
communications problem wearing a graphics costume.

**There is no back channel.** The sender's camera is not watching the receiver, so there
are no acknowledgements and no retransmit requests. The transport is therefore a
**rateless fountain code** (RaptorQ, RFC 6330): the skin loops forever and the eye
reconstructs from any sufficient subset of frames, in any order.

**A fountain code repairs erasures, not errors** — and a camera pointed at a screen
produces both. One silently corrupted symbol propagates through the decoder and poisons
the whole file. So the stack is concatenated:

```
skin:  file → fountain → framing → inner Reed-Solomon → cells → screen
eye:   camera → locate → sample → RS correct → CRC gate → fountain → BLAKE3 → file
```

The **CRC gate** in the middle is the load-bearing piece: it converts errors into
erasures, which is the one thing the fountain layer can actually repair. Nothing
unverified ever reaches the decoder — and the **BLAKE3 check** at the very end is the
only statement about the *file*: the eye holds the expected hash (it rides in the
manifest, every 8th pulse, along with the filename and mime type) and refuses to hand
anything back until the reconstruction matches. The same manifest is why the eye can
say *"receiving cuttlefish.pdf — 2.4 MB"* a second after it starts looking.

**The eye locates the grid rather than being told where it is.** Four QR-style
concentric-square finders, found by scanning for the 1:1:3:1:1 run-length ratio, then
four correspondences into a homography. Perspective is exact for a planar target under a
pinhole camera — it is the *easy* part of the problem.

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
privacy. Optional passphrase encryption is on the roadmap (`COMPARISON-decimen.md`, R2).

## Numbers

Measured, not estimated. Goodput is payload bytes per pulse after every layer of
overhead.

| Profile | Grid | Bits/cell | Payload cells | Goodput | At 20 Hz |
|---|---|---|---|---|---|
| M1 — safe | 64 × 36 | 1 | 73% | 160 B/pulse | 3.2 KB/s |
| M2 — dense | 192 × 108 | 1 | 91% | **2128 B/pulse** | 43 KB/s |
| M3 — colour | 96 × 54 | 3 | 83% | 1408 B/pulse | 28 KB/s |
| M4 — dense colour | 192 × 108 | 3 | 91% | **6560 B/pulse** | 131 KB/s |

The skin picks one; the eye works out which by trying each grid until one passes the
CRC gate, so density is a menu on one device only.

Two things that table is really saying. **M2 beats M3** — density is a bigger lever
than colour, and it is the safer one, since nothing about a mono grid depends on a
camera's white balance. That is `DESIGN.md` §2's bottleneck ordering showing up as a
number. And **the payload column is why**: registration costs the same four finders
whatever the grid, so a 9× cell count buys 13× the bytes. Small grids do not merely
carry less, they spend a quarter of themselves saying where they are.

20 Hz is the measured optimum against a 30 fps camera, and 4 px/cell at the sensor is
the measured floor — the cliff is between 3 and 2. Every figure here comes out of the
simulator. Nothing in this table has met a real camera yet, which is exactly what the
eye's goodput readout exists to settle.

## Layout

```
crates/cuttl-codec/   the shared definition — geometry, palette, framing, FEC, the eye
                      pipeline. Compiles native and to wasm32; both ends run this.
crates/cuttl-sim/     synthetic optical channel. Native only, and the primary test
                      surface: thousands of frames per second, no camera required.
crates/cuttl-cli/     `cuttl` — encode and decode PNG pulse directories.
crates/cuttl-wasm/    wasm-bindgen shim: Skin and Eye.
web/                  Vite + TypeScript. No framework, no server, no build-time magic.
DESIGN.md             the architecture document, and the reasoning behind every choice.
ROADMAP.md            the ordered work list, and the measurements behind the order.
```

`DESIGN.md` is worth reading before changing anything. It records not just what was
decided but what was measured, including several predictions that turned out wrong and
were corrected in place.

## Development

```sh
cargo test --workspace     # the simulator is the main test surface
cargo fmt --all && cargo clippy --workspace --all-targets
cd web && npm test         # JS boundary round trip, no browser needed
```

## Prior art

**txqr** (divan) — animated QR plus fountain coding; the closest relative, and the only
one that shares the temporal dimension. **decimen-optical-transfer** — the same thesis
on the modern browser stack (animated QR + LT codes + zxing WASM), with a real-device
~129 KB/s claim; it rents its symbol layer from QR, which is exactly the layer this
project builds. **Twibright Optar** — paper-based optical
storage, the right reference for how dense a raster can get before the optics give up.
**JAB Code** (ISO/IEC 23634:2022) — the polychrome symbology line; adjacent but built for
a single static read, which changes the economics completely. `DESIGN.md` §9 covers what
transfers from that work and what does not.

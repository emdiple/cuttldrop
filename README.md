# Cuttldrop

Move a file between two devices that have no network path between them. No wifi, no
bluetooth, no server, no pairing, no account.

One device — the **skin** — renders the file as a rapidly refreshing sequence of QR
frames. The other — the **eye** — points its camera at that screen and rebuilds the
file. The only thing crossing the gap is light.

The name is cephalopod. Cuttlefish strobe colour across their skin via chromatophores at
roughly 10 Hz; the physics here lands at nearly the same rate, for the same reason —
that is about as fast as you can change a surface and still have something read it. The
RGB mode even strobes colour.

## Status

**Works end to end in software. Not yet run against a real camera.**

| | |
|---|---|
| Transport: RaptorQ fountain, per-packet CRC gate, periodic manifest | done |
| Mandatory BLAKE3 verify | done — files arrive named, typed, and hash-checked |
| Adaptive compression | done — raw DEFLATE only when it pays for itself |
| QR ladder | fixed standard QR v27/v35/v40-L writer + local ZXing reader |
| QR RGB colour mode | three standard symbols multiplexed into R/G/B per frame, 3× payload |
| Optical seam test | every packet write→ZXing→ingest in Node, b/w and RGB, all rungs |
| Product interface | responsive role flow, drag/drop skin, live eye states |
| iOS camera handling | exact/ideal fps negotiation, classified errors, retry, wake lock |
| **A real file across a real air gap** | **not done** — needs two physical devices |

That last row is the honest headline. Everything upstream of the camera is verified;
the camera itself is not.

## Try it

### In a browser

```sh
cd web && npm install && npm run cert && npm run dev
```

Open `/skin.html` on the sending device and `/eye.html` on the receiving one.

The skin's profile menu is a density ladder with two halves. The black-and-white rungs
put one fixed standard QR symbol per frame: v27-L is 125 × 125 modules carrying 1,432
RaptorQ bytes, v35-L is 157 × 157 carrying 2,264, v40-L is 177 × 177 carrying 2,920 —
each plus a white four-module quiet zone, and each denser rung needing larger, sharper
modules at the camera. The **QR RGB** rungs multiplex three standard symbols of the same
version into the red, green and blue channels of a single frame, for three packets — up
to 8.8 KB — per refresh. Function patterns coincide across same-version symbols, so
finders, timing and alignment stay black and ZXing detects each separated channel as an
ordinary QR code. Match the mode in the eye's receiver menu before starting the camera:
black-and-white reads one symbol per frame, QR RGB reads each colour channel as its own.

Whatever the rung, the file goes through the same stack: compression when it pays,
RaptorQ, a periodic manifest, the CRC gate, and mandatory BLAKE3 verification. The ZXing
reader WASM ships inside the web build, so the eye does not fetch a decoder from a CDN
while receiving.

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

Two of the three test surfaces need no camera and no network at all.

```sh
cargo test --workspace     # framing, fountain, manifest, BLAKE3 — native
cd web && npm test         # the same, through wasm-bindgen — plus the optical
                           # seam: every packet rasterized as a real QR symbol,
                           # decoded by the bundled ZXing reader, reingested
```

The third is the browser, and it has a one-machine mode. Open `/skin.html` in its own
window, open `/eye.html` in another, and press **Read a window instead** — the eye takes
a `getDisplayMedia` stream in place of a camera and reads the skin's window directly.
Everything downstream is the real path: rVFC pacing, the transferred-buffer hop to the
worker, ZXing detection, channel separation in RGB mode, the CRC gate, the fountain,
BLAKE3.

What it does **not** exercise is the optics — no perspective, no rolling-shutter tear,
no glare, no lens blur, no auto-exposure fighting a strobing panel, and above all no
colour crosstalk between a screen's subpixels and a camera's Bayer filter, which is the
open question hanging over the RGB mode. A pass means the software is right. It says
nothing about a camera, which is why the readout says `screen` rather than `camera`.

## How it works

The interesting part is not the picture, it is what is underneath it. This is a
communications problem wearing a graphics costume.

**A fountain code repairs erasures, not errors** — and a camera pointed at a screen
produces both. One silently corrupted symbol propagates through the decoder and poisons
the whole file. So the stack is concatenated:

```
skin:  file → useful compression → fountain → framing → QR writer → screen
eye:   camera → ZXing decode → CRC gate → fountain → restore → BLAKE3 → file
```

QR's own ECC corrects a symbol or its decoder rejects it wholesale. The **CRC gate**
behind it is the load-bearing piece: it converts whatever survives into a clean
accept-or-erase decision, which is the one thing the fountain layer can actually repair.
Nothing unverified ever reaches the decoder — and the **BLAKE3 check** at the very end is
the only statement about the *file*: the eye holds the expected hash (it rides in the
manifest, every 8th packet, along with the filename and mime type) and refuses to hand
anything back until the reconstruction matches. The same manifest is why the eye can
say *"receiving cuttlefish.pdf — 2.4 MB"* a second after it starts looking.

The skin prepares RaptorQ once and rasterizes packets on demand behind a three-frame
lookahead; selecting a large file does not materialise its entire repair loop. Before
fountain coding, the skin tries raw DEFLATE for plausibly compressible data and keeps it
only when at least 64 B are saved. Already-compressed media stays untouched; the
manifest carries the original length and the final BLAKE3 is always checked against the
restored original.

**The QR RGB mode multiplexes three symbols into one frame's colour channels.** Nothing
changes in Rust — which packets share a frame is a rasterization detail, and every
packet still crosses the CRC gate alone, so a channel ruined by Bayer-filter or subpixel
crosstalk costs one symbol, never the frame. This is JAB Code's colour thesis on
standard-QR geometry: the mature detector, finders and quiet zone are untouched, and
colour only ever adds payload on top of a symbol a plain reader could refuse. If
black-and-white works in a physical setup and RGB does not, the camera's colour handling
is implicated — the ladder is an experiment you can climb one variable at a time.

**The eye sheds load rather than queueing it.** ZXing detection runs in a small pool of
workers — frames are independent, so they pipeline across cores, and the three passes an
RGB frame costs no longer serialize the stream — while a single sink worker owns the
stream state behind them. Frames cross as transferred GPU-backed bitmaps — the pixel
readback happens in the worker, not on the page's thread — or as plain transferred
buffers where the platform insists, and a frame captured while every decoder is busy is
dropped — the skin repeats everything anyway. A rateless stream has no packet you
cannot afford to miss.

**The human is the back channel.** Nothing adapts automatically, because nothing can. The
eye displays `FILL THE FRAME` / `HOLD STILL`, and a person acts on it. That is a real,
if low-bandwidth, control loop.

**No network also means no confidentiality.** Whatever the skin shows, any camera with
line of sight can read. What this design buys is the absence of a network path — not
privacy. Optional passphrase encryption is planned for a later product milestone.

## Numbers

Payload per frame is exact; the rates are arithmetic at each rung's default sender
frequency, **not** measured optical throughput — nothing here has met a real camera yet,
which is exactly what the eye's goodput readout exists to settle.

| Profile | Symbol | RaptorQ payload/frame | Default rate | Arithmetic ceiling |
|---|---|---|---|---|
| QR v27-L | 125 × 125 | 1,432 B | 24 Hz | 34.4 KB/s |
| QR v35-L | 157 × 157 | 2,264 B | 20 Hz | 45.3 KB/s |
| QR v40-L | 177 × 177 | 2,920 B | 15 Hz | 43.8 KB/s |
| QR RGB v27-L | 125 × 125 × 3 | 4,296 B | 15 Hz | 64.4 KB/s |
| QR RGB v35-L | 157 × 157 × 3 | 6,792 B | 12 Hz | 81.5 KB/s |
| QR RGB v40-L | 177 × 177 × 3 | 8,760 B | 10 Hz | 87.6 KB/s |

The default rates encode a real trade: denser rungs cost the eye more detection time per
frame, and the RGB rungs cost three ZXing passes, so their defaults sit lower — the
tripled payload is what keeps them ahead. Real goodput will sit below every number in
the last column, because some captures tear, blur, or miss; the eye's telemetry
(capture rate, decode rate, new/duplicate, goodput, ETA) is the honest scoreboard.

## Layout

```
crates/cuttl-codec/   the shared transport — packet framing, CRC gate, RaptorQ
                      fountain, manifest, BLAKE3 verify. Compiles native and to
                      wasm32; both ends run this.
crates/cuttl-wasm/    wasm-bindgen shim: ReferenceSkin (packets out) and
                      ReferenceEye (packets in). No pixels cross this boundary.
web/                  Vite + TypeScript. QR writer (`qrcode`), reader
                      (`zxing-wasm`, bundled), camera handling, pacing, UI.
                      No framework, no server, no build-time magic.
README.md             the tracked project documentation.
```

## Development

```sh
cargo test --workspace
cargo fmt --all && cargo clippy --workspace --all-targets
cd web && npm test         # JS boundary + optical seam, no browser needed
```

## Prior art

**txqr** (divan) — animated QR plus fountain coding; the closest relative.
**decimen-optical-transfer** — the same thesis on the modern browser stack (animated QR
+ LT codes + ZXing WASM), with a real-device ~129 KB/s claim; its `qrcode`/ZXing optical
pairing is the direct ancestor of Cuttldrop's. Cuttldrop differs in the transport above
the symbol: RaptorQ rather than LT, a CRC gate ahead of the fountain, and a manifest
with mandatory BLAKE3 verification. **JAB Code** (ISO/IEC 23634:2022) — the polychrome
symbology line, and the inspiration for the QR RGB mode; JAB redesigns the whole symbol
around colour, where Cuttldrop keeps standard-QR geometry and lets colour carry extra
standard symbols. **Twibright Optar** — paper-based optical storage, the right reference
for how dense a raster can get before the optics give up.

## License

Apache-2.0. See [LICENSE](LICENSE).

The copyright appendix ships as the ASF distributes it, with the template placeholders
unfilled — naming a copyright holder is a decision for whoever owns the work, not a
detail to be inherited from a build tool's warning.

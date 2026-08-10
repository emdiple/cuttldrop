// The QR Reference optical seam, with no browser and no camera.
//
// roundtrip.mjs hands packet bytes across the WASM boundary directly; this is
// the only test that crosses the *picture*. Every packet is rasterized exactly
// as the skin paints it, decoded by the same locally bundled ZXing reader the
// eye worker runs, and only then fed to the ReferenceEye — so the writer, the
// reader, and their agreement about version, mask and byte mode are all under
// test. The RGB half additionally proves the colour-multiplexing claim: each
// separated channel of a colored frame is a complete standard QR symbol.
//
// What this deliberately does not exercise is the optics — no perspective and
// no blur, and the channel crosstalk at the end is a linear *model* of a real
// screen and sensor, not the thing itself. A pass here means the software is
// right; only a camera can say the rest.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { prepareZXingModule, readBarcodes } from "zxing-wasm/reader";
import {
  QR_QUIET_MODULES,
  QR_REFERENCE_PROFILES,
  RGB_CHANNELS,
  TILE_COUNT,
  rasterizeReferencePacket,
  rasterizeRgbReferencePackets,
  rasterizeTiledReferencePackets,
} from "../src/qr-reference.ts";
import { channelToGrey } from "../src/rgb-channel.ts";
import { READER_OPTIONS } from "../src/reader-options.ts";
import { applyUnmix, calibrateFrame } from "../src/rgb-calibration.ts";

const pkg = new URL("../pkg/cuttl_wasm.js", import.meta.url);
const wasm = new URL("../pkg/cuttl_wasm_bg.wasm", import.meta.url);
const { default: init, ReferenceEye, ReferenceSkin, Outcome } = await import(pkg.href);
await init({ module_or_path: await readFile(fileURLToPath(wasm)) });

// The browser resolves the reader WASM through Vite's asset pipeline; Node
// reads the same file straight out of the installed package.
const zxingWasm = fileURLToPath(import.meta.resolve("zxing-wasm/reader/zxing_reader.wasm"));
await prepareZXingModule({ overrides: { wasmBinary: (await readFile(zxingWasm)).buffer } });

// The decode workers' exact options — imported, not copied, so this test
// cannot silently drift from the path the eye actually runs.
const ZXING_READ = READER_OPTIONS;

const NAME = "optical.bin";
const MIME = "application/test";

// Deterministic but incompressible-looking bytes: a compressible pattern would
// deflate to a handful of packets and leave most of the loop unexercised.
const object = new Uint8Array(30_000);
let state = 0x2c1b3c6d;
for (let i = 0; i < object.length; i += 1) {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  object[i] = state & 0xff;
}

/** Decode one raster the way the worker would, returning the packet bytes. */
async function decodeRaster(rgba, width, height) {
  const results = await readBarcodes({ data: rgba, width, height }, ZXING_READ);
  const decoded = results.find((result) => result.isValid && result.bytes.length > 0);
  return decoded?.bytes ?? null;
}

/** One channel replicated to grey — the exact separation the worker runs. */
function channelPlane(rgba, channel) {
  return channelToGrey(rgba, channel, new Uint8ClampedArray(rgba.length));
}

for (const [profile, spec] of Object.entries(QR_REFERENCE_PROFILES)) {
  // Black and white: one packet per frame through writer and reader.
  const skin = new ReferenceSkin(object, NAME, MIME, profile, 0x0b1a, 0.5);
  const eye = new ReferenceEye();
  let frames = 0;
  let outcome;
  for (let i = 0; i < skin.packetCount; i += 1) {
    const raster = rasterizeReferencePacket(skin.packet(i), profile);
    assert.equal(raster.width, spec.size, `${profile} raster width drifted`);
    const bytes = await decodeRaster(raster.rgba, raster.width, raster.height);
    assert.ok(bytes, `${profile} packet ${i} did not decode as a QR symbol`);
    frames += 1;
    outcome = eye.ingest(bytes);
    if (outcome === Outcome.Completed) break;
  }
  assert.equal(outcome, Outcome.Completed, `${profile} never completed`);
  assert.equal(eye.fileName, NAME);
  assert.deepEqual(eye.takeObject(), object, `${profile} optical round trip differs`);
  console.log(`ok — ${profile}: ${object.length} B through write→ZXing→ingest in ${frames} symbols`);
}

for (const [profile, spec] of Object.entries(QR_REFERENCE_PROFILES)) {
  // RGB: three packets per frame, one standard QR per colour channel.
  const skin = new ReferenceSkin(object, NAME, MIME, profile, 0xc0107, 0.5);
  const eye = new ReferenceEye();
  let next = 0;
  const take = () => {
    const packet = skin.packet(next);
    next = (next + 1) % skin.packetCount;
    return packet;
  };
  let frames = 0;
  let colored = false;
  let done = false;
  while (!done) {
    assert.ok(frames <= skin.packetCount, `${profile} RGB never completed`);
    const raster = rasterizeRgbReferencePackets(
      Array.from({ length: RGB_CHANNELS }, take),
      profile,
    );
    frames += 1;
    // The frame must actually be colored *inside the symbol*: some module
    // dark in one channel and light in another, or the channels collapsed
    // into one symbol. Bounded to the square symbol region deliberately —
    // the calibration strip below it is colored by construction and would
    // make a whole-frame check pass vacuously.
    for (let at = 0; at < raster.width * raster.width * 4 && !colored; at += 4) {
      colored = new Set([raster.rgba[at], raster.rgba[at + 1], raster.rgba[at + 2]]).size > 1;
    }
    for (let channel = 0; channel < RGB_CHANNELS && !done; channel += 1) {
      const bytes = await decodeRaster(
        channelPlane(raster.rgba, channel),
        raster.width,
        raster.height,
      );
      assert.ok(bytes, `${profile} RGB frame ${frames} channel ${channel} did not decode`);
      done = eye.ingest(bytes) === Outcome.Completed;
    }
  }
  assert.ok(colored, `${profile} RGB frames were never actually colored`);
  assert.equal(eye.fileName, NAME);
  assert.deepEqual(eye.takeObject(), object, `${profile} RGB optical round trip differs`);
  console.log(
    `ok — ${profile} RGB: ${object.length} B through ${frames} colored frames, ` +
      `${spec.symbolBytes * RGB_CHANNELS} B carried per frame`,
  );
}

{
  // A camera never hands the eye clean channels: a screen's primaries and a
  // sensor's colour dyes overlap, and ambient light plus panel brightness
  // compress the range. Model that capture — 20% of each neighbouring
  // channel leaking in, then the whole scale squeezed into 96..190 — and
  // require the separated channels to still decode. The compression alone
  // defeats any fixed mid-scale threshold, so this is the contrast stretch
  // in channelToGrey earning its keep. Linear leak keeps brightness *order*
  // intact until 25%, where the channels collapse; a real sensor's curve is
  // the physical test's question.
  const LEAK = 0.2;
  const FLOOR = 96;
  const CEIL = 190;
  const skin = new ReferenceSkin(object, NAME, MIME, "qr27", 0xbead, 0.5);
  const eye = new ReferenceEye();
  let next = 0;
  const take = () => {
    const packet = skin.packet(next);
    next = (next + 1) % skin.packetCount;
    return packet;
  };
  let frames = 0;
  let done = false;
  while (!done) {
    assert.ok(frames <= skin.packetCount, "crosstalked RGB never completed");
    const raster = rasterizeRgbReferencePackets(Array.from({ length: RGB_CHANNELS }, take), "qr27");
    frames += 1;
    const mixed = new Uint8ClampedArray(raster.rgba.length);
    for (let at = 0; at < raster.rgba.length; at += 4) {
      for (let c = 0; c < RGB_CHANNELS; c += 1) {
        const own = raster.rgba[at + c];
        const others = raster.rgba[at + ((c + 1) % 3)] + raster.rgba[at + ((c + 2) % 3)];
        const leaked = own * (1 - 2 * LEAK) + others * LEAK;
        mixed[at + c] = FLOOR + (leaked * (CEIL - FLOOR)) / 255;
      }
      mixed[at + 3] = 255;
    }
    for (let channel = 0; channel < RGB_CHANNELS && !done; channel += 1) {
      const bytes = await decodeRaster(channelPlane(mixed, channel), raster.width, raster.height);
      assert.ok(bytes, `crosstalked frame ${frames} channel ${channel} did not decode`);
      done = eye.ingest(bytes) === Outcome.Completed;
    }
  }
  assert.deepEqual(eye.takeObject(), object, "crosstalked round trip differs");
  console.log(
    `ok — ${Math.round(LEAK * 100)}% crosstalk and ${FLOOR}..${CEIL} compression undone by the contrast stretch`,
  );
}

{
  // Past ~25% leak the channels change ORDER: a module black here but white
  // in the other two channels captures *brighter* than its opposite, and no
  // monotone per-channel transform — no stretch, no threshold — can undo an
  // order swap. The calibration strip can: its pure-colour patches measure
  // the mixing matrix directly and inverting it restores the channels. This
  // drives the eye's actual recovery path with the worker's own helpers.
  const LEAK = 0.3;
  const FLOOR = 96;
  const CEIL = 190;
  const spec = QR_REFERENCE_PROFILES.qr27;
  const skin = new ReferenceSkin(object, NAME, MIME, "qr27", 0xca11b, 0.5);
  const eye = new ReferenceEye();
  let next = 0;
  const take = () => {
    const packet = skin.packet(next);
    next = (next + 1) % skin.packetCount;
    return packet;
  };
  const capture = (raster) => {
    const mixed = new Uint8ClampedArray(raster.rgba.length);
    for (let at = 0; at < raster.rgba.length; at += 4) {
      for (let c = 0; c < RGB_CHANNELS; c += 1) {
        const own = raster.rgba[at + c];
        const others = raster.rgba[at + ((c + 1) % 3)] + raster.rgba[at + ((c + 2) % 3)];
        const leaked = own * (1 - 2 * LEAK) + others * LEAK;
        mixed[at + c] = FLOOR + (leaked * (CEIL - FLOOR)) / 255;
      }
      mixed[at + 3] = 255;
    }
    return mixed;
  };
  // The symbol's outer corners — what ZXing's quad reports on a real frame.
  const edge = QR_QUIET_MODULES;
  const quad = [
    { x: edge, y: edge },
    { x: edge + spec.modules, y: edge },
    { x: edge + spec.modules, y: edge + spec.modules },
    { x: edge, y: edge + spec.modules },
  ];

  const first = capture(rasterizeRgbReferencePackets(Array.from({ length: RGB_CHANNELS }, take), "qr27"));
  const width = spec.size;
  const height = spec.size + 4;
  // The stretch alone must fail here — that is the whole reason the strip
  // exists. If this ever starts passing, the leak model got weaker, not the
  // transport stronger; move the threshold, don't delete the assertion.
  const raw = [];
  for (let channel = 0; channel < RGB_CHANNELS; channel += 1) {
    raw.push(await decodeRaster(channelPlane(first, channel), width, height));
  }
  assert.ok(raw.some((bytes) => !bytes), "30% leak should defeat the per-channel stretch");

  next = 0; // replay the stream through the calibrated path
  let frames = 0;
  let done = false;
  const corrected = new Uint8ClampedArray(width * height * 4);
  while (!done) {
    assert.ok(frames <= skin.packetCount, "calibrated RGB never completed");
    const mixed = capture(
      rasterizeRgbReferencePackets(Array.from({ length: RGB_CHANNELS }, take), "qr27"),
    );
    frames += 1;
    const unmix = calibrateFrame(mixed, width, height, quad, [spec.modules]);
    assert.ok(unmix, `frame ${frames}: the calibration strip did not solve`);
    applyUnmix(mixed, unmix, corrected);
    for (let channel = 0; channel < RGB_CHANNELS && !done; channel += 1) {
      const bytes = await decodeRaster(channelPlane(corrected, channel), width, height);
      assert.ok(bytes, `calibrated frame ${frames} channel ${channel} did not decode`);
      done = eye.ingest(bytes) === Outcome.Completed;
    }
  }
  assert.deepEqual(eye.takeObject(), object, "calibrated round trip differs");
  console.log(
    `ok — ${Math.round(LEAK * 100)}% crosstalk defeats the stretch; the calibration strip undoes it`,
  );
}

{
  // The tiled rung: a 2×2 grid of standard symbols, each locating and
  // decoding independently — and composing with RGB for twelve packets per
  // frame. Raising maxNumberOfSymbols is the only reader change tiling asks
  // of ZXing.
  const TILED_READ = { ...ZXING_READ, maxNumberOfSymbols: TILE_COUNT };
  const readAll = async (rgba, width, height) => {
    const results = await readBarcodes({ data: rgba, width, height }, TILED_READ);
    return results.filter((r) => r.isValid && r.bytes.length > 0).map((r) => r.bytes);
  };

  let skin = new ReferenceSkin(object, NAME, MIME, "qr27", 0x711e, 0.5);
  let eye = new ReferenceEye();
  let next = 0;
  const take = () => {
    const packet = skin.packet(next);
    next = (next + 1) % skin.packetCount;
    return packet;
  };

  let frames = 0;
  let done = false;
  while (!done) {
    assert.ok(frames <= skin.packetCount, "tiled b/w never completed");
    const raster = rasterizeTiledReferencePackets(
      Array.from({ length: TILE_COUNT }, take),
      "qr27",
      1,
    );
    frames += 1;
    const decoded = await readAll(raster.rgba, raster.width, raster.height);
    assert.equal(decoded.length, TILE_COUNT, `tiled frame ${frames} read ${decoded.length}/4 symbols`);
    for (const bytes of decoded) {
      if (eye.ingest(bytes) === Outcome.Completed) {
        done = true;
        break;
      }
    }
  }
  assert.deepEqual(eye.takeObject(), object, "tiled b/w round trip differs");
  console.log(`ok — qr27 tiled: ${object.length} B through ${frames} four-symbol frames`);

  skin = new ReferenceSkin(object, NAME, MIME, "qr27", 0x711f, 0.5);
  eye = new ReferenceEye();
  next = 0;
  frames = 0;
  done = false;
  while (!done) {
    assert.ok(frames <= skin.packetCount, "tiled RGB never completed");
    const raster = rasterizeTiledReferencePackets(
      Array.from({ length: TILE_COUNT * RGB_CHANNELS }, take),
      "qr27",
      RGB_CHANNELS,
    );
    frames += 1;
    for (let channel = 0; channel < RGB_CHANNELS && !done; channel += 1) {
      const decoded = await readAll(channelPlane(raster.rgba, channel), raster.width, raster.height);
      assert.equal(decoded.length, TILE_COUNT, `tiled RGB channel ${channel} read ${decoded.length}/4`);
      for (const bytes of decoded) {
        if (eye.ingest(bytes) === Outcome.Completed) {
          done = true;
          break;
        }
      }
    }
  }
  assert.deepEqual(eye.takeObject(), object, "tiled RGB round trip differs");
  console.log(`ok — qr27 RGB tiled: ${object.length} B through ${frames} twelve-packet frames`);
}

{
  // Not an assertion — a scoreboard. Decode the densest rung with the tuned
  // options and with the library's stock set, so any future knob change has
  // a number to answer to. Node timings are indicative rather than gospel,
  // but it is the same WASM the browser runs.
  const skin = new ReferenceSkin(object, NAME, MIME, "qr40", 0x7e57, 0.5);
  const rasters = [];
  for (let i = 0; i < skin.packetCount; i += 1) {
    rasters.push(rasterizeReferencePacket(skin.packet(i), "qr40"));
  }
  const time = async (options) => {
    const begin = performance.now();
    for (const raster of rasters) {
      const results = await readBarcodes(
        { data: raster.rgba, width: raster.width, height: raster.height },
        options,
      );
      assert.ok(
        results.some((result) => result.isValid),
        "benchmark frame failed to decode",
      );
    }
    return performance.now() - begin;
  };
  await time(ZXING_READ); // first pass pays one-off warm-up costs; discard it
  const tuned = await time(ZXING_READ);
  const stock = await time({ formats: ["QRCode"], maxNumberOfSymbols: 1 });
  console.log(
    `ok — qr40 ×${rasters.length}: ${tuned.toFixed(0)} ms tuned vs ${stock.toFixed(0)} ms stock options`,
  );

  // The knobs earn nothing on a frame that decodes first pass — the fallback
  // passes they disable never ran. Where they pay is the frame that *fails*,
  // which is the eye's steady state while the human is still aiming: stock
  // options exhaust invert and rotate variants before giving up.
  const noise = new Uint8ClampedArray(1280 * 720 * 4);
  let rng = 0x5eed;
  for (let at = 0; at < noise.length; at += 4) {
    rng ^= rng << 13;
    rng ^= rng >>> 17;
    rng ^= rng << 5;
    noise[at] = noise[at + 1] = noise[at + 2] = rng & 0xff;
    noise[at + 3] = 255;
  }
  const miss = async (options) => {
    const begin = performance.now();
    for (let rep = 0; rep < 5; rep += 1) {
      const results = await readBarcodes({ data: noise, width: 1280, height: 720 }, options);
      assert.equal(results.filter((result) => result.isValid).length, 0);
    }
    return performance.now() - begin;
  };
  await miss(ZXING_READ); // warm-up, as above
  const tunedMiss = await miss(ZXING_READ);
  const stockMiss = await miss({ formats: ["QRCode"], maxNumberOfSymbols: 1 });
  console.log(
    `ok — 1280×720 miss ×5: ${tunedMiss.toFixed(0)} ms tuned vs ${stockMiss.toFixed(0)} ms stock options`,
  );
}

{
  // The eye's live overlay draws the corner quad ZXing reports alongside the
  // bytes. Make sure the reader really reports one: four corners inside the
  // raster, spanning the symbol rather than a degenerate point.
  const skin = new ReferenceSkin(object, NAME, MIME, "qr27", 0xd0e, 0.5);
  const raster = rasterizeReferencePacket(skin.packet(0), "qr27");
  const [found] = await readBarcodes(
    { data: raster.rgba, width: raster.width, height: raster.height },
    ZXING_READ,
  );
  const corners = [
    found.position.topLeft,
    found.position.topRight,
    found.position.bottomRight,
    found.position.bottomLeft,
  ];
  for (const corner of corners) {
    assert.ok(
      corner.x >= 0 && corner.x <= raster.width && corner.y >= 0 && corner.y <= raster.height,
      "corner quad landed outside the raster",
    );
  }
  const xs = corners.map((corner) => corner.x);
  const ys = corners.map((corner) => corner.y);
  assert.ok(
    Math.max(...xs) - Math.min(...xs) > raster.width / 2 &&
      Math.max(...ys) - Math.min(...ys) > raster.height / 2,
    "corner quad does not span the symbol",
  );
  console.log("ok — ZXing reports the corner quad the live overlay draws");
}

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
  QR_REFERENCE_PROFILES,
  RGB_CHANNELS,
  rasterizeReferencePacket,
  rasterizeRgbReferencePackets,
} from "../src/qr-reference.ts";
import { channelToGrey } from "../src/rgb-channel.ts";

const pkg = new URL("../pkg/cuttl_wasm.js", import.meta.url);
const wasm = new URL("../pkg/cuttl_wasm_bg.wasm", import.meta.url);
const { default: init, ReferenceEye, ReferenceSkin, Outcome } = await import(pkg.href);
await init({ module_or_path: await readFile(fileURLToPath(wasm)) });

// The browser resolves the reader WASM through Vite's asset pipeline; Node
// reads the same file straight out of the installed package.
const zxingWasm = fileURLToPath(import.meta.resolve("zxing-wasm/reader/zxing_reader.wasm"));
await prepareZXingModule({ overrides: { wasmBinary: (await readFile(zxingWasm)).buffer } });

// Exactly what the eye worker asks for, so the test exercises the same path.
const ZXING_READ = { formats: ["QRCode"], maxNumberOfSymbols: 1 };

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
    // The frame must actually be colored: some pixel dark in one channel and
    // light in another, or the channels have collapsed into one symbol.
    for (let at = 0; at < raster.rgba.length && !colored; at += 4) {
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

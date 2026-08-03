// End-to-end check of the JavaScript boundary, with no browser involved.
//
// The Rust side already has a Skin→Eye round trip, so this is not testing the
// codec. It tests the part that only exists in JS: that wasm-bindgen's glue
// hands back the types we think it does, that `pulseRgba` really is RGBA at
// grid resolution, that `ingest` accepts exactly what `ImageData.data` would
// give it, and that a decoded file comes back as bytes.
//
// Those are the assumptions the browser code is built on, and every one of them
// would otherwise stay unverified until someone pointed a camera at a screen.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = new URL("../pkg/cuttl_wasm.js", import.meta.url);
const wasm = new URL("../pkg/cuttl_wasm_bg.wasm", import.meta.url);

const { default: init, Skin, Eye, Outcome } = await import(pkg.href);
// The `web` target normally fetches its own binary; in Node we hand it over.
await init({ module_or_path: await readFile(fileURLToPath(wasm)) });

const PROFILE = "m1";
const NAME = "boundary.bin";
const MIME = "application/test";
const object = Uint8Array.from({ length: 12_000 }, (_, i) => (i * 37) & 0xff);

const skin = new Skin(object, NAME, MIME, PROFILE, 0x5eed, 0.5);
assert.ok(skin.pulseCount > 0, "skin produced no pulses");
assert.equal(skin.cols, 64, "unexpected grid width");
assert.equal(skin.rows, 36, "unexpected grid height");

const first = skin.pulseRgba(0);
assert.ok(first instanceof Uint8Array, "pulseRgba should hand back a Uint8Array");
assert.equal(first.length, skin.cols * skin.rows * 4, "pulseRgba is not RGBA at grid size");
assert.ok(
  first.filter((_, i) => i % 4 === 3).every((a) => a === 255),
  "every pixel should be fully opaque",
);
// Wraps, because the skin loops forever.
assert.deepEqual(skin.pulseRgba(skin.pulseCount), first, "pulse indexing should wrap");

// "auto", exactly as the browser eye constructs it: no profile is agreed
// out of band, so the eye has to work the grid out from the frames alone.
const eye = new Eye("auto");
assert.equal(eye.profile, undefined, "eye should not claim a profile before seeing one");
assert.equal(eye.symbols, 0);
assert.equal(eye.isComplete, false);
assert.equal(eye.fileName, undefined, "no manifest should mean no name");

let frames = 0;
for (let i = 0; i < skin.pulseCount; i += 1) {
  frames += 1;
  const outcome = eye.ingest(skin.pulseRgba(i), skin.cols, skin.rows);
  if (i === 0) {
    // Pulse 0 carries the manifest: the eye names the file before it has it.
    assert.equal(eye.fileName, NAME, "manifest name should arrive with the first pulse");
    assert.equal(eye.fileMime, MIME, "manifest mime should arrive with the first pulse");
    assert.equal(eye.expectedBytes, object.length, "size should be known from the OTI");
    assert.equal(eye.isComplete, false, "one pulse must not complete a transfer");
    assert.equal(eye.profile, PROFILE, "eye should have locked onto the skin's profile");
    assert.ok(eye.symbolBytes > 0, "goodput needs a symbol size to multiply by");
  }
  if (outcome === Outcome.Completed) break;
  assert.notEqual(outcome, Outcome.Unlocatable, `frame ${i} could not be located`);
}

assert.ok(eye.isComplete, "eye never completed");
const received = eye.takeObject();
assert.ok(received instanceof Uint8Array, "takeObject should hand back a Uint8Array");
assert.deepEqual(received, object, "received file differs from the sent one");

// A frame that is not a pulse must be reported, not thrown.
const noise = new Uint8Array(skin.cols * skin.rows * 4).fill(0);
assert.doesNotThrow(() => new Eye(PROFILE).ingest(noise, skin.cols, skin.rows));
assert.equal(
  new Eye(PROFILE).ingest(noise, skin.cols, skin.rows),
  Outcome.Unlocatable,
  "a blank frame should read as unlocatable",
);

// A bad profile must reject rather than produce a broken object.
assert.throws(() => new Eye("nonsense"), "unknown profiles should throw");

// Auto-detection must reach the dense profiles too, not just the default —
// that is the whole reason the skin can offer a density menu on one device.
for (const profile of ["m2", "m3", "m4"]) {
  const dense = new Skin(object, NAME, MIME, profile, 0x5eed, 0.5);
  const watcher = new Eye("auto");
  for (let i = 0; i < dense.pulseCount; i += 1) {
    if (watcher.ingest(dense.pulseRgba(i), dense.cols, dense.rows) === Outcome.Completed) break;
  }
  assert.equal(watcher.profile, profile, `auto-detect settled on the wrong grid for ${profile}`);
  assert.deepEqual(watcher.takeObject(), object, `${profile} round trip differs`);
}

console.log(
  `ok — ${object.length} B through the JS boundary in ${frames} of ${skin.pulseCount} pulses`,
);

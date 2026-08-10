// End-to-end check of the JavaScript boundary, with no browser involved.
//
// The Rust side already has a packet round trip, so this is not testing the
// codec. It tests the part that only exists in JS: that wasm-bindgen's glue
// hands back the types we think it does, that `packet` really is bytes, that
// `ingest` accepts exactly what a QR decoder returns, and that a decoded file
// comes back as bytes. The picture itself — rasterize, ZXing, channel
// separation — is qr-optical.mjs's job.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import QRCode from "qrcode";
import { QR_REFERENCE_PROFILES } from "../src/qr-reference.ts";

const pkg = new URL("../pkg/cuttl_wasm.js", import.meta.url);
const wasm = new URL("../pkg/cuttl_wasm_bg.wasm", import.meta.url);

const { default: init, ReferenceEye, ReferenceSkin, Outcome } = await import(pkg.href);
// The `web` target normally fetches its own binary; in Node we hand it over.
await init({ module_or_path: await readFile(fileURLToPath(wasm)) });

const NAME = "boundary.bin";
// A mime the encoder treats as precompressed, so packets stay full-size and
// the version-boundary check below tests the real limit, not a shrunk symbol.
const MIME = "application/zip";
const object = Uint8Array.from({ length: 12_000 }, (_, i) => (i * 37) & 0xff);

for (const [profile, spec] of Object.entries(QR_REFERENCE_PROFILES)) {
  const skin = new ReferenceSkin(object, NAME, MIME, profile, 0xface, 0.5);
  const eye = new ReferenceEye();
  assert.ok(skin.packetCount > 1, `${profile} produced no QR packets`);
  assert.equal(skin.profile, profile);
  assert.equal(skin.qrVersion, spec.version);

  const first = skin.packet(1);
  assert.ok(first instanceof Uint8Array, "packet should hand back a Uint8Array");
  // Wraps, because the skin loops forever.
  assert.deepEqual(skin.packet(skin.packetCount + 1), first, "packet indexing should wrap");

  for (let i = 0; i < skin.packetCount; i += 1) {
    const outcome = eye.ingest(skin.packet(i));
    if (i === 0) {
      // Packet 0 carries the manifest: the eye knows what it is receiving
      // before it has received anything.
      assert.equal(eye.fileName, NAME);
      assert.equal(eye.expectedBytes, object.length);
      assert.ok(!eye.isComplete);
    }
    if (outcome === Outcome.Completed) break;
  }
  assert.equal(eye.fileMime, MIME);
  assert.deepEqual(eye.takeObject(), object, `${profile} packet round trip differs`);

  // Packets must remain inside the chosen standard QR version rather than
  // relying on the writer to silently enlarge the visual carrier.
  const qr = QRCode.create([{ data: first, mode: "byte" }], {
    version: spec.version,
    errorCorrectionLevel: spec.eccLevel,
    maskPattern: 4,
  });
  assert.equal(qr.modules.size, spec.modules, `${profile} escaped its fixed QR version`);
}

// Bytes that are not a packet must be reported, not thrown — and a frame with
// no symbol at all is a miss the page can count.
const noiseEye = new ReferenceEye();
assert.equal(noiseEye.ingest(new Uint8Array(64)), Outcome.Rejected);
noiseEye.miss();
assert.equal(noiseEye.unlocatable, 1);
assert.equal(noiseEye.takeObject(), undefined);

assert.throws(() => new ReferenceSkin(object, NAME, MIME, "m1", 1, 0.5), "unknown profiles should throw");

console.log(`ok — ${object.length} B through the JS boundary at every QR rung`);

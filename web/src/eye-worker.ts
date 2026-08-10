// The decode half of the eye, off the main thread.
//
// Everything heavy happens here — ZXing detection, the CRC gate, the
// fountain, the final BLAKE3 — so a slow frame can never stutter the video or
// the feedback overlay. The page keeps the camera and the human.

import init, { Outcome, ReferenceEye } from "../pkg/cuttl_wasm.js";
import type { FromWorker, QuadPoint, ToWorker, Transport } from "./protocol.js";
import { prepareZXingModule, readBarcodes } from "zxing-wasm/reader";
import zxingReaderWasm from "zxing-wasm/reader/zxing_reader.wasm?url";
import { RGB_CHANNELS } from "./qr-reference.js";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromWorker, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToWorker>) => void) | null;
};

let eye: ReferenceEye | null = null;
let transport: Transport = "qr";

function status(
  outcome: Outcome,
  decoder: ReferenceEye,
  quad: QuadPoint[] | null,
  frameWidth: number,
  frameHeight: number,
): FromWorker {
  return {
    kind: "status",
    outcome,
    quad,
    frameWidth,
    frameHeight,
    symbols: decoder.symbols,
    needed: decoder.needed,
    rejected: decoder.rejected,
    unlocatable: decoder.unlocatable,
    fileName: decoder.fileName,
    fileMime: decoder.fileMime,
    expectedBytes: decoder.expectedBytes,
    symbolBytes: decoder.symbolBytes,
  };
}

const ZXING_READ: Parameters<typeof readBarcodes>[1] = {
  formats: ["QRCode"],
  maxNumberOfSymbols: 1,
};

/**
 * Scratch for channel separation, reused across channels and frames.
 *
 * Three fresh RGBA buffers per capture would churn ~25 MB of garbage per
 * frame at 1920 wide. `readBarcodes` copies the pixels into the ZXing heap
 * before it returns, so one buffer can safely serve every channel in turn.
 */
let channelScratch: Uint8ClampedArray<ArrayBuffer> | null = null;

/** One channel of an RGBA frame, replicated to grey so ZXing's luminance
 * conversion reads exactly that channel. */
function channelImage(
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
  channel: number,
): ImageData {
  if (!channelScratch || channelScratch.length !== rgba.length) {
    channelScratch = new Uint8ClampedArray(rgba.length);
    for (let alpha = 3; alpha < rgba.length; alpha += 4) channelScratch[alpha] = 255;
  }
  const out = channelScratch;
  for (let at = 0; at < rgba.length; at += 4) {
    const value = rgba[at + channel];
    out[at] = value;
    out[at + 1] = value;
    out[at + 2] = value;
  }
  return new ImageData(out, width, height);
}

/** Later outcomes in a frame only ever upgrade the report, never bury it. */
function rank(outcome: Outcome): number {
  switch (outcome) {
    case Outcome.Completed:
      return 4;
    case Outcome.Accepted:
      return 3;
    case Outcome.Duplicate:
      return 2;
    case Outcome.Rejected:
      return 1;
    default:
      return 0;
  }
}

/**
 * Read every QR symbol one captured frame carries and feed each packet to the
 * ReferenceEye on its own.
 *
 * The black-and-white transport reads the frame once; the RGB transport
 * separates the three colour channels and reads each as an independent
 * standard symbol. A channel lost to crosstalk simply decodes nothing — the
 * frame's outcome is the best any channel achieved, and only a frame with no
 * symbol at all counts as a miss.
 *
 * The quad is where ZXing saw a symbol, kept even when its payload could not
 * be used — locating and reading fail separately, and the page draws that
 * difference. In RGB mode the channels share one geometry, so the first
 * channel to locate speaks for the frame.
 */
async function ingestQrFrame(
  qrEye: ReferenceEye,
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
): Promise<{ outcome: Outcome; quad: QuadPoint[] | null }> {
  const images =
    transport === "qr-rgb"
      ? Array.from({ length: RGB_CHANNELS }, (_, c) => () => channelImage(rgba, width, height, c))
      : [() => new ImageData(rgba, width, height)];

  let best: Outcome | null = null;
  let quad: QuadPoint[] | null = null;
  for (const image of images) {
    const results = await readBarcodes(image(), ZXING_READ);
    const located = results[0];
    if (located && !quad) {
      const { topLeft, topRight, bottomRight, bottomLeft } = located.position;
      quad = [topLeft, topRight, bottomRight, bottomLeft].map((p) => ({ x: p.x, y: p.y }));
    }
    const decoded = results.find((result) => result.isValid && result.bytes.length > 0);
    if (!decoded) continue;
    const outcome = qrEye.ingest(decoded.bytes);
    if (best === null || rank(outcome) > rank(best)) best = outcome;
    if (best === Outcome.Completed) break;
  }
  if (best === null) {
    qrEye.miss();
    return { outcome: Outcome.Unlocatable, quad };
  }
  return { outcome: best, quad };
}

async function handle(message: ToWorker): Promise<void> {
  if (message.kind === "init") {
    await init();
    transport = message.transport;
    await prepareZXingModule({
      overrides: {
        // Never accept the package default CDN URL. The transfer must remain
        // as air-gapped as its premise once the page is loaded, and Vite
        // emits this URL as a local build asset.
        locateFile: (path, prefix) =>
          path.endsWith(".wasm") ? zxingReaderWasm : prefix + path,
      },
    });
    // Instantiation is expensive enough to make the first camera frame look
    // broken. Warm it with a disposable image before the camera starts.
    await readBarcodes(new ImageData(8, 8), { formats: ["QRCode"] }).catch(() => []);
    eye = new ReferenceEye();
    scope.postMessage({ kind: "ready" });
    return;
  }
  // A frame racing ahead of init is dropped, like any other missed frame.
  if (!eye) return;

  const rgba = new Uint8ClampedArray(message.buffer);
  const { outcome, quad } = await ingestQrFrame(eye, rgba, message.width, message.height);
  scope.postMessage(status(outcome, eye, quad, message.width, message.height));

  if (outcome === Outcome.Completed) {
    const bytes = eye.takeObject();
    if (bytes) {
      scope.postMessage(
        {
          kind: "complete",
          bytes,
          fileName: eye.fileName ?? "received.bin",
          fileMime: eye.fileMime ?? "",
        },
        [bytes.buffer],
      );
    }
  }
}

scope.onmessage = (event) => {
  handle(event.data).catch((error: unknown) => {
    scope.postMessage({ kind: "error", message: `Decode failed: ${error}` });
  });
};

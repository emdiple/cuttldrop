// The decode half of the eye, off the main thread (DESIGN.md §3e).
//
// Everything WASM happens here — locate, sample, inner RS, the CRC gate, the
// fountain, the final BLAKE3 — so a slow frame can never stutter the video or
// the feedback overlay. The page keeps the camera and the human.

import init, { Eye, Outcome, ReferenceEye } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker, Transport } from "./protocol.js";
import { prepareZXingModule, readBarcodes } from "zxing-wasm/reader";
import zxingReaderWasm from "zxing-wasm/reader/zxing_reader.wasm?url";
import { RGB_CHANNELS } from "./qr-reference.js";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromWorker, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToWorker>) => void) | null;
};

type Decoder = Eye | ReferenceEye;

let eye: Decoder | null = null;
let transport: Transport = "custom";

function status(outcome: Outcome, decoder: Decoder): FromWorker {
  return {
    kind: "status",
    outcome,
    symbols: decoder.symbols,
    needed: decoder.needed,
    torn: decoder.torn,
    rejected: decoder.rejected,
    unlocatable: decoder.unlocatable,
    fileName: decoder.fileName,
    fileMime: decoder.fileMime,
    expectedBytes: decoder.expectedBytes,
    symbolBytes: decoder.symbolBytes,
    profile: decoder.profile,
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
 */
async function ingestQrFrame(
  qrEye: ReferenceEye,
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
): Promise<Outcome> {
  const images =
    transport === "qr-rgb"
      ? Array.from({ length: RGB_CHANNELS }, (_, c) => () => channelImage(rgba, width, height, c))
      : [() => new ImageData(rgba, width, height)];

  let best: Outcome | null = null;
  for (const image of images) {
    const results = await readBarcodes(image(), ZXING_READ);
    const decoded = results.find((result) => result.isValid && result.bytes.length > 0);
    if (!decoded) continue;
    const outcome = qrEye.ingest(decoded.bytes);
    if (best === null || rank(outcome) > rank(best)) best = outcome;
    if (best === Outcome.Completed) break;
  }
  if (best === null) {
    qrEye.miss();
    return Outcome.Unlocatable;
  }
  return best;
}

async function handle(message: ToWorker): Promise<void> {
  if (message.kind === "init") {
    await init();
    transport = message.transport;
    if (transport !== "custom") {
      await prepareZXingModule({
        overrides: {
          // Never accept the package default CDN URL. This reference mode must
          // remain as air-gapped as Cuttldrop's custom raster once the page is
          // loaded, and Vite emits this URL as a local build asset.
          locateFile: (path, prefix) =>
            path.endsWith(".wasm") ? zxingReaderWasm : prefix + path,
        },
      });
      // Instantiation is expensive enough to make the first camera frame look
      // broken. Warm it with a disposable image before the camera starts.
      await readBarcodes(new ImageData(8, 8), { formats: ["QRCode"] }).catch(() => []);
      eye = new ReferenceEye();
    } else {
      eye = new Eye(message.profile);
    }
    scope.postMessage({ kind: "ready" });
    return;
  }
  // A frame racing ahead of init is dropped, like any other missed frame.
  if (!eye) return;

  let outcome: Outcome;
  if (transport !== "custom") {
    const rgba = new Uint8ClampedArray(message.buffer);
    outcome = await ingestQrFrame(eye as ReferenceEye, rgba, message.width, message.height);
  } else {
    outcome = (eye as Eye).ingest(new Uint8Array(message.buffer), message.width, message.height);
  }
  scope.postMessage(status(outcome, eye));

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

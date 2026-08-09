// The decode half of the eye, off the main thread (DESIGN.md §3e).
//
// Everything WASM happens here — locate, sample, inner RS, the CRC gate, the
// fountain, the final BLAKE3 — so a slow frame can never stutter the video or
// the feedback overlay. The page keeps the camera and the human.

import init, { Eye, Outcome, ReferenceEye } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker } from "./protocol.js";
import { prepareZXingModule, readBarcodes } from "zxing-wasm/reader";
import zxingReaderWasm from "zxing-wasm/reader/zxing_reader.wasm?url";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromWorker, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToWorker>) => void) | null;
};

type Decoder = Eye | ReferenceEye;

let eye: Decoder | null = null;
let transport: "custom" | "qr" = "custom";

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

async function handle(message: ToWorker): Promise<void> {
  if (message.kind === "init") {
    await init();
    transport = message.transport;
    if (transport === "qr") {
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
  if (transport === "qr") {
    const image = new ImageData(new Uint8ClampedArray(message.buffer), message.width, message.height);
    const results = await readBarcodes(image, { formats: ["QRCode"], maxNumberOfSymbols: 1 });
    const decoded = results.find((result) => result.isValid && result.bytes.length > 0);
    const qrEye = eye as ReferenceEye;
    if (decoded) {
      outcome = qrEye.ingest(decoded.bytes);
    } else {
      qrEye.miss();
      outcome = Outcome.Unlocatable;
    }
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

// The decode half of the eye, off the main thread (DESIGN.md §3e).
//
// Everything WASM happens here — locate, sample, inner RS, the CRC gate, the
// fountain, the final BLAKE3 — so a slow frame can never stutter the video or
// the feedback overlay. The page keeps the camera and the human.

import init, { Eye, Outcome } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker } from "./protocol.js";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromWorker, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToWorker>) => void) | null;
};

let eye: Eye | null = null;

function status(outcome: Outcome, decoder: Eye): FromWorker {
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
    eye = new Eye(message.profile);
    scope.postMessage({ kind: "ready" });
    return;
  }
  // A frame racing ahead of init is dropped, like any other missed frame.
  if (!eye) return;

  const outcome = eye.ingest(
    new Uint8Array(message.buffer),
    message.width,
    message.height,
  );
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

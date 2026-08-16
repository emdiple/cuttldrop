// The packet sink half of the eye, off the main thread.
//
// Exactly one of these runs, no matter how many ZXing workers decode frames
// ahead of it: the CRC gate, the fountain and the final BLAKE3 need a single
// authoritative copy of the stream state. Ingesting a payload is cheap next
// to detection, but the fountain solve on the completing packet and the hash
// over the whole file are not — which is why the sink is still a worker and
// not the page.

import init, { Outcome, ReferenceEye } from "../pkg/cuttl_wasm.js";
import type { FromSink, QuadPoint, ToSink } from "./protocol.ts";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromSink, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToSink>) => void) | null;
};

let eye: ReferenceEye | null = null;
/** Set once the object is taken; late in-flight frames must not touch a spent eye. */
let finished = false;

function status(
  outcome: Outcome,
  decoder: ReferenceEye,
  quads: QuadPoint[][],
  frameWidth: number,
  frameHeight: number,
): FromSink {
  return {
    kind: "status",
    outcome,
    quads,
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

/** Later payloads in a frame only ever upgrade the report, never bury it. */
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

async function handle(message: ToSink): Promise<void> {
  if (message.kind === "init") {
    await init();
    eye = new ReferenceEye();
    finished = false;
    scope.postMessage({ kind: "ready" });
    return;
  }
  // A frame racing ahead of init — or trailing a completed transfer — is
  // dropped, like any other missed frame.
  if (!eye || finished) return;

  // Every payload a frame carried crosses the CRC gate on its own — in RGB
  // mode a channel ruined by crosstalk costs one symbol, never the frame.
  let best: Outcome | null = null;
  for (const payload of message.payloads) {
    const outcome = eye.ingest(payload);
    if (best === null || rank(outcome) > rank(best)) best = outcome;
    if (best === Outcome.Completed) break;
  }
  // Only a frame with no readable symbol at all counts as a miss.
  if (best === null) eye.miss();
  const outcome = best ?? Outcome.Unlocatable;
  scope.postMessage(status(outcome, eye, message.quads, message.frameWidth, message.frameHeight));

  if (outcome === Outcome.Completed) {
    finished = true;
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
    scope.postMessage({ kind: "error", message: `Ingest failed: ${error}` });
  });
};

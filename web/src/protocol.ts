// Messages between the eye page and its decode worker.
//
// The page owns the camera; the worker owns ZXing and the WASM ReferenceEye.
// Frames cross as *transferred* ArrayBuffers — no copy — and any frame
// captured while the worker is still chewing is dropped on the page side: a
// decoder that falls behind a live camera must shed load, not queue it.

import type { Outcome } from "../pkg/cuttl_wasm.js";

/**
 * Which optical carrier the eye should read.
 *
 * `qr` is one black-and-white standard QR per frame; `qr-rgb` is three
 * standard QR symbols multiplexed into the R, G and B channels of one frame.
 * Both feed the same `ReferenceEye` packet sink.
 */
export type Transport = "qr" | "qr-rgb";

/** One corner of a located symbol, in captured-frame pixels. */
export interface QuadPoint {
  x: number;
  y: number;
}

/** Page → worker. Frames only start once `ready` has come back. */
export type ToWorker =
  | { kind: "init"; transport: Transport }
  | { kind: "frame"; buffer: ArrayBuffer; width: number; height: number };

/** Worker → page: `ready` once, one `status` per frame, `complete` at most once. */
export type FromWorker =
  | { kind: "ready" }
  | { kind: "error"; message: string }
  | {
      kind: "status";
      outcome: Outcome;
      /**
       * Corners of the symbol ZXing located this frame — TL, TR, BR, BL — or
       * null when nothing was found. Present even when the payload was then
       * rejected: "seen but unreadable" is exactly what the page's overlay
       * needs to distinguish from "not seen".
       */
      quad: QuadPoint[] | null;
      /** Dimensions of the captured frame the quad is measured in. */
      frameWidth: number;
      frameHeight: number;
      symbols: number;
      needed: number;
      rejected: number;
      unlocatable: number;
      /** From the manifest, once one has arrived — long before the file. */
      fileName?: string;
      fileMime?: string;
      expectedBytes?: number;
      /** Object bytes one symbol is worth — the goodput readout's multiplier. */
      symbolBytes?: number;
    }
  | { kind: "complete"; bytes: Uint8Array; fileName: string; fileMime: string };

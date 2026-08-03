// Messages between the eye page and its decode worker.
//
// The page owns the camera; the worker owns the WASM Eye. Frames cross as
// *transferred* ArrayBuffers — no copy — and any frame captured while the
// worker is still chewing is dropped on the page side: a decoder that falls
// behind a live camera must shed load, not queue it.

import type { Outcome } from "../pkg/cuttl_wasm.js";

/** Page → worker. Frames only start once `ready` has come back. */
export type ToWorker =
  | { kind: "init"; profile: string }
  | { kind: "frame"; buffer: ArrayBuffer; width: number; height: number };

/** Worker → page: `ready` once, one `status` per frame, `complete` at most once. */
export type FromWorker =
  | { kind: "ready" }
  | { kind: "error"; message: string }
  | {
      kind: "status";
      outcome: Outcome;
      symbols: number;
      needed: number;
      torn: number;
      rejected: number;
      unlocatable: number;
      /** From the manifest, once one has arrived — long before the file. */
      fileName?: string;
      fileMime?: string;
      expectedBytes?: number;
    }
  | { kind: "complete"; bytes: Uint8Array; fileName: string; fileMime: string };

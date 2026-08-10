// Messages between the eye page and its workers.
//
// Decoding is split so it can scale. A small pool of ZXing workers turns
// captured frames into decoded payloads — that is the expensive part, and
// frames are independent, so they pipeline across cores. A single sink worker
// owns the WASM ReferenceEye: the CRC gate, the fountain and BLAKE3 need one
// authoritative copy of the stream state. The page routes between the two,
// owns the camera, and keeps the human informed.
//
// Frames cross to the decoders as *transferred* ArrayBuffers — no copy — and
// a frame captured while every decoder is chewing is dropped on the page
// side: a pipeline that falls behind a live camera must shed load, not queue
// it. The fountain makes the drop free; the skin repeats everything.

import type { Outcome } from "../pkg/cuttl_wasm.js";

/**
 * Which optical carrier the eye should read.
 *
 * `qr` is one black-and-white standard QR per frame; `qr-rgb` is three
 * standard QR symbols multiplexed into the R, G and B channels of one frame;
 * the `-tile` variants read a 2×2 grid of symbols per frame (times the
 * colour channels for `qr-rgb-tile`). All feed the same `ReferenceEye`
 * packet sink.
 */
export type Transport = "qr" | "qr-rgb" | "qr-tile" | "qr-rgb-tile";

/** One corner of a located symbol, in captured-frame pixels. */
export interface QuadPoint {
  x: number;
  y: number;
}

/** Page → decoder. Frames only start once its `ready` has come back. */
export type ToDecoder =
  | { kind: "init"; transport: Transport }
  | {
      kind: "frame";
      /**
       * Exactly one of these carries the pixels: a transferred RGBA buffer
       * from the page's canvas readback, or a transferred ImageBitmap the
       * decoder reads back itself on an OffscreenCanvas. The bitmap path is
       * the cheaper one — crop and scale stay on the GPU and the readback
       * happens off the main thread — and is used whenever the platform
       * grants it.
       */
      buffer?: ArrayBuffer;
      bitmap?: ImageBitmap;
      width: number;
      height: number;
      /**
       * Mapping from this buffer's pixels back to camera-source pixels:
       * `source = origin + px × scale`. The search path downscales the whole
       * source frame (origin 0, scale > 1); the region-of-interest path crops
       * around the last located symbol at native resolution (scale 1). The
       * decoder answers in source coordinates either way, so the page's
       * overlay and its next crop never care which path produced a frame.
       */
      originX: number;
      originY: number;
      scale: number;
      /** Dimensions of the camera source the mapping lands in. */
      sourceWidth: number;
      sourceHeight: number;
    };

/** Decoder → page: one `decoded` per frame, whatever it found. */
export type FromDecoder =
  | { kind: "ready" }
  | { kind: "error"; message: string }
  | {
      kind: "decoded";
      /**
       * Every payload ZXing could read out of the frame — at most one for
       * the black-and-white transport, up to three in RGB mode. Not yet
       * packets: nothing here has crossed the CRC gate.
       */
      payloads: Uint8Array[];
      /**
       * Corners of the symbol ZXing located this frame — TL, TR, BR, BL — or
       * null when nothing was found. Present even when no payload could be
       * read: "seen but unreadable" is exactly what the page's overlay needs
       * to distinguish from "not seen". Always in camera-source pixels,
       * whatever crop or downscale the frame arrived as.
       */
      quad: QuadPoint[] | null;
      /** Dimensions of the camera source the quad is measured in. */
      frameWidth: number;
      frameHeight: number;
    };

/** Page → sink. `init` builds a fresh stream state; `ingest` is one frame's harvest. */
export type ToSink =
  | { kind: "init" }
  | {
      kind: "ingest";
      payloads: Uint8Array[];
      /** Passed through untouched so `status` can echo where the symbol was. */
      quad: QuadPoint[] | null;
      frameWidth: number;
      frameHeight: number;
    };

/** Sink → page: `ready` once per init, one `status` per frame, `complete` at most once. */
export type FromSink =
  | { kind: "ready" }
  | { kind: "error"; message: string }
  | {
      kind: "status";
      outcome: Outcome;
      quad: QuadPoint[] | null;
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

// The detection half of the eye — one worker of a small pool.
//
// Each instance owns a ZXing reader and nothing else: it turns one captured
// frame into whatever payloads its symbols carried and hands them straight
// back. No stream state lives here — that is the sink worker's job — which
// is exactly what makes it safe to run several of these side by side on one
// camera feed.

import type { FromDecoder, QuadPoint, ToDecoder, Transport } from "./protocol.js";
import { prepareZXingModule, readBarcodes } from "zxing-wasm/reader";
import zxingReaderWasm from "zxing-wasm/reader/zxing_reader.wasm?url";
import { RGB_CHANNELS, TILE_COUNT } from "./qr-reference.js";
import { channelToGrey } from "./rgb-channel.js";
import { READER_OPTIONS } from "./reader-options.js";

// The DOM lib types `self` as a Window; this is the shape a dedicated worker
// actually has, narrowed to what this file uses.
const scope = self as unknown as {
  postMessage(message: FromDecoder, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent<ToDecoder>) => void) | null;
};

let transport: Transport = "qr";
let prepared = false;

/** The reader options for the current transport: tiled frames carry up to
 * [`TILE_COUNT`] symbols per image, everything else exactly one. */
let readOptions = READER_OPTIONS;

/** Grow-only raster target for reading transferred ImageBitmaps back. */
let rasterCanvas: OffscreenCanvas | null = null;
let rasterCtx: OffscreenCanvasRenderingContext2D | null = null;

/** Read a transferred ImageBitmap back to RGBA — the readback the page's
 * bitmap capture path deliberately left to this worker. */
function bitmapToRgba(bitmap: ImageBitmap): Uint8ClampedArray<ArrayBuffer> {
  if (!rasterCanvas || rasterCanvas.width < bitmap.width || rasterCanvas.height < bitmap.height) {
    rasterCanvas = new OffscreenCanvas(
      Math.max(bitmap.width, rasterCanvas?.width ?? 0),
      Math.max(bitmap.height, rasterCanvas?.height ?? 0),
    );
    rasterCtx = rasterCanvas.getContext("2d", { willReadFrequently: true });
  }
  if (!rasterCtx) throw new Error("OffscreenCanvas refused a 2d context");
  rasterCtx.drawImage(bitmap, 0, 0);
  const rgba = rasterCtx.getImageData(0, 0, bitmap.width, bitmap.height).data;
  bitmap.close();
  return rgba;
}

/**
 * Scratch for channel separation, reused across channels and frames.
 *
 * Three fresh RGBA buffers per capture would churn ~25 MB of garbage per
 * frame at 1920 wide. `readBarcodes` copies the pixels into the ZXing heap
 * before it returns, so one buffer can safely serve every channel in turn.
 */
let channelScratch: Uint8ClampedArray<ArrayBuffer> | null = null;

/** One channel of an RGBA frame as grey, contrast-stretched against
 * channel crosstalk — the details live with `channelToGrey`. */
function channelImage(
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
  channel: number,
): ImageData {
  if (!channelScratch || channelScratch.length !== rgba.length) {
    channelScratch = new Uint8ClampedArray(rgba.length);
  }
  channelToGrey(rgba, channel, channelScratch);
  return new ImageData(channelScratch, width, height);
}

/**
 * Read every QR symbol one captured frame carries.
 *
 * The black-and-white transport reads the frame once; the RGB transport
 * separates the three colour channels and reads each as an independent
 * standard symbol. A channel lost to crosstalk simply decodes nothing — it
 * shortens the harvest, never poisons it.
 *
 * The quad is where ZXing saw a symbol, kept even when its payload could not
 * be read — locating and reading fail separately, and the page draws that
 * difference. In RGB mode the channels share one geometry, so the first
 * channel to locate speaks for the frame.
 */
async function decodeFrame(
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
): Promise<{ payloads: Uint8Array[]; quad: QuadPoint[] | null }> {
  const images = transport.includes("rgb")
    ? Array.from({ length: RGB_CHANNELS }, (_, c) => () => channelImage(rgba, width, height, c))
    : [() => new ImageData(rgba, width, height)];

  const payloads: Uint8Array[] = [];
  let quad: QuadPoint[] | null = null;
  for (const image of images) {
    const results = await readBarcodes(image(), readOptions);
    const located = results[0];
    if (located && !quad) {
      const { topLeft, topRight, bottomRight, bottomLeft } = located.position;
      quad = [topLeft, topRight, bottomRight, bottomLeft].map((p) => ({ x: p.x, y: p.y }));
    }
    // Everything readable in the image — one symbol normally, up to four on
    // the tiled rungs. Each payload crosses the CRC gate on its own.
    for (const result of results) {
      if (result.isValid && result.bytes.length > 0) payloads.push(result.bytes);
    }
  }
  return { payloads, quad };
}

async function handle(message: ToDecoder): Promise<void> {
  if (message.kind === "init") {
    transport = message.transport;
    readOptions = transport.includes("tile")
      ? { ...READER_OPTIONS, maxNumberOfSymbols: TILE_COUNT }
      : READER_OPTIONS;
    // Re-inits only retune the transport; ZXing itself is prepared once.
    if (!prepared) {
      prepared = true;
      await prepareZXingModule({
        overrides: {
          // Never accept the package default CDN URL. The transfer must
          // remain as air-gapped as its premise once the page is loaded, and
          // Vite emits this URL as a local build asset.
          locateFile: (path, prefix) =>
            path.endsWith(".wasm") ? zxingReaderWasm : prefix + path,
        },
      });
      // Instantiation is expensive enough to make the first camera frame look
      // broken. Warm it with a disposable image before the camera starts.
      await readBarcodes(new ImageData(8, 8), READER_OPTIONS).catch(() => []);
    }
    scope.postMessage({ kind: "ready" });
    return;
  }

  const rgba =
    message.bitmap !== undefined
      ? bitmapToRgba(message.bitmap)
      : message.buffer !== undefined
        ? new Uint8ClampedArray(message.buffer)
        : null;
  // A frame with no pixels cannot happen from the page as written — but a
  // silent return here would leak the page's pool slot forever, so answer
  // with an empty harvest instead.
  const { payloads, quad } = rgba
    ? await decodeFrame(rgba, message.width, message.height)
    : { payloads: [], quad: null };
  // ZXing measured the quad in this buffer's pixels; answer in source pixels
  // so the page never cares whether the frame was a downscale or a crop.
  const mapped =
    quad?.map((p) => ({
      x: message.originX + p.x * message.scale,
      y: message.originY + p.y * message.scale,
    })) ?? null;
  // Payloads are a few KB each and ZXing owns their buffers' provenance —
  // cloned, not transferred; a detached heap is not worth saving 3 KB.
  scope.postMessage({
    kind: "decoded",
    payloads,
    quad: mapped,
    frameWidth: message.sourceWidth,
    frameHeight: message.sourceHeight,
  });
}

scope.onmessage = (event) => {
  handle(event.data).catch((error: unknown) => {
    scope.postMessage({ kind: "error", message: `Decode failed: ${error}` });
  });
};

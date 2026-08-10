// The one set of ZXing ReaderOptions, shared by the decode workers and the
// Node optical seam test — the test's claim to "exercise the same path" is
// only true while this stays the single copy.

import type { readBarcodes } from "zxing-wasm/reader";

/**
 * Tuned for this transport rather than for a general-purpose scanner.
 *
 * `tryHarder` stays on: camera frames are the entire workload, and the
 * marginal ones are exactly the frames worth extra detection effort.
 * `tryRotate` is off — QR finder patterns make detection rotation-invariant
 * on their own; the extra 90/180/270° passes only pay for symbologies whose
 * detectors are not. `tryInvert` is off — the skin never renders
 * light-on-dark. `tryDownscale` stays on for captures where the symbol
 * out-resolves the sensor (moiré, or a screen filling the frame). The
 * `LocalAverage` binarizer is the library default, kept explicitly because
 * it is load-bearing here: it thresholds each pixel against its
 * neighbourhood, which is what survives a glare gradient across a panel.
 */
export const READER_OPTIONS: Parameters<typeof readBarcodes>[1] = {
  formats: ["QRCode"],
  maxNumberOfSymbols: 1,
  tryHarder: true,
  tryRotate: false,
  tryInvert: false,
  tryDownscale: true,
  binarizer: "LocalAverage",
};

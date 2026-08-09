/** Standard-QR raster for the reference transport.
 *
 * This is intentionally separate from `optical-display.ts`: Cuttldrop's
 * custom carrier is white-on-black and needs a black quiet zone, while an ISO
 * QR code is black-on-white and needs a white one. Mixing their polarities
 * would make the comparison meaningless.
 */

import QRCode from "qrcode";
import type { PulseRaster } from "./optical-display.js";

export const QR_REFERENCE_VERSION = 27;
export const QR_REFERENCE_ECC = "L";
export const QR_REFERENCE_MASK = 4;
export const QR_QUIET_MODULES = 4;

/** QR Model 2 side length: 21 + 4 × (version - 1). */
export const QR_REFERENCE_MODULES = 21 + 4 * (QR_REFERENCE_VERSION - 1);
export const QR_REFERENCE_SIZE = QR_REFERENCE_MODULES + QR_QUIET_MODULES * 2;

/** Turn one Cuttldrop packet into a fixed v27-L QR raster at one px/module. */
export function rasterizeReferencePacket(packet: Uint8Array): PulseRaster {
  const qr = QRCode.create([{ data: packet, mode: "byte" }], {
    version: QR_REFERENCE_VERSION,
    errorCorrectionLevel: QR_REFERENCE_ECC,
    maskPattern: QR_REFERENCE_MASK,
  });
  if (qr.modules.size !== QR_REFERENCE_MODULES) {
    throw new Error(`reference QR changed size to ${qr.modules.size}`);
  }

  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(
    QR_REFERENCE_SIZE * QR_REFERENCE_SIZE * 4,
  );
  rgba.fill(255); // opaque white modules and the required quiet zone
  for (let y = 0; y < QR_REFERENCE_MODULES; y += 1) {
    for (let x = 0; x < QR_REFERENCE_MODULES; x += 1) {
      if (!qr.modules.data[y * QR_REFERENCE_MODULES + x]) continue;
      const at = ((y + QR_QUIET_MODULES) * QR_REFERENCE_SIZE + x + QR_QUIET_MODULES) * 4;
      rgba[at] = 0;
      rgba[at + 1] = 0;
      rgba[at + 2] = 0;
    }
  }
  return { width: QR_REFERENCE_SIZE, height: QR_REFERENCE_SIZE, rgba };
}

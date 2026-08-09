/** Standard-QR raster for the reference transport.
 *
 * This is intentionally separate from `optical-display.ts`: Cuttldrop's
 * custom carrier is white-on-black and needs a black quiet zone, while an ISO
 * QR code is black-on-white and needs a white one. Mixing their polarities
 * would make the comparison meaningless.
 */

import QRCode from "qrcode";
import type { PulseRaster } from "./optical-display.js";

export const QR_REFERENCE_ECC = "L";
export const QR_REFERENCE_MASK = 4;
export const QR_QUIET_MODULES = 4;

export type QrReferenceProfile = "qr27" | "qr35" | "qr40";

export interface QrReferenceSpec {
  readonly version: 27 | 35 | 40;
  /** RaptorQ application bytes per packet, after its packet id. */
  readonly symbolBytes: number;
  /** QR Model 2 side length: 21 + 4 × (version - 1). */
  readonly modules: number;
  /** QR modules plus the required four-module margin on every side. */
  readonly size: number;
}

/**
 * Fixed QR configurations used for optical A/B testing.
 *
 * `symbolBytes` includes the eight-byte RFC 6330 alignment rule. With our
 * 32-byte header/id/CRC envelope, each value fits within byte-mode QR-L
 * capacity without allowing the writer to alter the QR version.
 */
export const QR_REFERENCE_PROFILES: Record<QrReferenceProfile, QrReferenceSpec> = {
  qr27: { version: 27, symbolBytes: 1432, modules: 125, size: 133 },
  qr35: { version: 35, symbolBytes: 2264, modules: 157, size: 165 },
  qr40: { version: 40, symbolBytes: 2920, modules: 177, size: 185 },
};

export function referenceProfile(name: string): QrReferenceSpec {
  const profile = QR_REFERENCE_PROFILES[name as QrReferenceProfile];
  if (!profile) throw new Error(`unknown QR reference profile ${JSON.stringify(name)}`);
  return profile;
}

/** Turn one Cuttldrop packet into a fixed QR-L raster at one px/module. */
export function rasterizeReferencePacket(packet: Uint8Array, profile: string): PulseRaster {
  const spec = referenceProfile(profile);
  const qr = QRCode.create([{ data: packet, mode: "byte" }], {
    version: spec.version,
    errorCorrectionLevel: QR_REFERENCE_ECC,
    maskPattern: QR_REFERENCE_MASK,
  });
  if (qr.modules.size !== spec.modules) {
    throw new Error(`reference QR ${profile} changed size to ${qr.modules.size}`);
  }

  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(
    spec.size * spec.size * 4,
  );
  rgba.fill(255); // opaque white modules and the required quiet zone
  for (let y = 0; y < spec.modules; y += 1) {
    for (let x = 0; x < spec.modules; x += 1) {
      if (!qr.modules.data[y * spec.modules + x]) continue;
      const at = ((y + QR_QUIET_MODULES) * spec.size + x + QR_QUIET_MODULES) * 4;
      rgba[at] = 0;
      rgba[at + 1] = 0;
      rgba[at + 2] = 0;
    }
  }
  return { width: spec.size, height: spec.size, rgba };
}

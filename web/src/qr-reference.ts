/** Standard-QR raster for the reference transport.
 *
 * This is intentionally separate from `optical-display.ts`: Cuttldrop's
 * custom carrier is white-on-black and needs a black quiet zone, while an ISO
 * QR code is black-on-white and needs a white one. Mixing their polarities
 * would make the comparison meaningless.
 */

import QRCode from "qrcode";
import type { PulseRaster } from "./optical-display.js";

export const QR_REFERENCE_MASK = 4;
export const QR_QUIET_MODULES = 4;

export type QrReferenceProfile = "qr27" | "qr35" | "qr40" | "qr27m" | "qr35m" | "qr40m";

export interface QrReferenceSpec {
  readonly version: 27 | 35 | 40;
  /** L is the capacity half of the ladder; M the hardened half — same
   * geometry, ~24% less payload, double the codeword correction. */
  readonly eccLevel: "L" | "M";
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
 * 32-byte header/id/CRC envelope, each value fits within the byte-mode
 * capacity of its QR version and ECC level without allowing the writer to
 * alter the QR version. These must agree exactly with the Rust
 * `ReferenceProfile` table — the seam test rasterizes real packets at every
 * rung, so a drift fails loudly.
 */
export const QR_REFERENCE_PROFILES: Record<QrReferenceProfile, QrReferenceSpec> = {
  qr27: { version: 27, eccLevel: "L", symbolBytes: 1432, modules: 125, size: 133 },
  qr35: { version: 35, eccLevel: "L", symbolBytes: 2264, modules: 157, size: 165 },
  qr40: { version: 40, eccLevel: "L", symbolBytes: 2920, modules: 177, size: 185 },
  qr27m: { version: 27, eccLevel: "M", symbolBytes: 1088, modules: 125, size: 133 },
  qr35m: { version: 35, eccLevel: "M", symbolBytes: 1776, modules: 157, size: 165 },
  qr40m: { version: 40, eccLevel: "M", symbolBytes: 2296, modules: 177, size: 185 },
};

export function referenceProfile(name: string): QrReferenceSpec {
  const profile = QR_REFERENCE_PROFILES[name as QrReferenceProfile];
  if (!profile) throw new Error(`unknown QR reference profile ${JSON.stringify(name)}`);
  return profile;
}

/** QR symbols carried by one colored frame — one per RGB channel. */
export const RGB_CHANNELS = 3;

/** Encode one packet at a fixed version, or throw if it would not fit. */
function moduleMatrix(packet: Uint8Array, spec: QrReferenceSpec, profile: string) {
  const qr = QRCode.create([{ data: packet, mode: "byte" }], {
    version: spec.version,
    errorCorrectionLevel: spec.eccLevel,
    maskPattern: QR_REFERENCE_MASK,
  });
  if (qr.modules.size !== spec.modules) {
    throw new Error(`reference QR ${profile} changed size to ${qr.modules.size}`);
  }
  return qr.modules.data;
}

/** Turn one Cuttldrop packet into a fixed standard-QR raster at one px/module. */
export function rasterizeReferencePacket(packet: Uint8Array, profile: string): PulseRaster {
  const spec = referenceProfile(profile);
  const modules = moduleMatrix(packet, spec, profile);

  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(
    spec.size * spec.size * 4,
  );
  rgba.fill(255); // opaque white modules and the required quiet zone
  for (let y = 0; y < spec.modules; y += 1) {
    for (let x = 0; x < spec.modules; x += 1) {
      if (!modules[y * spec.modules + x]) continue;
      const at = ((y + QR_QUIET_MODULES) * spec.size + x + QR_QUIET_MODULES) * 4;
      rgba[at] = 0;
      rgba[at + 1] = 0;
      rgba[at + 2] = 0;
    }
  }
  return { width: spec.size, height: spec.size, rgba };
}

/**
 * Turn three packets into one colored raster — one standard QR per RGB channel.
 *
 * JAB Code's colour thesis on standard-QR geometry. Same-version symbols put
 * their function patterns (finders, timing, alignment, format) on the same
 * modules, and byte mode with a fixed mask keeps every structural module
 * identical across the three symbols — so those modules go dark in all three
 * channels at once and render black, exactly as a plain QR would. Only data
 * modules diverge into colour. Each channel, separated by the eye, is a
 * complete standards-compliant symbol for an unmodified ZXing reader; a
 * channel ruined by crosstalk costs its packet, never the frame, because every
 * packet still crosses the CRC gate on its own.
 */
export function rasterizeRgbReferencePackets(
  packets: readonly Uint8Array[],
  profile: string,
): PulseRaster {
  if (packets.length !== RGB_CHANNELS) {
    throw new Error(`RGB reference frame needs ${RGB_CHANNELS} packets, got ${packets.length}`);
  }
  const spec = referenceProfile(profile);
  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(
    spec.size * spec.size * 4,
  );
  rgba.fill(255); // white quiet zone in every channel
  for (let channel = 0; channel < RGB_CHANNELS; channel += 1) {
    const modules = moduleMatrix(packets[channel], spec, profile);
    for (let y = 0; y < spec.modules; y += 1) {
      for (let x = 0; x < spec.modules; x += 1) {
        if (!modules[y * spec.modules + x]) continue;
        rgba[((y + QR_QUIET_MODULES) * spec.size + x + QR_QUIET_MODULES) * 4 + channel] = 0;
      }
    }
  }
  return { width: spec.size, height: spec.size, rgba };
}

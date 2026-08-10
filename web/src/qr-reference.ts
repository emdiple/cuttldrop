/** Standard-QR raster for the reference transport.
 *
 * This is intentionally separate from `optical-display.ts`: Cuttldrop's
 * custom carrier is white-on-black and needs a black quiet zone, while an ISO
 * QR code is black-on-white and needs a white one. Mixing their polarities
 * would make the comparison meaningless.
 */

import QRCode from "qrcode";
import type { PulseRaster } from "./optical-display.ts";

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

/** Side of the tiled rung's symbol grid. */
export const TILE_GRID = 2;
/** Symbols per tiled frame. */
export const TILE_COUNT = TILE_GRID * TILE_GRID;

/**
 * The calibration strip under every RGB symbol: five patches — pure red,
 * green, blue, black, white — spanning the symbol's width.
 *
 * A screen's primaries land smeared across a camera's colour dyes, and past
 * ~25% leak the channels *reorder*: a module black in this channel but white
 * in the others captures brighter than its opposite, and no per-channel
 * stretch or threshold can undo a reordering. The patches give the eye the
 * mixing matrix itself, measured in-frame every frame — solved and inverted
 * in rgb-calibration.ts. Black and white anchor the offset and validate the
 * solve.
 */
export const CALIBRATION_PATCHES = 5;
/** Patch height in modules. */
export const CALIBRATION_ROWS = 3;
/** Extra raster rows an RGB frame carries: the patches plus a white margin. */
export const CALIBRATION_STRIP_MODULES = CALIBRATION_ROWS + 1;
/** Patch colours in strip order: R, G, B, K, W. */
export const CALIBRATION_COLORS: ReadonlyArray<readonly [number, number, number]> = [
  [255, 0, 0],
  [0, 255, 0],
  [0, 0, 255],
  [0, 0, 0],
  [255, 255, 255],
];

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
  const height = spec.size + CALIBRATION_STRIP_MODULES;
  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(spec.size * height * 4);
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
  // The calibration strip sits below the symbol's own quiet zone — the
  // reader still sees its required four white modules, and the eye finds
  // the patches at a fixed offset from the corners ZXing reports.
  for (let patch = 0; patch < CALIBRATION_PATCHES; patch += 1) {
    const from = QR_QUIET_MODULES + Math.floor((patch * spec.modules) / CALIBRATION_PATCHES);
    const to = QR_QUIET_MODULES + Math.floor(((patch + 1) * spec.modules) / CALIBRATION_PATCHES);
    const [red, green, blue] = CALIBRATION_COLORS[patch];
    for (let y = spec.size; y < spec.size + CALIBRATION_ROWS; y += 1) {
      for (let x = from; x < to; x += 1) {
        const at = (y * spec.size + x) * 4;
        rgba[at] = red;
        rgba[at + 1] = green;
        rgba[at + 2] = blue;
      }
    }
  }
  return { width: spec.size, height, rgba };
}

/**
 * Turn `TILE_COUNT × channels` packets into a 2×2 grid of standard symbols.
 *
 * The tiled rung climbs density the other way: not a bigger symbol, more
 * small ones. A v27 grid carries nearly double a single v40's payload at a
 * similar overall module pitch, but each symbol locates and decodes on its
 * own — glare across one corner costs that corner's packets, never the
 * frame, where one big symbol is all-or-nothing. Every tile keeps its own
 * four-module quiet zone, so neighbouring symbols sit eight white modules
 * apart and an unmodified reader sees four ordinary QR codes. With
 * `channels` = [`RGB_CHANNELS`] each tile is additionally
 * colour-multiplexed; packets fill tile by tile, channels within a tile.
 */
export function rasterizeTiledReferencePackets(
  packets: readonly Uint8Array[],
  profile: string,
  channels: number,
): PulseRaster {
  if (packets.length !== TILE_COUNT * channels) {
    throw new Error(
      `tiled reference frame needs ${TILE_COUNT * channels} packets, got ${packets.length}`,
    );
  }
  const cells = Array.from({ length: TILE_COUNT }, (_, tile) => {
    const group = packets.slice(tile * channels, (tile + 1) * channels);
    return channels === RGB_CHANNELS
      ? rasterizeRgbReferencePackets(group, profile)
      : rasterizeReferencePacket(group[0], profile);
  });
  // RGB cells are taller than they are wide — the calibration strip rides
  // below each tile's symbol — so the grid tracks both cell dimensions.
  const width = cells[0].width * TILE_GRID;
  const height = cells[0].height * TILE_GRID;
  const rgba: Uint8ClampedArray<ArrayBuffer> = new Uint8ClampedArray(width * height * 4);
  rgba.fill(255);
  for (let tile = 0; tile < TILE_COUNT; tile += 1) {
    const cell = cells[tile];
    const originX = (tile % TILE_GRID) * cell.width;
    const originY = Math.floor(tile / TILE_GRID) * cell.height;
    for (let y = 0; y < cell.height; y += 1) {
      const from = y * cell.width * 4;
      rgba.set(cell.rgba.subarray(from, from + cell.width * 4), ((originY + y) * width + originX) * 4);
    }
  }
  return { width, height, rgba };
}

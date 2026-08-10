// The eye's half of the calibration strip: find the patches from the corner
// quad ZXing reported, measure the screen-to-sensor colour mixing, and undo
// it. The strip itself is painted in qr-reference.ts; this module never sees
// a symbol, only pixels and geometry.
//
// Why a matrix and not more stretching: per-channel transforms are monotone,
// and monotone maps preserve order. Past ~25% leak the order itself breaks —
// a module black in this channel but white in the other two captures
// *brighter* than its opposite — so the only way back is to measure the
// 3×3 mixing and invert it. The patches are the measurement: pure R, G and B
// give the matrix columns, black gives the offset, and white validates the
// solve before it is trusted.

import { CALIBRATION_PATCHES, CALIBRATION_ROWS, QR_QUIET_MODULES } from "./qr-reference.ts";
import type { QuadPoint } from "./protocol.ts";

/** A solved unmix: `corrected = matrix × (observed − black)`, per pixel. */
export interface Unmix {
  /** Row-major 3×3. */
  readonly matrix: Float64Array;
  /** The observed black patch — the leak-and-ambient floor. */
  readonly black: Float64Array;
}

/**
 * Square-to-quad perspective mapping (Heckbert's classic formulation).
 *
 * `(u, v)` in symbol units — (0,0) the symbol's top-left corner, (1,1) its
 * bottom-right — to frame pixels. The strip lives *below* the symbol, at
 * v > 1; a projective map extrapolates there exactly, which is the whole
 * point of deriving it from the corners ZXing already reports.
 */
function projectFromQuad(quad: readonly QuadPoint[]): ((u: number, v: number) => QuadPoint) | null {
  const [c0, c1, c2, c3] = quad; // TL, TR, BR, BL
  const dx1 = c1.x - c2.x;
  const dy1 = c1.y - c2.y;
  const dx2 = c3.x - c2.x;
  const dy2 = c3.y - c2.y;
  const sx = c0.x - c1.x + c2.x - c3.x;
  const sy = c0.y - c1.y + c2.y - c3.y;
  const den = dx1 * dy2 - dx2 * dy1;
  if (den === 0) return null;
  const g = (sx * dy2 - dx2 * sy) / den;
  const h = (dx1 * sy - sx * dy1) / den;
  const a = c1.x - c0.x + g * c1.x;
  const b = c3.x - c0.x + h * c3.x;
  const d = c1.y - c0.y + g * c1.y;
  const e = c3.y - c0.y + h * c3.y;
  return (u, v) => {
    const w = g * u + h * v + 1;
    return { x: (a * u + b * v + c0.x) / w, y: (d * u + e * v + c0.y) / w };
  };
}

/** Mean of a 3×3 neighbourhood in one channel, or null off the frame. */
function sampleAt(
  rgba: Uint8ClampedArray,
  width: number,
  height: number,
  point: QuadPoint,
  channel: number,
): number | null {
  const x = Math.round(point.x);
  const y = Math.round(point.y);
  if (x < 1 || y < 1 || x >= width - 1 || y >= height - 1) return null;
  let sum = 0;
  for (let dy = -1; dy <= 1; dy += 1) {
    for (let dx = -1; dx <= 1; dx += 1) {
      sum += rgba[((y + dy) * width + x + dx) * 4 + channel];
    }
  }
  return sum / 9;
}

/**
 * Observed RGB of each calibration patch, in strip order, or null when any
 * patch centre lands outside the frame. `modules` is the symbol's module
 * count — the strip's vertical offset does not scale with the symbol, so the
 * caller supplies candidates and lets the white patch arbitrate.
 */
export function sampleStrip(
  rgba: Uint8ClampedArray,
  width: number,
  height: number,
  quad: readonly QuadPoint[],
  modules: number,
): number[][] | null {
  if (quad.length !== 4) return null;
  const project = projectFromQuad(quad);
  if (!project) return null;
  const v = (modules + QR_QUIET_MODULES + CALIBRATION_ROWS / 2) / modules;
  const patches: number[][] = [];
  for (let patch = 0; patch < CALIBRATION_PATCHES; patch += 1) {
    const point = project((patch + 0.5) / CALIBRATION_PATCHES, v);
    const sample = [0, 1, 2].map((channel) => sampleAt(rgba, width, height, point, channel));
    if (sample.some((value) => value === null)) return null;
    patches.push(sample as number[]);
  }
  return patches;
}

/** How far the corrected white patch may sit from pure white, per channel. */
const WHITE_TOLERANCE = 70;

/**
 * Solve the unmix from one strip measurement, or null when the measurement
 * does not deserve trust. The R/G/B patches give the mixing matrix's
 * columns (black-relative), inversion gives the unmix, and the white patch —
 * which took no part in the solve — must come out white, or the geometry
 * was wrong and the whole sample is discarded. That check is what lets the
 * caller probe several module-count candidates safely.
 */
export function solveUnmix(patches: readonly number[][]): Unmix | null {
  const [r, g, b, k, w] = patches;
  // Columns of the observed mixing, with the black floor removed.
  const m = [r[0] - k[0], g[0] - k[0], b[0] - k[0]];
  const n = [r[1] - k[1], g[1] - k[1], b[1] - k[1]];
  const o = [r[2] - k[2], g[2] - k[2], b[2] - k[2]];
  const det =
    m[0] * (n[1] * o[2] - n[2] * o[1]) -
    m[1] * (n[0] * o[2] - n[2] * o[0]) +
    m[2] * (n[0] * o[1] - n[1] * o[0]);
  // Pure singularity guard; the white check below is the real gate.
  if (Math.abs(det) < 100) return null;
  const matrix = new Float64Array([
    (n[1] * o[2] - n[2] * o[1]) / det,
    (m[2] * o[1] - m[1] * o[2]) / det,
    (m[1] * n[2] - m[2] * n[1]) / det,
    (n[2] * o[0] - n[0] * o[2]) / det,
    (m[0] * o[2] - m[2] * o[0]) / det,
    (m[2] * n[0] - m[0] * n[2]) / det,
    (n[0] * o[1] - n[1] * o[0]) / det,
    (m[1] * o[0] - m[0] * o[1]) / det,
    (m[0] * n[1] - m[1] * n[0]) / det,
  ]);
  for (let i = 0; i < 9; i += 1) matrix[i] *= 255;
  const black = new Float64Array([k[0], k[1], k[2]]);
  for (let channel = 0; channel < 3; channel += 1) {
    const corrected =
      matrix[channel * 3] * (w[0] - black[0]) +
      matrix[channel * 3 + 1] * (w[1] - black[1]) +
      matrix[channel * 3 + 2] * (w[2] - black[2]);
    if (Math.abs(corrected - 255) > WHITE_TOLERANCE) return null;
  }
  return { matrix, black };
}

/**
 * Measure the unmix from one frame, probing each module-count candidate
 * until a solve survives the white check. Null when nothing does — the
 * caller keeps whatever calibration it last trusted.
 */
export function calibrateFrame(
  rgba: Uint8ClampedArray,
  width: number,
  height: number,
  quad: readonly QuadPoint[],
  moduleCandidates: readonly number[],
): Unmix | null {
  for (const modules of moduleCandidates) {
    const patches = sampleStrip(rgba, width, height, quad, modules);
    if (!patches) continue;
    const unmix = solveUnmix(patches);
    if (unmix) return unmix;
  }
  return null;
}

/** Undo the measured mixing across a whole frame. `out` is clamped RGBA. */
export function applyUnmix(
  rgba: Uint8ClampedArray,
  unmix: Unmix,
  out: Uint8ClampedArray,
): Uint8ClampedArray {
  const m = unmix.matrix;
  const k = unmix.black;
  for (let at = 0; at < rgba.length; at += 4) {
    const x = rgba[at] - k[0];
    const y = rgba[at + 1] - k[1];
    const z = rgba[at + 2] - k[2];
    out[at] = m[0] * x + m[1] * y + m[2] * z;
    out[at + 1] = m[3] * x + m[4] * y + m[5] * z;
    out[at + 2] = m[6] * x + m[7] * y + m[8] * z;
    out[at + 3] = 255;
  }
  return out;
}

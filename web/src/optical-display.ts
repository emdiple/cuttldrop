/** Pure optical-display primitives shared by the skin and its Node tests. */

export interface PulseRaster {
  readonly width: number;
  readonly height: number;
  readonly rgba: Uint8ClampedArray<ArrayBuffer>;
}

export interface PhysicalFit {
  /** Whole physical device pixels occupied by every chroma cell. */
  readonly scale: number;
  readonly maxScale: number;
  readonly backingWidth: number;
  readonly backingHeight: number;
  readonly cssWidth: number;
  readonly cssHeight: number;
}

/**
 * Fit a raster using one whole number of physical pixels per chroma cell.
 *
 * CSS pixels are not physical pixels on a Retina/HiDPI display. Performing the
 * fit in device pixels keeps every cell exactly the same width even at a
 * fractional DPR such as 2.625, then exposes the corresponding CSS size for
 * layout.
 */
export function fitPhysicalScale(
  cols: number,
  rows: number,
  stageCssWidth: number,
  stageCssHeight: number,
  devicePixelRatio: number,
  requestedScale?: number,
): PhysicalFit {
  if (!Number.isInteger(cols) || cols <= 0 || !Number.isInteger(rows) || rows <= 0) {
    throw new RangeError("raster dimensions must be positive integers");
  }

  const dpr = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
  const width = Math.max(0, stageCssWidth);
  const height = Math.max(0, stageCssHeight);
  const maxScale = Math.max(1, Math.floor(Math.min((width * dpr) / cols, (height * dpr) / rows)));
  const wanted = requestedScale == null || !Number.isFinite(requestedScale) ? maxScale : Math.floor(requestedScale);
  const scale = Math.min(maxScale, Math.max(1, wanted));
  const backingWidth = cols * scale;
  const backingHeight = rows * scale;

  return {
    scale,
    maxScale,
    backingWidth,
    backingHeight,
    cssWidth: backingWidth / dpr,
    cssHeight: backingHeight / dpr,
  };
}

/**
 * Time-based pulse pacing with a hard two-refresh visibility floor.
 *
 * The skin paints pulse zero before this pacer starts. Each call represents a
 * display-refresh callback; `tick` returns true only when the current pulse has
 * survived at least two callbacks and its requested time interval has elapsed.
 */
export class PulsePacer {
  private refreshes = 0;
  private nextAt: number;
  private targetHz: number;

  constructor(now: number, targetHz: number) {
    this.targetHz = PulsePacer.validRate(targetHz);
    this.nextAt = now + 1000 / this.targetHz;
  }

  tick(now: number, requestedHz: number): boolean {
    const targetHz = PulsePacer.validRate(requestedHz);
    if (targetHz !== this.targetHz) {
      this.targetHz = targetHz;
      this.nextAt = now + 1000 / targetHz;
      this.refreshes = 0;
    }

    this.refreshes += 1;
    // rAF timestamps and decimal refresh intervals accumulate tiny binary
    // rounding differences; do not turn an on-time callback into a missed one.
    if (this.refreshes < 2 || now + 1e-6 < this.nextAt) return false;

    this.refreshes = 0;
    const interval = 1000 / this.targetHz;
    this.nextAt += interval;
    // A hidden or heavily loaded tab must not burst through deadlines that the
    // panel never displayed. Resume one interval from the present instead.
    if (now - this.nextAt > 3 * interval) this.nextAt = now + interval;
    return true;
  }

  private static validRate(value: number): number {
    return Number.isFinite(value) ? Math.max(1, value) : 1;
  }
}

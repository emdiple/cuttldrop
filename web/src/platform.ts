// Device quirks, probed where probing is possible.
//
// Policy, adopted from decimen's field notes (`COMPARISON-decimen.md` R5–R7):
// **probe, do not sniff**. Every camera capability here is asked of the track
// itself, because the answer varies by camera and by mode, not by browser. The
// one user-agent test is quarantined at the bottom and exists only for
// behaviour that is not observable until it has already gone wrong.

/**
 * Capabilities this camera actually reports, as opposed to what the spec says
 * it should have.
 *
 * `getCapabilities` is itself optional, and iOS Safari exposes almost none of
 * the constrainable properties Chrome does — so every field is optional and
 * every caller has to work without it.
 */
export interface CameraCapabilities {
  /** Continuous autofocus. The single most valuable one: a lens hunting between
   *  frames blurs whole captures, and the sender never stops moving on screen. */
  continuousFocus: boolean;
  manualWhiteBalance: boolean;
  manualExposure: boolean;
  /** Highest frame rate the *current* mode admits to, when it says. */
  maxFrameRate?: number;
}

// Constrainable properties Chrome ships and `lib.dom` does not type.
type Extended = MediaTrackCapabilities & {
  focusMode?: string[];
  whiteBalanceMode?: string[];
  exposureMode?: string[];
};

export function probeCamera(track: MediaStreamTrack): CameraCapabilities {
  const caps: Extended = track.getCapabilities?.() ?? {};
  const has = (list: string[] | undefined, value: string) =>
    Array.isArray(list) && list.includes(value);
  return {
    continuousFocus: has(caps.focusMode, "continuous"),
    manualWhiteBalance: has(caps.whiteBalanceMode, "manual"),
    manualExposure: has(caps.exposureMode, "manual"),
    maxFrameRate: caps.frameRate?.max,
  };
}

/**
 * Best-effort advanced constraint; `true` when the camera took it.
 *
 * The spec says an advanced constraint set never rejects — browsers disagree,
 * so this catches anyway. A camera that refuses is left exactly as it was,
 * which is the correct outcome: it still works, just less well.
 */
export async function tryConstraint(
  track: MediaStreamTrack,
  set: Record<string, unknown>,
): Promise<boolean> {
  try {
    await track.applyConstraints({ advanced: [set] } as MediaTrackConstraints);
    return true;
  } catch {
    return false;
  }
}

/** A `getUserMedia` failure, said in words a human can act on. */
export function cameraError(error: unknown): string {
  const name = error instanceof DOMException ? error.name : "";
  switch (name) {
    case "NotAllowedError":
      return "Camera permission was denied. Allow it in the browser's site settings, then tap Try again.";
    case "NotFoundError":
    case "OverconstrainedError":
      return "No camera matched what this page asked for. If the device has one, try again — the second attempt asks for less.";
    case "NotReadableError":
      return "The camera is already in use by another app or tab. Close it and tap Try again.";
    default:
      return `Camera failed to open: ${error instanceof Error ? error.message : String(error)}`;
  }
}

/**
 * Keep the screen awake, and keep it awake across backgrounding.
 *
 * This matters at both ends and for different reasons. On the skin the screen
 * *is* the transmitter, so a display timeout mid-transfer stops the send. On the
 * eye a locked screen stops the camera. Neither reports anything useful when it
 * happens — the transfer simply stalls.
 *
 * A wake lock is released automatically whenever the page becomes hidden and is
 * **not** restored when it comes back, so re-acquiring on `visibilitychange` is
 * not belt-and-braces; without it the lock is gone after the first notification
 * the user swipes away.
 */
export class ScreenAwake {
  private sentinel: WakeLockSentinel | null = null;
  private wanted = false;
  private listening = false;

  async acquire(): Promise<void> {
    this.wanted = true;
    if (!this.listening) {
      this.listening = true;
      document.addEventListener("visibilitychange", () => {
        if (this.wanted && document.visibilityState === "visible") void this.request();
      });
    }
    await this.request();
  }

  async release(): Promise<void> {
    this.wanted = false;
    const sentinel = this.sentinel;
    this.sentinel = null;
    await sentinel?.release().catch(() => {});
  }

  private async request(): Promise<void> {
    // Unsupported on iOS before 16.4 and on Firefox; absence is not an error.
    if (!("wakeLock" in navigator) || this.sentinel) return;
    try {
      this.sentinel = await navigator.wakeLock.request("screen");
      this.sentinel.addEventListener("release", () => {
        this.sentinel = null;
      });
    } catch {
      // Denied while hidden, or refused outright. The transfer still works;
      // the user just has to keep the screen alive themselves.
    }
  }
}

/**
 * Mobile WebKit — which is what "iOS" means here, since every browser on iOS is
 * WebKit underneath, Chrome included.
 *
 * The only user-agent test in the codebase, and it is deliberately not used to
 * decide whether a *capability* exists — `probeCamera` does that. It is for
 * quirks with no observable signal until they have already cost a transfer:
 * chiefly that iOS answers `frameRate: {ideal: n}` with whatever it feels like
 * and reports success either way.
 *
 * iPadOS claims to be `MacIntel`; the touch-point count is what gives it away.
 */
export const isMobileWebKit: boolean =
  typeof navigator !== "undefined" &&
  (/iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1));

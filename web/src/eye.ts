// The eye: read pulses off a camera and rebuild the file (DESIGN.md §3e).
//
// Split across two threads. This file owns the camera, the capture loop and
// the feedback overlay; decoding — the same WASM the simulator runs — lives in
// eye-worker.ts (§3e: "run it in a worker so the UI thread stays free").
// Frames cross as transferred buffers, and a frame captured while the worker
// is busy is simply dropped — the skin repeats everything anyway.

import { Outcome } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker } from "./protocol.js";
import { ScreenAwake, cameraError, probeCamera, tryConstraint } from "./platform.js";

// "auto": the eye works the density out from the first frame it understands,
// so a density change on the skin needs no matching change here.
const PROFILE = "auto";

/**
 * Working resolution for decoding.
 *
 * Not the camera's resolution — decoding scans every row and column, so this is
 * the single biggest lever on CPU cost. It is set by the *densest* profile, not
 * the default one, because the eye auto-detects and must be able to read
 * whatever the skin chose.
 *
 * The arithmetic: 192 columns need ~4 px/cell (the measured cliff is between 3
 * and 2), and a handheld frame is rarely more than ~70% filled by the sending
 * screen, so the budget is `192 × 4 ÷ 0.7 ≈ 1100`. 1280 leaves a little room
 * above that. At the 64-column default the same width is a luxurious 14 px/cell.
 */
const WORK_WIDTH = 1280;

/** Frames to look back over when deciding what to tell the human. */
const HINT_WINDOW = 30;

/** Seconds of history the frame-rate readouts average over. */
const RATE_WINDOW = 2;

const video = document.querySelector<HTMLVideoElement>("#camera")!;
const begin = document.querySelector<HTMLButtonElement>("#begin")!;
const hint = document.querySelector<HTMLParagraphElement>("#hint")!;
const progress = document.querySelector<HTMLParagraphElement>("#progress")!;
const counters = document.querySelector<HTMLParagraphElement>("#counters")!;
const barFill = document.querySelector<HTMLDivElement>("#bar-fill")!;
const download = document.querySelector<HTMLAnchorElement>("#download")!;
const cameraMode = document.querySelector<HTMLParagraphElement>("#camera-mode")!;
const tile = (name: string) => document.querySelector<HTMLElement>(`#t-${name}`)!;
const tiles = {
  capture: tile("capture"),
  decode: tile("decode"),
  goodput: tile("goodput"),
  elapsed: tile("elapsed"),
  newdup: tile("newdup"),
  eta: tile("eta"),
};

const awake = new ScreenAwake();

const work = document.createElement("canvas");
const workCtx = work.getContext("2d", { willReadFrequently: true })!;

const worker = new Worker(new URL("./eye-worker.ts", import.meta.url), {
  type: "module",
});
const post = (message: ToWorker, transfer: Transferable[] = []) =>
  worker.postMessage(message, transfer);

const recent: Outcome[] = [];
let last: Extract<FromWorker, { kind: "status" }> | null = null;
let busy = false;
let done = false;

/**
 * Rolling event rate over the last [`RATE_WINDOW`] seconds.
 *
 * Two of these run: one on captures, one on decodes. The *gap* between them is
 * the load the worker shed — frames dropped by the `busy` flag are invisible
 * everywhere else, and "capture 40, decode 12" is the difference between a
 * camera problem and a CPU problem.
 */
class Rate {
  private readonly stamps: number[] = [];

  mark(now: number): void {
    this.stamps.push(now);
    while (this.stamps.length > 0 && now - this.stamps[0] > RATE_WINDOW * 1000) {
      this.stamps.shift();
    }
  }

  /** Per second, or null before there is enough history to divide by. */
  perSecond(now: number): number | null {
    if (this.stamps.length < 2) return null;
    const span = now - this.stamps[0];
    return span > 0 ? ((this.stamps.length - 1) / span) * 1000 : null;
  }
}

const captureRate = new Rate();
const decodeRate = new Rate();
let newFrames = 0;
let dupFrames = 0;
/**
 * When the transfer began — the first frame that yielded a *symbol*, not page
 * load and not the first capture.
 *
 * Aiming time is not transfer time. Starting the clock at page load would
 * charge every second spent lining up the phone against the goodput figure,
 * which is precisely the number we are trying to measure honestly.
 */
let firstSymbolAt: number | null = null;

function formatRate(bytesPerSecond: number): string {
  if (bytesPerSecond >= 1e6) return `${(bytesPerSecond / 1e6).toFixed(2)} MB/s`;
  if (bytesPerSecond >= 1e3) return `${(bytesPerSecond / 1e3).toFixed(1)} KB/s`;
  return `${Math.round(bytesPerSecond)} B/s`;
}

function formatSeconds(seconds: number): string {
  if (!Number.isFinite(seconds)) return "—";
  if (seconds < 60) return `${Math.round(seconds)} s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${String(Math.round(seconds % 60)).padStart(2, "0")}s`;
}

/**
 * Ask the camera to stop helping.
 *
 * Autofocus hunting and auto-exposure both fight a strobing screen. Support is
 * patchy and iOS Safari grants almost none of it, so each is attempted
 * separately and failure is ignored — a camera that refuses still works, just
 * less well. Continuous focus is the one that matters most: a lens hunting
 * between frames blurs whole captures, and the target is never still.
 */
async function steady(track: MediaStreamTrack): Promise<void> {
  const caps = probeCamera(track);
  if (caps.continuousFocus) await tryConstraint(track, { focusMode: "continuous" });
  if (caps.manualWhiteBalance) await tryConstraint(track, { whiteBalanceMode: "manual" });
  if (caps.manualExposure) await tryConstraint(track, { exposureMode: "manual" });
}

/** Turn recent outcomes into one instruction (§1e — the human is the back channel). */
function advise(): string {
  if (recent.length < 5) return "Point this camera at the sending screen";

  const share = (outcome: Outcome) =>
    recent.filter((o) => o === outcome).length / recent.length;

  if (share(Outcome.Unlocatable) > 0.5) return "Fill the frame with the screen";
  if (share(Outcome.Torn) > 0.3) return "Slow the sender down";
  if (share(Outcome.Rejected) > 0.4) return "Hold still";
  if (share(Outcome.Duplicate) > 0.8) return "Reading — nothing new arriving";
  return "Reading";
}

/**
 * Fill the telemetry tiles.
 *
 * Goodput is `symbols × symbolBytes ÷ elapsed`, which needs saying plainly: a
 * fountain delivers no file at all until it converges, so there is no such
 * thing as "bytes received so far". What there is, is a count of symbols that
 * passed the CRC gate, each worth exactly `symbolBytes` of the object. The
 * surplus above K is real work but not useful bytes, so the total is clamped
 * to the file size — an honest average, not a headline.
 */
let baseCameraMode = "";

function meter(now: number): void {
  // Which grid the eye settled on, once it has. Worth showing: it is the only
  // confirmation that the density chosen on the *other* device took effect.
  if (last?.profile && baseCameraMode) {
    cameraMode.textContent = `${baseCameraMode} · profile ${last.profile}`;
  }
  const capture = captureRate.perSecond(now);
  const decode = decodeRate.perSecond(now);
  tiles.capture.textContent = capture === null ? "—" : capture.toFixed(1);
  tiles.decode.textContent = decode === null ? "—" : decode.toFixed(1);
  tiles.newdup.textContent = `${newFrames}/${dupFrames}`;

  if (firstSymbolAt === null || !last) {
    tiles.elapsed.textContent = "—";
    return;
  }
  const elapsed = (now - firstSymbolAt) / 1000;
  tiles.elapsed.textContent = formatSeconds(elapsed);

  const { symbols, needed, symbolBytes, expectedBytes } = last;
  if (!symbolBytes || elapsed <= 0) return;

  const delivered = Math.min(symbols * symbolBytes, expectedBytes ?? Infinity);
  const goodput = delivered / elapsed;
  tiles.goodput.textContent = formatRate(goodput);

  const remaining = Math.max(0, needed - symbols) * symbolBytes;
  tiles.eta.textContent =
    remaining === 0 ? "—" : goodput > 0 ? formatSeconds(remaining / goodput) : "—";
}

function render(): void {
  if (!last) return;
  const { symbols, needed, torn, rejected, unlocatable, fileName, expectedBytes } = last;
  // The manifest names the file long before the file arrives (§3c).
  const label = fileName
    ? `${fileName}${expectedBytes ? ` — ${expectedBytes.toLocaleString()} B` : ""} · `
    : "";
  progress.textContent = `${label}${symbols} / ${needed || "—"} symbols`;
  barFill.style.width = needed > 0 ? `${Math.min(100, (symbols / needed) * 100)}%` : "0%";
  counters.textContent = `${torn} torn · ${rejected} rejected · ${unlocatable} not found`;
  hint.textContent = advise();
}

function finish(bytes: Uint8Array, name: string, mime: string): void {
  done = true;
  // The file is here and verified; holding the camera and the wake lock past
  // that point drains the battery and leaves the indicator light on for no
  // reason. `done` already stopped the capture loop.
  releaseCamera();
  void awake.release();
  const blob = new Blob([bytes as BlobPart], {
    type: mime || "application/octet-stream",
  });
  download.href = URL.createObjectURL(blob);
  download.download = name;
  download.textContent = `Save ${name}`;
  download.hidden = false;
  hint.textContent = `Complete — ${name}, ${bytes.length.toLocaleString()} B, BLAKE3 verified`;
  barFill.style.width = "100%";
}

/**
 * Size the work canvas to the camera's real aspect, the first time the camera
 * admits to having one.
 *
 * Deliberately lazy. `videoWidth` is 0 until metadata lands, and on iOS that
 * can be *after* `play()` resolves — reading it too early gives 0, and a
 * guessed aspect ratio stretches every frame. A stretched pulse still shows
 * video and still finds nothing: the grid is no longer square, so sampling
 * lands between cells and every frame fails the CRC gate. That failure mode
 * looks exactly like "the camera doesn't work", which is why it is worth the
 * two extra lines to never guess.
 */
function sized(): boolean {
  if (work.width > 0) return true;
  if (!video.videoWidth || !video.videoHeight) return false;
  work.width = WORK_WIDTH;
  work.height = Math.round((WORK_WIDTH * video.videoHeight) / video.videoWidth);
  return true;
}

function capture(): void {
  if (done) return;
  // Counted even when dropped: this is the camera's rate, and a frame the
  // worker was too busy to take still arrived.
  captureRate.mark(performance.now());
  if (busy || !sized()) return;

  workCtx.drawImage(video, 0, 0, work.width, work.height);
  const frame = workCtx.getImageData(0, 0, work.width, work.height);
  busy = true;
  // Transferred, not copied: the worker borrows these bytes as RGBA directly,
  // and the next capture allocates a fresh buffer.
  post(
    { kind: "frame", buffer: frame.data.buffer, width: work.width, height: work.height },
    [frame.data.buffer],
  );
}

/**
 * Which camera session the capture loop belongs to.
 *
 * Every `pump` carries the generation it started in and stops the moment that
 * stops being current. Without it, a second successful `start()` — which the
 * retry path now makes reachable — leaves the first loop running against a
 * dead video element, and the two race for the `busy` flag: captures double,
 * decode rate halves, and the readouts blame the camera. decimen shipped this
 * bug and fixed it with the same counter (R7).
 */
let captureGen = 0;

function pump(gen: number): void {
  const step = () => {
    if (done || gen !== captureGen) return;
    capture();
    // `requestVideoFrameCallback` fires once per *decoded* video frame, which
    // is what we actually want to sample. Where it is missing (Safari before
    // 15.4, some others) the paint clock is a workable stand-in.
    if (typeof video.requestVideoFrameCallback === "function") {
      video.requestVideoFrameCallback(step);
    } else {
      requestAnimationFrame(step);
    }
  };
  step();
}

/**
 * Why there is no camera, when there is no camera.
 *
 * Overwhelmingly the answer is *not* permissions: `navigator.mediaDevices` is
 * not exposed at all outside a secure context, and while `localhost` counts as
 * one, the `http://192.168.x.x` a phone uses to reach a dev laptop does not.
 * So the laptop's own camera works and the phone's appears broken — with the
 * raw `TypeError` as the only clue. Say the real thing instead.
 */
function unavailable(): string | null {
  // Typed as always present; on http it genuinely is not there.
  const media = navigator.mediaDevices as MediaDevices | undefined;
  if (media?.getUserMedia) return null;
  if (!window.isSecureContext) {
    return `${location.protocol}//${location.host} is not a secure context, so the browser hides the camera. Serve this over https — in web/: npm run cert, then npm run dev.`;
  }
  return "This browser exposes no camera API.";
}

/**
 * Frame rate to ask the camera for.
 *
 * 30, not 60. The measured pulse-rate optimum is 20 Hz against a 30 fps camera,
 * and asking for more on a phone tends to buy a lower-resolution sensor mode
 * rather than more frames — the wrong trade when px/cell is the binding
 * constraint.
 */
const WANT_FPS = 30;

/** Let go of the camera. Called on completion and on every failed start. */
function releaseCamera(): void {
  const stream = video.srcObject as MediaStream | null;
  stream?.getTracks().forEach((track) => track.stop());
  video.srcObject = null;
}

/**
 * Put the page back where a second attempt can succeed.
 *
 * A start that fails halfway leaves a live `MediaStream` nobody is reading. On
 * a phone that is a camera the user cannot get back without reloading — and on
 * iOS a second `getUserMedia` while the first stream is open is one of the ways
 * `NotReadableError` happens. So retry always releases first.
 */
function offerRetry(message: string): void {
  releaseCamera();
  hint.textContent = message;
  begin.hidden = false;
  begin.disabled = false;
  begin.textContent = "Try again";
}

/**
 * Open the camera, `exact` frame rate first and `ideal` as the fallback.
 *
 * The two-step is decimen's field note (R6) and it is not defensive coding: iOS
 * accepts `frameRate: {ideal: 60}`, delivers 30, and reports success. An
 * `exact` constraint is the only way to find out whether the mode is real —
 * it *rejects* instead of quietly substituting, which is why the fallback then
 * has to exist for every camera that genuinely cannot hit the number.
 */
async function openCamera(): Promise<MediaStream> {
  const base: MediaTrackConstraints = {
    facingMode: { ideal: "environment" },
    // Matched to the decode width rather than maximised. Anything above
    // WORK_WIDTH is downscaled away, and asking for 1920 on a phone often
    // selects a mode that is slower without being sharper where it counts.
    width: { ideal: WORK_WIDTH },
    height: { ideal: Math.round((WORK_WIDTH * 9) / 16) },
  };
  try {
    return await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { ...base, frameRate: { exact: WANT_FPS } },
    });
  } catch {
    return await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { ...base, frameRate: { ideal: WANT_FPS } },
    });
  }
}

async function start(): Promise<void> {
  const blocked = unavailable();
  if (blocked) {
    offerRetry(blocked);
    return;
  }

  let stream: MediaStream;
  try {
    stream = await openCamera();
  } catch (error) {
    offerRetry(cameraError(error));
    return;
  }

  video.srcObject = stream;
  // iOS rejects `play()` in plenty of situations a desktop never hits, and an
  // unhandled rejection here leaves the page sitting on its opening hint
  // forever — the silent failure this whole function exists to avoid.
  try {
    await video.play();
  } catch (error) {
    offerRetry(`Camera opened but would not play: ${error}`);
    return;
  }
  begin.hidden = true;
  const track = stream.getVideoTracks()[0];
  await steady(track);
  // The camera is the only thing on this page that matters, and a display
  // timeout stops it dead with no message at all.
  void awake.acquire();

  // What the camera *granted*, not what we asked for. This is why the exact/
  // ideal dance above exists, and printing the gap is what makes it visible:
  // a 30 fps request answered with 15 means the sender's pulse rate is wrong
  // for this device, and nothing else on the page would ever say so.
  const settings = track.getSettings();
  const granted = Math.round(settings.frameRate ?? 0);
  const fps = granted ? `@${granted} fps${granted === WANT_FPS ? "" : ` (asked ${WANT_FPS})`}` : "";
  cameraMode.textContent =
    settings.width && settings.height
      ? `camera ${settings.width}×${settings.height}${fps} · decoding at ${WORK_WIDTH} px wide`
      : "camera — resolution unreported";
  baseCameraMode = cameraMode.textContent;

  captureGen += 1;
  pump(captureGen);
}

/**
 * Resolves when the worker has its WASM up.
 *
 * The camera used to start on this signal. It cannot: iOS wants a *user
 * gesture* behind `getUserMedia` and `play()`, and a page-load prompt is the
 * one most likely to be dismissed or ignored. So the tap starts the camera and
 * this only decides whether the tap has to wait.
 */
let workerReady!: () => void;
const ready = new Promise<void>((resolve) => {
  workerReady = resolve;
});

begin.addEventListener("click", () => {
  begin.disabled = true;
  begin.textContent = "Starting…";
  hint.textContent = "Opening the camera…";
  void ready.then(start).then(() => {
    // Still visible means start() bailed and wrote its reason into the hint.
    if (!begin.hidden) {
      begin.disabled = false;
      begin.textContent = "Try again";
    }
  });
});

worker.onmessage = (event: MessageEvent<FromWorker>) => {
  const message = event.data;
  switch (message.kind) {
    case "ready":
      workerReady();
      break;
    case "error":
      hint.textContent = message.message;
      break;
    case "status": {
      busy = false;
      last = message;
      const now = performance.now();
      decodeRate.mark(now);
      if (message.outcome === Outcome.Duplicate) dupFrames += 1;
      if (message.outcome === Outcome.Accepted || message.outcome === Outcome.Completed) {
        newFrames += 1;
        firstSymbolAt ??= now;
      }
      recent.push(message.outcome);
      if (recent.length > HINT_WINDOW) recent.shift();
      render();
      meter(now);
      break;
    }
    case "complete":
      finish(message.bytes, message.fileName, message.fileMime);
      break;
  }
};

post({ kind: "init", profile: PROFILE });

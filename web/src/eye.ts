// The eye: read QR frames off a camera and rebuild the file.
//
// Split across two threads. This file owns the camera, the capture loop and
// the feedback overlay; decoding — ZXing plus the WASM packet sink — lives in
// eye-worker.ts, so a slow frame can never stutter the video or the overlay.
// Frames cross as transferred buffers, and a frame captured while the worker
// is busy is simply dropped — the skin repeats everything anyway.

import { Outcome } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker, Transport } from "./protocol.js";
import { ScreenAwake, cameraError, probeCamera, tryConstraint } from "./platform.js";

/**
 * Working resolution for decoding.
 *
 * Not the camera's resolution — ZXing scans the whole frame, so this is the
 * single biggest lever on CPU cost. A v40 symbol is 185 modules across with
 * its quiet zone; at 1280 wide and ~70% frame fill that is ~4.8 px/module,
 * which ZXing's detector handles while keeping three-channel RGB decodes
 * cheap enough to run per frame. The camera is asked to *preserve* a
 * 1920-wide source so the downscale starts from real detail.
 */
const WORK_WIDTH = 1280;
const SOURCE_WIDTH = 1920;

/** Frames to look back over when deciding what to tell the human. */
const HINT_WINDOW = 30;

/** Seconds of history the frame-rate readouts average over. */
const RATE_WINDOW = 2;

const video = document.querySelector<HTMLVideoElement>("#camera")!;
const stage = document.querySelector<HTMLElement>(".camera-stage")!;
const lock = document.querySelector<SVGSVGElement>("#lock")!;
const lockQuad = lock.querySelector("polygon")!;
const begin = document.querySelector<HTMLButtonElement>("#begin")!;
const beginScreen = document.querySelector<HTMLButtonElement>("#begin-screen")!;
const stopCamera = document.querySelector<HTMLButtonElement>("#stop-camera")!;
const captureFps = document.querySelector<HTMLSelectElement>("#capture-fps")!;
const captureSetting = document.querySelector<HTMLLabelElement>("#capture-setting")!;
const transport = document.querySelector<HTMLSelectElement>("#transport")!;
const liveState = document.querySelector<HTMLSpanElement>(".live-state")!;
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

function receiverState(state: "ready" | "scanning" | "attention" | "complete"): void {
  liveState.className = `live-state ${state}`;
  liveState.innerHTML = `<i></i>${state === "attention" ? "Needs attention" : state[0].toUpperCase() + state.slice(1)}`;
}

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

function currentTransport(): Transport {
  return transport.value === "qr-rgb" ? "qr-rgb" : "qr";
}

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

function showCameraMode(): void {
  if (!baseCameraMode) return;
  cameraMode.textContent = `${baseCameraMode} · decoding at ${work.width || WORK_WIDTH}px wide`;
}

function meter(now: number): void {
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
  const { symbols, needed, rejected, unlocatable, fileName, expectedBytes } = last;
  // The manifest names the file long before the file arrives (§3c).
  const label = fileName
    ? `${fileName}${expectedBytes ? ` — ${expectedBytes.toLocaleString()} B` : ""} · `
    : "";
  progress.textContent = `${label}${symbols} / ${needed || "—"} symbols`;
  barFill.style.width = needed > 0 ? `${Math.min(100, (symbols / needed) * 100)}%` : "0%";
  counters.textContent = `${rejected} rejected · ${unlocatable} not found`;
  hint.textContent = advise();
}

/** Let the live quad fade and bring the static aim frame back. */
function lockDrop(): void {
  lock.classList.remove("live");
  stage.classList.remove("locked");
}

/**
 * Draw ZXing's corner quad over the live video.
 *
 * The corners arrive in captured-frame pixels; the video is displayed with
 * `object-fit: cover`, which scales the frame up to fill the stage and crops
 * the overflow symmetrically — so the mapping is one scale and one centring
 * offset per axis. The quad updates at the decode rate, not the display rate;
 * the CSS fade covers the frames in between.
 */
function trackSymbol(message: Extract<FromWorker, { kind: "status" }>): void {
  if (!message.quad || done) {
    lockDrop();
    return;
  }
  const { quad, frameWidth, frameHeight } = message;
  const width = video.clientWidth;
  const height = video.clientHeight;
  if (!width || !height || !frameWidth || !frameHeight) return;
  const scale = Math.max(width / frameWidth, height / frameHeight);
  const dx = (width - frameWidth * scale) / 2;
  const dy = (height - frameHeight * scale) / 2;
  lock.setAttribute("viewBox", `0 0 ${width} ${height}`);
  lockQuad.setAttribute(
    "points",
    quad
      .map((p) => `${(p.x * scale + dx).toFixed(1)},${(p.y * scale + dy).toFixed(1)}`)
      .join(" "),
  );
  // Green while packets land; amber when the symbol is seen but its payload
  // is not usable — the visual line between an aiming problem and a decode
  // problem.
  const landing =
    message.outcome === Outcome.Accepted ||
    message.outcome === Outcome.Completed ||
    message.outcome === Outcome.Duplicate;
  lock.classList.toggle("poor", !landing);
  lock.classList.add("live");
  stage.classList.add("locked");
}

function finish(bytes: Uint8Array, name: string, mime: string): void {
  done = true;
  lockDrop();
  receiverState("complete");
  // The file is here and verified; holding the camera and the wake lock past
  // that point drains the battery and leaves the indicator light on for no
  // reason. `done` already stopped the capture loop.
  releaseCamera();
  void awake.release();
  stopCamera.hidden = true;
  transport.disabled = false;
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
  if (!video.videoWidth || !video.videoHeight) return false;
  // Never manufacture detail the source did not grant. When the camera
  // supplied less than the working width, keep the honest smaller frame.
  const width = Math.min(WORK_WIDTH, video.videoWidth);
  const height = Math.round((width * video.videoHeight) / video.videoWidth);
  if (work.width === width && work.height === height) return true;
  work.width = width;
  work.height = height;
  showCameraMode();
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
 * Reliable defaults to 30: the measured pulse-rate optimum is 20–25 Hz against
 * a 30 fps camera, and asking for more on a phone can buy a lower-resolution
 * sensor mode rather than useful frames. A deliberate 60-fps option exists for
 * hardware that can prove it granted both rate and resolution; telemetry says
 * what actually happened.
 */
function wantedFps(): number {
  return Number(captureFps.value) === 60 ? 60 : 30;
}

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
  lockDrop();
  receiverState("attention");
  hint.textContent = message;
  captureSetting.hidden = false;
  begin.hidden = false;
  begin.disabled = false;
  begin.textContent = "Try again";
  beginScreen.hidden = !hasScreenCapture;
  beginScreen.disabled = false;
  stopCamera.hidden = true;
  transport.disabled = false;
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
  const fps = wantedFps();
  const base: MediaTrackConstraints = {
    facingMode: { ideal: "environment" },
    // Ask the camera to preserve real source detail; the work canvas
    // downscales from it rather than upscaling a soft mode.
    width: { ideal: SOURCE_WIDTH },
    height: { ideal: Math.round((SOURCE_WIDTH * 9) / 16) },
  };
  try {
    return await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { ...base, frameRate: { exact: fps } },
    });
  } catch {
    return await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { ...base, frameRate: { ideal: fps } },
    });
  }
}

/**
 * Capture a window or screen instead of a camera.
 *
 * This is how the browser half gets tested without a second device: put the
 * skin in its own window, share that window here, and every stage downstream is
 * the one that runs for real — rVFC pacing, the transferred-buffer hop to the
 * worker, locate, homography, sampling, RS, the CRC gate, the fountain, BLAKE3.
 *
 * What it deliberately does *not* test is the optics: no perspective, no
 * rolling-shutter tear, no glare, no lens blur, no auto-exposure fighting a
 * strobing panel. Those are exactly the things `cuttl-sim` models and the M1
 * observable exists to measure. A pass here means the software is right; it
 * says nothing about whether a camera can read the screen.
 */
async function openScreen(): Promise<MediaStream> {
  const fps = wantedFps();
  return await navigator.mediaDevices.getDisplayMedia({
    audio: false,
    video: { frameRate: { ideal: fps } },
  });
}

async function start(source: () => Promise<MediaStream> = openCamera): Promise<void> {
  const blocked = unavailable();
  if (blocked) {
    offerRetry(blocked);
    return;
  }

  let stream: MediaStream;
  try {
    stream = await source();
  } catch (error) {
    offerRetry(cameraError(error));
    return;
  }

  video.srcObject = stream;
  work.width = 0;
  work.height = 0;
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
  beginScreen.hidden = true;
  captureSetting.hidden = true;
  stopCamera.hidden = false;
  transport.disabled = true;
  receiverState("scanning");
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
  const requested = wantedFps();
  const fps = granted ? `@${granted} fps${granted === requested ? "" : ` (asked ${requested})`}` : "";
  // Say which source this is. A screen-capture run has no optics in it, and a
  // goodput number from one must never be quoted as if a camera produced it.
  const kind = source === openScreen ? "screen" : "camera";
  baseCameraMode =
    settings.width && settings.height
      ? `${kind} ${settings.width}×${settings.height}${fps}`
      : `${kind} — resolution unreported`;
  showCameraMode();

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

/** Present on desktop, absent on every iOS browser. */
const hasScreenCapture =
  typeof navigator.mediaDevices?.getDisplayMedia === "function";
beginScreen.hidden = !hasScreenCapture;

function wire(button: HTMLButtonElement, source: () => Promise<MediaStream>, opening: string) {
  button.addEventListener("click", () => {
    begin.disabled = true;
    beginScreen.disabled = true;
    button.textContent = "Starting…";
    hint.textContent = opening;
    void ready.then(() => start(source)).then(() => {
      // Still visible means start() bailed and wrote its reason into the hint.
      if (!begin.hidden) {
        begin.disabled = false;
        begin.textContent = "Try again";
      }
    });
  });
}

wire(begin, openCamera, "Opening the camera…");
wire(beginScreen, openScreen, "Pick the window showing the skin…");

/**
 * Stop the camera and put the page back to its opening state, in place.
 *
 * Not a navigation. A fountain either converges or it holds nothing, so an
 * abandoned transfer has no half-result worth keeping — the honest outcome of
 * stopping is a page identical to a fresh load, ready to point at the next
 * screen. The worker is re-inited so the abandoned run's partial symbols
 * cannot leak into it.
 */
function stopReceiving(): void {
  captureGen += 1;
  releaseCamera();
  void awake.release();
  lockDrop();
  done = false;
  busy = false;
  last = null;
  recent.length = 0;
  newFrames = 0;
  dupFrames = 0;
  firstSymbolAt = null;
  baseCameraMode = "";
  post({ kind: "init", transport: currentTransport() });
  receiverState("ready");
  hint.textContent = "Point this camera at the sending screen";
  progress.textContent = "Waiting for the first pulse";
  counters.textContent = "";
  barFill.style.width = "0%";
  download.hidden = true;
  stopCamera.hidden = true;
  captureSetting.hidden = false;
  begin.hidden = false;
  begin.disabled = false;
  begin.textContent = "Start camera";
  beginScreen.hidden = !hasScreenCapture;
  beginScreen.disabled = false;
  transport.disabled = false;
  cameraMode.textContent = "Camera not started";
  for (const cell of Object.values(tiles)) cell.textContent = "—";
  tiles.newdup.textContent = "0 / 0";
}

stopCamera.addEventListener("click", stopReceiving);

transport.addEventListener("change", () => {
  if (video.srcObject) return;
  last = null;
  recent.length = 0;
  newFrames = 0;
  dupFrames = 0;
  firstSymbolAt = null;
  progress.textContent = "Waiting for the first pulse";
  counters.textContent = "";
  barFill.style.width = "0%";
  post({ kind: "init", transport: currentTransport() });
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
      // A frame still in flight when the camera was stopped reports into a
      // page that has already been reset; drop it rather than repaint it.
      if (!video.srcObject) break;
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
      trackSymbol(message);
      render();
      meter(now);
      break;
    }
    case "complete":
      // Same guard: completion from a run the user already walked away from
      // must not resurrect its UI.
      if (video.srcObject) finish(message.bytes, message.fileName, message.fileMime);
      break;
  }
};

post({ kind: "init", transport: currentTransport() });

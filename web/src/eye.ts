// The eye: read QR frames off a camera and rebuild the file.
//
// Split across threads. This file owns the camera, the capture loop and the
// feedback overlay; ZXing detection runs in a small pool of zxing-worker.ts
// instances, and every payload they decode funnels into the one packet sink
// in eye-worker.ts — so a slow frame can never stutter the video or the
// overlay. Frames cross as transferred ImageBitmaps (or RGBA buffers where
// the platform insists), and a frame captured while every decoder is busy is
// simply dropped — the skin repeats everything anyway.

import { Outcome } from "../pkg/cuttl_wasm.js";
import type {
  FromDecoder,
  FromSink,
  QuadPoint,
  ToDecoder,
  ToSink,
  Transport,
} from "./protocol.js";
import { ScreenAwake, cameraError, probeCamera, tryConstraint } from "./platform.js";

/**
 * Working width for the full-frame *search* pass, and the ceiling for
 * region-of-interest crops.
 *
 * Not the camera's resolution — ZXing scans whatever it is handed, so this is
 * the single biggest lever on CPU cost. While the eye is still hunting, the
 * whole source frame is downscaled to this width. Once a symbol is located,
 * capture switches to cropping around it at *native* resolution (`grabRoi`):
 * a v40 symbol filling 60% of a 1920-wide source jumps from ~4 px/module to
 * ~6, exactly where its densest modules need the help — and the crop is
 * smaller than a full frame, so the sharper pass is also the cheaper one.
 * The camera is asked to *preserve* a 1920-wide source so both paths start
 * from real detail.
 */
const WORK_WIDTH = 1280;
const SOURCE_WIDTH = 1920;

/** How long a located quad keeps steering region-of-interest crops. */
const ROI_TTL_MS = 1200;
/** Margin around the located symbol, as a fraction of its larger side —
 * covers the quiet zone plus a hand-held frame's worth of drift. */
const ROI_PAD = 0.35;

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

/** Crop canvas for the region-of-interest path; grows, never shrinks. */
const roi = document.createElement("canvas");
const roiCtx = roi.getContext("2d", { willReadFrequently: true })!;

/**
 * How many ZXing workers run side by side.
 *
 * Decode throughput, not camera rate, is what caps goodput — an RGB frame
 * costs three ZXing passes, and with a single worker every frame captured
 * while it chewed was shed. Frames are independent (a fountain has no
 * ordering), so they pipeline across a small pool instead; the cap leaves
 * cores for the camera pipeline, the sink and the page itself.
 */
const DECODER_POOL = Math.min(4, Math.max(1, (navigator.hardwareConcurrency || 4) - 2));

interface Decoder {
  worker: Worker;
  /** Free for a frame — false from dispatch until its `decoded` comes back. */
  idle: boolean;
}

const decoders: Decoder[] = Array.from({ length: DECODER_POOL }, () => ({
  worker: new Worker(new URL("./zxing-worker.ts", import.meta.url), { type: "module" }),
  idle: false,
}));
const decoderPost = (decoder: Decoder, message: ToDecoder, transfer: Transferable[] = []) =>
  decoder.worker.postMessage(message, transfer);

/** The one worker owning stream state; every decoded payload funnels here. */
const sink = new Worker(new URL("./eye-worker.ts", import.meta.url), {
  type: "module",
});
const sinkPost = (message: ToSink) => sink.postMessage(message);

const recent: Outcome[] = [];
let last: Extract<FromSink, { kind: "status" }> | null = null;
let done = false;
/** Where a symbol was last seen, in source pixels — steers the next crops. */
let roiQuad: QuadPoint[] | null = null;
let roiSeenAt = 0;

function currentTransport(): Transport {
  return transport.value === "qr-rgb" ? "qr-rgb" : "qr";
}

/**
 * Rolling event rate over the last [`RATE_WINDOW`] seconds.
 *
 * Two of these run: one on captures, one on decodes. The *gap* between them is
 * the load the pool shed — frames dropped because no decoder was free are
 * invisible everywhere else, and "capture 40, decode 12" is the difference
 * between a camera problem and a CPU problem.
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
  const workers = `${DECODER_POOL} decode worker${DECODER_POOL === 1 ? "" : "s"}`;
  const path = bitmapCapture ? "bitmap capture" : "canvas capture";
  cameraMode.textContent = `${baseCameraMode} · search at ${work.width || WORK_WIDTH}px wide, crops at source · ${workers} · ${path}`;
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
function trackSymbol(message: Extract<FromSink, { kind: "status" }>): void {
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

/** A capture region in source pixels, its output size, and the mapping. */
interface Region {
  left: number;
  top: number;
  width: number;
  height: number;
  outWidth: number;
  outHeight: number;
  scale: number;
}

/** The whole source frame, downscaled to the search width. */
function wholeRegion(): Region {
  return {
    left: 0,
    top: 0,
    width: video.videoWidth,
    height: video.videoHeight,
    outWidth: work.width,
    outHeight: work.height,
    scale: video.videoWidth / work.width,
  };
}

/**
 * The crop around the last located symbol, at native source resolution.
 *
 * The search pass hands ZXing a downscale because it has to look everywhere;
 * once the symbol has been *found*, looking everywhere is waste. Cropping
 * the source instead of downscaling it cuts the pixels ZXing scans and
 * raises px/module by the whole downscale factor — the dense rungs live on
 * that margin. The quad steers for at most `ROI_TTL_MS` after it was last
 * seen, so a lost lock falls back to searching within a couple of frames.
 */
function roiRegion(now: number): Region | null {
  if (!roiQuad || now - roiSeenAt > ROI_TTL_MS) return null;
  const xs = roiQuad.map((p) => p.x);
  const ys = roiQuad.map((p) => p.y);
  const side = Math.max(Math.max(...xs) - Math.min(...xs), Math.max(...ys) - Math.min(...ys));
  const pad = side * ROI_PAD;
  const left = Math.max(0, Math.floor(Math.min(...xs) - pad));
  const top = Math.max(0, Math.floor(Math.min(...ys) - pad));
  const width = Math.min(video.videoWidth, Math.ceil(Math.max(...xs) + pad)) - left;
  const height = Math.min(video.videoHeight, Math.ceil(Math.max(...ys) + pad)) - top;
  if (width < 32 || height < 32) return null;
  // Native resolution unless the crop out-sizes the search pass itself.
  const scale = Math.max(1, width / WORK_WIDTH, height / WORK_WIDTH);
  return {
    left,
    top,
    width,
    height,
    outWidth: Math.round(width / scale),
    outHeight: Math.round(height / scale),
    scale,
  };
}

/**
 * Whether frames are captured as transferred ImageBitmaps instead of RGBA
 * buffers. The bitmap path keeps crop and scale on the GPU and moves the
 * pixel readback into the decode worker, so the main thread never blocks on
 * `getImageData`. Gated on OffscreenCanvas — the worker needs one for the
 * readback — and demoted permanently the first time the engine refuses a
 * video source, which older Safari has history of doing.
 */
let bitmapCapture =
  typeof createImageBitmap === "function" && typeof OffscreenCanvas === "function";

/** Canvas fallback: draw and read the region back on the main thread. */
function grabPixels(region: Region): ImageData {
  const crop = region.outWidth !== work.width || region.outHeight !== work.height;
  const canvas = crop ? roi : work;
  const ctx = crop ? roiCtx : workCtx;
  if (crop) {
    // Grow-only: a canvas resize is an allocation, and the crop size jitters
    // with the hand holding the phone. Stale pixels beyond the crop are
    // never read — getImageData takes exactly the region just drawn.
    if (canvas.width < region.outWidth) canvas.width = region.outWidth;
    if (canvas.height < region.outHeight) canvas.height = region.outHeight;
  }
  ctx.drawImage(
    video,
    region.left,
    region.top,
    region.width,
    region.height,
    0,
    0,
    region.outWidth,
    region.outHeight,
  );
  return ctx.getImageData(0, 0, region.outWidth, region.outHeight);
}

function captureBitmap(free: Decoder, region: Region): void {
  free.idle = false;
  const gen = captureGen;
  createImageBitmap(video, region.left, region.top, region.width, region.height, {
    resizeWidth: region.outWidth,
    resizeHeight: region.outHeight,
  }).then(
    (bitmap) => {
      // The camera may have stopped while the bitmap was being made.
      if (done || gen !== captureGen || !video.srcObject) {
        bitmap.close();
        free.idle = true;
        return;
      }
      decoderPost(
        free,
        {
          kind: "frame",
          bitmap,
          width: region.outWidth,
          height: region.outHeight,
          originX: region.left,
          originY: region.top,
          scale: region.scale,
          sourceWidth: video.videoWidth,
          sourceHeight: video.videoHeight,
        },
        [bitmap],
      );
    },
    () => {
      bitmapCapture = false;
      free.idle = true;
      showCameraMode();
    },
  );
}

function capture(): void {
  if (done) return;
  const now = performance.now();
  // Counted even when dropped: this is the camera's rate, and a frame no
  // decoder was free to take still arrived.
  captureRate.mark(now);
  const free = decoders.find((decoder) => decoder.idle);
  if (!free || !sized()) return;

  const region = roiRegion(now) ?? wholeRegion();
  if (bitmapCapture) {
    captureBitmap(free, region);
    return;
  }
  const frame = grabPixels(region);
  free.idle = false;
  // Transferred, not copied: the decoder borrows these bytes as RGBA
  // directly, and the next capture allocates a fresh buffer.
  decoderPost(
    free,
    {
      kind: "frame",
      buffer: frame.data.buffer,
      width: frame.width,
      height: frame.height,
      originX: region.left,
      originY: region.top,
      scale: region.scale,
      sourceWidth: video.videoWidth,
      sourceHeight: video.videoHeight,
    },
    [frame.data.buffer],
  );
}

/**
 * Which camera session the capture loop belongs to.
 *
 * Every `pump` carries the generation it started in and stops the moment that
 * stops being current. Without it, a second successful `start()` — which the
 * retry path now makes reachable — leaves the first loop running against a
 * dead video element, and the two race for the decoder pool: captures double,
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
 * decoder pool, ZXing detection, channel separation in RGB mode, the CRC
 * gate, the fountain, BLAKE3.
 *
 * What it deliberately does *not* test is the optics: no perspective, no
 * rolling-shutter tear, no glare, no lens blur, no auto-exposure fighting a
 * strobing panel — and no colour crosstalk between a screen's subpixels and
 * a camera's Bayer filter, the open question over the RGB mode. A pass here
 * means the software is right; it says nothing about whether a camera can
 * read the screen.
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
 * Resolves when every worker has its WASM up.
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
/** Counts first-boot `ready` replies: every decoder plus the sink. */
let waitingReady = decoders.length + 1;
const markReady = () => {
  waitingReady -= 1;
  if (waitingReady === 0) workerReady();
};

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
  last = null;
  roiQuad = null;
  roiSeenAt = 0;
  recent.length = 0;
  newFrames = 0;
  dupFrames = 0;
  firstSymbolAt = null;
  baseCameraMode = "";
  // A fresh stream state. The decoders hold no state beyond the transport,
  // which cannot have changed while the camera held the select disabled.
  sinkPost({ kind: "init" });
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
  roiQuad = null;
  roiSeenAt = 0;
  recent.length = 0;
  newFrames = 0;
  dupFrames = 0;
  firstSymbolAt = null;
  progress.textContent = "Waiting for the first pulse";
  counters.textContent = "";
  barFill.style.width = "0%";
  for (const decoder of decoders) {
    decoderPost(decoder, { kind: "init", transport: currentTransport() });
  }
  sinkPost({ kind: "init" });
});

for (const decoder of decoders) {
  decoder.worker.onmessage = (event: MessageEvent<FromDecoder>) => {
    const message = event.data;
    switch (message.kind) {
      case "ready":
        decoder.idle = true;
        markReady();
        break;
      case "error":
        // Recover the slot — a decoder that failed one frame takes the next.
        decoder.idle = true;
        hint.textContent = message.message;
        break;
      case "decoded":
        decoder.idle = true;
        // A result still in flight when the camera was stopped would report
        // into a stream state that has already been reset; drop it here.
        if (done || !video.srcObject) break;
        sinkPost({
          kind: "ingest",
          payloads: message.payloads,
          quad: message.quad,
          frameWidth: message.frameWidth,
          frameHeight: message.frameHeight,
        });
        break;
    }
  };
}

sink.onmessage = (event: MessageEvent<FromSink>) => {
  const message = event.data;
  switch (message.kind) {
    case "ready":
      markReady();
      break;
    case "error":
      hint.textContent = message.message;
      break;
    case "status": {
      // A frame still in flight when the camera was stopped reports into a
      // page that has already been reset; drop it rather than repaint it.
      if (!video.srcObject) break;
      last = message;
      const now = performance.now();
      decodeRate.mark(now);
      // Wherever the symbol was — even unreadable — is where to crop next.
      if (message.quad) {
        roiQuad = message.quad;
        roiSeenAt = now;
      }
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

for (const decoder of decoders) {
  decoderPost(decoder, { kind: "init", transport: currentTransport() });
}
sinkPost({ kind: "init" });

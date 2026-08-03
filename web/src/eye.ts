// The eye: read pulses off a camera and rebuild the file (DESIGN.md §3e).
//
// Split across two threads. This file owns the camera, the capture loop and
// the feedback overlay; decoding — the same WASM the simulator runs — lives in
// eye-worker.ts (§3e: "run it in a worker so the UI thread stays free").
// Frames cross as transferred buffers, and a frame captured while the worker
// is busy is simply dropped — the skin repeats everything anyway.

import { Outcome } from "../pkg/cuttl_wasm.js";
import type { FromWorker, ToWorker } from "./protocol.js";

const PROFILE = "m1";

/**
 * Working resolution for decoding.
 *
 * Not the camera's resolution: 64 columns need only ~4 px/cell to be
 * resolvable (§2), so 960 px across is already 15 px/cell — generous. Decoding
 * scans every row and column, so halving the working size quarters that cost
 * for no loss in what can actually be read.
 */
const WORK_WIDTH = 960;

/** Frames to look back over when deciding what to tell the human. */
const HINT_WINDOW = 30;

const video = document.querySelector<HTMLVideoElement>("#camera")!;
const hint = document.querySelector<HTMLParagraphElement>("#hint")!;
const progress = document.querySelector<HTMLParagraphElement>("#progress")!;
const counters = document.querySelector<HTMLParagraphElement>("#counters")!;
const barFill = document.querySelector<HTMLDivElement>("#bar-fill")!;
const download = document.querySelector<HTMLAnchorElement>("#download")!;

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
 * Ask the camera to stop helping.
 *
 * Autofocus hunting and auto-exposure both fight a strobing screen. Support for
 * these constraints is patchy and iOS Safari grants almost none of them (§2), so
 * every one is attempted separately and failure is ignored — a camera that
 * refuses still works, just less well.
 */
async function steady(track: MediaStreamTrack): Promise<void> {
  const capabilities = track.getCapabilities?.() as Record<string, unknown> | undefined;
  if (!capabilities) return;

  const wanted: Record<string, unknown>[] = [];
  const supports = (key: string, value: string) =>
    Array.isArray(capabilities[key]) && (capabilities[key] as string[]).includes(value);

  if (supports("focusMode", "continuous")) wanted.push({ focusMode: "continuous" });
  if (supports("whiteBalanceMode", "manual")) wanted.push({ whiteBalanceMode: "manual" });
  if (supports("exposureMode", "manual")) wanted.push({ exposureMode: "manual" });

  for (const constraint of wanted) {
    await track.applyConstraints({ advanced: [constraint] }).catch(() => {});
  }
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

function capture(): void {
  if (busy || done) return;

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

function pump(): void {
  const step = () => {
    capture();
    if (done) return;
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

async function start(): Promise<void> {
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: { ideal: "environment" }, width: { ideal: 1920 } },
    });
  } catch (error) {
    hint.textContent = `No camera: ${error}`;
    return;
  }

  video.srcObject = stream;
  await video.play();
  await steady(stream.getVideoTracks()[0]);

  const aspect = video.videoHeight / video.videoWidth || 9 / 16;
  work.width = WORK_WIDTH;
  work.height = Math.round(WORK_WIDTH * aspect);

  pump();
}

worker.onmessage = (event: MessageEvent<FromWorker>) => {
  const message = event.data;
  switch (message.kind) {
    case "ready":
      void start();
      break;
    case "error":
      hint.textContent = message.message;
      break;
    case "status":
      busy = false;
      last = message;
      recent.push(message.outcome);
      if (recent.length > HINT_WINDOW) recent.shift();
      render();
      break;
    case "complete":
      finish(message.bytes, message.fileName, message.fileMime);
      break;
  }
};

post({ kind: "init", profile: PROFILE });

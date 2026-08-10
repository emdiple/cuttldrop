// The skin: paint a file as a looping sequence of pulses (DESIGN.md §3d).
//
// Everything that decides *what* to paint is in WASM. This file owns only the
// things a browser does better: reading a file, sizing a canvas, and pacing.

import init, { ReferenceSkin } from "../pkg/cuttl_wasm.js";
import { PulsePacer, fitPhysicalScale } from "./optical-display.js";
import { ScreenAwake } from "./platform.js";
import {
  RGB_CHANNELS,
  rasterizeReferencePacket,
  rasterizeRgbReferencePackets,
  referenceProfile,
} from "./qr-reference.js";

/// Repair symbols per source symbol. The loop is longer, so a receiver that
/// missed a frame waits for a *different* one rather than the same one again.
const OVERHEAD = 2.0;

const file = document.querySelector<HTMLInputElement>("#file")!;
const dropZone = document.querySelector<HTMLLabelElement>("#drop-zone")!;
const detail = document.querySelector<HTMLParagraphElement>("#detail")!;
const start = document.querySelector<HTMLButtonElement>("#start")!;
const rate = document.querySelector<HTMLInputElement>("#rate")!;
const profile = document.querySelector<HTMLSelectElement>("#profile")!;
const rateValue = document.querySelector<HTMLOutputElement>("#rate-value")!;
const rateHint = document.querySelector<HTMLSpanElement>("#rate-hint")!;
const stage = document.querySelector<HTMLDivElement>("#stage")!;
const display = document.querySelector<HTMLCanvasElement>("#pulse")!;
const status = document.querySelector<HTMLDivElement>("#status")!;
const statusText = document.querySelector<HTMLSpanElement>("#status-text")!;
const stopAction = document.querySelector<HTMLButtonElement>("#stop")!;
const size = document.querySelector<HTMLInputElement>("#size")!;
const sizeValue = document.querySelector<HTMLOutputElement>("#size-value")!;

let skin: ReferenceSkin | null = null;
let selectedFile: File | null = null;
let prepareGen = 0;
/**
 * Which send the pacer loop belongs to. Stopping increments it, so the rAF
 * chain dies on its next callback — a plain `sending()` check would race a
 * stop-then-restart into two chains pacing one canvas.
 */
let sendGen = 0;
const LOOKAHEAD = 3;
let current: ImageData | null = null;
let queue: ImageData[] = [];
let nextIndex = 0;
/** True until the size slider is deliberately moved; see `resize`. */
let pinnedToMax = true;

/**
 * On this side the screen *is* the transmitter, so a display timeout does not
 * merely inconvenience the user — it stops the send, silently, with the page
 * still apparently running. The eye just sees the stream stop.
 */
const awake = new ScreenAwake();

/** Sender-rate defaults by profile. Denser rungs paint bigger symbols and
 * cost the eye more decode time per frame; RGB rungs cost three ZXing passes,
 * so their defaults sit lower still — the 3× per-frame payload keeps goodput
 * ahead. The user can override this immediately after choosing a profile —
 * the human remains the back channel. */
const PROFILE_RATE: Record<string, number> = {
  qr27: 24,
  qr35: 20,
  qr40: 15,
  rgb27: 15,
  rgb35: 12,
  rgb40: 10,
  // The hardened rungs paint the same geometry as their L counterpart, so
  // the eye's cost per frame is identical and the defaults carry over.
  qr27m: 24,
  qr35m: 20,
  qr40m: 15,
  rgb27m: 15,
  rgb35m: 12,
  rgb40m: 10,
};

/**
 * How a profile menu value maps onto the transport.
 *
 * The WASM `ReferenceSkin` only knows density rungs (`qr27`…): whether the
 * packets travel one per black-and-white frame or three per RGB frame is
 * purely a rasterization decision, so it lives here, not in Rust.
 */
function referenceChoice(value: string): { rung: string; channels: number } {
  if (value.startsWith("rgb")) return { rung: `qr${value.slice(3)}`, channels: RGB_CHANNELS };
  return { rung: value, channels: 1 };
}

/** Channels per reference frame for the prepared stream; 1 outside RGB mode. */
let referenceChannels = 1;

/** One physical pixel per raster cell; the visible canvas is an integer-scaled blit. */
const raster = document.createElement("canvas");
const rasterCtx = raster.getContext("2d", { willReadFrequently: false })!;
const displayCtx = display.getContext("2d")!;

/**
 * True when the layout has room for a resident side panel.
 *
 * Matches the `62rem` breakpoint in style.css. The script has to know because
 * the *behaviour* differs, not merely the arrangement: at this width the
 * controls stay up, so there is nothing to summon and nothing to retire.
 */
const wide = window.matchMedia("(min-width: 62rem)");

/** Whether a send is in progress; also the CSS hook for the sending layout. */
function sending(): boolean {
  return document.body.classList.contains("sending");
}

/**
 * Room the floating controls take at the bottom, measured rather than assumed.
 *
 * A constant here was wrong in both directions: the controls grow with the OS
 * text size and with the range control's native height, and they sit above the
 * home-indicator inset on an iPhone. Measuring from their own top edge folds
 * height, offset and safe-area inset into one number that cannot drift from
 * what is on screen.
 *
 * Zero once they are docked in the panel — then they are in flow, the grid has
 * already accounted for them, and subtracting again would double-count.
 */
function overlayRoom(): number {
  if (status.hidden || getComputedStyle(status).position !== "fixed") return 0;
  const bottom = window.visualViewport?.height ?? window.innerHeight;
  return Math.max(0, Math.ceil(bottom - status.getBoundingClientRect().top));
}

/** Display dimensions: QR modules plus the four-module quiet zone. */
function rasterSize(): { cols: number; rows: number } {
  if (!skin) return { cols: 1, rows: 1 };
  const { size } = referenceProfile(skin.profile);
  return { cols: size, rows: size };
}

function frameCount(): number {
  return skin?.packetCount ?? 0;
}

/**
 * Size the canvas to an *integer physical-pixel* multiple of the raster.
 *
 * This matters more than it looks. At a fractional scale, nearest-neighbour
 * upscaling gives some cells one more physical pixel than others, so the eye's run-length
 * ratios stop being clean 1:1:3:1:1 and finder detection gets harder for no
 * reason. An integer scale makes every cell identical.
 *
 * It is also why the size control counts *pixels per cell* rather than a
 * percentage: a percentage slider would offer positions that round to the same
 * scale, so most of its travel would do nothing visible. Here every notch is a
 * different physical raster, and the readout states both physical and CSS
 * pixels — those differ on HiDPI screens. The quiet zone participates in the
 * fit, so it can never push the pulse under the controls or outside the stage.
 */
function resize(): void {
  if (!skin) return;
  // Publish the measured reserve so the CSS that *centres* the pulse and the
  // arithmetic that *sizes* it agree by construction. The stylesheet's value is
  // only ever the starting guess, used for the frame before the first measure.
  document.body.style.setProperty("--overlay-room", `${overlayRoom()}px`);
  const { cols, rows } = rasterSize();
  const requested = pinnedToMax ? undefined : Number(size.value);
  const fit = fitPhysicalScale(
    cols,
    rows,
    Math.max(1, stage.clientWidth),
    Math.max(1, stage.clientHeight),
    window.devicePixelRatio,
    requested,
  );
  size.max = String(fit.maxScale);
  // Until the slider is touched, track the largest that fits. Otherwise hiding
  // the controls would free up room the pulse never reclaims — the default has
  // to follow the space available, and only a deliberate choice should pin it.
  // Keep the chosen scale when it still fits, clamp it when the window shrinks.
  size.value = String(fit.scale);
  size.disabled = fit.maxScale <= 1;
  const cssPerCell = fit.scale / (window.devicePixelRatio || 1);
  sizeValue.value =
    `${fit.scale} physical / ${cssPerCell.toFixed(2)} CSS px/cell · ` +
    `${fit.backingWidth}×${fit.backingHeight}`;
  display.width = fit.backingWidth;
  display.height = fit.backingHeight;
  display.style.width = `${fit.cssWidth}px`;
  display.style.height = `${fit.cssHeight}px`;
  // Set after every resize: the context resets its state when the canvas is
  // resized, and smoothing back on would blur every cell edge.
  displayCtx.imageSmoothingEnabled = false;
}

function makeFrame(): ImageData {
  if (!skin) throw new Error("no prepared stream");
  const reference = skin;
  const take = () => {
    const packet = reference.packet(nextIndex);
    nextIndex = (nextIndex + 1) % reference.packetCount;
    return packet;
  };
  if (referenceChannels === RGB_CHANNELS) {
    // Three consecutive packets share one frame. When the loop length is
    // not a multiple of three, the wrap rotates which packets travel
    // together — a receiver that lost a frame gets those packets back in
    // different company next loop, which suits a fountain fine.
    const packets = Array.from({ length: RGB_CHANNELS }, take);
    const qr = rasterizeRgbReferencePackets(packets, reference.profile);
    return new ImageData(qr.rgba, qr.width, qr.height);
  }
  const qr = rasterizeReferencePacket(take(), reference.profile);
  return new ImageData(qr.rgba, qr.width, qr.height);
}

/** Keep only a few pulses ahead, like Decimen's sender. Preparing the RaptorQ
 * state remains one-time work; raster/FEC generation is amortised one pulse per
 * display tick instead of materialising the complete loop at file selection. */
function pump(max = LOOKAHEAD): void {
  if (!skin) return;
  for (let made = 0; made < max && queue.length < LOOKAHEAD; made += 1) {
    queue.push(makeFrame());
  }
}

function paint(): void {
  if (!skin) return;
  current ??= queue.shift() ?? makeFrame();
  pump(1);
  rasterCtx.putImageData(current, 0, 0);
  displayCtx.imageSmoothingEnabled = false;
  displayCtx.drawImage(raster, 0, 0, display.width, display.height);
}

function advance(): void {
  if (!skin) return;
  current = queue.shift() ?? makeFrame();
  pump(1);
  paint();
}

function rewind(): void {
  current = null;
  queue = [];
  nextIndex = 0;
  pump();
}

/**
 * Pace pulses from elapsed time, with requestAnimationFrame as the commit
 * boundary.
 *
 * The old loop divided a hard-coded 60 Hz by the requested pulse rate. That
 * made a 120 Hz phone transmit twice as fast as its label claimed and made
 * several useful rates impossible on a 60 Hz panel (`25` rounded to two
 * refreshes and therefore became 30). Both errors create rolling-shutter tear,
 * which looks like poor optical throughput even though more pulses are being
 * painted.
 *
 * Time decides when a pulse is due; rAF still decides when it can actually be
 * committed. PulsePacer additionally requires two refresh callbacks between
 * changes, so a requested 60 Hz becomes at most 30 pulses/s on a 60 Hz panel
 * but remains 60 on a 120 Hz panel. If the tab falls behind, missed deadlines
 * are skipped rather than burst onto a panel that never displayed them.
 */
function loop(): void {
  const gen = ++sendGen;
  const pacer = new PulsePacer(performance.now(), Number(rate.value));
  const step = (now: number) => {
    if (!skin || gen !== sendGen) return;
    requestAnimationFrame(step);
    if (pacer.tick(now, Number(rate.value))) advance();
  };
  requestAnimationFrame(step);
}

rate.addEventListener("input", () => {
  rateValue.value = `${rate.value} Hz target`;
  rateHint.textContent =
    Number(rate.value) > 30
      ? "Experimental: each pulse still stays for two display refreshes, so this target needs a 120 Hz panel. If the eye reports tearing or decode fps falls behind, slow it down."
      : "No back channel exists, so nothing adapts this for you. Each pulse stays for at least two display refreshes; if the eye reports tearing, slow it down.";
  label();
});

// Resize repaints the same pulse, so dragging the slider never
// costs the eye a frame of the loop.
size.addEventListener("input", () => {
  pinnedToMax = false;
  resize();
  paint();
});

// Changing density re-encodes: the grid decides how much fits in a pulse, so
// there is nothing to reuse. Cheap enough to do on every change.
profile.addEventListener("change", () => {
  rate.value = String(PROFILE_RATE[profile.value] ?? 20);
  rateValue.value = `${rate.value} Hz target`;
  if (selectedFile) void prepare(selectedFile);
});

async function prepare(chosen: File): Promise<void> {
  const gen = ++prepareGen;
  selectedFile = chosen;
  detail.textContent = `Preparing ${chosen.name}…`;
  start.disabled = true;

  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await chosen.arrayBuffer());
  } catch (error) {
    if (gen !== prepareGen) return;
    detail.textContent = `Could not read ${chosen.name}: ${error}`;
    return;
  }
  if (gen !== prepareGen) return;
  const streamId = (Math.random() * 0xffffffff) >>> 0;
  try {
    // Name and mime ride in the manifest, so the eye can display and save the
    // file as itself rather than as received.bin (§3c).
    const reference = referenceChoice(profile.value);
    skin = new ReferenceSkin(bytes, chosen.name, chosen.type, reference.rung, streamId, OVERHEAD);
    referenceChannels = reference.channels;
  } catch (error) {
    if (gen !== prepareGen) return;
    detail.textContent = `Could not encode: ${error}`;
    return;
  }
  if (gen !== prepareGen) return;

  const framed = rasterSize();
  raster.width = framed.cols;
  raster.height = framed.rows;
  // A re-encode is a different grid and a different loop length. If one is
  // already on screen, refit it: the display canvas is still sized for the old
  // profile, and blitting the new grid into it would stretch every cell.
  rewind();
  if (sending()) {
    label();
    refit();
  }
  detail.textContent =
    `${chosen.name} — ${bytes.length.toLocaleString()} B, ` +
    `${skin.packetCount} QR packets at version ` +
    `${skin.qrVersion}-${referenceProfile(skin.profile).eccLevel}` +
    (referenceChannels === RGB_CHANNELS ? " · 3 per frame across R/G/B" : "");
  // A short loop is the one thing that can starve a transfer outright: the
  // fountain has too few distinct symbols to route around a bad frame. The
  // skin repeats forever so it recovers, but slowly — worth saying.
  if (frameCount() < 32) {
    detail.textContent += " · short loop for this density, expect repeats";
  }
  start.disabled = false;
}

file.addEventListener("change", () => {
  const chosen = file.files?.[0];
  if (chosen) void prepare(chosen);
});

for (const eventName of ["dragenter", "dragover"]) {
  dropZone.addEventListener(eventName, (event) => {
    event.preventDefault();
    dropZone.classList.add("dragging");
  });
}

for (const eventName of ["dragleave", "drop"]) {
  dropZone.addEventListener(eventName, (event) => {
    event.preventDefault();
    dropZone.classList.remove("dragging");
  });
}

dropZone.addEventListener("drop", (event) => {
  const chosen = event.dataTransfer?.files[0];
  if (chosen) void prepare(chosen);
});

start.addEventListener("click", () => {
  if (!skin) return;
  // On a laptop the panel stays, so Start remains reachable during a send —
  // and a second loop() would run two rAF chains against one canvas, doubling
  // the pulse rate the eye sees while the slider still claims the old one.
  if (sending()) {
    rewind();
    paint();
    return;
  }
  document.body.classList.add("sending");
  display.hidden = false;
  start.textContent = "Sending…";
  rewind();
  resize();
  paint();
  loop();
  void awake.acquire();
  applyMode();
});

function refit(): void {
  resize();
  paint();
}

/**
 * Stop the send and put the page back to its pre-send state, in place.
 *
 * Not a navigation — leaving the page lives on the brand link. The file stays
 * selected and the stream stays prepared, so the next send is one tap away;
 * the eye side loses nothing either way, since an interrupted fountain simply
 * resumes converging when the pulses return.
 */
function stopSending(): void {
  sendGen += 1;
  window.clearTimeout(dismiss);
  document.body.classList.remove("sending");
  status.hidden = true;
  display.hidden = true;
  void awake.release();
  document.body.style.removeProperty("--overlay-room");
  start.textContent = "Start transmission";
  start.disabled = !skin;
  rewind();
}

stopAction.addEventListener("click", stopSending);

/* ---------- controls: resident when wide, summoned when narrow ---------- */

/**
 * How long the controls stay up on a narrow screen. Longer the first time,
 * because that showing is the only thing that teaches they exist.
 */
const IDLE_MS = 4000;
const FIRST_MS = 9000;

let dismiss = 0;
let taught = false;

function label(): void {
  if (!skin) return;
  statusText.textContent =
    taught || wide.matches
      ? `${rate.value} Hz target · ${frameCount()} QR packets`
      : `${rate.value} Hz target · tap the pulse for these controls`;
}

function retire(): void {
  window.clearTimeout(dismiss);
  taught = true;
  status.hidden = true;
  refit();
}

/**
 * Show the controls and, on a narrow screen, start their timer.
 *
 * Summoned rather than resident *only* when narrow, and the reason is optical
 * rather than aesthetic: there the controls sit inside the pulse's own area and
 * therefore inside the receiving camera's frame, where a lit pill competes with
 * the pulse for auto-exposure. A laptop has room to put them beside the pulse
 * instead, so they stay. Either way they *shrink* the pulse to fit rather than
 * cover it — the bottom rows are the second beacon strip, and occluding those
 * turns detected tears back into silent CRC failures.
 */
function summon(hold = IDLE_MS): void {
  if (!skin || !sending()) return;
  status.hidden = false;
  label();
  refit();
  window.clearTimeout(dismiss);
  if (wide.matches) return;
  dismiss = window.setTimeout(retire, hold);
}

/** Put the controls into whichever state the current width calls for. */
function applyMode(): void {
  if (!sending()) return;
  if (wide.matches) {
    window.clearTimeout(dismiss);
    status.hidden = false;
    label();
    refit();
  } else {
    // Crossing down into the narrow layout is the first showing all over again:
    // the controls are about to start hiding themselves, which needs teaching.
    summon(FIRST_MS);
  }
}

// Tapping the pulse toggles; tapping the controls only restarts their timer, so
// an adjustment is never interrupted halfway through. Neither applies wide,
// where the controls never leave.
display.addEventListener("pointerdown", () => {
  if (wide.matches || !sending()) return;
  if (status.hidden) summon();
  else retire();
});

status.addEventListener("pointerdown", () => summon());
size.addEventListener("input", () => summon());
rate.addEventListener("input", () => summon());

wide.addEventListener("change", applyMode);
window.addEventListener("resize", refit);
window.addEventListener("orientationchange", refit);
// The URL bar sliding in and out changes the visible height without firing a
// window resize on iOS; without this the pulse keeps the size it had when the
// bar was hidden and runs under the overlay.
window.visualViewport?.addEventListener("resize", refit);

/*
 * Refit whenever the stage changes shape or the controls change height.
 *
 * Guarded on a signature rather than firing on every callback, because `resize`
 * is itself upstream of both: it publishes `--overlay-room`, which pads the body,
 * which resizes the stage. The guard is what makes that settle after one pass
 * instead of ringing. Width is deliberately in the signature for the stage and
 * out of it for the controls — the controls are re-laid-out on every scale
 * change, since the readout they carry says how many pixels wide the pulse is.
 */
let lastFit = "";
const watch = new ResizeObserver(() => {
  const signature = `${stage.clientWidth}×${stage.clientHeight}/${overlayRoom()}`;
  if (signature === lastFit) return;
  lastFit = signature;
  refit();
});
watch.observe(stage);
watch.observe(status);

await init();

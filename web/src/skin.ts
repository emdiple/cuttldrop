// The skin: paint a file as a looping sequence of pulses (DESIGN.md §3d).
//
// Everything that decides *what* to paint is in WASM. This file owns only the
// things a browser does better: reading a file, sizing a canvas, and pacing.

import init, { Skin } from "../pkg/cuttl_wasm.js";
import { ScreenAwake } from "./platform.js";

/// Repair symbols per source symbol. The loop is longer, so a receiver that
/// missed a frame waits for a *different* one rather than the same one again.
const OVERHEAD = 2.0;

const file = document.querySelector<HTMLInputElement>("#file")!;
const detail = document.querySelector<HTMLParagraphElement>("#detail")!;
const start = document.querySelector<HTMLButtonElement>("#start")!;
const rate = document.querySelector<HTMLInputElement>("#rate")!;
const profile = document.querySelector<HTMLSelectElement>("#profile")!;
const rateValue = document.querySelector<HTMLOutputElement>("#rate-value")!;
const setup = document.querySelector<HTMLDivElement>("#setup")!;
const display = document.querySelector<HTMLCanvasElement>("#pulse")!;
const status = document.querySelector<HTMLDivElement>("#status")!;
const statusText = document.querySelector<HTMLSpanElement>("#status-text")!;
const size = document.querySelector<HTMLInputElement>("#size")!;
const sizeValue = document.querySelector<HTMLOutputElement>("#size-value")!;

let skin: Skin | null = null;
let index = 0;

/**
 * On this side the screen *is* the transmitter, so a display timeout does not
 * merely inconvenience the user — it stops the send, silently, with the page
 * still apparently running. The eye just sees the stream stop.
 */
const awake = new ScreenAwake();

/** Off-screen canvas at *grid* resolution; the display is a scaled blit of it. */
const grid = document.createElement("canvas");
const gridCtx = grid.getContext("2d", { willReadFrequently: false })!;
const displayCtx = display.getContext("2d")!;

/**
 * The viewport the user can actually see.
 *
 * `innerHeight` is the wrong number on a phone: mobile Safari counts the strip
 * under a retracted URL bar, so a canvas sized to it is taller than the screen
 * and its bottom rows — the *second beacon strip* — sit off the visible area or
 * under the overlay. `visualViewport` is what is on the glass.
 */
function viewport(): { width: number; height: number } {
  const vv = window.visualViewport;
  return {
    width: Math.floor(vv?.width ?? window.innerWidth),
    height: Math.floor(vv?.height ?? window.innerHeight),
  };
}

/**
 * Height the status overlay owns at the bottom, measured rather than assumed.
 *
 * A constant here was wrong in both directions: the overlay grows with the OS
 * text size and with the range control's native height, and it sits above the
 * home-indicator inset on an iPhone. Measuring from the overlay's own top edge
 * folds its height, its bottom offset and the safe-area inset into one number
 * that cannot drift from what is on screen.
 */
function overlayRoom(): number {
  if (status.hidden) return 0;
  const box = status.getBoundingClientRect();
  return Math.max(0, Math.ceil(viewport().height - box.top));
}

/** Largest whole pixels-per-cell that still fits the entire pulse on screen. */
function maxScale(): number {
  if (!skin) return 1;
  const view = viewport();
  const room = Math.max(1, view.height - overlayRoom());
  return Math.max(1, Math.floor(Math.min(view.width / skin.cols, room / skin.rows)));
}

/**
 * Size the canvas to an *integer* multiple of the grid.
 *
 * This matters more than it looks. At a fractional scale, nearest-neighbour
 * upscaling gives some cells one more pixel than others, so the eye's run-length
 * ratios stop being clean 1:1:3:1:1 and finder detection gets harder for no
 * reason. An integer scale makes every cell identical.
 *
 * It is also why the size control counts *pixels per cell* rather than a
 * percentage: a percentage slider would offer positions that round to the same
 * scale, so most of its travel would do nothing visible. Here every notch is a
 * different grid, and the number shown is the one that governs whether the eye
 * can resolve a cell at all — the measured floor is 4 px/cell at the sensor.
 */
function resize(): void {
  if (!skin) return;
  // Publish the measured reserve so the CSS that *centres* the pulse and the
  // arithmetic that *sizes* it agree by construction. The stylesheet's value is
  // only ever the starting guess, used for the frame before the first measure.
  document.body.style.setProperty("--overlay-room", `${overlayRoom()}px`);
  const limit = maxScale();
  size.max = String(limit);
  // Keep the chosen scale when it still fits, clamp it when the window shrinks.
  const scale = Math.min(limit, Math.max(1, Number(size.value) || limit));
  size.value = String(scale);
  size.disabled = limit <= 1;
  sizeValue.value = `${scale} px/cell · ${skin.cols * scale}×${skin.rows * scale}`;
  display.width = skin.cols * scale;
  display.height = skin.rows * scale;
  // Set after every resize: the context resets its state when the canvas is
  // resized, and smoothing back on would blur every cell edge.
  displayCtx.imageSmoothingEnabled = false;
}

function paint(): void {
  if (!skin) return;
  const rgba = skin.pulseRgba(index);
  gridCtx.putImageData(new ImageData(new Uint8ClampedArray(rgba), skin.cols, skin.rows), 0, 0);
  displayCtx.imageSmoothingEnabled = false;
  displayCtx.drawImage(grid, 0, 0, display.width, display.height);
}

/**
 * Hold each pulse for a whole number of display refreshes.
 *
 * Never try to change pulses faster than the display can commit them (§3d): a
 * pulse the panel never fully showed is one the camera can only catch mid-flip.
 */
function loop(): void {
  let held = 0;
  const step = () => {
    if (!skin) return;
    const refreshRate = 60;
    const hold = Math.max(1, Math.round(refreshRate / Number(rate.value)));
    if (held >= hold) {
      index = (index + 1) % skin.pulseCount;
      paint();
      held = 0;
    }
    held += 1;
    requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

rate.addEventListener("input", () => {
  rateValue.value = rate.value;
  if (skin) statusText.textContent = `${rate.value} Hz · ${skin.pulseCount} pulses in the loop`;
});

// Resize repaints from the same pulse index, so dragging the slider never
// costs the eye a frame of the loop.
size.addEventListener("input", () => {
  resize();
  paint();
});

// Changing density re-encodes: the grid decides how much fits in a pulse, so
// there is nothing to reuse. Cheap enough to do on every change.
profile.addEventListener("change", () => {
  file.dispatchEvent(new Event("change"));
});

file.addEventListener("change", async () => {
  const chosen = file.files?.[0];
  if (!chosen) return;
  detail.textContent = "Encoding…";
  start.disabled = true;

  const bytes = new Uint8Array(await chosen.arrayBuffer());
  const streamId = (Math.random() * 0xffffffff) >>> 0;
  try {
    // Name and mime ride in the manifest, so the eye can display and save the
    // file as itself rather than as received.bin (§3c).
    skin = new Skin(bytes, chosen.name, chosen.type, profile.value, streamId, OVERHEAD);
  } catch (error) {
    detail.textContent = `Could not encode: ${error}`;
    return;
  }

  grid.width = skin.cols;
  grid.height = skin.rows;
  detail.textContent =
    `${chosen.name} — ${bytes.length.toLocaleString()} B, ` +
    `${skin.pulseCount} pulses at ${skin.cols}×${skin.rows}`;
  // A short loop is the one thing that can starve a transfer outright: the
  // fountain has too few distinct symbols to route around a bad frame. The
  // skin repeats forever so it recovers, but slowly — worth saying.
  if (skin.pulseCount < 32) {
    detail.textContent += " · short loop for this density, expect repeats";
  }
  start.disabled = false;
});

start.addEventListener("click", () => {
  if (!skin) return;
  setup.hidden = true;
  display.hidden = false;
  status.hidden = false;
  statusText.textContent = `${rate.value} Hz · ${skin.pulseCount} pulses in the loop`;
  // Start filling the screen; the slider only ever goes down from here.
  size.value = String(maxScale());
  resize();
  paint();
  loop();
  void awake.acquire();
});

function refit(): void {
  resize();
  paint();
}

window.addEventListener("resize", refit);
window.addEventListener("orientationchange", refit);
// The URL bar sliding in and out changes the visible height without firing a
// window resize on iOS; without this the pulse keeps the size it had when the
// bar was hidden and runs under the overlay.
window.visualViewport?.addEventListener("resize", refit);

// The overlay's own height moves with the OS text size and with how wide the
// numbers in it are. Refit only when its *height* changes: it is re-laid-out on
// every scale change, and reacting to width would be a loop.
let lastRoom = -1;
new ResizeObserver(() => {
  const room = overlayRoom();
  if (room === lastRoom) return;
  lastRoom = room;
  refit();
}).observe(status);

await init();

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
const stage = document.querySelector<HTMLDivElement>("#stage")!;
const display = document.querySelector<HTMLCanvasElement>("#pulse")!;
const status = document.querySelector<HTMLDivElement>("#status")!;
const statusText = document.querySelector<HTMLSpanElement>("#status-text")!;
const size = document.querySelector<HTMLInputElement>("#size")!;
const sizeValue = document.querySelector<HTMLOutputElement>("#size-value")!;

let skin: Skin | null = null;
let index = 0;
/** True until the size slider is deliberately moved; see `resize`. */
let pinnedToMax = true;

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

/**
 * Largest whole pixels-per-cell that still fits the entire pulse in the stage.
 *
 * Measured off the stage element, not computed from the viewport. The stage is
 * the full screen on a phone and the column beside the panel on a laptop, so one
 * routine covers both — and it cannot disagree with what CSS actually did.
 */
function maxScale(): number {
  if (!skin) return 1;
  const width = Math.max(1, stage.clientWidth);
  const height = Math.max(1, stage.clientHeight);
  return Math.max(1, Math.floor(Math.min(width / skin.cols, height / skin.rows)));
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
  // Until the slider is touched, track the largest that fits. Otherwise hiding
  // the controls would free up room the pulse never reclaims — the default has
  // to follow the space available, and only a deliberate choice should pin it.
  // Keep the chosen scale when it still fits, clamp it when the window shrinks.
  const scale = pinnedToMax ? limit : Math.min(limit, Math.max(1, Number(size.value) || limit));
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
  label();
});

// Resize repaints from the same pulse index, so dragging the slider never
// costs the eye a frame of the loop.
size.addEventListener("input", () => {
  pinnedToMax = false;
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
  // A re-encode is a different grid and a different loop length. If one is
  // already on screen, refit it: the display canvas is still sized for the old
  // profile, and blitting the new grid into it would stretch every cell.
  index = 0;
  if (sending()) {
    label();
    refit();
  }
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
  // On a laptop the panel stays, so Start remains reachable during a send —
  // and a second loop() would run two rAF chains against one canvas, doubling
  // the pulse rate the eye sees while the slider still claims the old one.
  if (sending()) {
    index = 0;
    return;
  }
  document.body.classList.add("sending");
  display.hidden = false;
  start.textContent = "Sending…";
  loop();
  void awake.acquire();
  applyMode();
});

function refit(): void {
  resize();
  paint();
}

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
      ? `${rate.value} Hz · ${skin.pulseCount} pulses`
      : `${rate.value} Hz · tap the pulse for these controls`;
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

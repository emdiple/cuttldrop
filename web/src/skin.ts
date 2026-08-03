// The skin: paint a file as a looping sequence of pulses (DESIGN.md §3d).
//
// Everything that decides *what* to paint is in WASM. This file owns only the
// things a browser does better: reading a file, sizing a canvas, and pacing.

import init, { Skin } from "../pkg/cuttl_wasm.js";

const PROFILE = "m1";
/// Repair symbols per source symbol. The loop is longer, so a receiver that
/// missed a frame waits for a *different* one rather than the same one again.
const OVERHEAD = 2.0;

const file = document.querySelector<HTMLInputElement>("#file")!;
const detail = document.querySelector<HTMLParagraphElement>("#detail")!;
const start = document.querySelector<HTMLButtonElement>("#start")!;
const rate = document.querySelector<HTMLInputElement>("#rate")!;
const rateValue = document.querySelector<HTMLOutputElement>("#rate-value")!;
const setup = document.querySelector<HTMLDivElement>("#setup")!;
const display = document.querySelector<HTMLCanvasElement>("#pulse")!;
const status = document.querySelector<HTMLDivElement>("#status")!;

let skin: Skin | null = null;
let index = 0;

/** Off-screen canvas at *grid* resolution; the display is a scaled blit of it. */
const grid = document.createElement("canvas");
const gridCtx = grid.getContext("2d", { willReadFrequently: false })!;
const displayCtx = display.getContext("2d")!;

/**
 * Size the canvas to an *integer* multiple of the grid.
 *
 * This matters more than it looks. At a fractional scale, nearest-neighbour
 * upscaling gives some cells one more pixel than others, so the eye's run-length
 * ratios stop being clean 1:1:3:1:1 and finder detection gets harder for no
 * reason. An integer scale makes every cell identical.
 */
function resize(): void {
  if (!skin) return;
  const scale = Math.max(
    1,
    Math.floor(Math.min(window.innerWidth / skin.cols, window.innerHeight / skin.rows)),
  );
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
  if (skin) status.textContent = `${rate.value} Hz · ${skin.pulseCount} pulses in the loop`;
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
    skin = new Skin(bytes, chosen.name, chosen.type, PROFILE, streamId, OVERHEAD);
  } catch (error) {
    detail.textContent = `Could not encode: ${error}`;
    return;
  }

  grid.width = skin.cols;
  grid.height = skin.rows;
  detail.textContent =
    `${chosen.name} — ${bytes.length.toLocaleString()} B, ` +
    `${skin.pulseCount} pulses at ${skin.cols}×${skin.rows}`;
  start.disabled = false;
});

start.addEventListener("click", () => {
  if (!skin) return;
  setup.hidden = true;
  display.hidden = false;
  status.hidden = false;
  status.textContent = `${rate.value} Hz · ${skin.pulseCount} pulses in the loop`;
  resize();
  paint();
  loop();
});

window.addEventListener("resize", () => {
  resize();
  paint();
});

await init();

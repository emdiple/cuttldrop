/**
 * Chromatophore field — the logo's dissolving-pixel motif, made live.
 *
 * A fixed grid of dormant pixel cells covers the page. The pointer is an
 * attractor: cells near it swell — each with its own random gain, colour and
 * shimmer phase — and settle back to nothing as it moves away, the way a
 * cuttlefish's chromatophores dilate under a passing shadow. A tap sends a
 * ring pulse through the grid. Landing page only: the canvas never exists on
 * the skin or eye pages, where stray lit pixels would compete with the
 * optical raster.
 */

const PALETTE = [
  "#3f8cff",
  "#43dced",
  "#5ac8fa",
  "#8f7cff",
  "#b06dff",
  "#f45fd3",
  "#ff9042",
  "#ffd24d",
  "#4ade80",
];

const SPACING = 26; // grid pitch in CSS px
const RADIUS = 190; // attractor reach
const PULSE_SPEED = 620; // ring pulse expansion, px/s
const PULSE_LIFE = 0.9; // seconds
const AMBIENT_EVERY = 0.4; // seconds between idle cell twinkles
const AMBIENT_LIFE = 1.8; // twinkle duration

const CORNER = 0.3; // corner radius as a fraction of the cell's size

interface Twinkle {
  index: number;
  age: number;
}

interface Pulse {
  x: number;
  y: number;
  age: number;
}

const canvas = document.getElementById("chroma-field") as HTMLCanvasElement | null;
const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

if (canvas) start(canvas);

/** Path one cell as a rounded square, falling back to a hard square. */
function cellPath(ctx: CanvasRenderingContext2D, cx: number, cy: number, s: number): void {
  const half = s / 2;
  ctx.beginPath();
  if (typeof ctx.roundRect === "function") {
    ctx.roundRect(cx - half, cy - half, s, s, s * CORNER);
  } else {
    ctx.rect(cx - half, cy - half, s, s);
  }
}

function start(field: HTMLCanvasElement): void {
  const ctx = field.getContext("2d");
  if (!ctx) return;

  let width = 0;
  let height = 0;
  let cols = 0;
  let rows = 0;
  let offsetX = 0;
  let offsetY = 0;

  // Per-cell state, index = row * cols + col.
  let size = new Float32Array(0); // current rendered size
  let gain = new Float32Array(0); // random per-cell responsiveness
  let phase = new Float32Array(0); // random shimmer offset
  let colorIndex = new Uint8Array(0);
  let hollow = new Uint8Array(0); // minority render as outlined QR-ish blocks

  const twinkles: Twinkle[] = [];
  const pulses: Pulse[] = [];
  let raf = 0;
  let last = 0;
  let clock = 0;
  let ambientClock = 0;
  let pointerX: number | null = null;
  let pointerY: number | null = null;

  const rebuild = (): void => {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    width = window.innerWidth;
    height = window.innerHeight;
    field.width = Math.round(width * dpr);
    field.height = Math.round(height * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    cols = Math.ceil(width / SPACING);
    rows = Math.ceil(height / SPACING);
    offsetX = (width - (cols - 1) * SPACING) / 2;
    offsetY = (height - (rows - 1) * SPACING) / 2;

    const count = cols * rows;
    size = new Float32Array(count);
    gain = new Float32Array(count);
    phase = new Float32Array(count);
    colorIndex = new Uint8Array(count);
    hollow = new Uint8Array(count);
    for (let i = 0; i < count; i++) {
      gain[i] = 0.45 + Math.random() * 0.55;
      phase[i] = Math.random() * Math.PI * 2;
      colorIndex[i] = Math.floor(Math.random() * PALETTE.length);
      hollow[i] = Math.random() < 0.15 ? 1 : 0;
    }
    twinkles.length = 0;
  };

  const onPointerMove = (event: PointerEvent): void => {
    pointerX = event.clientX;
    pointerY = event.clientY;
  };

  const onPointerOut = (): void => {
    pointerX = null;
    pointerY = null;
  };

  const onPointerDown = (event: PointerEvent): void => {
    if (reduceMotion.matches) return;
    if (pulses.length < 4) pulses.push({ x: event.clientX, y: event.clientY, age: 0 });
  };

  const frame = (now: number): void => {
    raf = requestAnimationFrame(frame);
    const dt = Math.min(0.05, (now - last) / 1000 || 0);
    last = now;
    clock += dt;

    ambientClock += dt;
    if (ambientClock >= AMBIENT_EVERY) {
      ambientClock = 0;
      if (twinkles.length < 10 && cols * rows > 0) {
        twinkles.push({ index: Math.floor(Math.random() * cols * rows), age: 0 });
      }
    }
    for (let i = twinkles.length - 1; i >= 0; i--) {
      twinkles[i].age += dt;
      if (twinkles[i].age >= AMBIENT_LIFE) twinkles.splice(i, 1);
    }
    for (let i = pulses.length - 1; i >= 0; i--) {
      pulses[i].age += dt;
      if (pulses[i].age >= PULSE_LIFE) pulses.splice(i, 1);
    }

    ctx.clearRect(0, 0, width, height);

    const maxSize = SPACING * 0.82;
    for (let row = 0; row < rows; row++) {
      const cy = offsetY + row * SPACING;
      for (let col = 0; col < cols; col++) {
        const i = row * cols + col;
        const cx = offsetX + col * SPACING;

        // The attractor: proximity sets the cell's target dilation.
        let target = 0;
        if (pointerX !== null && pointerY !== null) {
          const d = Math.hypot(cx - pointerX, cy - (pointerY as number));
          if (d < RADIUS) {
            const near = 1 - d / RADIUS;
            target = maxSize * near * near * gain[i];
          }
        }

        // Tap pulses: a ring sweeping outward briefly dilates cells it crosses.
        for (const pulse of pulses) {
          const ring = pulse.age * PULSE_SPEED;
          const d = Math.hypot(cx - pulse.x, cy - pulse.y);
          const band = Math.exp(-((d - ring) * (d - ring)) / (2 * 34 * 34));
          const fade = 1 - pulse.age / PULSE_LIFE;
          const boost = maxSize * 0.8 * band * fade * gain[i];
          if (boost > target) target = boost;
        }

        if (target > 0) {
          // Random per-cell shimmer while dilated.
          target *= 0.84 + 0.16 * Math.sin(clock * 2.4 + phase[i]);
        }

        // Grow eagerly, relax slowly.
        const rate = target > size[i] ? 16 : 5.5;
        size[i] += (target - size[i]) * Math.min(1, dt * rate);
        if (size[i] < 0.3) {
          size[i] = 0;
          continue;
        }

        const s = size[i];
        ctx.globalAlpha = Math.min(1, s / (SPACING * 0.4)) * 0.5;
        const color = PALETTE[colorIndex[i]];
        cellPath(ctx, cx, cy, s);
        if (hollow[i] === 1 && s > 5) {
          ctx.strokeStyle = color;
          ctx.lineWidth = Math.max(1, s / 7);
          ctx.stroke();
        } else {
          ctx.fillStyle = color;
          ctx.fill();
        }
      }
    }

    // Idle twinkles: lone grid cells breathing so the field reads as alive.
    for (const twinkle of twinkles) {
      const i = twinkle.index;
      const t = twinkle.age / AMBIENT_LIFE;
      const envelope = Math.sin(Math.PI * t);
      const s = SPACING * 0.34 * envelope * gain[i];
      if (s < 0.3 || size[i] > s) continue;
      const cx = offsetX + (i % cols) * SPACING;
      const cy = offsetY + Math.floor(i / cols) * SPACING;
      ctx.globalAlpha = envelope * 0.55;
      ctx.fillStyle = PALETTE[colorIndex[i]];
      cellPath(ctx, cx, cy, s);
      ctx.fill();
    }
    ctx.globalAlpha = 1;
  };

  const stop = (): void => {
    cancelAnimationFrame(raf);
    raf = 0;
    size.fill(0);
    twinkles.length = 0;
    pulses.length = 0;
    ctx.clearRect(0, 0, width, height);
  };

  const run = (): void => {
    if (raf === 0 && !reduceMotion.matches) {
      last = performance.now();
      raf = requestAnimationFrame(frame);
    }
  };

  rebuild();
  window.addEventListener("resize", rebuild);
  window.addEventListener("pointermove", onPointerMove, { passive: true });
  window.addEventListener("pointerdown", onPointerDown, { passive: true });
  window.addEventListener("pointerout", onPointerOut, { passive: true });
  window.addEventListener("blur", onPointerOut);
  reduceMotion.addEventListener("change", () => {
    if (reduceMotion.matches) stop();
    else run();
  });
  run();
}

import assert from "node:assert/strict";
import { PulsePacer, QUIET_CELLS, fitPhysicalScale, rasterizePulse } from "../src/optical-display.ts";

function pixel(rgba: Uint8ClampedArray, width: number, x: number, y: number): number[] {
  const start = (y * width + x) * 4;
  return Array.from(rgba.subarray(start, start + 4));
}

{
  const source = new Uint8ClampedArray([
    1, 2, 3, 4,
    11, 12, 13, 14,
    21, 22, 23, 24,
    31, 32, 33, 34,
  ]);
  const raster = rasterizePulse(source, 2, 2);

  assert.equal(raster.width, 2 + QUIET_CELLS * 2);
  assert.equal(raster.height, 2 + QUIET_CELLS * 2);

  for (let y = 0; y < raster.height; y += 1) {
    for (let x = 0; x < raster.width; x += 1) {
      const inside =
        x >= QUIET_CELLS && x < QUIET_CELLS + 2 && y >= QUIET_CELLS && y < QUIET_CELLS + 2;
      if (!inside) assert.deepEqual(pixel(raster.rgba, raster.width, x, y), [0, 0, 0, 255]);
    }
  }

  const recovered: number[] = [];
  for (let y = 0; y < 2; y += 1) {
    for (let x = 0; x < 2; x += 1) {
      recovered.push(...pixel(raster.rgba, raster.width, x + QUIET_CELLS, y + QUIET_CELLS));
    }
  }
  assert.deepEqual(recovered, Array.from(source));
}

{
  const fit = fitPhysicalScale(200, 116, 900, 600, 2.625);
  assert.equal(Number.isInteger(fit.scale), true);
  assert.equal(fit.backingWidth % 200, 0);
  assert.equal(fit.backingHeight % 116, 0);
  assert.ok(fit.cssWidth <= 900);
  assert.ok(fit.cssHeight <= 600);

  const pinned = fitPhysicalScale(200, 116, 900, 600, 2.625, 3.9);
  assert.equal(pinned.scale, 3);
  assert.equal(pinned.cssWidth, 600 / 2.625);
}

function advances(refreshHz: number, targetHz: number): number[] {
  const pacer = new PulsePacer(0, targetHz);
  const hits: number[] = [];
  for (let callback = 1; callback <= refreshHz; callback += 1) {
    if (pacer.tick((callback * 1000) / refreshHz, targetHz)) hits.push(callback);
  }
  return hits;
}

{
  const at60 = advances(60, 60);
  assert.ok(at60.length <= 30, `60 Hz panel advanced ${at60.length} times`);
  assert.ok(at60.every((callback, index) => index === 0 || callback - at60[index - 1]! >= 2));

  const at120 = advances(120, 60);
  assert.equal(at120.length, 60);
  assert.ok(at120.every((callback, index) => index === 0 || callback - at120[index - 1]! >= 2));
}

console.log("ok — optical display keeps a black quiet zone and two-refresh pulse floor");

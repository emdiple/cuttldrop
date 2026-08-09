import assert from "node:assert/strict";
import { PulsePacer, fitPhysicalScale } from "../src/optical-display.ts";

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

console.log("ok — optical display keeps the physical-pixel fit and two-refresh pulse floor");

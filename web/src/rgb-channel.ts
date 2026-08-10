// Channel separation for the QR RGB transport — the eye-side inverse of
// rasterizeRgbReferencePackets, shared between the decode workers and the
// Node optical seam test so both read a colored frame the same way.

/**
 * Below this peak-to-peak range a channel is treated as carrying no symbol.
 * Stretching a flat channel would only amplify sensor noise into something
 * for ZXing to chew on and reject.
 */
const MIN_CONTRAST = 24;

/**
 * Replicate one channel of an RGBA frame to grey, stretched to full range,
 * so a luminance-based QR reader sees exactly that channel at full contrast.
 *
 * A screen's primaries and a camera's colour dyes overlap, so each captured
 * channel is a leaky mix of all three symbols: a module that is black in
 * this channel's symbol but white in the other two sits at the leak floor,
 * well above zero, and ambient light and panel brightness compress the range
 * further. The linear stretch pins the darkest value seen to 0 and the
 * brightest to 255 — both references are always in frame, because the finder
 * patterns are black and the quiet zone white in every channel. This buys
 * back contrast, not separation: if the leak ever makes "black here, white
 * elsewhere" brighter than "white here, black elsewhere", no per-channel
 * transform can help, and that is what the calibration-matrix experiment is
 * for.
 *
 * Writes grey (RGB replicated, alpha opaque) into `out` and returns it.
 */
export function channelToGrey(
  rgba: Uint8ClampedArray,
  channel: number,
  out: Uint8ClampedArray,
): Uint8ClampedArray {
  let min = 255;
  let max = 0;
  for (let at = 0; at < rgba.length; at += 4) {
    const value = rgba[at + channel];
    if (value < min) min = value;
    if (value > max) max = value;
  }
  const range = max - min;
  const stretch = range >= MIN_CONTRAST && range < 255;
  for (let at = 0; at < rgba.length; at += 4) {
    const raw = rgba[at + channel];
    const value = stretch ? ((raw - min) * 255) / range : raw;
    out[at] = value;
    out[at + 1] = value;
    out[at + 2] = value;
    out[at + 3] = 255;
  }
  return out;
}

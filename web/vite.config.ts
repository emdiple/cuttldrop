import { readFileSync } from "node:fs";
import { defineConfig } from "vite";

/**
 * A dev cert, if `npm run cert` has made one.
 *
 * The eye needs a *secure context*: `navigator.mediaDevices` does not exist at
 * all on plain http, and iOS Safari is strict about it — `localhost` is exempt,
 * but a phone reaching this laptop over the LAN is on `http://192.168.x.x`,
 * which is not. That is why a phone camera "does not work" while the laptop's
 * does. Serving TLS is the whole fix.
 *
 * Optional on purpose: without a cert the dev server still runs for laptop work
 * on localhost, and only the two-device run needs the extra step.
 */
function devCert(): { key: Buffer; cert: Buffer } | undefined {
  try {
    return {
      key: readFileSync(new URL("./.cert/key.pem", import.meta.url)),
      cert: readFileSync(new URL("./.cert/cert.pem", import.meta.url)),
    };
  } catch {
    return undefined;
  }
}

export default defineConfig({
  // No server component ever (DESIGN.md §4), so this builds to plain static
  // files. Relative base so it works from any subdirectory, or off a file path.
  base: "./",
  server: {
    // Bind every interface: the M1 observable is two *physical* devices, so the
    // phone has to be able to reach this at all.
    host: true,
    https: devCert(),
  },
  build: {
    target: "es2022",
    rollupOptions: {
      // Three entry points: the landing page, the sender, the receiver.
      input: {
        index: "index.html",
        skin: "skin.html",
        eye: "eye.html",
      },
    },
  },
});

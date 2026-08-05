import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { networkInterfaces } from "node:os";
import { defineConfig, type Plugin } from "vite";

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

/** Every IPv4 address a phone could plausibly use to reach this machine. */
function lanAddresses(): string[] {
  return Object.values(networkInterfaces())
    .flat()
    .filter((n) => n && n.family === "IPv4" && !n.internal)
    .map((n) => n!.address);
}

/** Addresses the current cert actually vouches for. */
function certAddresses(): string[] | null {
  try {
    const text = execFileSync(
      "openssl",
      ["x509", "-in", ".cert/cert.pem", "-noout", "-ext", "subjectAltName"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    );
    return [...text.matchAll(/IP Address:([0-9.]+)/g)].map((m) => m[1]);
  } catch {
    return null;
  }
}

/**
 * Warn when the cert no longer covers the address the phone will use.
 *
 * This is the failure that keeps costing an afternoon. A cert is bound to
 * *addresses*, and the address moves: a phone hotspot hands out a new lease on
 * every reconnect, and wifi-to-hotspot changes the subnet entirely. Nothing
 * about the resulting failure says "stale certificate" — Safari shows a warning
 * naming the wrong host, or refuses outright, and it reads as "the server is not
 * on the network". Which is exactly what it does not mean.
 *
 * A warning, never an error: serving an unmatched cert still works if you tap
 * through, and a laptop-only session on localhost does not care at all.
 */
function certCoverage(): Plugin {
  return {
    name: "cuttldrop-cert-coverage",
    apply: "serve",
    configureServer(server) {
      server.httpServer?.once("listening", () => {
        const covered = certAddresses();
        if (covered === null) return;
        const missing = lanAddresses().filter((ip) => !covered.includes(ip));
        if (missing.length === 0) return;
        server.config.logger.warn(
          `\n  ⚠ the dev certificate does not cover ${missing.join(", ")}.` +
            `\n    It was made for ${covered.join(", ")}, and this machine has moved since.` +
            `\n    Run \`npm run cert\` again, or the phone will refuse the address above.\n`,
        );
      });
    },
  };
}

export default defineConfig({
  plugins: [certCoverage()],
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

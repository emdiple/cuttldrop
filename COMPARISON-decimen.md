# Cuttldrop vs. decimen-optical-transfer

Repo: <https://github.com/bashalarmistalt/decimen-optical-transfer> (MIT)
Reviewed: 2026-08-03, against its README. Summarised in `DESIGN.md` §8; this file is
the working comparison and the roadmap items adopted from it.

Decimen is the same thesis as Cuttldrop — file transfer as light, screen to camera, no
network path, no pairing — built in one night on the opposite foundational bet: it
**rents its symbol layer from QR** (animated B/W QR up to v40 through zxing-cpp WASM),
where Cuttldrop **builds the symbol layer** (custom colour-capable raster, own
finder/homography/sampling pipeline). It claims **129.2 KB/s goodput phone-to-phone**,
handheld — a real-device number, which is the one kind of number this project does not
yet have.

## Where the two projects independently converged

Every one of these was arrived at separately, which is decent evidence they are forced
moves rather than fashion:

| Decision | decimen | cuttldrop |
|---|---|---|
| Transport | fountain code (hand-rolled LT, robust soliton, ~K·1.15) | fountain code (RaptorQ, RFC 6330, ~K·1.02) |
| Errors → erasures before the fountain | QR either decodes through zxing or is discarded | inner RS → CRC-32 gate per band |
| Mid-stream join, no handshake | self-describing 20 B header per frame | 24 B stream header + OTI in band 0 of every pulse |
| File identity | filename + media type carried, SHA-256 verified before download | manifest (name, mime) + mandatory BLAKE3 verify |
| Receiver plumbing | zxing WASM in workers, `requestVideoFrameCallback` | own WASM in a worker, `requestVideoFrameCallback` |
| Stack | TypeScript + Vite, static deploy, no framework | TypeScript + Vite, static deploy, no framework |
| Honest progress | frames + decoded blocks, not fake percent | symbols / K, not fake percent |

## The divergence, and what each side buys

Decimen's QR bet buys a decade-hardened detector at ~31k modules/frame and 60 fps on
day one — hence 129 KB/s handheld against what was, when this file was written, a
simulated 14 KB/s colour ceiling.
Cuttldrop's custom-raster bet buys the three things QR forecloses by spec: colour
(3 bits/cell, M3), sub-frame erasure granularity (per-band salvage of torn frames), and
an owned detector that can be tuned past QR's model (M4). Their number de-risks our M4
density direction: real handheld cameras demonstrably resolve ~6× our current spatial
density when the detector is good enough.

## Their limitations, and where Cuttldrop stands on each

### Already addressed here

- **~15% LT reception overhead** → RaptorQ's ~2% (`DESIGN.md` §1a).
- **Whole-frame erasure granularity** (a straddled frame is wholly wasted) → per-band
  symbols; a torn frame loses the bands the tear crossed, the beacon reports *why*.
- **B/W only, minimum in-frame ECC (level L)** → 8-colour path built behind the M3 A/B
  toggle; inner RS sized from measurement, CRC gate above it.
- **`Math.log` disagrees between JS engines** — they hand-roll a deterministic IEEE-754
  log so sender and receiver derive identical soliton distributions from a seed. This is
  precisely the hand-rolled-LT failure class §1a predicted when choosing RaptorQ:
  RFC 6330 symbol generation is integer-deterministic, so the bug cannot exist here.
- **Heavy receiver payload** (940 KB zxing WASM; 1.3 MB standalone) → owning the codec
  keeps ours at ~350 KB total.

### Shared limitations → adopted as roadmap (see below)

- **No encryption.** Their README says it plainly: "whatever is on the sending screen is
  readable by any camera pointed at it. The property this gives you is no network, not
  confidentiality." True of Cuttldrop too, and currently said nowhere. → R1, R2.
- **No compression.** They gzip adaptively, only when it helps; we send raw bytes. At
  1.6 KB/s mono, compression is a bigger lever than most optical work. → R3.
- **File size cap.** Theirs is a stated 64 MB; ours is unstated and unmeasured (wasm
  memory and single-source-block behaviour are the real bounds). An undocumented cap is
  worse than a documented one. → R4.
- **Display refresh assumptions.** They default to 60 TX fps and tell 60 Hz screens to
  drop to 24–30; our `skin.ts` pacing loop hardcodes `refreshRate = 60`. → R5.
- **iOS camera lies.** `frameRate: {ideal: 60}` silently delivers 30; exact constraints
  with fallback are required. Directly feeds our M1 iPhone-as-eye probe. → R6.
- **Capture-loop lifecycle.** Their rVFC chains survived stream restarts until guarded
  by generation counters. Ours never restarts a stream yet; the moment the eye UI gains
  a retry button, the same zombie-loop bug is waiting. → R7.
- **`file://` opaque origins block the camera** on iOS Safari and Android Chrome. We
  document the https/localhost requirement; their standalone single-file HTML build
  (~55 KB sender) is still a deployment mode worth having. → R8.

### Theirs by design, not adopted

- Density/EC as user-facing knobs (bytes/frame, EC level): our density is a profile, and
  M4 owns that lever with measurements rather than a slider.
- Stacked codes / 120 Hz ProMotion pushing: that is their M4-equivalent; ours goes
  through colour and glare masking first (§2 bottleneck order).

## Roadmap adopted from this comparison

| # | Item | Where it lands |
|---|---|---|
| R1 | State the confidentiality trade-off in README and eye/skin UI — no network ≠ private | immediate, docs |
| R2 | Optional passphrase encryption (XChaCha20-Poly1305, key never on the wire; manifest gains a flag) | M5 |
| R3 | Adaptive compression before encode, applied only when it shrinks the object; flagged in the manifest | M4 |
| R4 | Measure and document the practical file-size ceiling (wasm memory, multi-source-block RaptorQ); sim test at 64 MB | M4 |
| R5 | Measure real display refresh from rAF timestamps instead of the hardcoded 60 in `skin.ts` | M1 polish |
| R6 | iOS camera probe: exact `frameRate`/width constraints with fallback, per decimen's findings | M1 (already planned; now with specifics) |
| R7 | Generation counter on the eye's capture loop before any stream-restart UI ships | with M5 UI work |
| R8 | Single-file standalone builds (sender ~small, receiver with embedded WASM) alongside the M5 PWA | M5 |

## Both questions their 129 KB/s raised are now answered

**How fast should the skin strobe?** Goodput is linear in pulse rate up to **20 Hz**
against a 30 fps camera — now the skin's default. Integer ratios of the capture rate
freeze the phase and can starve a transfer outright, which is their field note about
dropping 60 Hz screens to 24–30 arrived at from the shutter arithmetic instead.

**How dense can a frame get?** Dense enough. The profile ladder shipped: 192×108 mono
carries 2128 B/pulse and 192×108 colour carries 6560 B/pulse, against the old default's
160 B. At 20 Hz that is **43 KB/s and 131 KB/s** — decimen's territory, reached through
colour and overhead amortisation rather than through QR's module count. The skin picks
from a menu; the eye trial-decodes and locks, so density is set on one device only.

Two findings worth putting next to their numbers. **Density beat colour** (39 pulses vs
56 for the same 24 KB), so the lever we spent §1c arguing about turns out to be the
second-best one we had. And **their 31,329-module v40 frame is only 1.5× our dense grid**
— the gap was never physics, it was that they rented a decade-hardened detector from
zxing on day one while ours started conservative and stayed there. See `DESIGN.md` §3d
and §5 M4 for the measured tables.

The eye also grew their instrument panel — capture fps, decode fps, goodput, elapsed,
frames new/dup, ETA, camera mode — for the reason in `DESIGN.md` §1e: our version of
their 129 KB/s does not exist until something on a real device measures it.

The sharpest thing this comparison says is not on the list: decimen has demonstrated,
on hardware, everything Cuttldrop has only simulated. The M1 observable — one real file
across one real air gap — is still the highest-value step, and it got more urgent.

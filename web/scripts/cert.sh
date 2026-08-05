#!/bin/sh
# Make a self-signed cert for the dev server, so a *phone* can be the eye.
#
# `navigator.mediaDevices` is undefined outside a secure context. localhost is
# exempt, a LAN address is not — so the laptop's camera works over `npm run dev`
# and the phone's silently does not. This is the smallest fix that does not add
# a dependency: openssl ships with macOS.
#
# Two ways to get one, and the difference is only whether the phone warns:
#
#   mkcert  — signed by a local root you install on the phone once. No warning.
#   openssl — signed by nobody. Works identically, but every device shows an
#             interstitial the first time and you have to tap through it.
#
# The warning is not a bug and tapping through it is not a workaround: the
# browser is correctly reporting that nothing vouched for this certificate. It
# has no bearing on whether the camera works — a secure context is a secure
# context once the connection is accepted.
set -eu

cd "$(dirname "$0")/.."
mkdir -p .cert

# Every address the phone might use to reach this machine. A cert is bound to
# names, and "the laptop's IP" is not knowable ahead of time — nor is the
# interface, since a phone hotspot, wifi and ethernet all land differently.
# Take every IPv4 the machine has and let the browser pick.
ips=$(ifconfig 2>/dev/null | awk '$1 == "inet" && $2 != "127.0.0.1" { print $2 }')
hosts="localhost $(hostname) $(hostname -s).local 127.0.0.1 $ips"

if command -v mkcert >/dev/null 2>&1; then
  # shellcheck disable=SC2086
  mkcert -cert-file .cert/cert.pem -key-file .cert/key.pem $hosts >/dev/null 2>&1
  echo "cert written to web/.cert/ by mkcert, for: $hosts"
  echo
  echo "To get no warning on the phone, install the root once:"
  echo "  mkcert -CAROOT   # the folder holding rootCA.pem"
  echo "  # AirDrop/email rootCA.pem to the phone, open it, then:"
  echo "  # iOS: Settings > General > VPN & Device Management > install"
  echo "  #      Settings > General > About > Certificate Trust Settings > enable"
else
  san="DNS:localhost,DNS:$(hostname),DNS:$(hostname -s).local,IP:127.0.0.1"
  for ip in $ips; do
    san="$san,IP:$ip"
  done
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout .cert/key.pem -out .cert/cert.pem \
    -days 365 -subj "/CN=cuttldrop dev" \
    -addext "subjectAltName=$san" 2>/dev/null
  echo "cert written to web/.cert/ by openssl, for $san"
  echo
  echo "Self-signed, so each device warns once. On the phone that is:"
  echo "  Safari: Show Details > visit this website"
  echo "  Chrome: Advanced > Proceed"
  echo "For no warning at all: brew install mkcert, then re-run this."
fi

echo
echo "now: npm run dev  — then open the https:// LAN address on the phone"

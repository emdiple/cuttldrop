#!/bin/sh
# Make a self-signed cert for the dev server, so a *phone* can be the eye.
#
# `navigator.mediaDevices` is undefined outside a secure context. localhost is
# exempt, a LAN address is not — so the laptop's camera works over `npm run dev`
# and the phone's silently does not. This is the smallest fix that does not add
# a dependency: openssl ships with macOS.
#
# Safari will still warn (nothing signed this but us). Tap Show Details →
# visit this website, once per device. To skip that warning entirely, use
# mkcert instead and install its root on the phone.
set -eu

cd "$(dirname "$0")/.."
mkdir -p .cert

# Every address the phone might use to reach this machine. A cert is bound to
# names, and "the laptop's IP" is not knowable ahead of time — nor is the
# interface, since a phone hotspot, wifi and ethernet all land differently.
# Take every IPv4 the machine has and let the browser pick.
ips=$(ifconfig 2>/dev/null | awk '$1 == "inet" && $2 != "127.0.0.1" { print $2 }')
san="DNS:localhost,DNS:$(hostname),DNS:$(hostname -s).local,IP:127.0.0.1"
for ip in $ips; do
  san="$san,IP:$ip"
done

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout .cert/key.pem -out .cert/cert.pem \
  -days 365 -subj "/CN=cuttldrop dev" \
  -addext "subjectAltName=$san" 2>/dev/null

echo "cert written to web/.cert/ for $san"
echo "now: npm run dev  — then open the https:// LAN address on the phone"

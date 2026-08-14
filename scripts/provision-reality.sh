#!/usr/bin/env bash
#
# Provision a VLESS + REALITY server on a fresh Debian/Ubuntu VPS.
#
# REALITY borrows a real, popular site's TLS certificate for the handshake. An
# active prober that connects to this port is transparently forwarded to that
# site and gets its genuine response, so the port cannot be distinguished from
# an ordinary reverse proxy. That is what makes it hold up where Shadowsocks
# and plain Trojan do not.
#
# It says nothing about anonymity: this VPS is rented in your name, paid with
# your card, and used by you alone. See the README.
#
# Usage:  sudo bash provision-reality.sh [borrowed-sni]

set -euo pipefail

BORROWED_SNI="${1:-www.lovelive-anime.jp}"
PORT=443

if [[ $EUID -ne 0 ]]; then
  echo "run with sudo" >&2
  exit 1
fi

echo "==> installing sing-box"
if ! command -v sing-box >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq curl ca-certificates
  # Official install script; pins to the latest stable release.
  curl -fsSL https://sing-box.app/install.sh | sh
fi

echo "==> generating credentials"
UUID="$(sing-box generate uuid)"
KEYPAIR="$(sing-box generate reality-keypair)"
PRIVATE_KEY="$(echo "$KEYPAIR" | awk '/PrivateKey/ {print $2}')"
PUBLIC_KEY="$(echo "$KEYPAIR" | awk '/PublicKey/ {print $2}')"
SHORT_ID="$(openssl rand -hex 4)"

echo "==> verifying the borrowed SNI supports TLS 1.3 and HTTP/2"
if ! curl -sI --tls-max 1.3 --http2 "https://${BORROWED_SNI}" >/dev/null 2>&1; then
  echo "warning: ${BORROWED_SNI} did not answer as expected." >&2
  echo "         REALITY needs a target that speaks TLS 1.3 and HTTP/2 and is" >&2
  echo "         not blocked from here. Pick another and rerun." >&2
fi

echo "==> writing /etc/sing-box/config.json"
mkdir -p /etc/sing-box
cat >/etc/sing-box/config.json <<JSON
{
  "log": { "level": "warn", "timestamp": true },
  "inbounds": [
    {
      "type": "vless",
      "tag": "vless-in",
      "listen": "::",
      "listen_port": ${PORT},
      "users": [
        { "uuid": "${UUID}", "flow": "xtls-rprx-vision" }
      ],
      "tls": {
        "enabled": true,
        "server_name": "${BORROWED_SNI}",
        "reality": {
          "enabled": true,
          "handshake": { "server": "${BORROWED_SNI}", "server_port": 443 },
          "private_key": "${PRIVATE_KEY}",
          "short_id": ["${SHORT_ID}"]
        }
      }
    }
  ],
  "outbounds": [ { "type": "direct" } ]
}
JSON

echo "==> validating"
sing-box check -c /etc/sing-box/config.json

echo "==> starting"
systemctl enable --now sing-box
systemctl restart sing-box
sleep 2
systemctl is-active --quiet sing-box || { journalctl -u sing-box -n 30 --no-pager; exit 1; }

SERVER_IP="$(curl -fsS https://api.ipify.org || hostname -I | awk '{print $1}')"

cat <<EOF

==============================================================
Paste this into Veil (프로필 → 서버 추가):

vless://${UUID}@${SERVER_IP}:${PORT}?encryption=none&security=reality&sni=${BORROWED_SNI}&fp=chrome&pbk=${PUBLIC_KEY}&sid=${SHORT_ID}&type=tcp&flow=xtls-rprx-vision#REALITY

Keep this line secret: it is the whole credential.
==============================================================
EOF

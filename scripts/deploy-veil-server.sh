#!/usr/bin/env bash
#
# Build and install veil-server on the box it will run on.
#
# Built here rather than cross-compiled because the target is a small Oracle
# instance and a build host that matches it exactly is already available: the
# instance itself. The cost is one slow build; the saving is never having to
# wonder whether the binary matches the libc it landed on.
#
# The existing sing-box on 443 is left completely alone. This listens on its own
# port, so the desktop's REALITY profile keeps working while the phone uses the
# new one.
#
# Usage (on the server):
#   sudo bash deploy-veil-server.sh [port]

set -euo pipefail

PORT="${1:-8443}"
PREFIX=/usr/local/bin
CONFIG_DIR=/etc/veil
SOURCE_DIR="${SOURCE_DIR:-/opt/veil-src}"

say() { printf '\n\033[1;33m==> %s\033[0m\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { echo "sudo 로 실행하세요"; exit 1; }

# ---------------------------------------------------------------------------
say "빌드에 필요한 메모리 확보"
# A release build of rustls and tokio needs more than this instance has. Swap is
# slow but it is the difference between a build that finishes and one that is
# killed halfway through with a message nobody reads.
if [ "$(free -m | awk '/^Swap:/ {print $2}')" -lt 1024 ]; then
  if [ ! -f /swapfile ]; then
    fallocate -l 2G /swapfile
    chmod 600 /swapfile
    mkswap /swapfile
  fi
  swapon /swapfile || true
  grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi
free -m | head -3

# ---------------------------------------------------------------------------
say "빌드 도구 설치"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential pkg-config >/dev/null

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
fi
export PATH="/root/.cargo/bin:$PATH"
cargo --version

# ---------------------------------------------------------------------------
say "빌드"
cd "$SOURCE_DIR"
# One job: parallel codegen on a single core with swap thrashes and is slower
# than doing it in order.
CARGO_BUILD_JOBS=1 cargo build --release -p veil-server
install -m 0755 target/release/veil-server "$PREFIX/veil-server"
"$PREFIX/veil-server" 2>&1 | head -3 || true

# ---------------------------------------------------------------------------
say "설정 생성"
SERVER_IP="$(curl -s --max-time 10 https://api.ipify.org || true)"
[ -n "$SERVER_IP" ] || SERVER_IP="$(hostname -I | awk '{print $1}')"

if [ -f "$CONFIG_DIR/config.toml" ]; then
  echo "설정이 이미 있습니다: $CONFIG_DIR/config.toml (그대로 사용)"
else
  # Kept on disk as well as printed: the link is the only thing the phone
  # needs, and losing it means regenerating the certificate.
  "$PREFIX/veil-server" setup "$SERVER_IP" "$PORT" "$CONFIG_DIR" | tee "$CONFIG_DIR/setup.txt"
  chmod 600 "$CONFIG_DIR/setup.txt"
fi

# ---------------------------------------------------------------------------
say "서비스 등록"
cat > /etc/systemd/system/veil-server.service <<'UNIT'
[Unit]
Description=Veil tunnel server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/veil-server run /etc/veil/config.toml
Restart=on-failure
RestartSec=3
# Binding a privileged port without running as root for everything else.
AmbientCapabilities=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/etc/veil

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now veil-server
sleep 2
systemctl is-active veil-server || { journalctl -u veil-server -n 30 --no-pager; exit 1; }

# ---------------------------------------------------------------------------
say "방화벽"
# Oracle images ship a REJECT rule near the end of the INPUT chain, so a new
# rule has to be inserted above it rather than appended after.
if ! iptables -C INPUT -p tcp --dport "$PORT" -j ACCEPT 2>/dev/null; then
  iptables -I INPUT 1 -p tcp --dport "$PORT" -j ACCEPT
  command -v netfilter-persistent >/dev/null && netfilter-persistent save || \
    (command -v iptables-save >/dev/null && iptables-save > /etc/iptables/rules.v4) || true
fi
iptables -L INPUT -n --line-numbers | head -8

# ---------------------------------------------------------------------------
say "완료"
echo "포트      : $PORT"
echo "상태      : $(systemctl is-active veil-server)"
echo "설정      : $CONFIG_DIR/config.toml"
echo
echo "접속 링크 (폰의 Veil 에 붙여넣으세요):"
grep -h 'trojan://' "$CONFIG_DIR/setup.txt" 2>/dev/null || echo "  (없음 — $CONFIG_DIR/setup.txt 확인)"
echo
echo "오라클 콘솔에서 Security List 에 TCP $PORT 인그레스 규칙을 추가해야 외부에서 닿습니다."

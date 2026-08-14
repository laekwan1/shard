#!/usr/bin/env bash
#
# Keep a DuckDNS name pointed at this machine.
#
# The phone's link carries the server's address, and the client resolves that
# name every time it opens a connection — so a name that follows the machine is
# a link that never has to be reissued. An address written into the link does
# have to be: an Oracle instance can come back with a different public IP after
# a stop, a move, or a reassignment, and every phone carrying the old link is
# then pointed at somebody else's machine.
#
# This installs a timer that *checks* where the machine is every five minutes
# and tells DuckDNS only when the answer has changed.
#
# The check costs nothing: an Oracle instance can read its own public address
# from the platform's metadata service, which answers on a link-local address
# and never leaves the machine. Asking an outside service what our address is
# would be a request in itself, so "only update when it changes" built on one of
# those saves nothing — this saves something real.
#
# A name nobody speaks to is eventually removed, so an unchanged address is
# still published once a day. DuckDNS answers "OK" or "KO" and nothing else;
# both are recorded, so a name that stops updating can be explained rather than
# guessed at.
#
# Usage (on the server):
#   sudo bash setup-duckdns.sh <subdomain>
#
# The token is not passed on the command line — a command line is visible to
# every process on the machine and is kept in the shell's history. The script
# asks for it, or reads it from the DUCKDNS_TOKEN environment variable.

set -euo pipefail

DOMAIN="${1:-}"
CONFIG=/etc/duckdns
UPDATER=/usr/local/bin/duckdns-update
LOG=/var/log/duckdns.log

say() { printf '\n\033[1;33m==> %s\033[0m\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { echo "sudo 로 실행하세요"; exit 1; }
[ -n "$DOMAIN" ] || { echo "사용법: sudo bash setup-duckdns.sh <서브도메인>"; exit 1; }

# The name as DuckDNS knows it, not the full host name. Someone who types the
# whole thing means the same thing, so both are accepted.
DOMAIN="${DOMAIN%.duckdns.org}"

# ---------------------------------------------------------------------------
say "토큰 확인"
TOKEN="${DUCKDNS_TOKEN:-}"
if [ -z "$TOKEN" ]; then
  # Read without echoing: a token on screen is a token in a screenshot.
  read -r -s -p "DuckDNS 토큰을 붙여넣으세요: " TOKEN
  echo
fi
[ -n "$TOKEN" ] || { echo "토큰이 비어 있습니다"; exit 1; }

install -d -m 700 "$CONFIG"
printf 'DOMAIN=%s\nTOKEN=%s\n' "$DOMAIN" "$TOKEN" > "$CONFIG/config"
chmod 600 "$CONFIG/config"

# ---------------------------------------------------------------------------
say "주소를 읽어오는 도우미 설치"
# Kept as its own file rather than embedded in the updater: parsing JSON inside
# a shell script means a pipeline of text tools that is wrong in one way or
# another, and this is short enough to read.
install -d -m 755 /usr/local/lib/duckdns
cat > /usr/local/lib/duckdns/public-address.py <<'PYTHON'
"""Print this machine's public address, or nothing.

Oracle's metadata service is asked first: it answers on a link-local address,
so on an instance that exposes the public IP there no traffic leaves the
machine. Not every instance does — some only carry the private IP in metadata,
this one among them — so a small check-IP service is the fallback. That request
is tiny (the address and nothing else), and it is only what lets the DuckDNS
update be skipped when nothing has changed, which is the larger saving.
"""

import json
import sys
import urllib.request


def from_metadata():
    request = urllib.request.Request(
        "http://169.254.169.254/opc/v2/vnics/",
        headers={"Authorization": "Bearer Oracle"},
    )
    with urllib.request.urlopen(request, timeout=3) as answer:
        for vnic in json.load(answer):
            address = vnic.get("publicIp")
            if address:
                return address
    return None


def from_outside():
    # Several, because any one can be down, and being unable to tell where we
    # are would otherwise stop the name being kept current.
    for url in (
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://checkip.amazonaws.com",
    ):
        try:
            with urllib.request.urlopen(url, timeout=5) as answer:
                address = answer.read().decode().strip()
                # A rough check that it is an address and not an error page.
                if address.count(".") == 3 and all(
                    part.isdigit() for part in address.split(".")
                ):
                    return address
        except Exception:
            continue
    return None


try:
    address = None
    try:
        address = from_metadata()
    except Exception:
        address = None
    if not address:
        address = from_outside()
    if address:
        print(address)
except Exception:
    # Could not tell. The caller treats silence as "publish anyway", so the
    # name is kept current even when the address cannot be confirmed.
    sys.exit(0)
PYTHON
chmod 644 /usr/local/lib/duckdns/public-address.py

# ---------------------------------------------------------------------------
say "갱신 스크립트 설치"
cat > "$UPDATER" <<'SCRIPT'
#!/usr/bin/env bash
#
# Tell DuckDNS where this machine is — when that has changed.

set -euo pipefail
. /etc/duckdns/config

STATE=/var/lib/duckdns/published
LOG=/var/log/duckdns.log

# Published again after this long even when nothing changed: a name nobody
# speaks to is eventually removed, and a daily word keeps this one.
STALE_SECONDS=$((24 * 60 * 60))

note() {
    printf '%s  %s  %s\n' "$(date -Is)" "$DOMAIN" "$*" >> "$LOG"
}

publish() {
    local why="$1"
    local address="${2:-unknown}"
    local answer
    # The address is left for DuckDNS to work out from the request rather than
    # being sent: whatever address it sees is the one the rest of the internet
    # will use to reach this machine, and anything found here is a guess at it.
    answer=$(curl --silent --show-error --max-time 20 --retry 2 \
        "https://www.duckdns.org/update?domains=${DOMAIN}&token=${TOKEN}&ip=" \
        || echo "unreachable")
    note "$why -> $answer"
    # "OK" means the record is right; "KO" means the name or the token is
    # wrong, and a wrong token fails silently for ever unless something says so.
    if [ "$answer" = "OK" ]; then
        install -d -m 700 "$(dirname "$STATE")"
        printf '%s\n' "$address" > "$STATE"
        return 0
    fi
    return 1
}

now=$(python3 /usr/local/lib/duckdns/public-address.py 2>/dev/null || true)
was=$(cat "$STATE" 2>/dev/null || true)
age=$(( $(date +%s) - $(stat -c %Y "$STATE" 2>/dev/null || echo 0) ))

if [ -z "$now" ]; then
    # Nothing local to compare against, so say where we are and be done.
    publish "no metadata" "unknown"
elif [ "$now" != "$was" ]; then
    publish "address is now $now (was ${was:-none})" "$now"
elif [ "$age" -ge "$STALE_SECONDS" ]; then
    publish "unchanged, keeping the record alive" "$now"
fi
SCRIPT
chmod 755 "$UPDATER"
touch "$LOG"
chmod 640 "$LOG"

# ---------------------------------------------------------------------------
say "타이머 설치"
cat > /etc/systemd/system/duckdns.service <<'UNIT'
[Unit]
Description=Point a DuckDNS name at this machine
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/duckdns-update
UNIT

cat > /etc/systemd/system/duckdns.timer <<'UNIT'
[Unit]
Description=Keep the DuckDNS name current

[Timer]
# Once shortly after boot, because that is when the address is most likely to
# have changed, and then every five minutes. The check is a local read, so the
# interval costs nothing — five minutes is simply the longest the name should
# ever be wrong for.
OnBootSec=30s
OnUnitActiveSec=5min
Persistent=true

[Install]
WantedBy=timers.target
UNIT

systemctl daemon-reload
systemctl enable --now duckdns.timer

# ---------------------------------------------------------------------------
say "지금 한 번 갱신"
# The state file is cleared so this first run really does publish, rather than
# deciding nothing has changed and telling the user nothing.
rm -f /var/lib/duckdns/published
if "$UPDATER"; then
  echo "성공"
else
  echo "실패 — 도메인 이름과 토큰을 확인하세요"
  tail -3 "$LOG"
  exit 1
fi

# ---------------------------------------------------------------------------
say "확인"
echo "이름      : ${DOMAIN}.duckdns.org"
echo "가리키는 곳: $(getent hosts "${DOMAIN}.duckdns.org" | awk '{print $1}' | head -1)"
echo "이 서버   : $(python3 /usr/local/lib/duckdns/public-address.py || echo '알 수 없음')"
echo "타이머    : $(systemctl is-active duckdns.timer)"
echo "갱신 방식 : 5분마다 주소를 확인하고, 바뀌었을 때만 DuckDNS에 알립니다 (하루 한 번은 무조건)"
echo "기록      : $LOG"
echo
echo "위 두 주소가 같으면 됐습니다."
echo "이제 Veil 링크의 주소를 ${DOMAIN}.duckdns.org 로 바꾸세요 —"
echo "클라이언트는 접속할 때마다 이름을 다시 풀기 때문에, 서버 IP가 바뀌어도 따라갑니다."

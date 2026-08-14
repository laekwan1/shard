#!/usr/bin/env bash
#
# Turn this instance's public address into one it keeps.
#
# An Oracle instance is given an *ephemeral* public address by default, and an
# ephemeral address is only borrowed: stopping the instance, moving it, or a
# reassignment on Oracle's side can all give it a different one. Every phone
# carrying a link with the old address in it is then pointed somewhere else.
#
# A *reserved* address belongs to the account rather than to the instance, and
# stays put — including across a stop and start. It is inside the free tier.
#
# This is the belt to DuckDNS's braces: the reserved address means it should
# never change, and the name means the phones follow it if it ever does.
#
# Usage (on the server, or anywhere the OCI CLI is configured):
#   bash reserve-oracle-ip.sh
#
# Needs the OCI CLI, set up once with `oci setup config`. If that is more
# trouble than it is worth, the console does the same thing in four clicks —
# the steps are printed at the end.

set -euo pipefail

say() { printf '\n\033[1;33m==> %s\033[0m\n' "$*"; }

console_steps() {
  cat <<'STEPS'

콘솔에서 하는 방법 (CLI 없이):

  1. 인스턴스 페이지 → 리소스 → 연결된 VNIC → VNIC 이름 클릭
  2. 리소스 → IPv4 주소 → 기본 주소 행의 ⋮ → "편집"
  3. 공용 IP 유형에서 "예약된 공용 IP" 선택
     - "새 예약된 IP 생성"을 고르고 이름을 지정하면 지금 쓰는 주소가 그대로 예약됩니다
  4. 업데이트

  주의: 잠시 연결이 끊겼다가 돌아옵니다. 주소 자체는 바뀌지 않습니다.

STEPS
}

# ---------------------------------------------------------------------------
say "OCI CLI 확인"
if ! command -v oci >/dev/null 2>&1; then
  echo "OCI CLI가 없습니다."
  console_steps
  exit 0
fi

# The instance can ask the platform who it is; nothing has to be typed in.
say "이 인스턴스 확인"
METADATA=http://169.254.169.254/opc/v2
INSTANCE=$(curl --silent --max-time 5 -H 'Authorization: Bearer Oracle' "$METADATA/instance/id" || true)
if [ -z "$INSTANCE" ]; then
  echo "인스턴스 메타데이터를 읽지 못했습니다 — 이 스크립트는 서버 위에서 실행해야 합니다."
  console_steps
  exit 1
fi
echo "인스턴스: $INSTANCE"

VNIC=$(oci compute instance list-vnics --instance-id "$INSTANCE" \
  --query 'data[0].id' --raw-output)
echo "VNIC    : $VNIC"

PRIVATE=$(oci network private-ip list --vnic-id "$VNIC" \
  --query 'data[?"is-primary"].id | [0]' --raw-output)
echo "기본 주소: $PRIVATE"

# ---------------------------------------------------------------------------
say "현재 공용 주소 확인"
PUBLIC_JSON=$(oci network public-ip get-public-ip-by-private-ip-id \
  --private-ip-id "$PRIVATE" 2>/dev/null || echo '{}')
CURRENT=$(echo "$PUBLIC_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("ip-address",""))' 2>/dev/null || true)
LIFETIME=$(echo "$PUBLIC_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("lifetime",""))' 2>/dev/null || true)
PUBLIC_ID=$(echo "$PUBLIC_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("id",""))' 2>/dev/null || true)

echo "주소    : ${CURRENT:-없음}"
echo "종류    : ${LIFETIME:-알 수 없음}"

if [ "$LIFETIME" = "RESERVED" ]; then
  say "이미 예약된 주소입니다 — 할 일이 없습니다"
  exit 0
fi

if [ -z "$PUBLIC_ID" ]; then
  echo "공용 주소를 찾지 못했습니다."
  console_steps
  exit 1
fi

# ---------------------------------------------------------------------------
say "임시 주소를 예약 주소로 승격"
# Oracle promotes in place: the address stays the same and only its lifetime
# changes, so nothing that already points here has to be told anything.
oci network public-ip update --public-ip-id "$PUBLIC_ID" --lifetime RESERVED --force

say "확인"
AFTER=$(oci network public-ip get --public-ip-id "$PUBLIC_ID" \
  --query 'data.lifetime' --raw-output)
echo "주소    : $CURRENT"
echo "종류    : $AFTER"
if [ "$AFTER" = "RESERVED" ]; then
  echo
  echo "됐습니다. 이 주소는 인스턴스를 껐다 켜도 유지됩니다."
else
  echo
  echo "승격되지 않았습니다 — 콘솔에서 확인하세요."
  console_steps
fi

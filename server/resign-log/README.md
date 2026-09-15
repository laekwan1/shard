# Shard 재서명 이력 서버 (Phase 1)

각 기기의 앱이 재서명·발급·자체업데이트 때마다 이벤트를 POST 하면 SQLite에 쌓고, 브라우저로
이력을 조회한다. **죽은 폰은 말이 없어도**, 그 폰의 인증서를 폐기한 다른 폰의 `revoke` 이벤트가
서버에 남아 "누가 누구 인증서를 폐기했는지" 교차 대조로 검은화면 사망의 원인을 짚는다.

무의존(Python stdlib `http.server` + `sqlite3`)이라 Veil 박스에 `python3 server.py`만으로 뜬다.
pip 설치·프레임워크 없음. 스키마는 `events.detail`(JSON blob) + 열린 `event_type`라 마이그레이션
없이 확장된다.

## 1. 서버 띄우기 (Veil 박스에서 — 사용자가 실행)

```bash
export SHARD_LOG_TOKEN='<길고 랜덤한 토큰 — 직접 정한다>'   # 필수. 없으면 시작 거부.
export SHARD_LOG_DB=/var/lib/shard/resign-log.db          # 선택(기본 ./resign-log.db)
export SHARD_LOG_ADDR=127.0.0.1                            # 로컬만 열고 앞단 TLS 프록시 권장
export SHARD_LOG_PORT=8788
python3 server.py
```

> **토큰은 내가(Claude가) 절대 만들지도 보지도 않는다.** 사용자가 직접 정해 서버 환경변수와 앱
> 설정 양쪽에 같은 값을 넣는다. 커밋·대화에 토큰을 넣지 않는다(.gitignore가 `*.env`를 막는다).

### systemd 유닛 (권장 — 재부팅에도 살아남게)

`/etc/systemd/system/shard-resign-log.service`:

```ini
[Unit]
Description=Shard resign-log server
After=network.target

[Service]
Type=simple
# 토큰은 파일로 — 유닛에 평문으로 박지 않는다. EnvironmentFile은 0600으로.
EnvironmentFile=/etc/shard/resign-log.env
WorkingDirectory=/opt/shard/resign-log
ExecStart=/usr/bin/python3 /opt/shard/resign-log/server.py
Restart=on-failure
RestartSec=3
User=shard
Group=shard

[Install]
WantedBy=multi-user.target
```

`/etc/shard/resign-log.env` (권한 `chmod 600`):

```
SHARD_LOG_TOKEN=<사용자가 정한 토큰>
SHARD_LOG_DB=/var/lib/shard/resign-log.db
SHARD_LOG_ADDR=127.0.0.1
SHARD_LOG_PORT=8788
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now shard-resign-log
sudo systemctl status shard-resign-log
```

### 앞단 TLS (권장)

`SHARD_LOG_ADDR=127.0.0.1`로 로컬만 열고, 이미 Veil 박스에 있는 리버스 프록시(caddy/nginx)로
`https://<veil-host>/resign-log/ → 127.0.0.1:8788`을 프록시한다. 앱은 https URL만 신뢰한다.

## 2. 앱 쪽 설정 (기기마다 — 앱 화면에서 입력, PC 불필요)

앱의 `state_dir`은 iOS **Application Support** 폴더라 파일앱으로 넣을 수 없다. 그래서 앱 안에서
입력하면 앱이 `resign_log_url.txt`(1줄 URL, 2줄 토큰)를 대신 써 준다(anisette 서버와 같은 방식).

1. 앱 → **자체 서명(재서명) 화면** → 계정이 이미 있으면 **"변경"**을 눌러 설정칸을 편다.
2. **이력 서버 URL**: `https://<veil-host>/resign-log/events` (반드시 `/events`로 끝. `http`로 시작).
3. **이력 서버 토큰**: 서버의 `SHARD_LOG_TOKEN`과 **똑같은 값**.
4. 입력하면 즉시 저장된다. URL을 **비우면 전송 OFF**(온디바이스 이력만, `resign-history.jsonl`).

- 전송은 **재서명할 때**와 **앱을 켤 때(.active)** 자동으로, 미전송분만 배치로 밀어 보낸다.
  실패해도 재서명을 절대 안 깨고(무해·논블로킹), 다음 기회에 다시 시도한다(워터마크로 중복 방지).
- 온디바이스 이력만 볼 거면 서버 없이 재서명 화면의 **'로그' 버튼**만 눌러도 된다.

## 3. 조회

- `GET /`                     — HTML 대시보드(기기 목록 + 최근 100 이벤트). Bearer 인증.
- `GET /events?device=&limit=` — JSON 이벤트 목록.
- `GET /revoked/<serial>`      — 그 시리얼을 폐기한 이벤트들(**누가 죽였나** 교차 대조).
- `GET /health`               — 무인증 헬스체크 `{"ok":true}`.

브라우저로 대시보드를 열 땐 `Authorization: Bearer <토큰>` 헤더가 필요하므로, 프록시에서 접근을
제한하거나 헤더를 붙이는 확장/도구로 연다.

## 왜 이 설계인가 (확장성)

- **서버 전송이 본체**: 폰이 검은화면으로 죽으면 온디바이스 이력은 못 본다. 그래서 살아 있을 때
  주기적으로(재서명·앱 켤 때) 서버로 밀어 둔다. 죽은 뒤엔 서버 기록 + 다른 폰의 `revoke` 이벤트로
  원인을 역추적한다.
- **`detail` JSON 컬럼 + 열린 `event_type`**: 새 필드/이벤트 종류를 서버 스키마 변경 없이 늘린다
  (Phase 2에서 기기 상태 스냅샷·프로파일 만료 알림 등 추가 가능).
- **비밀번호는 어디에도 안 남긴다**: 이벤트 모델(ResignEvent)에 Apple ID 비밀번호 필드가 없다.

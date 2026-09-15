#!/usr/bin/env python3
"""Shard 재서명 이력 수집 서버 — 무의존(stdlib http.server + sqlite3).

각 기기의 앱이 재서명/발급/업데이트 때마다 이벤트를 POST 하면 SQLite에 쌓고, 브라우저로 이력을
조회한다. 죽은 폰은 말이 없어도, 그 폰을 죽인 다른 폰의 revoke 이벤트가 남아 "누가 누구 인증서를
폐기했는지" 교차 대조로 결함을 짚을 수 있다(server-side correlation).

왜 무의존 stdlib인가: Veil 박스에 `python3 server.py`만으로 뜨게 — pip 설치·프레임워크 없이 배포.
스키마는 events.detail(JSON blob) + 열린 event_type라 마이그레이션 없이 확장된다.

환경변수:
  SHARD_LOG_TOKEN   필수. 쓰기(POST)·조회(GET) 인증용 Bearer 토큰. 없으면 시작 거부.
  SHARD_LOG_DB      SQLite 경로(기본 ./resign-log.db)
  SHARD_LOG_ADDR    바인드 주소(기본 127.0.0.1) — Veil에선 로컬만 열고 앞단에 TLS 리버스프록시 권장
  SHARD_LOG_PORT    포트(기본 8788)

엔드포인트:
  POST /events        Bearer 인증. body = 이벤트 1개(객체) 또는 배열. {"ok":true,"stored":N} 반환.
  GET  /events?device=&limit=   Bearer 인증. JSON 이벤트 목록.
  GET  /revoked/<serial>        Bearer 인증. 그 시리얼을 폐기한 이벤트들(교차 대조).
  GET  /                        Bearer 인증. HTML 대시보드(기기·최근 이벤트).
  GET  /health                  무인증. {"ok":true}.
"""
import json
import os
import sqlite3
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs, unquote

DB_PATH = os.environ.get("SHARD_LOG_DB", "resign-log.db")
TOKEN = os.environ.get("SHARD_LOG_TOKEN", "")
ADDR = os.environ.get("SHARD_LOG_ADDR", "127.0.0.1")
PORT = int(os.environ.get("SHARD_LOG_PORT", "8788"))

SCHEMA = """
CREATE TABLE IF NOT EXISTS devices (
  device_id   TEXT PRIMARY KEY,
  name        TEXT,
  apple_email TEXT,
  first_seen  INTEGER,
  last_seen   INTEGER,
  last_ios    TEXT,
  last_build  INTEGER
);
CREATE TABLE IF NOT EXISTS events (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id     TEXT NOT NULL,
  ts            REAL NOT NULL,
  received_at   REAL NOT NULL,
  event_type    TEXT NOT NULL,
  op            TEXT,
  app_build     INTEGER,
  ios_version   TEXT,
  result        TEXT,
  step          TEXT,
  error         TEXT,
  cert_issued   TEXT,
  certs_revoked TEXT,          -- JSON array
  profile_expiry REAL,
  resign_stamp  REAL,
  detail        TEXT           -- JSON blob (확장)
);
CREATE INDEX IF NOT EXISTS ix_ev_dev_ts ON events(device_id, ts);
CREATE INDEX IF NOT EXISTS ix_ev_type ON events(event_type);
CREATE INDEX IF NOT EXISTS ix_ev_result ON events(result);
"""

def db():
    c = sqlite3.connect(DB_PATH)
    c.row_factory = sqlite3.Row
    return c

def init_db():
    c = db()
    c.executescript(SCHEMA)
    c.commit()
    c.close()

# events.certs_revoked는 JSON 배열 문자열로 저장하되, 시리얼 조회를 위해 LIKE로도 찾을 수 있게 둔다.
EVENT_COLS = ["device_id", "ts", "event_type", "op", "app_build", "ios_version",
              "result", "step", "error", "cert_issued", "certs_revoked",
              "profile_expiry", "resign_stamp", "detail"]

def store_events(items):
    c = db()
    now = time.time()
    n = 0
    for ev in items:
        if not isinstance(ev, dict):
            continue
        dev = str(ev.get("device_id") or "").strip()
        if not dev:
            continue
        revoked = ev.get("certs_revoked")
        revoked_s = json.dumps(revoked) if revoked is not None else None
        detail = ev.get("detail")
        detail_s = detail if isinstance(detail, str) else (json.dumps(detail) if detail is not None else None)
        ts = float(ev.get("ts") or now)
        c.execute(
            """INSERT INTO events
               (device_id, ts, received_at, event_type, op, app_build, ios_version,
                result, step, error, cert_issued, certs_revoked, profile_expiry, resign_stamp, detail)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
            (dev, ts, now, str(ev.get("event_type") or "unknown"), ev.get("op"),
             ev.get("app_build"), ev.get("ios_version"), ev.get("result"), ev.get("step"),
             ev.get("error"), ev.get("cert_issued"), revoked_s,
             ev.get("profile_expiry"), ev.get("resign_stamp"), detail_s),
        )
        # 기기 최신 상태 upsert
        c.execute(
            """INSERT INTO devices (device_id, name, apple_email, first_seen, last_seen, last_ios, last_build)
               VALUES (?,?,?,?,?,?,?)
               ON CONFLICT(device_id) DO UPDATE SET
                 name=COALESCE(excluded.name, devices.name),
                 apple_email=COALESCE(excluded.apple_email, devices.apple_email),
                 last_seen=excluded.last_seen,
                 last_ios=COALESCE(excluded.last_ios, devices.last_ios),
                 last_build=COALESCE(excluded.last_build, devices.last_build)""",
            (dev, ev.get("device_name"), ev.get("apple_email"), ts, ts,
             ev.get("ios_version"), ev.get("app_build")),
        )
        n += 1
    c.commit()
    c.close()
    return n

def query_events(device=None, limit=200):
    c = db()
    if device:
        rows = c.execute("SELECT * FROM events WHERE device_id=? ORDER BY ts DESC LIMIT ?",
                         (device, limit)).fetchall()
    else:
        rows = c.execute("SELECT * FROM events ORDER BY ts DESC LIMIT ?", (limit,)).fetchall()
    c.close()
    return [dict(r) for r in rows]

def query_revoked(serial):
    """이 시리얼을 폐기한 이벤트들 — 죽은 기기의 인증서를 누가 언제 폐기했는지 교차 대조."""
    c = db()
    rows = c.execute(
        "SELECT * FROM events WHERE certs_revoked LIKE ? ORDER BY ts DESC LIMIT 200",
        ("%" + serial + "%",)).fetchall()
    c.close()
    return [dict(r) for r in rows]

def esc(s):
    return (str(s) if s is not None else "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

def dashboard_html():
    c = db()
    devs = c.execute("SELECT * FROM devices ORDER BY last_seen DESC").fetchall()
    evs = c.execute("SELECT * FROM events ORDER BY ts DESC LIMIT 100").fetchall()
    c.close()
    def tfmt(t):
        try:
            return time.strftime("%m-%d %H:%M:%S", time.localtime(float(t)))
        except Exception:
            return esc(t)
    rows = []
    for d in devs:
        rows.append(f"<tr><td>{esc(d['device_id'])[:12]}</td><td>{esc(d['name'])}</td>"
                    f"<td>{esc(d['apple_email'])}</td><td>{tfmt(d['last_seen'])}</td>"
                    f"<td>iOS {esc(d['last_ios'])}</td><td>빌드 {esc(d['last_build'])}</td></tr>")
    erows = []
    for e in evs:
        color = "#c00" if e["result"] == "fail" else "#080"
        erows.append(
            f"<tr><td>{tfmt(e['ts'])}</td><td>{esc(e['device_id'])[:8]}</td>"
            f"<td>{esc(e['event_type'])}/{esc(e['op'])}</td><td>{esc(e['app_build'])}</td>"
            f"<td style='color:{color}'>{esc(e['result'])}</td><td>{esc(e['step'])}</td>"
            f"<td>{esc(e['cert_issued'])}</td><td>{esc(e['certs_revoked'])}</td>"
            f"<td>{esc(e['error'])[:80]}</td></tr>")
    return f"""<!doctype html><meta charset=utf-8><title>Shard 재서명 이력</title>
<style>body{{font:13px system-ui;margin:1.2rem;background:#0e1116;color:#e9eef4}}
table{{border-collapse:collapse;width:100%;margin:.5rem 0 1.5rem}}td,th{{border:1px solid #2a3340;padding:4px 8px;text-align:left}}
th{{background:#1c222c}}h2{{color:#2dd4bf}}</style>
<h2>기기 ({len(devs)})</h2><table><tr><th>id</th><th>이름</th><th>Apple ID</th><th>마지막</th><th>iOS</th><th>빌드</th></tr>{''.join(rows)}</table>
<h2>최근 이벤트 (100)</h2><table><tr><th>시각</th><th>기기</th><th>종류/작업</th><th>빌드</th><th>결과</th><th>단계</th><th>발급</th><th>폐기</th><th>에러</th></tr>{''.join(erows)}</table>
<p style=color:#94a3b8>폐기 시리얼로 누가 죽였는지: <code>GET /revoked/&lt;serial&gt;</code></p>"""

class Handler(BaseHTTPRequestHandler):
    def _auth(self):
        got = self.headers.get("Authorization", "")
        return got == f"Bearer {TOKEN}"

    def _send(self, code, body, ctype="application/json; charset=utf-8"):
        b = body.encode("utf-8") if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/health":
            return self._send(200, json.dumps({"ok": True}))
        if not self._auth():
            return self._send(401, json.dumps({"ok": False, "error": "unauthorized"}))
        q = parse_qs(u.query)
        if u.path == "/":
            return self._send(200, dashboard_html(), "text/html; charset=utf-8")
        if u.path == "/events":
            dev = q.get("device", [None])[0]
            lim = int(q.get("limit", ["200"])[0])
            return self._send(200, json.dumps({"ok": True, "events": query_events(dev, lim)}))
        if u.path.startswith("/revoked/"):
            serial = unquote(u.path[len("/revoked/"):])
            return self._send(200, json.dumps({"ok": True, "events": query_revoked(serial)}))
        return self._send(404, json.dumps({"ok": False, "error": "not found"}))

    def do_POST(self):
        u = urlparse(self.path)
        if not self._auth():
            return self._send(401, json.dumps({"ok": False, "error": "unauthorized"}))
        if u.path != "/events":
            return self._send(404, json.dumps({"ok": False, "error": "not found"}))
        try:
            n = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(n) if n > 0 else b""
            data = json.loads(raw.decode("utf-8")) if raw else []
        except Exception as e:
            return self._send(400, json.dumps({"ok": False, "error": f"bad json: {e}"}))
        items = data if isinstance(data, list) else [data]
        if len(items) > 500:
            items = items[:500]  # 방어적 상한
        try:
            stored = store_events(items)
        except Exception as e:
            return self._send(500, json.dumps({"ok": False, "error": str(e)}))
        return self._send(200, json.dumps({"ok": True, "stored": stored}))

    def log_message(self, *a):
        pass  # 접근 로그 소음 억제

def main():
    if not TOKEN:
        raise SystemExit("SHARD_LOG_TOKEN 환경변수가 필요합니다(쓰기/조회 인증). 설정 후 다시 실행.")
    init_db()
    srv = ThreadingHTTPServer((ADDR, PORT), Handler)
    print(f"Shard 재서명 이력 서버: http://{ADDR}:{PORT}  (DB={DB_PATH})")
    srv.serve_forever()

if __name__ == "__main__":
    main()

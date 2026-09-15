import SwiftUI
import UIKit

/// 재서명/발급/업데이트 이벤트 한 건. 온디바이스 이력(jsonl)과 서버 전송에 같은 모델을 쓴다.
/// **비밀번호는 절대 넣지 않는다.** 필드명은 서버(server/resign-log/server.py)와 1:1(snake_case).
/// 확장은 `detail`(로그 꼬리·향후 JSON)로 — 서버 스키마 변경 없이 늘린다.
struct ResignEvent: Codable, Identifiable {
    var id: String = UUID().uuidString
    var device_id: String
    var device_name: String?
    var apple_email: String?
    var ts: Double                 // unix 초
    var event_type: String         // "resign" | "issue" | "update" | ...
    var op: String?                // "foreground" | "silent-homelock" | "bgtask" | "version-update" | "manual" | "issue"
    var app_build: Int?
    var ios_version: String?
    var result: String             // "ok" | "fail"
    var step: String?              // 마지막 단계: "sign" | "install" | ...
    var error: String?
    var cert_issued: String?
    var certs_revoked: [String]?
    var profile_expiry: Double?
    var resign_stamp: Double?
    var detail: String?            // 로그 꼬리(진단용)
}

/// 온디바이스 이력 저장 + 서버 전송. **전부 실패-무해**(파일/네트워크 오류가 재서명을 절대 안 깨게 try?·무음).
/// - 저장: state_dir/resign-history.jsonl (한 줄 = 한 이벤트, 최근 300건 유지)
/// - 전송: state_dir/resign_log_url.txt (1줄=URL, 2줄=토큰)가 있으면 미전송분을 배치 POST. 없으면 OFF.
/// - 폰이 죽으면 온디바이스는 못 보므로 **서버 전송이 본체**; 전송은 재서명 때·앱 켤 때 자동.
enum ResignHistory {
    private static let cap = 300
    private static let sentKey = "resign.logSentThroughTs"

    private static func historyURL(_ stateDir: String) -> URL {
        URL(fileURLWithPath: stateDir).appendingPathComponent("resign-history.jsonl")
    }

    /// 기기 고유 id — UDID가 아니라 앱이 만든 UUID(사생활). state_dir에 1회 생성·재사용.
    static func deviceID(_ stateDir: String) -> String {
        let url = URL(fileURLWithPath: stateDir).appendingPathComponent("device-id.txt")
        if let s = try? String(contentsOf: url, encoding: .utf8) {
            let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
            if !t.isEmpty { return t }
        }
        let id = UUID().uuidString
        try? id.write(to: url, atomically: true, encoding: .utf8)
        return id
    }

    /// 서버 설정(있을 때만 전송). resign_log_url.txt: 1줄 URL(http/https), 2줄 쓰기 토큰(선택).
    static func serverConfig(_ stateDir: String) -> (url: URL, token: String)? {
        let f = URL(fileURLWithPath: stateDir).appendingPathComponent("resign_log_url.txt")
        guard let raw = try? String(contentsOf: f, encoding: .utf8) else { return nil }
        let parts = raw.split(whereSeparator: { $0 == "\n" || $0 == "\r" })
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        guard let first = parts.first, first.hasPrefix("http"), let u = URL(string: first) else { return nil }
        return (u, parts.count >= 2 ? parts[1] : "")
    }

    static func loadAll(stateDir: String) -> [ResignEvent] {
        guard let raw = try? String(contentsOf: historyURL(stateDir), encoding: .utf8) else { return [] }
        let dec = JSONDecoder()
        return raw.split(separator: "\n").compactMap { line in
            guard let d = line.data(using: .utf8) else { return nil }
            return try? dec.decode(ResignEvent.self, from: d)
        }
    }

    /// 이벤트 1건 기록: jsonl에 append(최근 cap건 유지) 후 서버로 전송 시도. 실패해도 무해.
    static func record(_ ev: ResignEvent, stateDir: String) {
        if let data = try? JSONEncoder().encode(ev), let s = String(data: data, encoding: .utf8) {
            let url = historyURL(stateDir)
            var lines = (try? String(contentsOf: url, encoding: .utf8))?
                .split(separator: "\n").map(String.init) ?? []
            lines.append(s.replacingOccurrences(of: "\n", with: " "))
            if lines.count > cap { lines = Array(lines.suffix(cap)) }
            try? lines.joined(separator: "\n").write(to: url, atomically: true, encoding: .utf8)
        }
        flush(stateDir: stateDir)
    }

    /// 미전송 이벤트(ts > 마지막 전송)를 배치로 서버에 POST. 200이면 워터마크 전진. 논블로킹·무음.
    static func flush(stateDir: String) {
        guard let cfg = serverConfig(stateDir) else { return }
        let sentThrough = UserDefaults.standard.double(forKey: sentKey)
        let pending = loadAll(stateDir: stateDir).filter { $0.ts > sentThrough }
        guard !pending.isEmpty, let body = try? JSONEncoder().encode(pending) else { return }
        let maxTs = pending.map { $0.ts }.max() ?? sentThrough
        var req = URLRequest(url: cfg.url)
        req.httpMethod = "POST"
        req.timeoutInterval = 15
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if !cfg.token.isEmpty { req.setValue("Bearer \(cfg.token)", forHTTPHeaderField: "Authorization") }
        req.httpBody = body
        URLSession.shared.dataTask(with: req) { _, resp, _ in
            if let h = resp as? HTTPURLResponse, h.statusCode == 200 {
                UserDefaults.standard.set(maxTs, forKey: sentKey)  // 성공분까지만 전진(재전송 방지)
            }
        }.resume()
    }
}

/// 재서명 화면의 '로그' 버튼이 띄우는 **별도 시트**(재서명 화면엔 인라인으로 안 뿌린다 — 화면 깔끔하게).
/// 온디바이스 이력을 최신순으로 보여준다. 전송 설정이 있으면 상단에 "서버 전송 켜짐"도 표시.
struct ResignLogView: View {
    let events: [ResignEvent]
    let serverOn: Bool
    @Environment(\.dismiss) private var dismiss

    private func tfmt(_ t: Double) -> String {
        let f = DateFormatter(); f.dateFormat = "MM/dd HH:mm:ss"
        return f.string(from: Date(timeIntervalSince1970: t))
    }

    var body: some View {
        NavigationView {
            List {
                if events.isEmpty {
                    Text("아직 재서명 이력이 없습니다.").foregroundColor(.secondary)
                }
                ForEach(events.reversed()) { e in   // 최신이 위
                    VStack(alignment: .leading, spacing: 3) {
                        HStack {
                            Text(tfmt(e.ts)).font(.caption).foregroundColor(.secondary)
                            Text("\(e.event_type)\(e.op.map { "/\($0)" } ?? "")")
                                .font(.caption.weight(.semibold))
                            Spacer()
                            Text(e.result == "ok" ? "성공" : "실패")
                                .font(.caption.weight(.bold))
                                .foregroundColor(e.result == "ok" ? .green : .red)
                        }
                        HStack(spacing: 8) {
                            if let b = e.app_build { Text("빌드 \(b)").font(.caption2).foregroundColor(.secondary) }
                            if let s = e.step { Text("단계 \(s)").font(.caption2).foregroundColor(.secondary) }
                            if let ios = e.ios_version { Text("iOS \(ios)").font(.caption2).foregroundColor(.secondary) }
                        }
                        if let issued = e.cert_issued { Text("발급 \(issued)").font(.caption2).foregroundColor(.blue) }
                        if let rev = e.certs_revoked, !rev.isEmpty {
                            Text("폐기 \(rev.joined(separator: ", "))").font(.caption2).foregroundColor(.orange)
                        }
                        if let err = e.error, !err.isEmpty {
                            Text(err).font(.caption2).foregroundColor(.red).lineLimit(3)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
            .navigationTitle("재서명 이력")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Text(serverOn ? "서버 전송 켜짐" : "로컬만").font(.caption).foregroundColor(.secondary)
                }
                ToolbarItem(placement: .topBarTrailing) { Button("닫기") { dismiss() } }
            }
        }
    }
}

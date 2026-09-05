import SwiftUI
import UIKit  // UIApplication/UIResponder — 키보드 내리기(hideKeyboard). SwiftUI가 늘 재노출하진 않음.
import UniformTypeIdentifiers
import Darwin  // freopen/setvbuf/stderr/_IONBF — idevice C stderr를 파일로 붙잡으려고(④ 진단)

// 현재 앱 서명의 남은 유효기간. 앱 번들의 `embedded.mobileprovision`(현재 서명한 도구가 넣은 것 —
// 지금은 SideStore/Sideloadly, 나중엔 우리 엔진)의 ExpirationDate를 읽는다. 모래시계가 이 값을 담는다.
enum SigningInfo {
    /// 현재 서명의 만료일(없으면 nil — 시뮬레이터/미서명 등).
    static func expirationDate() -> Date? {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let plist = plistFromMobileprovision(data),
              let exp = plist["ExpirationDate"] as? Date
        else { return nil }
        return exp
    }

    /// 남은 일수(**올림** — 만료 순간까지 'N일 남음'으로 센다). 갓 서명(≈7일)이면 7. 만료면 0 이하.
    /// 내림이면 6.9일도 '6일'로 나와 사용자가 갓 설치인데 6일로 본다(지적) → 올림으로 7일.
    static func daysLeft() -> Int? {
        guard let exp = expirationDate() else { return nil }
        return Int(ceil(exp.timeIntervalSinceNow / 86400))
    }

    /// 남은 '모래 양' 비율(0…1) — 무료 프로파일 유효기간 7일 대비 **실제 남은 시간**(정수 일수가
    /// 아니라 초 단위). 정수 일수(6)로 나누면 갓 설치도 6/7≈0.86이라 안 차 보였다(지적) → 초 단위로
    /// 해 갓 서명(≈6.99일)은 ≈1.0(가득)에서 시작해 만료까지 매끄럽게 줄어든다.
    static func fraction() -> CGFloat {
        guard let exp = expirationDate() else { return 0 }
        let full = 7.0 * 86400.0
        return CGFloat(min(max(exp.timeIntervalSinceNow / full, 0), 1))
    }

    // mobileprovision은 CMS로 감싼 XML plist다. engine.rs의 plist_from_mobileprovision과 같은 방식으로
    // <?xml … </plist> 구간을 스캔해 파싱한다(전체 CMS 파싱 대신).
    private static func plistFromMobileprovision(_ data: Data) -> [String: Any]? {
        let startIdx = data.range(of: Data("<?xml".utf8))?.lowerBound
            ?? data.range(of: Data("<plist".utf8))?.lowerBound
        guard let start = startIdx,
              let end = data.range(of: Data("</plist>".utf8))?.upperBound
        else { return nil }
        let slice = data.subdata(in: start..<end)
        return try? PropertyListSerialization.propertyList(from: slice, options: [], format: nil)
            as? [String: Any]
    }
}

// 발급에 성공한 Apple ID(팀·App ID)를 기억한다 — 어떤 계정으로 서명받았는지 관리·표시하려고.
// UserDefaults에 JSON으로 저장(계정 수가 소수라 충분). 비밀번호는 절대 저장하지 않는다.
struct SignedAccount: Codable, Identifiable {
    var email: String
    var teamId: String
    var appId: String
    var date: Date
    var id: String { email }
}

// Apple ID 비밀번호를 **앱 컨테이너 파일**에 저장한다(사용자 요청: 매번 안 치고 "지금 갱신"만).
// Keychain을 먼저 썼으나 사이드로드 앱에선 접근 그룹 문제로 저장이 안 됐다(사용자 확인). 그래서 앱
// 샌드박스 파일로 — 이 앱만 읽고, `.completeFileProtection`으로 기기 잠금 시 암호화된다. 코드베이스가
// gstoken/anisette 캐시도 컨테이너 파일로 두는 것과 같은 결. 계정별로 담아 계정 변경도 처리한다.
// Claude가 값을 만지지 않는다 — 사용자의 앱이 사용자의 비밀번호를 사용자 기기에 기억하는 기능일 뿐.
enum PasswordStore {
    private static var fileURL: URL? {
        let fm = FileManager.default
        guard let dir = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else { return nil }
        try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("resign_pw.json")
    }

    private static func loadMap() -> [String: String] {
        guard let url = fileURL, let data = try? Data(contentsOf: url),
              let map = try? JSONSerialization.jsonObject(with: data) as? [String: String]
        else { return [:] }
        return map
    }

    static func save(_ password: String, for email: String) {
        let key = email.lowercased()
        guard !key.isEmpty, let url = fileURL else { return }
        var map = loadMap()
        if password.isEmpty { map.removeValue(forKey: key) } else { map[key] = password }
        guard let data = try? JSONSerialization.data(withJSONObject: map) else { return }
        try? data.write(to: url, options: [.atomic, .completeFileProtection])
    }

    static func load(for email: String) -> String {
        loadMap()[email.lowercased()] ?? ""
    }
}

enum SignedAccountStore {
    private static let key = "resign.accounts"

    static func load() -> [SignedAccount] {
        guard let data = UserDefaults.standard.data(forKey: key),
              let list = try? JSONDecoder().decode([SignedAccount].self, from: data)
        else { return [] }
        return list
    }

    static func save(_ list: [SignedAccount]) {
        if let data = try? JSONEncoder().encode(list) {
            UserDefaults.standard.set(data, forKey: key)
        }
    }

    /// 같은 이메일이면 갱신, 아니면 추가(최신이 위로). 반환은 갱신된 목록.
    static func upsert(_ acct: SignedAccount) -> [SignedAccount] {
        var list = load()
        list.removeAll { $0.email.caseInsensitiveCompare(acct.email) == .orderedSame }
        list.insert(acct, at: 0)
        save(list)
        return list
    }

    static func remove(email: String) -> [SignedAccount] {
        var list = load()
        list.removeAll { $0.email.caseInsensitiveCompare(email) == .orderedSame }
        save(list)
        return list
    }
}

// iOS 자체 서명 엔진(Rust `resign`)을 부르는 Swift 층.
//
// 현재 단계는 **인증서 발급**: .ipa 서명·설치 없이 로그인 → 인증서 → App ID → 프로파일 발급까지
// 애플 실서버로 수행한다(shard_resign_verify). 가장 어려운 인증·발급이 폰에서 되는지가 먼저다.
// 성공한 계정은 SignedAccount로 기억해 체크 표시로 관리한다.
//
// C ABI는 동기라 백그라운드 스레드에서 부르고, 2FA 코드는 세마포어로 UI에서 받아 넘긴다.
// 로그 콜백은 진행 상황을 화면에 스트리밍한다.

final class ResignModel: ObservableObject {
    @Published var logLines: [String] = []
    @Published var running = false
    @Published var summary: String?
    @Published var errorText: String?
    @Published var needs2FA = false
    // 발급에 성공한 계정 목록(체크 표시로 관리). 시작 시 저장소에서 읽는다.
    @Published var accounts: [SignedAccount] = SignedAccountStore.load()

    private let tfaSem = DispatchSemaphore(value: 0)
    private var tfaCode = ""
    // 방금 시도한 이메일 — 성공하면 이 값으로 계정을 기록한다(run이 받은 걸 finish가 씀).
    private var lastEmail = ""
    private var tfaCPtr: UnsafeMutablePointer<CChar>?

    // anisette·세션 캐시 폴더(앱 컨테이너). 기기별로 유지된다.
    private var stateDir: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("resign", isDirectory: true)
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return base.path
    }

    // ④ 페어링 파일(신뢰 자격) — state_dir에 저장. 폰마다 고유라 내장 불가, 1회 임포트해 재사용.
    var pairingURL: URL {
        URL(fileURLWithPath: stateDir).appendingPathComponent("pairing.plist")
    }
    var hasPairing: Bool { FileManager.default.fileExists(atPath: pairingURL.path) }

    /// Files에서 고른 페어링 파일을 state_dir에 복사(보안 스코프 처리). hasPairing 갱신을 알린다.
    func importPairing(from url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        objectWillChange.send()
        guard let data = try? Data(contentsOf: url) else {
            errorText = "페어링 파일을 읽지 못했습니다"; return
        }
        do { try data.write(to: pairingURL) } catch {
            errorText = "페어링 저장 실패: \(error.localizedDescription)"; return
        }
        summary = "페어링 파일 임포트 완료"
    }

    /// 전용 anisette 서버 주소를 state_dir에 저장 → Rust auth.rs가 `anisette_url.txt`로 읽는다. 비우면
    /// 파일을 지워 기본 공유서버(ani.sidestore.io)로 되돌린다. 전용 서버 = 이 계정만 쓰는 고정 기기
    /// 정체성 = 세션 복원 됨 + 계정 잠금 감소(공유 서버의 회전·공유 정체성이 반복 잠금의 근본 원인이었다).
    func saveAnisetteURL(_ s: String) {
        let url = URL(fileURLWithPath: stateDir).appendingPathComponent("anisette_url.txt")
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.isEmpty {
            try? FileManager.default.removeItem(at: url)
        } else {
            try? t.write(to: url, atomically: true, encoding: .utf8)
        }
    }

    /// 로그 파일에서 keyword에 맞는 줄만 골라 마지막 몇 줄을 준다(맞는 게 없으면 그냥 tail). 줄당 150자 컷.
    /// 폰에선 파일을 못 빼므로 UI로 끌어올려 근거로 다음 수를 정한다(추측 금지).
    private func fileTail(_ url: URL, maxLines: Int, keywords: [String]) -> [String] {
        guard let text = try? String(contentsOf: url, encoding: .utf8), !text.isEmpty else {
            return ["(\(url.lastPathComponent) 없음/비어있음)"]
        }
        let lines = text.split(separator: "\n").map(String.init)
        let picked = lines.filter { line in keywords.contains { line.contains($0) } }
        let chosen = (picked.isEmpty ? lines : picked).suffix(maxLines)
        return chosen.map { $0.count > 150 ? String($0.suffix(150)) : $0 }
    }

    /// minimuxer.log(minimuxer Info + idevice Debug): ready() 매 폴의 "…success: false…" 브레이크다운.
    private func minimuxerLogTail(sd: String, maxLines: Int = 8) -> [String] {
        fileTail(URL(fileURLWithPath: sd).appendingPathComponent("minimuxer.log"), maxLines: maxLines,
                 keywords: ["not ready", "rror", "arn", "eartbeat", "air", "SSL", "ssl", "handshake",
                            "ockdown", "refused", "timed out", "Invalid", "HostID", "denied"])
    }

    /// ④ 공통 — libusbmuxd를 minimuxer 내부 muxer(127.0.0.1:27015)로 향하게 한 뒤 start→ready까지 올린다.
    /// **`target_minimuxer_address()`를 `start()` 전에 부르지 않으면** libusbmuxd가 기본 usbmuxd
    /// 소켓(iOS엔 없음)을 봐서 `fetch_first_device()`가 실패 → `ready()`가 영원히 false다. 이 한 줄이
    /// ④의 make-or-break였다(폰: "start OK"인데 "기기 준비 안 됨"으로 대기 초과). 반환 (ready, 진단문).
    private func minimuxerReady(pairingPath: String, sd: String) throws -> (Bool, String) {
        // libimobiledevice/idevice의 C-레벨 상세(핸드셰이크/SSL/lockdown 원시 에러)는 stderr로 나가는데
        // minimuxer가 그걸 UnknownError로 뭉갠다. 부르기 전에 stderr를 파일로 돌려(freopen) 원시 이유를 붙잡는다.
        let cErrPath = URL(fileURLWithPath: sd).appendingPathComponent("idevice_stderr.log").path
        freopen(cErrPath, "w", stderr); setvbuf(stderr, nil, _IONBF, 0)
        let pairing = try String(contentsOf: URL(fileURLWithPath: pairingPath), encoding: .utf8)
        set_debug(true)                            // idevice_set_debug_level(1) → 위 stderr 파일에 상세가 쌓인다
        target_minimuxer_address()                 // USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015 심음 — 이게 없으면 아래 start는 떠도 기기를 못 찾는다
        try start(pairing, "file://" + sd)         // 내부 muxer가 10.7.0.1:62078(터널 너머 기기)로 다리를 놓음. STARTED 플래그로 중복 호출 안전.
        DispatchQueue.main.async { self.logLines.append("start OK — 기기 준비 대기(하트비트)...") }
        var okReady = false
        for _ in 0..<40 { if ready() { okReady = true; break }; Thread.sleep(forTimeInterval: 0.25) }
        if okReady { return (true, "") }
        // 실패: 세 신호로 어디서 막혔는지 가르고, minimuxer.log(진짜 이유)를 UI로 끌어올린다.
        let tunnel = test_device_connection()      // 10.7.0.1:62078 직접 TCP (페어링 무관 — 터널만 봄)
        let udid = fetch_udid()?.toString()        // libusbmuxd→내부 muxer로 기기가 잡히나(여기부턴 페어링 SSL 필요)
        let mmTail = self.minimuxerLogTail(sd: sd)
        let cErr = URL(fileURLWithPath: sd).appendingPathComponent("idevice_stderr.log")
        let cTail = self.fileTail(cErr, maxLines: 14,
                                  keywords: ["ERROR", "rror", "SSL", "ssl", "andshake", "ockdown",
                                             "air", "efus", "denied", "ervice", "rust", "scrow",
                                             "HostID", "Invalid", "onnect"])
        DispatchQueue.main.async {
            self.logLines.append("── 진단: TCP(터널) \(tunnel ? "열림" : "막힘") · 기기UDID \(udid ?? "못 찾음") ──")
            self.logLines.append("── idevice(C) stderr — 진짜 이유 ──")
            for l in cTail { self.logLines.append(l) }
            self.logLines.append("── minimuxer.log ──")
            for l in mmTail { self.logLines.append(l) }
        }
        let diag: String
        if !tunnel { diag = "터널로 기기(10.7.0.1:62078) 못 닿음 — LocalDevVPN 연결 확인" }
        else if udid == nil { diag = "터널은 열렸는데 페어링 SSL로 기기를 못 잡음 — 페어링 재발급" }
        else { diag = "하트비트 lockdown 실패 — 아래 idevice(C) stderr가 진짜 이유" }
        return (false, diag)
    }

    /// ④ RSD 연결 테스트 (iOS 17+) — classic lockdown이 죽은 iOS 26의 진짜 경로. rppairing 터널(직접
    /// TCP→RemotePairing→TLS-PSK→jktcp 어댑터)을 세우고 **터널 안** RSD 서비스 목록을 확인한다.
    /// **RP 페어링 파일 필요**(idevice_pair 발급, classic과 다름). 포트는 StikDebug 기본 49152.
    func rsdProbe(addr: String) {
        guard !running, hasPairing else { return }
        running = true
        logLines = []; summary = nil; errorText = nil
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let pairingPath = pairingURL.path
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = addr.withCString { a in pairingPath.withCString { p in
                shard_rsd_probe(a, 49152, p, ResignModel.logCb, ctx)
            } }
            let json = raw.map { String(cString: $0) } ?? #"{"ok":false,"error":"응답 없음"}"#
            if let raw = raw { shard_string_free(raw) }
            DispatchQueue.main.async {
                self.running = false
                if let d = json.data(using: .utf8),
                   let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                    if (obj["ok"] as? Bool) == true { self.summary = obj["path"] as? String }
                    else { self.errorText = obj["error"] as? String ?? "알 수 없는 오류" }
                } else { self.errorText = "응답 파싱 실패" }
            }
        }
    }

    /// ④ (구) 연결 테스트 — minimuxer(classic lockdown). iOS 26에서 QueryType RST로 막다른 길 확정.
    /// RSD 전환 완료 시 제거 예정. 지금은 비교용으로 남겨둠(호출 안 함).
    func minimuxerProbe() {
        guard !running, hasPairing else { return }
        running = true
        logLines = []; summary = nil; errorText = nil
        let sd = stateDir
        let pairingPath = pairingURL.path
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                DispatchQueue.main.async { self.logLines.append("minimuxer 시작(페어링 로드)...") }
                let (okReady, diag) = try self.minimuxerReady(pairingPath: pairingPath, sd: sd)
                DispatchQueue.main.async {
                    self.running = false
                    if okReady { self.summary = "minimuxer 연결 OK — 설치 준비됨(이제 '지금 갱신')" }
                    else { self.errorText = "minimuxer 시작됨, 기기 준비 안 됨 — \(diag)" }
                }
            } catch let e as MinimuxerError {
                DispatchQueue.main.async { self.running = false; self.errorText = "minimuxer 실패: \(describe_error(e).toString())" }
            } catch {
                DispatchQueue.main.async { self.running = false; self.errorText = "페어링 읽기 실패: \(error.localizedDescription)" }
            }
        }
    }

    // 프로브 결과 처리 — 발급(finish)과 달리 계정을 기록하지 않는다.
    private func finishProbe(_ json: String) {
        running = false
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { errorText = "응답 파싱 실패"; return }
        if (obj["ok"] as? Bool) == true { summary = obj["path"] as? String }
        else { errorText = obj["error"] as? String ?? "알 수 없는 오류" }
    }

    /// ④+⑤ 자기 자신 갱신 — Rust가 발급+재서명(설치 제외)해 서명된 .ipa를 만들고, RSD(rppairing 터널)로
    /// 폰에 업로드(AFC)+설치한다. 실행 중 번들ID로 서명해야 installation_proxy가 in-place 업그레이드(데이터 보존).
    func selfUpdate(email: String, password: String, addr: String) {
        guard !running, hasPairing else { return }
        running = true
        logLines = []; summary = nil; errorText = nil
        lastEmail = email
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let bundlePath = Bundle.main.bundlePath
        let bundleId = Bundle.main.bundleIdentifier ?? "net.sw.shard"
        let sd = stateDir
        let work = URL(fileURLWithPath: sd).appendingPathComponent("work").path
        let pairingPath = pairingURL.path
        DispatchQueue.global(qos: .userInitiated).async {
            // 1) Rust: 발급 + 자기 재서명 (설치는 minimuxer가 → device_addr/pairing_path = NULL로 서명만).
            let raw: UnsafeMutablePointer<CChar>? =
                email.withCString { e in password.withCString { p in bundleId.withCString { b in
                "Shard".withCString { n in bundlePath.withCString { ab in sd.withCString { s in
                work.withCString { w in
                    shard_resign_selfupdate(e, p, b, n, ab, s, w, nil, nil,
                                            ResignModel.tfaCb, ResignModel.logCb, ctx)
                }}}}}}}
            let json = raw.map { String(cString: $0) } ?? #"{"ok":false,"error":"응답 없음"}"#
            if let raw = raw { shard_string_free(raw) }
            guard let d = json.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
                  (obj["ok"] as? Bool) == true, let ipaPath = obj["path"] as? String else {
                let err = (json.data(using: .utf8)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["error"] as? String
                DispatchQueue.main.async { self.running = false; self.errorText = "서명 단계 실패 — \(err ?? json)" }
                return
            }
            // 2) RSD(iOS 17+): rppairing 터널 위에서 서명된 .ipa를 업로드(AFC)+설치. classic minimuxer는
            //    iOS 26에서 죽어(QueryType RST) RSD로 대체. pairing은 RP 페어링(idevice_pair 발급).
            DispatchQueue.main.async { self.logLines.append("서명 완료. RSD 터널로 설치...") }
            let raw2: UnsafeMutablePointer<CChar>? =
                addr.withCString { a in pairingPath.withCString { pp in ipaPath.withCString { ip in
                    shard_rsd_install(a, 49152, pp, ip, ResignModel.logCb, ctx)
                } } }
            let json2 = raw2.map { String(cString: $0) } ?? #"{"ok":false,"error":"응답 없음"}"#
            if let raw2 = raw2 { shard_string_free(raw2) }
            DispatchQueue.main.async {
                self.running = false
                if let d2 = json2.data(using: .utf8),
                   let obj2 = try? JSONSerialization.jsonObject(with: d2) as? [String: Any] {
                    if (obj2["ok"] as? Bool) == true { self.summary = obj2["path"] as? String }
                    else { self.errorText = "설치 실패 — \(obj2["error"] as? String ?? json2)" }
                } else { self.errorText = "설치 응답 파싱 실패" }
            }
        }
    }

    func run(email: String, password: String) {
        guard !running else { return }
        running = true
        logLines = []
        summary = nil
        errorText = nil
        lastEmail = email
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = email.withCString { e in
                password.withCString { p in
                    "net.sw.shard".withCString { b in
                        "Shard".withCString { n in
                            self.stateDir.withCString { s in
                                shard_resign_verify(e, p, b, n, s, ResignModel.tfaCb, ResignModel.logCb, ctx)
                            }
                        }
                    }
                }
            }
            let json = raw.map { String(cString: $0) } ?? #"{"ok":false,"error":"응답 없음"}"#
            if let raw = raw { shard_string_free(raw) }
            DispatchQueue.main.async { self.finish(json) }
        }
    }

    func submit2FA(_ code: String) {
        tfaCode = code
        needs2FA = false
        tfaSem.signal()
    }

    // 백그라운드 스레드에서 호출된다. 메인에 2FA 요청을 띄우고 대기한 뒤 코드를 C 문자열로 돌려준다.
    // 반환 포인터는 Rust가 즉시 복사하므로 다음 호출 전까지만 유효하면 된다.
    fileprivate func provideTFA() -> UnsafePointer<CChar>? {
        DispatchQueue.main.async { self.needs2FA = true }
        tfaSem.wait()
        if let old = tfaCPtr { free(old) }
        tfaCPtr = strdup(tfaCode)
        return UnsafePointer(tfaCPtr)
    }

    fileprivate func appendLog(_ line: String) {
        DispatchQueue.main.async { self.logLines.append(line) }
    }

    private func finish(_ json: String) {
        running = false
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            errorText = "응답 파싱 실패"
            return
        }
        if (obj["ok"] as? Bool) == true {
            let s = obj["path"] as? String ?? ""
            summary = s
            // 발급 성공 — 이 계정을 기록해 체크 표시로 관리한다(팀/App ID는 요약에서 파싱).
            if !lastEmail.isEmpty {
                let acct = SignedAccount(
                    email: lastEmail,
                    teamId: Self.field(s, "team"),
                    appId: Self.field(s, "appId"),
                    date: Date()
                )
                accounts = SignedAccountStore.upsert(acct)
            }
        } else {
            errorText = obj["error"] as? String ?? "알 수 없는 오류"
        }
    }

    /// 저장소에서 계정을 지운다(관리용).
    func forget(email: String) {
        accounts = SignedAccountStore.remove(email: email)
    }

    /// "team=…; appId=…; profile=…B" 요약에서 `key=` 뒤 값을 ; 전까지 뽑는다.
    private static func field(_ s: String, _ key: String) -> String {
        for part in s.components(separatedBy: ";") {
            let kv = part.trimmingCharacters(in: .whitespaces)
            if kv.hasPrefix("\(key)=") { return String(kv.dropFirst(key.count + 1)) }
        }
        return ""
    }

    deinit {
        if let old = tfaCPtr { free(old) }
    }

    // C 콜백은 캡처가 없어야 한다(@convention(c)) — self는 ctx로 복원한다.
    static let tfaCb: ShardTfa = { ctx in
        guard let ctx = ctx else { return nil }
        return Unmanaged<ResignModel>.fromOpaque(ctx).takeUnretainedValue().provideTFA()
    }
    static let logCb: ShardLog = { ctx, line in
        guard let ctx = ctx else { return }
        let s = line.map { String(cString: $0) } ?? ""
        Unmanaged<ResignModel>.fromOpaque(ctx).takeUnretainedValue().appendLog(s)
    }
}

struct ResignView: View {
    @StateObject private var model = ResignModel()
    @AppStorage("resign.email") private var email = ""
    @State private var password = ""
    @State private var tfaInput = ""
    @State private var showPairingPicker = false
    @AppStorage("resign.tunnelAddr") private var probeAddr = "10.7.0.1"
    // 전용 anisette 서버 주소(비우면 기본 공유서버). 고정 기기 정체성 → 잠금·재로그인 근본 차단.
    @AppStorage("resign.anisetteURL") private var anisetteURL = ""
    // 발급·페어링이 끝난 뒤엔 ID·anisette·터널 칸을 접어 두고(값은 @AppStorage로 기억됨) "변경"으로만
    // 편다 — 매번 다시 입력할 필요가 없고 화면도 깔끔해진다(사용자 요청). 처음이거나 계정 기록이
    // 없으면 펴진 채로 시작한다(accountKnown).
    @State private var editingAccount = false
    @State private var editingTunnel = false
    @Environment(\.dismiss) private var dismiss

    // iOS 15 배포 타깃이라 NavigationStack(16+)·alert 속 TextField(16+)를 피하고 커스텀 헤더 +
    // 인라인 2FA 입력으로 짠다.
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("자체 서명").font(.headline).foregroundColor(.onSurface)
                Spacer()
                Button("닫기") { dismiss() }.foregroundColor(.accent)
            }
            .padding()
            Divider().background(Color.toolbar)

            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    signatureStatus
                    if !model.accounts.isEmpty { signedAccounts }

                    Text("Apple ID로 로그인해 이 앱의 개발 인증서·프로비저닝 프로파일을 발급받습니다. (.ipa 서명·설치는 다음 단계)")
                        .font(.caption).foregroundColor(.muted)

                    // ID·anisette는 값이 기억돼 있으면(accountKnown) 접어 요약만 보이고, "변경"으로 편다.
                    if accountKnown && !editingAccount {
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("계정: \(shownEmail)")
                                    .font(.footnote.weight(.semibold)).foregroundColor(.onSurface)
                                Text("anisette: \(anisetteURL.isEmpty ? "기본 서버(앱 내장)" : anisetteURL)")
                                    .font(.caption2).foregroundColor(.muted)
                            }
                            Spacer()
                            Button("변경") { editingAccount = true }
                                .font(.footnote.weight(.semibold)).foregroundColor(.accent)
                        }
                        .padding(12).background(Color.chrome)
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                    } else {
                        labeled("Apple ID (보조 계정 권장)") {
                            TextField("you@example.com", text: $email)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.emailAddress)
                                .disableAutocorrection(true)
                        }
                        labeled("anisette 서버 (비우면 앱 내장 기본)") {
                            TextField("http://<홈서버>:6969", text: $anisetteURL)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.URL)
                                .disableAutocorrection(true)
                                .onChange(of: anisetteURL) { v in model.saveAnisetteURL(v) }
                        }
                        Text("전용 서버(고정 기기 정체성)를 쓰면 계정 잠금·재로그인이 근본적으로 준다. 도커 anisette-v3-server를 홈서버에 올리고 폰에서 닿게(LAN 또는 DuckDNS:6969).")
                            .font(.caption2).foregroundColor(.muted)
                    }
                    // 비밀번호는 보안상 기억하지 않으므로 늘 보인다 — 세션 만료 시 로그인에 필요하고, 발급·
                    // 갱신 버튼(canSelfUpdate)도 이 값을 요구한다.
                    labeled("비밀번호 (앱 암호 권장)") {
                        SecureField("••••••••", text: $password)
                    }

                    // "인증서 발급"은 계정 설정·변경용이다 — 이미 발급받은 계정이 있고 변경 중이 아니면
                    // 숨긴다(사용자 지적: 발급은 계정 변경할 때만). 평소엔 "지금 갱신"만 쓰면 되고, 갱신이
                    // 로그인→발급→재서명→설치를 안에서 한 번에 한다.
                    if editingAccount || model.accounts.isEmpty {
                        Button {
                            PasswordStore.save(password, for: email)
                            model.run(email: email, password: password)
                        } label: {
                            Text(model.running ? "진행 중..." : "인증서 발급")
                                .font(.body.weight(.semibold))
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 12)
                                .background(runnable ? Color.accent : Color.toolbar)
                                .foregroundColor(runnable ? .onAccent : .muted)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        }
                        .disabled(!runnable)
                    }

                    // 2FA 코드 입력 — 로그인 중 코드가 필요할 때만 나타난다(alert 대신 인라인).
                    if model.needs2FA {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("2단계 인증 코드 — 기기로 전송된 코드를 입력하세요")
                                .font(.caption).foregroundColor(.accent)
                            HStack(spacing: 8) {
                                TextField("6자리 코드", text: $tfaInput)
                                    .keyboardType(.numberPad)
                                    .foregroundColor(.onSurface)
                                    .padding(.horizontal, 12).padding(.vertical, 10)
                                    .background(Color.chrome)
                                    .clipShape(RoundedRectangle(cornerRadius: 10))
                                Button("확인") {
                                    model.submit2FA(tfaInput)
                                    tfaInput = ""
                                }
                                .font(.body.weight(.semibold)).foregroundColor(.onAccent)
                                .padding(.horizontal, 16).padding(.vertical, 10)
                                .background(Color.accent)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                            }
                        }
                    }

                    if let s = model.summary {
                        Text("발급 성공 — \(s)").font(.footnote).foregroundColor(.accent)
                    }
                    if let e = model.errorText {
                        Text("실패 — \(e)").font(.footnote).foregroundColor(.red)
                    }

                    // ④ 설치 연결 테스트 — RP 페어링 + LocalDevVPN으로 rppairing 터널을 세워 터널 안
                    // RSD(설치 서비스)에 붙는지 확인(설치의 전제). idevice_pair로 RP 페어링을 1회 발급.
                    Divider().background(Color.toolbar)
                    VStack(alignment: .leading, spacing: 8) {
                        Text("④ 설치 연결 테스트 (실험)").font(.caption).foregroundColor(.muted)
                        HStack(spacing: 8) {
                            Image(systemName: model.hasPairing ? "checkmark.seal.fill" : "doc.badge.plus")
                                .foregroundColor(model.hasPairing ? .accent : .muted)
                            Text(model.hasPairing ? "RP 페어링 있음" : "RP 페어링 없음 (idevice_pair 발급)")
                                .font(.footnote).foregroundColor(.onSurface)
                            Spacer()
                            Button(model.hasPairing ? "교체" : "가져오기") { showPairingPicker = true }
                                .font(.footnote.weight(.semibold)).foregroundColor(.accent)
                        }
                        // 터널 주소도 기억되므로 접어 두고 "변경"으로만 편다.
                        if !editingTunnel {
                            HStack {
                                Text("터널 주소: \(probeAddr)")
                                    .font(.footnote).foregroundColor(.onSurface)
                                Spacer()
                                Button("변경") { editingTunnel = true }
                                    .font(.footnote.weight(.semibold)).foregroundColor(.accent)
                            }
                        } else {
                            labeled("터널 주소 (LocalDevVPN 기본 10.7.0.1)") {
                                TextField("10.7.0.1", text: $probeAddr)
                                    .keyboardType(.numbersAndPunctuation)
                                    .disableAutocorrection(true)
                            }
                        }
                        // RSD(iOS 17+) 연결 테스트 — iOS 26의 진짜 경로. rppairing 터널(TCP→RemotePairing
                        // →TLS-PSK→jktcp 어댑터)을 세우고 터널 안 RSD 서비스 목록을 확인. RP 페어링 필요.
                        // 한 번 통과하면 끝이므로, 페어링이 있고 터널을 바꾸는 중이 아니면 숨긴다(사용자
                        // 지적: 연결 테스트는 끝난 것). 터널 변경(editingTunnel) 때나 페어링 전에만 보인다.
                        if editingTunnel || !model.hasPairing {
                            Button {
                                model.rsdProbe(addr: probeAddr)
                            } label: {
                                Text(model.running ? "확인 중..." : "연결 테스트 (RSD)")
                                    .font(.body.weight(.semibold))
                                    .frame(maxWidth: .infinity).padding(.vertical, 10)
                                    .background(model.hasPairing && !model.running ? Color.accent : Color.toolbar)
                                    .foregroundColor(model.hasPairing && !model.running ? .onAccent : .muted)
                                    .clipShape(RoundedRectangle(cornerRadius: 10))
                            }
                            .disabled(!model.hasPairing || model.running)
                            Text("LocalDevVPN 켜고 누르세요. 로그의 ‘터널 안 서비스’에 installation_proxy가 보이면 설치 준비 완료. 페어링은 idevice_pair로 만든 RP 페어링이어야 합니다(classic .mobiledevicepairing은 안 됨).")
                                .font(.caption2).foregroundColor(.muted)
                        }

                        // 전 과정 한 번에: 발급 → 자기 재서명(⑤) → 자기 재설치(④ 업그레이드).
                        Button {
                            PasswordStore.save(password, for: email)
                            model.selfUpdate(email: email, password: password, addr: probeAddr)
                        } label: {
                            Text(model.running ? "진행 중..." : "지금 갱신 (서명+설치)")
                                .font(.body.weight(.semibold))
                                .frame(maxWidth: .infinity).padding(.vertical, 10)
                                .background(canSelfUpdate ? Color.accent : Color.toolbar)
                                .foregroundColor(canSelfUpdate ? .onAccent : .muted)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        }
                        .disabled(!canSelfUpdate)
                        Text("위 Apple ID로 발급 → 자기 자신 재서명 → 설치까지. LocalDevVPN 켜고 페어링·비밀번호 필요. 끝나면 앱을 다시 여세요.")
                            .font(.caption2).foregroundColor(.muted)
                    }
                    .fileImporter(isPresented: $showPairingPicker, allowedContentTypes: [.item]) { result in
                        if case .success(let url) = result { model.importPairing(from: url) }
                    }

                    if !model.logLines.isEmpty {
                        Divider().background(Color.toolbar)
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(model.logLines.indices, id: \.self) { i in
                                Text(model.logLines[i])
                                    .font(.caption2.monospaced())
                                    .foregroundColor(.muted)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
                .padding()
                // anisette 칸을 접어 두면 그 칸의 onAppear가 안 뜨므로, 저장은 여기서(늘 실행) 한다.
                // 저장해 둔 비밀번호가 있으면 채운다 — 그러면 재입력 없이 "지금 갱신"만 누르면 된다.
                .onAppear {
                    model.saveAnisetteURL(anisetteURL)
                    if password.isEmpty { password = PasswordStore.load(for: email) }
                }
            }
        }
        .background(Color.surface.ignoresSafeArea())
        // 키보드는 **화면의 빈 곳을 탭**하면 내려간다(버튼 말고 — 사용자 지적). 컨트롤(필드·버튼) 위
        // 탭은 그 컨트롤이 먼저 먹으므로 입력·동작엔 지장 없고, 스크롤(드래그)과도 구분된다.
        .contentShape(Rectangle())
        .onTapGesture { hideKeyboard() }
    }

    // 현재 서명의 정확한 남은 일수(모래시계는 '양'으로, 여기선 숫자로). 없으면 정보 없음.
    private var signatureStatus: some View {
        let days = SigningInfo.daysLeft()
        let low = (days ?? 99) <= 3
        return HStack(spacing: 12) {
            // SF Symbol hourglass 아이콘 그대로 + 모래 양(주소창과 동일). 옆에 정확한 일수 텍스트.
            HourglassSand(fraction: SigningInfo.fraction(),
                          sand: days == nil ? .muted : (low ? .accent : .onSurface),
                          frameColor: .muted)
                .frame(width: 22, height: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text("현재 서명").font(.caption).foregroundColor(.muted)
                if let d = days {
                    Text(d > 0 ? "남은 유효기간 \(d)일" : "만료됨")
                        .font(.body.weight(.semibold))
                        .foregroundColor(d > 0 ? (low ? .accent : .onSurface) : .red)
                } else {
                    Text("서명 정보 없음").font(.body.weight(.semibold)).foregroundColor(.muted)
                }
            }
            Spacer()
        }
        .padding(12)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    // 발급받은 계정 — 이메일 + 체크 표시로 관리. ✕로 기록 삭제.
    private var signedAccounts: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("발급받은 계정").font(.caption).foregroundColor(.muted)
            ForEach(model.accounts) { acct in
                HStack(spacing: 10) {
                    Image(systemName: "checkmark.seal.fill").foregroundColor(.accent)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(acct.email).font(.footnote.weight(.semibold)).foregroundColor(.onSurface)
                        if !acct.teamId.isEmpty {
                            Text("팀 \(acct.teamId) · \(shortDate(acct.date))")
                                .font(.caption2).foregroundColor(.muted)
                        }
                    }
                    Spacer()
                    Button { model.forget(email: acct.email) } label: {
                        Image(systemName: "xmark.circle.fill").foregroundColor(.muted)
                    }
                }
                .padding(.vertical, 8).padding(.horizontal, 10)
                .background(Color.chrome)
                .clipShape(RoundedRectangle(cornerRadius: 10))
            }
        }
    }

    private func shortDate(_ d: Date) -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyy.MM.dd"
        return f.string(from: d)
    }

    // 계정 기록이 있으면(이메일이 기억돼 있거나 발급한 계정이 있으면) ID·anisette 칸을 접는다.
    private var accountKnown: Bool { !email.isEmpty || !model.accounts.isEmpty }

    // 접힌 요약에 보일 이메일 — 입력값이 있으면 그것, 없으면 발급한 첫 계정.
    private var shownEmail: String { email.isEmpty ? (model.accounts.first?.email ?? "") : email }

    // 키보드를 내린다 — 지금 first responder에게 사임을 보내는 표준 방법(NavigationView 없이도 됨).
    private func hideKeyboard() {
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    private var runnable: Bool {
        !model.running && !email.isEmpty && !password.isEmpty
    }

    private var canSelfUpdate: Bool {
        model.hasPairing && !model.running && !email.isEmpty && !password.isEmpty
    }

    @ViewBuilder
    private func labeled<Content: View>(_ title: String, @ViewBuilder _ field: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption).foregroundColor(.muted)
            field()
                .foregroundColor(.onSurface)
                .padding(.horizontal, 12).padding(.vertical, 10)
                .background(Color.chrome)
                .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }
}

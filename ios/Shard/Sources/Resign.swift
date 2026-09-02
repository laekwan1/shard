import SwiftUI

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

    /// 남은 일수(내림). 이미 만료면 0 이하.
    static func daysLeft() -> Int? {
        guard let exp = expirationDate() else { return nil }
        return Int(floor(exp.timeIntervalSinceNow / 86400))
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

                    labeled("Apple ID (보조 계정 권장)") {
                        TextField("you@example.com", text: $email)
                            .textInputAutocapitalization(.never)
                            .keyboardType(.emailAddress)
                            .disableAutocorrection(true)
                    }
                    labeled("비밀번호 (앱 암호 권장)") {
                        SecureField("••••••••", text: $password)
                    }

                    Button {
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
            }
        }
        .background(Color.surface.ignoresSafeArea())
    }

    // 현재 서명의 정확한 남은 일수(모래시계는 '양'으로, 여기선 숫자로). 없으면 정보 없음.
    private var signatureStatus: some View {
        let days = SigningInfo.daysLeft()
        let low = (days ?? 99) <= 3
        return HStack(spacing: 12) {
            Image(systemName: "hourglass")
                .font(.title3)
                .foregroundColor(days == nil ? .muted : (low ? .accent : .onSurface))
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

    private var runnable: Bool {
        !model.running && !email.isEmpty && !password.isEmpty
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

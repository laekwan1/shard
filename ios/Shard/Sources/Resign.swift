import SwiftUI

// iOS 자체 서명 엔진(Rust `resign`)을 부르는 Swift 층.
//
// 첫 단계는 **발급 테스트**: .ipa 서명·설치 없이 로그인 → 인증서 → App ID → 프로파일 발급까지만
// 애플 실서버로 확인한다(shard_resign_verify). 가장 어려운 인증·발급이 폰에서 되는지가 먼저다.
//
// C ABI는 동기라 백그라운드 스레드에서 부르고, 2FA 코드는 세마포어로 UI에서 받아 넘긴다.
// 로그 콜백은 진행 상황을 화면에 스트리밍한다.

final class ResignModel: ObservableObject {
    @Published var logLines: [String] = []
    @Published var running = false
    @Published var summary: String?
    @Published var errorText: String?
    @Published var needs2FA = false

    private let tfaSem = DispatchSemaphore(value: 0)
    private var tfaCode = ""
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
            summary = obj["path"] as? String
        } else {
            errorText = obj["error"] as? String ?? "알 수 없는 오류"
        }
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

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Apple ID로 로그인해 개발 인증서·프로비저닝 프로파일이 발급되는지 확인합니다. (.ipa 서명·설치는 다음 단계)")
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
                        Text(model.running ? "진행 중..." : "발급 테스트")
                            .font(.body.weight(.semibold))
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(runnable ? Color.accent : Color.toolbar)
                            .foregroundColor(runnable ? .onAccent : .muted)
                            .clipShape(RoundedRectangle(cornerRadius: 10))
                    }
                    .disabled(!runnable)

                    if let s = model.summary {
                        Text("성공 — \(s)").font(.footnote).foregroundColor(.accent)
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
            .background(Color.surface.ignoresSafeArea())
            .navigationTitle("자체 서명 — 발급 테스트")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("닫기") { dismiss() }.foregroundColor(.accent)
                }
            }
            .alert("2단계 인증 코드", isPresented: $model.needs2FA) {
                TextField("6자리 코드", text: $tfaInput).keyboardType(.numberPad)
                Button("확인") {
                    model.submit2FA(tfaInput)
                    tfaInput = ""
                }
            } message: {
                Text("기기로 전송된 코드를 입력하세요.")
            }
        }
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

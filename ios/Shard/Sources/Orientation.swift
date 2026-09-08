import SwiftUI
import BackgroundTasks  // 새벽 자동 재서명(BGProcessingTask) 등록·예약

/// The one place the app's allowed orientations live. The rotate button and the
/// full-screen player set this; the app delegate reports it to the system.
final class Orientation {
    static let shared = Orientation()
    var mask: UIInterfaceOrientationMask = .all

    /// Force the interface to an orientation and hold it there.
    func lock(_ mask: UIInterfaceOrientationMask, to orientation: UIInterfaceOrientation) {
        self.mask = mask
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let active = scenes.first(where: { $0.activationState == .foregroundActive }) ?? scenes.first
        // Tell EVERY controller in the active scene its allowed orientations have
        // changed, not just the root — the SwiftUI host stack has more than one and
        // the geometry request is judged against the top-most one.
        if #available(iOS 16.0, *) {
            active?.windows.forEach { window in
                var vc = window.rootViewController
                while let v = vc { v.setNeedsUpdateOfSupportedInterfaceOrientations(); vc = v.presentedViewController }
            }
            active?.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { _ in }
        }
        // Also nudge the device orientation — on some builds the geometry request
        // alone did not turn the interface, and this kicks it.
        UIDevice.current.setValue(orientation.rawValue, forKey: "orientation")
        UIViewController.attemptRotationToDeviceOrientation()
    }

    /// Back to free rotation — but nudge the interface to portrait as it releases,
    /// so a previously-forced landscape does not leave the window stuck at the
    /// landscape geometry (which showed up as a cropped/zoomed library).
    func free() {
        mask = .all
        if #available(iOS 16.0, *) {
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
                .forEach { $0.keyWindow?.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations() }
        }
        UIDevice.current.setValue(UIInterfaceOrientation.portrait.rawValue, forKey: "orientation")
        UIViewController.attemptRotationToDeviceOrientation()
    }
}

/// Reports the allowed orientations, and registers/schedules the pre-dawn auto-resign
/// background task. Attached in ShardApp with `@UIApplicationDelegateAdaptor`.
final class AppDelegate: NSObject, UIApplicationDelegate {
    /// BGProcessingTask 식별자 — Info.plist의 BGTaskSchedulerPermittedIdentifiers와 **글자 그대로 같아야**
    /// 한다(다르면 register가 false·submit이 거부).
    private static let renewTaskID = "net.sw.shard.autorenew"

    func application(_ application: UIApplication,
                     supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        Orientation.shared.mask
    }

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // 핸들러 등록은 반드시 launch 종료 전에(iOS 규칙). 식별자가 plist에 없으면 false 반환일 뿐 크래시 아님.
        _ = BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.renewTaskID, using: nil) { task in
            self.handleRenew(task as! BGProcessingTask)
        }
        scheduleRenew()
        return true
    }

    /// 다음 새벽 4시(대략)로 재서명 백그라운드 작업을 예약. 정확한 시각은 iOS가 정하지만(idle·자원 여유),
    /// earliestBeginDate 이후로 잡는다. 네트워크는 필요(애플 로그인·설치), 충전은 요구하지 않는다(요구하면
    /// 새벽에 안 도는 경우가 많다). 실패해도(이미 예약됨 등) try?로 삼킨다.
    func scheduleRenew() {
        let req = BGProcessingTaskRequest(identifier: Self.renewTaskID)
        req.requiresNetworkConnectivity = true
        req.requiresExternalPower = false
        req.earliestBeginDate = Self.next4am()
        try? BGTaskScheduler.shared.submit(req)
    }

    /// 지금 이후의 가장 가까운 새벽 4시.
    private static func next4am() -> Date {
        let cal = Calendar.current
        let now = Date()
        var comps = cal.dateComponents([.year, .month, .day], from: now)
        comps.hour = 4; comps.minute = 0
        let today4 = cal.date(from: comps) ?? now.addingTimeInterval(4 * 3600)
        return today4 > now ? today4
            : (cal.date(byAdding: .day, value: 1, to: today4) ?? today4.addingTimeInterval(86400))
    }

    private func handleRenew(_ task: BGProcessingTask) {
        scheduleRenew()   // 성공/실패와 무관하게 다음 새벽을 다시 예약(자동으로 이어지게)
        task.expirationHandler = { }   // iOS가 회수하면 그대로 둔다(스테이징까지 갔으면 다음 콜드런치에 적용)
        // 새벽으로 예약돼 오므로 시간 게이트 없이 시도(preferredWindowOnly=false). 백그라운드라 재생 없음으로 본다.
        DispatchQueue.main.async {
            ResignModel.shared.autoRenewIfNeeded(nothingPlaying: true, preferredWindowOnly: false)
        }
        // 재서명(터널·서명·설치명령)이 끝날 때까지 작업을 잡아 둔다 — 안 그러면 완료 처리 뒤 iOS가 앱을
        // 재우며 진행 중인 서명을 죽인다. running이 내려가거나 최대 150초까지. (VPN 꺼짐이면 running이
        // 안 올라가 곧바로 완료된다.)
        DispatchQueue.global(qos: .utility).async {
            let start = Date()
            Thread.sleep(forTimeInterval: 3)   // autoRenew의 VPN 확인+selfUpdate가 running을 올릴 틈
            while ResignModel.shared.running && Date().timeIntervalSince(start) < 150 {
                Thread.sleep(forTimeInterval: 2)
            }
            task.setTaskCompleted(success: true)
        }
    }
}

import SwiftUI
import BackgroundTasks  // 새벽 자동 재서명(BGProcessingTask) 등록·예약
import UserNotifications  // 백그라운드 갱신 때 VPN 꺼짐 알림 권한 요청

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
        // 백그라운드 갱신 때 VPN 꺼짐을 알리려면 알림 권한이 필요하다(요청). 소리는 안 쓰므로 .alert만.
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert]) { _, _ in }
        return true
    }

    /// 재서명 백그라운드 작업을 예약한다. **밤낮 가리지 않고**(사용자 요청) 다음 기회에 iOS가 돌리게 한다 —
    /// 새벽 4시로 묶으면 그만큼 실행 기회가 줄어 잘 안 돈다. earliestBeginDate만 살짝 뒤로 두고, 실제 실행
    /// 시각은 iOS가 정한다(대개 충전·유휴·네트워크 여유 시). 재서명은 조용하고(무팝업) 만료 임박일 때만 실제로
    /// 서명하므로 아무 때나 돌아도 무해하다. 네트워크 필요, 충전 불요(요구하면 실행이 더 드묾). try?로 삼킨다.
    /// ※ 배경 자동 재서명의 진짜 제약은 시각이 아니라 **그때 LocalDevVPN이 켜져 있어야** 설치가 되는 것 —
    ///   앱은 백그라운드에서 VPN을 못 켠다. 꺼져 있으면 설치가 실패하고 로컬 알림(notifyVpnOff)으로 알린다.
    func scheduleRenew() {
        let req = BGProcessingTaskRequest(identifier: Self.renewTaskID)
        req.requiresNetworkConnectivity = true
        req.requiresExternalPower = false
        req.earliestBeginDate = Date(timeIntervalSinceNow: ResignModel.testRenew ? 20 : 60)
        try? BGTaskScheduler.shared.submit(req)
    }

    private func handleRenew(_ task: BGProcessingTask) {
        scheduleRenew()   // 성공/실패와 무관하게 다음 새벽을 다시 예약(자동으로 이어지게)
        task.expirationHandler = { }   // iOS가 회수하면 그대로 둔다(스테이징까지 갔으면 다음 콜드런치에 적용)
        // BGTask 백그라운드 자동 재서명(5단계 #1, ≤4일). 백그라운드라 재생 없음으로 본다.
        DispatchQueue.main.async {
            ResignModel.shared.autoRenewIfNeeded(nothingPlaying: true, fromBackground: true)
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

import SwiftUI

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

/// Reports the allowed orientations. Attached in ShardApp with
/// `@UIApplicationDelegateAdaptor`.
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        Orientation.shared.mask
    }
}

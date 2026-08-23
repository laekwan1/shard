import SwiftUI

/// The one place the app's allowed orientations live. The rotate button and the
/// full-screen player set this; the app delegate reports it to the system.
final class Orientation {
    static let shared = Orientation()
    var mask: UIInterfaceOrientationMask = .all

    /// Force the interface to an orientation and hold it there.
    func lock(_ mask: UIInterfaceOrientationMask, to orientation: UIInterfaceOrientation) {
        self.mask = mask
        if #available(iOS 16.0, *) {
            // Update the controller's reported orientations FIRST, then ask the
            // active scene to rotate. Doing it in the other order let the system
            // reject the geometry update against the still-old mask, which is why
            // a wide video stayed portrait. Target the foreground-active scene
            // specifically — connectedScenes.first was sometimes a background one.
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            let active = scenes.first(where: { $0.activationState == .foregroundActive }) ?? scenes.first
            active?.keyWindow?.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations()
            active?.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { error in
                // Silent: nothing actionable if the system declines, and it logs
                // its own reason.
                _ = error
            }
        } else {
            UIDevice.current.setValue(orientation.rawValue, forKey: "orientation")
            UIViewController.attemptRotationToDeviceOrientation()
        }
    }

    /// Back to free rotation.
    func free() {
        mask = .all
        if #available(iOS 16.0, *) {
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
                .forEach { $0.keyWindow?.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations() }
        }
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

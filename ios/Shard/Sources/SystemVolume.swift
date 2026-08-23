import UIKit
import MediaPlayer
import AVFoundation

/// Drives the phone's own output volume, so the in-app slider and the hardware
/// volume buttons are one and the same. libVLC's own gain is left at unity; this
/// moves the system level through a hidden `MPVolumeView` slider (the only public
/// way to set it) and reads it back from `AVAudioSession.outputVolume`.
final class SystemVolume {
    static let shared = SystemVolume()
    private let volumeView = MPVolumeView(frame: CGRect(x: -3000, y: -3000, width: 1, height: 1))
    private var slider: UISlider?

    /// Put the (offscreen) MPVolumeView in the window once — its slider does not
    /// exist until it is in a live view hierarchy.
    func attach() {
        guard slider == nil else { return }
        let window = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?
            .keyWindow
        guard let window = window else { return }
        window.addSubview(volumeView)
        slider = volumeView.subviews.compactMap { $0 as? UISlider }.first
    }

    /// The current system output level, 0…1.
    var level: Float { AVAudioSession.sharedInstance().outputVolume }

    func set(_ v: Float) {
        attach()
        let clamped = max(0, min(1, v))
        // Setting the slider value is what actually moves the system volume.
        DispatchQueue.main.async { self.slider?.setValue(clamped, animated: false) }
    }
}

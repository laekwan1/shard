import SwiftUI

/// UIKit gesture recognizers over the player, because SwiftUI's
/// `onLongPressGesture(pressing:)` fires "pressing" the instant a finger lands —
/// so a plain tap was triggering 2×/rewind. A real UILongPressGestureRecognizer
/// only begins after the hold, and ends on release, which is what the player
/// needs.
struct PlayerGestures: UIViewRepresentable {
    enum Phase { case began, changed, ended }

    var onTap: () -> Void
    /// Double-tap with the tap's x and the view width, so the caller can pick a third.
    var onDoubleTap: (_ x: CGFloat, _ width: CGFloat) -> Void
    /// Hold began/ended, with the start x and width.
    var onHold: (_ active: Bool, _ x: CGFloat, _ width: CGFloat) -> Void
    /// A vertical drag: phase, start x, width, and cumulative y translation.
    var onVerticalDrag: (_ phase: Phase, _ startX: CGFloat, _ width: CGFloat, _ dy: CGFloat) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.backgroundColor = .clear
        let c = context.coordinator

        let single = UITapGestureRecognizer(target: c, action: #selector(Coordinator.tapped))
        let double = UITapGestureRecognizer(target: c, action: #selector(Coordinator.doubleTapped))
        double.numberOfTapsRequired = 2
        single.require(toFail: double)

        let hold = UILongPressGestureRecognizer(target: c, action: #selector(Coordinator.held(_:)))
        hold.minimumPressDuration = 0.35

        let pan = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.panned(_:)))
        pan.maximumNumberOfTouches = 1

        for g in [single, double, hold, pan] as [UIGestureRecognizer] {
            g.delegate = c
            view.addGestureRecognizer(g)
        }
        return view
    }

    func updateUIView(_ uiView: UIView, context: Context) { context.coordinator.parent = self }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var parent: PlayerGestures
        init(_ parent: PlayerGestures) { self.parent = parent }

        @objc func tapped(_ g: UITapGestureRecognizer) { parent.onTap() }

        @objc func doubleTapped(_ g: UITapGestureRecognizer) {
            guard let v = g.view else { return }
            parent.onDoubleTap(g.location(in: v).x, v.bounds.width)
        }

        @objc func held(_ g: UILongPressGestureRecognizer) {
            guard let v = g.view else { return }
            let x = g.location(in: v).x, w = v.bounds.width
            switch g.state {
            case .began: parent.onHold(true, x, w)
            case .ended, .cancelled, .failed: parent.onHold(false, x, w)
            default: break
            }
        }

        @objc func panned(_ g: UIPanGestureRecognizer) {
            guard let v = g.view else { return }
            let startX = g.location(in: v).x - g.translation(in: v).x
            let dy = g.translation(in: v).y
            switch g.state {
            case .began: parent.onVerticalDrag(.began, startX, v.bounds.width, dy)
            case .changed: parent.onVerticalDrag(.changed, startX, v.bounds.width, dy)
            case .ended, .cancelled, .failed: parent.onVerticalDrag(.ended, startX, v.bounds.width, dy)
            default: break
            }
        }

        // The hold and the pan may run together (a hold that drifts), and taps
        // sit above both.
        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith o: UIGestureRecognizer) -> Bool { true }
    }
}

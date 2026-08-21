import SwiftUI

/// The same dark palette the Android app uses (res/values/colors.xml), so the
/// phone looks like the phone. Amber accent, near-black surface.
extension Color {
    static let surface = Color(hex: 0x0E0E10)
    static let chrome = Color(hex: 0x1A1A1D)
    static let toolbar = Color(hex: 0x2C2E32)
    static let onSurface = Color(hex: 0xE8E6E3)
    static let muted = Color(hex: 0x8A8A90)
    static let accent = Color(hex: 0xE0A030)
    static let onAccent = Color(hex: 0x141210)

    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

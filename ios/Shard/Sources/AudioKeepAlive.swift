import AVFoundation

/// Keeps the audio output route warm by looping a silent buffer forever.
///
/// The "텁" pop was not the player or a transition — it happened on plain
/// play/pause too. That is the hardware audio route powering up when sound starts
/// and down when it stops: each edge is a click. If *something* is always feeding
/// the route (this silent loop), it never powers down, so the real player's
/// play / pause / track-change no longer click.
final class AudioKeepAlive {
    static let shared = AudioKeepAlive()
    private var player: AVAudioPlayer?

    func start() {
        if player == nil {
            player = try? AVAudioPlayer(data: Self.silentWav(seconds: 1))
            player?.numberOfLoops = -1
            player?.volume = 0
            player?.prepareToPlay()
        }
        if player?.isPlaying == false { player?.play() }
    }

    /// A minimal PCM WAV of pure silence — enough to keep a player "playing".
    private static func silentWav(seconds: Double, rate: Int = 44100) -> Data {
        let channels = 1, bits = 16
        let frames = Int(Double(rate) * seconds)
        let dataSize = frames * channels * bits / 8
        var d = Data()
        func ascii(_ s: String) { d.append(s.data(using: .ascii)!) }
        func u32(_ v: UInt32) { var x = v.littleEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { var x = v.littleEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }
        ascii("RIFF"); u32(UInt32(36 + dataSize)); ascii("WAVE")
        ascii("fmt "); u32(16); u16(1); u16(UInt16(channels)); u32(UInt32(rate))
        u32(UInt32(rate * channels * bits / 8)); u16(UInt16(channels * bits / 8)); u16(UInt16(bits))
        ascii("data"); u32(UInt32(dataSize))
        d.append(Data(count: dataSize))   // zeros = silence
        return d
    }
}

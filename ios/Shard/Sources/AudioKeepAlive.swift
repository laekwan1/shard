import AVFoundation

/// Keeps the hardware audio output unit running at all times by rendering silence
/// through an AVAudioEngine source node.
///
/// The "텁" pop was not the player, the session, or a media transition — it fired
/// on a plain play/pause too. That is the output audio unit powering up when sound
/// starts and down when it stops; each edge clicks. A looping AVAudioPlayer was not
/// enough (iOS can idle a silent player). An AVAudioEngine that continuously feeds
/// silence to the output holds the unit definitively, so AVPlayer/libVLC starting
/// and stopping no longer power-cycles it.
final class AudioKeepAlive {
    static let shared = AudioKeepAlive()
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode?
    private var running = false

    func start() {
        guard !running else {
            if !engine.isRunning { try? engine.start() }
            return
        }
        let format = engine.outputNode.inputFormat(forBus: 0)
        // A guard against a zero/invalid hardware format before the session is up.
        guard format.sampleRate > 0, format.channelCount > 0 else { return }
        let src = AVAudioSourceNode { _, _, frameCount, audioBufferList -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(audioBufferList)
            for buffer in abl {
                if let data = buffer.mData { memset(data, 0, Int(buffer.mDataByteSize)) }
            }
            return noErr
        }
        engine.attach(src)
        engine.connect(src, to: engine.mainMixerNode, format: format)
        engine.mainMixerNode.outputVolume = 0   // pure silence; the unit still runs
        do { try engine.start(); source = src; running = true } catch { }
    }
}

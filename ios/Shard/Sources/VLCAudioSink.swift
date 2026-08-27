import AVFoundation

/// Plays libVLC's decoded PCM through AVAudioEngine — Apple's own audio output,
/// which rides Bluetooth link jitter (worst during an Apple Watch workout) cleanly,
/// the way AVPlayer does. libVLC hands us S16 interleaved stereo at 48kHz through its
/// C audio callbacks (see VLCAudioBridge); we buffer it and an AVAudioSourceNode
/// drains it at the output rate. This replaces libVLC's own iOS audio output, which
/// crackled over that link and added start/seek latency — so Opus/VP9/AV1 keep best
/// quality AND play clean, without falling back to AAC.
final class VLCAudioSink {
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode?
    private let format = AVAudioFormat(commonFormat: .pcmFormatFloat32,
                                       sampleRate: 48000, channels: 2, interleaved: true)!

    // Interleaved Float32 ring [L,R,L,R,…]. ~2s at 48k stereo.
    private var ring = [Float](repeating: 0, count: 48000 * 2 * 2)
    private var readIndex = 0
    private var writeIndex = 0
    private var filled = 0                 // valid floats in the ring
    private var lock = os_unfair_lock()

    /// Output silence until this many frames have been buffered, so playback starts
    /// with a cushion that absorbs the link's jitter instead of underrunning. Reset by
    /// a flush (a seek) so the cushion is rebuilt before sound resumes.
    private let primeFrames = 48000 / 5     // ~200ms
    private var primed = false
    private var running = false

    init() {
        let node = AVAudioSourceNode(format: format) { [weak self] _, _, frameCount, audioBufferList -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(audioBufferList)
            guard let buffer = abl.first,
                  let out = buffer.mData?.assumingMemoryBound(to: Float.self) else { return noErr }
            let wanted = Int(frameCount) * 2         // interleaved stereo
            self?.pull(into: out, count: wanted)
            return noErr
        }
        source = node
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
    }

    // MARK: called from libVLC's audio thread

    /// Buffer `frames` of S16 interleaved stereo. Converts to Float and appends; if the
    /// ring is full the oldest audio is dropped (better a tiny skip than growing lag).
    func push(_ samples: UnsafePointer<Int16>, frames: UInt32) {
        let count = Int(frames) * 2              // interleaved floats
        os_unfair_lock_lock(&lock)
        let cap = ring.count
        for i in 0..<count {
            ring[writeIndex] = Float(samples[i]) / 32768.0
            writeIndex = (writeIndex + 1) % cap
            if filled < cap {
                filled += 1
            } else {
                readIndex = (readIndex + 1) % cap   // full: advance read, dropping oldest
            }
        }
        if !primed && filled >= primeFrames * 2 { primed = true }
        os_unfair_lock_unlock(&lock)
    }

    /// Drop everything buffered (a seek/flush) and re-arm the prime cushion.
    func flush() {
        os_unfair_lock_lock(&lock)
        readIndex = 0; writeIndex = 0; filled = 0; primed = false
        os_unfair_lock_unlock(&lock)
    }

    // MARK: called from the audio render thread

    private func pull(into out: UnsafeMutablePointer<Float>, count: Int) {
        os_unfair_lock_lock(&lock)
        guard primed else {
            os_unfair_lock_unlock(&lock)
            for i in 0..<count { out[i] = 0 }       // still filling the cushion
            return
        }
        let cap = ring.count
        var given = 0
        while given < count && filled > 0 {
            out[given] = ring[readIndex]
            readIndex = (readIndex + 1) % cap
            filled -= 1
            given += 1
        }
        if filled == 0 { primed = false }           // underran: rebuild the cushion
        os_unfair_lock_unlock(&lock)
        for i in given..<count { out[i] = 0 }        // pad any shortfall with silence
    }

    // MARK: lifecycle (main thread)

    func start() {
        guard !running else { return }
        do { try engine.start(); running = true }
        catch { running = false }
    }

    func stop() {
        guard running else { return }
        engine.stop()
        running = false
        flush()
    }
}

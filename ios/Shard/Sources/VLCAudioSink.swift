import AVFoundation

/// Plays libVLC's decoded PCM through AVAudioEngine — Apple's own audio output, which
/// rides Bluetooth link jitter (worst during an Apple Watch workout) cleanly, the way
/// AVPlayer does. libVLC hands us S16 interleaved stereo at 48kHz through its C audio
/// callbacks (see VLCAudioBridge); we buffer it and an AVAudioSourceNode drains it at
/// the output rate. This replaces libVLC's own iOS output, which crackled over that
/// link and added start/seek latency — so Opus/VP9/AV1 keep best quality AND play clean.
final class VLCAudioSink {
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode?
    // The engine graph MUST use a non-interleaved (standard) format — AVAudioEngine
    // rejects an interleaved format on an internal connection (that crashed at launch).
    // libVLC still gives us interleaved S16; we de-interleave into the L/R buffers in
    // the render block.
    private let format = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 2)!

    // Interleaved Float32 ring [L,R,L,R,…]. ~2s at 48k stereo.
    private var ring = [Float](repeating: 0, count: 48000 * 2 * 2)
    private var readIndex = 0
    private var writeIndex = 0
    private var filled = 0                 // valid floats in the ring
    private var lock = os_unfair_lock()

    /// Output silence until this many frames are buffered, JUST enough to avoid an
    /// underrun on the very first render pulls. Kept small (~20ms): AVAudioEngine does
    /// its own buffering, and libVLC decodes ahead of realtime, so the ring fills on its
    /// own for steady-state jitter. A big cushion here only delayed the audio start,
    /// desyncing it behind the (immediately-shown) video. Reset by a flush (a seek).
    // The buffer B must equal the video's software-decode render lag R for A/V sync —
    // libVLC cannot be told our output latency (amem has no time-get), so it assumes the
    // audio plays immediately and matches video to that. At 10ms the sound led the video
    // (B < R), so R is larger; set ~35ms and err toward audio a touch LATE (tolerable)
    // rather than early (jarring). See the sync note in the commit / 결함-기록.
    private let primeFrames = 1680          // ~35ms
    private var primed = false
    /// Whether playback WANTS the engine running — used to restart it after the OS stops
    /// it on a route change. The engine's own `isRunning` is the truth for start/stop, so
    /// our intent and the OS's state never drift apart (a stale flag left it silent).
    private var wantRunning = false
    /// Silence the output at once (a user mute) without waiting for the buffered audio
    /// to drain — the ring is still consumed so unmuting resumes in sync.
    private var muted = false

    init() {
        let node = AVAudioSourceNode(format: format) { [weak self] _, _, frameCount, audioBufferList -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(audioBufferList)
            let left = abl.count > 0 ? abl[0].mData?.assumingMemoryBound(to: Float.self) : nil
            let right = abl.count > 1 ? abl[1].mData?.assumingMemoryBound(to: Float.self) : left
            self?.pull(left: left, right: right, frames: Int(frameCount))
            return noErr
        }
        source = node
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
        // The OS stops/reconfigures the engine when the audio route changes (earbuds in
        // or out) or an interruption ends — after which it stays STOPPED and playback
        // goes silent. Restart it here when that happens, if playback still wants it.
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            // Restart ONLY if the change actually left it stopped, and only if playback
            // still wants it. An unconditional stop→start on every change churned the
            // engine — a Watch workout fires a storm of route changes — which risked a
            // crash for no gain.
            guard let self = self, self.wantRunning, !self.engine.isRunning else { return }
            try? self.engine.start()
        }
    }

    private var configObserver: NSObjectProtocol?
    deinit { if let o = configObserver { NotificationCenter.default.removeObserver(o) } }

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

    /// Silence (or unsilence) the output instantly.
    func setMuted(_ m: Bool) {
        os_unfair_lock_lock(&lock)
        muted = m
        os_unfair_lock_unlock(&lock)
    }

    /// Drop everything buffered (a seek/flush) and re-arm the prime cushion.
    func flush() {
        os_unfair_lock_lock(&lock)
        readIndex = 0; writeIndex = 0; filled = 0; primed = false
        os_unfair_lock_unlock(&lock)
    }

    // MARK: called from the audio render thread

    private func pull(left: UnsafeMutablePointer<Float>?, right: UnsafeMutablePointer<Float>?, frames: Int) {
        os_unfair_lock_lock(&lock)
        guard primed else {
            os_unfair_lock_unlock(&lock)
            for i in 0..<frames { left?[i] = 0; right?[i] = 0 }   // still filling the cushion
            return
        }
        let cap = ring.count
        let silent = muted
        var given = 0
        while given < frames && filled >= 2 {
            // Consume the ring even when muted, so the timeline keeps advancing and
            // unmuting picks up in sync — just write silence to the output.
            left?[given] = silent ? 0 : ring[readIndex]
            right?[given] = silent ? 0 : ring[(readIndex + 1) % cap]
            readIndex = (readIndex + 2) % cap
            filled -= 2
            given += 1
        }
        if filled < 2 { primed = false }            // underran: rebuild the cushion
        os_unfair_lock_unlock(&lock)
        for i in given..<frames { left?[i] = 0; right?[i] = 0 }   // pad the shortfall
    }

    // MARK: lifecycle (main thread)

    func start() {
        wantRunning = true
        if !engine.isRunning { try? engine.start() }
    }

    func stop() {
        wantRunning = false
        if engine.isRunning { engine.stop() }
        flush()
    }
}

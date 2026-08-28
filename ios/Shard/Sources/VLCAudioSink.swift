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

    // Media time (seconds) of the ring's WRITE head — libVLC hands each audio block a
    // presentation pts, and this tracks the timeline just past the last sample written.
    // The READ head (what is about to be output) is this minus the samples still queued,
    // so the position AUDIBLE now is readHead − output latency. The VP9 video renderer
    // clocks its frames off exactly this, so picture lands with the sound at the same
    // media time regardless of how deep the buffer is (that is what makes A/V sync
    // automatic instead of a hand-tuned offset). See VLCVideoSink.
    private var writePtsSec: Double = 0

    /// Output silence until this many frames are buffered, JUST enough to avoid an
    /// underrun on the very first render pulls. Kept small (~20ms): AVAudioEngine does
    /// its own buffering, and libVLC decodes ahead of realtime, so the ring fills on its
    /// own for steady-state jitter. A big cushion here only delayed the audio start,
    /// desyncing it behind the (immediately-shown) video. Reset by a flush (a seek).
    // With the VP9 video renderer (VLCVideoSink) the picture is clocked off THIS sink's
    // audible position, so sync no longer depends on this value — it is purely an underrun
    // cushion. Kept small (~35ms) so playback starts promptly; the 800ms input cache and
    // libVLC's decode-ahead keep the ring full after that. (Only the fallback path, where
    // the video handle could not be reached and libVLC draws on its own clock, still leans
    // on this doubling as the A/V offset — 35ms errs toward audio a touch late there.)
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
    /// `ptsUs` is libVLC's presentation time (µs) of the first sample in this block —
    /// used to keep the media clock the video renderer reads.
    func push(_ samples: UnsafePointer<Int16>, frames: UInt32, ptsUs: Int64) {
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
        // The write head now sits just past this block: its pts plus the block's length.
        // Taken from the block's own pts each time (not accumulated), so a seek that jumps
        // the pts is followed exactly and drift never builds up.
        if ptsUs > 0 { writePtsSec = Double(ptsUs) / 1_000_000.0 + Double(frames) / 48000.0 }
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
        readIndex = 0; writeIndex = 0; filled = 0; primed = false; writePtsSec = 0
        os_unfair_lock_unlock(&lock)
    }

    /// The media time (seconds) AUDIBLE right now: the read head (write head minus the
    /// samples still queued) shifted back by the output latency (the sound already handed
    /// to the OS but not yet at the speaker). The video renderer sets its display clock to
    /// this so each frame appears exactly when its audio is heard. Returns nil until the
    /// cushion has primed and there is a real pts to report.
    var currentMediaTime: Double? {
        os_unfair_lock_lock(&lock)
        let ok = primed && writePtsSec > 0
        let readHead = writePtsSec - Double(filled / 2) / 48000.0
        os_unfair_lock_unlock(&lock)
        guard ok else { return nil }
        let session = AVAudioSession.sharedInstance()
        let latency = session.outputLatency + session.ioBufferDuration
        return max(0, readHead - latency)
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

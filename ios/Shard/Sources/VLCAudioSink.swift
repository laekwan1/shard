import AVFoundation
import CoreMedia

/// Plays libVLC's decoded PCM through an AVSampleBufferAudioRenderer, driven by an
/// AVSampleBufferRenderSynchronizer shared with the video (VLCVideoSink). Apple's own media
/// path — clean over Bluetooth like AVPlayer — and the synchronizer lines the picture up with
/// the sound using the renderer's REAL output latency, so there is no hand-tuned offset.
///
/// The renderer is fed the canonical way: `requestMediaDataWhenReady` calls us back on the
/// render queue whenever it wants more, and we hand it buffers from our own small queue. This
/// is what keeps playback gap-free — enqueuing straight from libVLC's thread and dropping when
/// the renderer was momentarily full left holes in the sound.
///
/// The pts we stamp is a REAL-TIME counter (samples delivered ÷ 48000), NOT libVLC's media
/// pts. That keeps the audio contiguous at any playback rate: libVLC does the speed-up and
/// delivers rate-adjusted 48k audio at real time, so consecutive blocks abut, and the
/// synchronizer runs at a constant rate 1. The video is stamped off the SAME counter (see
/// `videoPts`), so a frame shows exactly when its audio is heard.
final class VLCAudioSink {
    let renderer = AVSampleBufferAudioRenderer()

    private var formatDesc: CMAudioFormatDescription?
    private let renderQueue = DispatchQueue(label: "shard.vlc.audiorenderer")
    private var lock = os_unfair_lock()

    /// Buffers waiting for the renderer to ask for them, with each buffer's frame count so
    /// the queue can be capped by duration.
    private struct Pending { let sample: CMSampleBuffer; let frames: Int }
    private var pending = [Pending]()
    private var pendingFrames = 0
    private static let maxPendingFrames = 48000 * 2   // ~2s cushion; drop oldest beyond it

    /// Total frames ever handed in — a MONOTONIC real-time timeline (never reset, not even on
    /// a seek). Resetting it to 0 on every seek made the clock jump, which came out as a burst
    /// of noise or a dropout; keeping it continuous means a seek just swaps the content under a
    /// clock that keeps ticking.
    private var deliveredFrames = 0
    /// The latest audio block's libVLC media pts (µs) paired with the real-time frame it
    /// landed at — the map from libVLC's media clock to our real-time timeline. A video
    /// frame's media time (get_time) is placed on the same timeline through this, so the
    /// picture sits where its audio actually plays even though libVLC hands video slightly
    /// ahead of the matching audio.
    private var anchorMediaUs: Int64 = 0
    private var anchorFrame = 0
    private var haveAnchor = false
    /// Whether the next enqueued sample should (re)start the synchronizer's clock.
    private var needStart = true
    /// Called (on the render queue) with the first sample's pts after a start, so the
    /// controller can start the synchronizer there.
    var onFirstSample: ((CMTime) -> Void)?

    init() {
        var asbd = AudioStreamBasicDescription(
            mSampleRate: 48000, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4,
            mChannelsPerFrame: 2, mBitsPerChannel: 16, mReserved: 0)
        CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: &asbd,
                                       layoutSize: 0, layout: nil, magicCookieSize: 0,
                                       magicCookie: nil, extensions: nil, formatDescriptionOut: &formatDesc)
        // Ask once, forever: the renderer calls this back on the render queue whenever it can
        // take more, and we drain our queue into it. Survives flushes (a flush just empties
        // both), so track changes need no re-arming.
        renderer.requestMediaDataWhenReady(on: renderQueue) { [weak self] in
            self?.drain()
        }
    }

    /// Where a video frame whose libVLC media time is `mediaUs` (from get_time) belongs on our
    /// real-time timeline, at playback `rate`: the audio anchor gives media→real, and video
    /// shares libVLC's media clock, so this places the picture exactly where its audio plays —
    /// independent of how far ahead or behind libVLC happened to hand the frame.
    func videoPts(mediaUs: Int64, rate: Double) -> CMTime {
        os_unfair_lock_lock(&lock)
        let aMedia = anchorMediaUs, aFrame = anchorFrame, have = haveAnchor
        os_unfair_lock_unlock(&lock)
        guard have else { return CMTime(value: Int64(aFrame), timescale: 48000) }
        let deltaFrames = Double(mediaUs - aMedia) / 1_000_000.0 / max(rate, 0.01) * 48000.0
        return CMTime(value: Int64(max(0, Double(aFrame) + deltaFrames)), timescale: 48000)
    }

    // MARK: called from libVLC's audio thread

    /// Wrap `frames` of S16 interleaved stereo into a CMSampleBuffer stamped on the real-time
    /// timeline and queue it for the renderer. `ptsUs` (libVLC's media pts) is ignored — see
    /// the class note on why real-time stamping is what keeps rate changes gap-free.
    func push(_ samples: UnsafePointer<Int16>, frames: UInt32, ptsUs: Int64) {
        guard let fmt = formatDesc, frames > 0 else { return }
        let n = Int(frames)
        let byteCount = n * 4
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: byteCount,
                blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
                offsetToData: 0, dataLength: byteCount,
                flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &block) == kCMBlockBufferNoErr,
              let block = block else { return }
        // Copy libVLC's bytes out now — its buffer is reused after this call returns.
        guard CMBlockBufferReplaceDataBytes(with: samples, blockBuffer: block,
                offsetIntoDestination: 0, dataLength: byteCount) == kCMBlockBufferNoErr else { return }

        os_unfair_lock_lock(&lock)
        let startFrame = deliveredFrames
        deliveredFrames += n
        if ptsUs > 0 { anchorMediaUs = ptsUs; anchorFrame = startFrame; haveAnchor = true }
        os_unfair_lock_unlock(&lock)

        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 48000),
            presentationTimeStamp: CMTime(value: Int64(startFrame), timescale: 48000),
            decodeTimeStamp: .invalid)
        var sampleSize = 4
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
                allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: fmt,
                sampleCount: n, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                sampleSizeEntryCount: 1, sampleSizeArray: &sampleSize,
                sampleBufferOut: &sample) == noErr, let sample = sample else { return }

        os_unfair_lock_lock(&lock)
        pending.append(Pending(sample: sample, frames: n))
        pendingFrames += n
        while pendingFrames > Self.maxPendingFrames, !pending.isEmpty {
            pendingFrames -= pending.removeFirst().frames   // full: drop oldest, a tiny skip
        }
        os_unfair_lock_unlock(&lock)
        // Nudge the drain in case the renderer is already ready and idle.
        renderQueue.async { [weak self] in self?.drain() }
    }

    /// Renderer-ready callback (render queue): feed buffers while it will take them.
    private func drain() {
        while renderer.isReadyForMoreMediaData {
            os_unfair_lock_lock(&lock)
            if renderer.status == .failed {
                pending.removeAll(); pendingFrames = 0; os_unfair_lock_unlock(&lock)
                renderer.flush(); return
            }
            guard !pending.isEmpty else { os_unfair_lock_unlock(&lock); return }
            let next = pending.removeFirst()
            pendingFrames -= next.frames
            var fire: CMTime?
            if needStart { needStart = false; fire = next.sample.presentationTimeStamp }
            os_unfair_lock_unlock(&lock)
            renderer.enqueue(next.sample)
            if let t = fire { onFirstSample?(t) }
        }
    }

    /// Drop the queued audio (a seek/track change) but KEEP the real-time counter running —
    /// the next sample re-starts the synchronizer at the continuing time, so the clock never
    /// jumps (which is what made a seek burst noise or fall silent).
    func flush() {
        os_unfair_lock_lock(&lock)
        pending.removeAll(); pendingFrames = 0; needStart = true
        os_unfair_lock_unlock(&lock)
        let renderer = self.renderer
        renderQueue.async { renderer.flush() }
    }

    /// Silence instantly, without draining the buffer.
    func setMuted(_ m: Bool) { renderer.isMuted = m }

    // MARK: lifecycle (main thread)

    func start() {}   // the synchronizer drives; nothing to spin up

    func stop() { flush() }
}

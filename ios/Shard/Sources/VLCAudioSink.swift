import AVFoundation
import CoreMedia

/// Plays libVLC's decoded PCM through an AVSampleBufferAudioRenderer, driven by an
/// AVSampleBufferRenderSynchronizer shared with the video (VLCVideoSink). This is Apple's
/// own media path — the same clean-over-Bluetooth output AVPlayer uses — so Opus/VP9/AV1
/// keep best quality AND play without the crackle libVLC's own output made over a jittery
/// (Apple-Watch) A2DP link.
///
/// Why this and not AVAudioEngine: the engine could play the PCM cleanly, but WE then had
/// to guess the output latency to line the picture up with the sound — and Bluetooth
/// under-reports that latency, so the sync needed a hand-tuned pad. The render synchronizer
/// asks Apple's renderer for the REAL output timing (Bluetooth included) and lines audio
/// and video up itself. No magic number. libVLC hands us S16 interleaved stereo @ 48kHz
/// (see VLCAudioBridge), each block tagged with its presentation pts; we wrap it in a
/// CMSampleBuffer and enqueue it.
final class VLCAudioSink {
    /// Apple's audio renderer. The controller adds it (and the video layer) to one
    /// synchronizer, whose timebase presents both in step.
    let renderer = AVSampleBufferAudioRenderer()

    private var formatDesc: CMAudioFormatDescription?
    private var lock = os_unfair_lock()
    /// All renderer access (enqueue AND flush) runs here. The renderer is not safe to touch
    /// from two threads, and flush (main, on a seek) would otherwise race enqueue (libVLC's
    /// audio thread) — the same hazard that crashed the video layer. The PCM is copied out
    /// synchronously in push (libVLC reuses its buffer at once); only the ready sample is
    /// handed to this queue.
    private let renderQueue = DispatchQueue(label: "shard.vlc.audiorenderer")
    /// The synchronizer's clock is not running until the first sample gives it a start
    /// time. Re-armed on flush (a seek starts a fresh pts).
    private var needStart = true
    /// Called (on the libVLC audio thread) with the first sample's pts after a start, so
    /// the controller can start the synchronizer's clock there.
    var onFirstSample: ((CMTime) -> Void)?

    init() {
        var asbd = AudioStreamBasicDescription(
            mSampleRate: 48000,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4,
            mChannelsPerFrame: 2, mBitsPerChannel: 16, mReserved: 0)
        CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: &asbd,
                                       layoutSize: 0, layout: nil,
                                       magicCookieSize: 0, magicCookie: nil,
                                       extensions: nil, formatDescriptionOut: &formatDesc)
    }

    // MARK: called from libVLC's audio thread

    /// Wrap `frames` of S16 interleaved stereo (tagged with `ptsUs`, libVLC's presentation
    /// time in µs) into a CMSampleBuffer and enqueue it. The renderer buffers internally and
    /// back-pressures via `isReadyForMoreMediaData`; when it is not ready we drop, since the
    /// clock will have moved on anyway (better a tiny skip than growing lag).
    func push(_ samples: UnsafePointer<Int16>, frames: UInt32, ptsUs: Int64) {
        guard let fmt = formatDesc, frames > 0 else { return }
        guard renderer.isReadyForMoreMediaData else { return }

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

        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 48000),
            presentationTimeStamp: CMTime(value: ptsUs, timescale: 1_000_000),
            decodeTimeStamp: .invalid)
        var sampleSize = 4
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
                allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: fmt,
                sampleCount: n, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                sampleSizeEntryCount: 1, sampleSizeArray: &sampleSize,
                sampleBufferOut: &sample) == noErr, let sample = sample else { return }

        var fire: CMTime?
        os_unfair_lock_lock(&lock)
        if needStart { needStart = false; fire = CMTime(value: ptsUs, timescale: 1_000_000) }
        os_unfair_lock_unlock(&lock)
        let renderer = self.renderer
        let onFirst = self.onFirstSample
        renderQueue.async {
            if renderer.status == .failed { renderer.flush() }
            if renderer.isReadyForMoreMediaData { renderer.enqueue(sample) }
            if let t = fire { DispatchQueue.main.async { onFirst?(t) } }
        }
    }

    /// Drop everything buffered (a seek/flush) and re-arm the start so the synchronizer's
    /// clock restarts at the new pts.
    func flush() {
        let renderer = self.renderer
        renderQueue.async { renderer.flush() }
        os_unfair_lock_lock(&lock); needStart = true; os_unfair_lock_unlock(&lock)
    }

    /// Silence instantly, without draining the buffer.
    func setMuted(_ m: Bool) { renderer.isMuted = m }

    // MARK: lifecycle (main thread)

    /// The synchronizer drives playback; there is nothing to spin up here. Kept so the
    /// call sites read the same as before.
    func start() {}

    func stop() {
        let renderer = self.renderer
        renderQueue.async { renderer.flush() }
        os_unfair_lock_lock(&lock); needStart = true; os_unfair_lock_unlock(&lock)
    }
}

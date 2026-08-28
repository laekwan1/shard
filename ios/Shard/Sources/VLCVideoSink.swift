import AVFoundation
import CoreMedia
import CoreVideo

/// Presents libVLC's decoded VP9 frames ourselves, timed to the audio actually being
/// heard, so picture and sound land at the same media time WITHOUT a hand-tuned offset.
///
/// libVLC hands us BGRA frames (VLCVideoBridge), each tagged with its presentation time.
/// We drop each into a CVPixelBuffer and enqueue it on an AVSampleBufferDisplayLayer whose
/// controlTimebase we drive from VLCAudioSink's audible-position clock. The layer then
/// shows each frame the instant the timebase (= the sound) reaches that frame's time.
///
/// This path exists only for VP9 video — the one thing that reaches libVLC's video output
/// (AV1/H.264 play through AVPlayer, which syncs natively; music has no video). If the
/// bridge cannot attach, the controller leaves libVLC drawing to its view (old behaviour).
final class VLCVideoSink {
    /// The layer that shows the frames — created and put in the view tree on the main
    /// thread by the host view, then handed here.
    weak var displayLayer: AVSampleBufferDisplayLayer?
    private(set) var timebase: CMTimebase?

    private var pool: CVPixelBufferPool?
    private var poolWidth = 0
    private var poolHeight = 0
    private var lock = os_unfair_lock()
    /// True while the timebase is advancing. When it is NOT (priming, or between tracks),
    /// frames are dropped rather than enqueued — a stalled clock never drains the layer, so
    /// piling full-frame buffers behind it only grows memory toward a jetsam kill.
    private var clockRunning = false
    /// All AVSampleBufferDisplayLayer access (enqueue AND flush) runs here. The layer is
    /// NOT safe to touch from two threads, and a seek did exactly that — libVLC's video
    /// thread enqueuing while the main thread flushed — which crashed. Frames are copied
    /// out synchronously on the libVLC thread (its buffer is reused the moment we return),
    /// then the ready sample is handed to this queue; flush() hops here too.
    private let layerQueue = DispatchQueue(label: "shard.vlc.videolayer")

    /// Called on the main thread once the display layer exists: build the timebase (paused)
    /// and hang it on the layer.
    func attach(to layer: AVSampleBufferDisplayLayer) {
        displayLayer = layer
        var tb: CMTimebase?
        CMTimebaseCreateWithSourceClock(allocator: kCFAllocatorDefault,
                                        sourceClock: CMClockGetHostTimeClock(),
                                        timebaseOut: &tb)
        if let tb = tb {
            CMTimebaseSetTime(tb, time: .zero)
            CMTimebaseSetRate(tb, rate: 0)      // paused until the audio clock is valid
            layer.controlTimebase = tb
        }
        timebase = tb
    }

    // MARK: called from libVLC's video thread

    /// A new stream size: (re)build the pixel-buffer pool. IOSurface-backed so the frames
    /// display on the GPU without a further copy.
    func format(width: Int, height: Int, pitch: Int) {
        os_unfair_lock_lock(&lock)
        defer { os_unfair_lock_unlock(&lock) }
        guard width > 0, height > 0 else { return }
        let attrs: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [:],
        ]
        // Recycle buffers aggressively (age them out fast) so the pool cannot grow without
        // bound if the layer ever holds frames longer than expected.
        let poolAttrs: [String: Any] = [kCVPixelBufferPoolMaximumBufferAgeKey as String: 0.4]
        var p: CVPixelBufferPool?
        CVPixelBufferPoolCreate(kCFAllocatorDefault, poolAttrs as CFDictionary, attrs as CFDictionary, &p)
        pool = p; poolWidth = width; poolHeight = height
    }

    /// One frame: copy the BGRA bytes into a pooled pixel buffer and enqueue it with its
    /// presentation time. Enqueuing off the main thread is supported by the display layer.
    func frame(bgra: UnsafePointer<UInt8>, width: Int, height: Int, pitch: Int, timeMs: Int64) {
        os_unfair_lock_lock(&lock)
        let pool = self.pool
        let ok = (width == poolWidth && height == poolHeight)
        let running = clockRunning
        os_unfair_lock_unlock(&lock)
        // Drop while the clock is stalled: with no timebase advance the layer never shows or
        // frees these, so enqueuing them only piles memory. Video resumes on the next frame
        // once the clock runs again.
        guard ok, running, let pool = pool, let layer = displayLayer else { return }

        // Copy libVLC's frame out NOW (this thread): its buffer is reused the instant this
        // callback returns, so the pixel buffer must be filled synchronously here.
        var pb: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &pb) == kCVReturnSuccess,
              let buffer = pb else { return }
        CVPixelBufferLockBaseAddress(buffer, [])
        if let dst = CVPixelBufferGetBaseAddress(buffer) {
            let dstStride = CVPixelBufferGetBytesPerRow(buffer)
            let copyBytes = min(pitch, dstStride)
            for row in 0..<height {
                memcpy(dst.advanced(by: row * dstStride),
                       bgra.advanced(by: row * pitch), copyBytes)
            }
        }
        CVPixelBufferUnlockBaseAddress(buffer, [])

        var fmt: CMVideoFormatDescription?
        guard CMVideoFormatDescriptionCreateForImageBuffer(
                allocator: kCFAllocatorDefault, imageBuffer: buffer,
                formatDescriptionOut: &fmt) == noErr, let fmt = fmt else { return }
        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: timeMs, timescale: 1000),
            decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(
                allocator: kCFAllocatorDefault, imageBuffer: buffer,
                formatDescription: fmt, sampleTiming: &timing,
                sampleBufferOut: &sample) == noErr, let sample = sample else { return }
        // Hand the ready sample to the layer's own queue — never touch the layer from this
        // thread while the main thread might be flushing it (a seek did, and it crashed).
        layerQueue.async {
            if layer.status == .failed { layer.flush() }
            if layer.isReadyForMoreMediaData { layer.enqueue(sample) }
        }
    }

    // MARK: main thread

    /// Point the video clock at the media time the audio is currently at. Keeps the layer's
    /// timebase running at 1× and only re-anchors on a real gap (a seek), so steady playback
    /// is smooth rather than nudged every tick.
    func setClock(_ mediaTime: Double) {
        os_unfair_lock_lock(&lock); clockRunning = true; os_unfair_lock_unlock(&lock)
        guard let tb = timebase else { return }
        let now = CMTimeGetSeconds(CMTimebaseGetTime(tb))
        if now <= 0 || abs(now - mediaTime) > 0.08 {
            CMTimebaseSetTime(tb, time: CMTime(seconds: mediaTime, preferredTimescale: 1000))
        }
        if CMTimebaseGetRate(tb) != 1 { CMTimebaseSetRate(tb, rate: 1) }
    }

    /// No valid audio clock (priming / stopped): hold the picture.
    func pauseClock() {
        os_unfair_lock_lock(&lock); clockRunning = false; os_unfair_lock_unlock(&lock)
        guard let tb = timebase else { return }
        if CMTimebaseGetRate(tb) != 0 { CMTimebaseSetRate(tb, rate: 0) }
    }

    /// Seek / track change: drop queued frames and reset the clock. The layer flush runs on
    /// the same queue as enqueue, so it never races a frame coming in on the libVLC thread.
    func flush() {
        os_unfair_lock_lock(&lock); clockRunning = false; os_unfair_lock_unlock(&lock)
        let layer = displayLayer
        layerQueue.async { layer?.flush() }
        if let tb = timebase {
            CMTimebaseSetRate(tb, rate: 0)
            CMTimebaseSetTime(tb, time: .zero)
        }
    }
}

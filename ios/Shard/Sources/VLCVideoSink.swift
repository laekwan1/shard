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
        var p: CVPixelBufferPool?
        CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attrs as CFDictionary, &p)
        pool = p; poolWidth = width; poolHeight = height
    }

    /// One frame: copy the BGRA bytes into a pooled pixel buffer and enqueue it with its
    /// presentation time. Enqueuing off the main thread is supported by the display layer.
    func frame(bgra: UnsafePointer<UInt8>, width: Int, height: Int, pitch: Int, timeMs: Int64) {
        os_unfair_lock_lock(&lock)
        let pool = self.pool
        let ok = (width == poolWidth && height == poolHeight)
        os_unfair_lock_unlock(&lock)
        guard ok, let pool = pool, let layer = displayLayer else { return }
        if layer.status == .failed { layer.flush() }

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
        if layer.isReadyForMoreMediaData { layer.enqueue(sample) }
    }

    // MARK: main thread

    /// Point the video clock at the media time the audio is currently at. Keeps the layer's
    /// timebase running at 1× and only re-anchors on a real gap (a seek), so steady playback
    /// is smooth rather than nudged every tick.
    func setClock(_ mediaTime: Double) {
        guard let tb = timebase else { return }
        let now = CMTimeGetSeconds(CMTimebaseGetTime(tb))
        if now <= 0 || abs(now - mediaTime) > 0.08 {
            CMTimebaseSetTime(tb, time: CMTime(seconds: mediaTime, preferredTimescale: 1000))
        }
        if CMTimebaseGetRate(tb) != 1 { CMTimebaseSetRate(tb, rate: 1) }
    }

    /// No valid audio clock (priming / stopped): hold the picture.
    func pauseClock() {
        guard let tb = timebase else { return }
        if CMTimebaseGetRate(tb) != 0 { CMTimebaseSetRate(tb, rate: 0) }
    }

    /// Seek / track change: drop queued frames and reset the clock.
    func flush() {
        displayLayer?.flush()
        if let tb = timebase {
            CMTimebaseSetRate(tb, rate: 0)
            CMTimebaseSetTime(tb, time: .zero)
        }
    }
}

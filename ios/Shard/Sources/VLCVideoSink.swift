import AVFoundation
import CoreMedia
import CoreVideo

/// Presents libVLC's decoded VP9 frames through an AVSampleBufferDisplayLayer that shares
/// one AVSampleBufferRenderSynchronizer with the audio (VLCAudioSink). The synchronizer's
/// timebase — driven by Apple's audio renderer, which knows the real output latency — shows
/// each frame exactly when its audio is heard. So picture and sound line up automatically,
/// on any route (Bluetooth included), with no hand-tuned offset.
///
/// libVLC hands us BGRA frames (VLCVideoBridge), each tagged with its presentation time.
/// We wrap each in a CVPixelBuffer → CMSampleBuffer and enqueue it. The synchronizer does
/// the timing; this sink only supplies frames.
///
/// VP9 video only — AV1/H.264 play through AVPlayer, music has no video. If the bridge
/// cannot attach, the controller leaves libVLC drawing to its view (old behaviour).
final class VLCVideoSink {
    /// The layer that shows the frames. Created on the main thread by the host view; the
    /// controller adds it to the shared synchronizer.
    let displayLayer = AVSampleBufferDisplayLayer()

    private var pool: CVPixelBufferPool?
    private var poolWidth = 0
    private var poolHeight = 0
    private var lock = os_unfair_lock()
    /// Where a frame's presentation time comes from: the audio's real-time counter (see
    /// VLCAudioSink.currentPts). libVLC hands us audio and video in step, so the frame
    /// arriving now belongs with the audio delivered now — stamping it off that one clock is
    /// what keeps the two in sync at any playback rate, with the synchronizer at rate 1.
    var ptsProvider: (() -> CMTime)?
    /// All AVSampleBufferDisplayLayer access (enqueue AND flush) runs here. The layer is
    /// NOT safe to touch from two threads, and a seek did exactly that — libVLC's video
    /// thread enqueuing while the main thread flushed — which crashed. Frames are copied
    /// out synchronously on the libVLC thread (its buffer is reused the moment we return),
    /// then the ready sample is handed to this queue; flush() hops here too.
    private let layerQueue = DispatchQueue(label: "shard.vlc.videolayer")

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
        // Recycle buffers aggressively so the pool cannot grow without bound.
        let poolAttrs: [String: Any] = [kCVPixelBufferPoolMaximumBufferAgeKey as String: 0.4]
        var p: CVPixelBufferPool?
        CVPixelBufferPoolCreate(kCFAllocatorDefault, poolAttrs as CFDictionary, attrs as CFDictionary, &p)
        pool = p; poolWidth = width; poolHeight = height
    }

    /// One frame: copy the BGRA bytes into a pooled pixel buffer and enqueue it, stamped off
    /// the audio clock. `timeMs` (libVLC's own clock) is the fallback if no provider is set.
    func frame(bgra: UnsafePointer<UInt8>, width: Int, height: Int, pitch: Int, timeMs: Int64) {
        os_unfair_lock_lock(&lock)
        let pool = self.pool
        let ok = (width == poolWidth && height == poolHeight)
        os_unfair_lock_unlock(&lock)
        guard ok, let pool = pool else { return }
        let layer = displayLayer
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
        let pts = ptsProvider?() ?? CMTime(value: timeMs, timescale: 1000)
        var timing = CMSampleTimingInfo(
            duration: .invalid, presentationTimeStamp: pts, decodeTimeStamp: .invalid)
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

    /// Seek / track change: drop queued frames. Runs on the same queue as enqueue, so it
    /// never races a frame coming in on the libVLC thread.
    func flush() {
        let layer = displayLayer
        layerQueue.async { layer.flush() }
    }
}

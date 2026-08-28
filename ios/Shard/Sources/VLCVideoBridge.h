// Capture libVLC's DECODED video frames (into memory) instead of letting libVLC draw
// them to a view on its own clock.
//
// Why: for VP9 (the only format that reaches libVLC's VIDEO path — AV1/H.264 play through
// AVPlayer, which syncs natively), libVLC decodes in software with a render lag we cannot
// measure, and its memory-audio output (VLCAudioBridge) carries no way to tell libVLC our
// output latency. So libVLC shows the picture on its own clock while our audio comes out a
// buffer later, and the two drift by an amount that could only be hand-tuned. Taking the
// frames here and presenting them OURSELVES — timed to the audio actually being heard
// (VLCVideoSink drives an AVSampleBufferDisplayLayer's timebase from the audio clock) —
// makes "show the picture when its sound is heard" exact, with no magic offset.
//
// Same private-handle reach as VLCAudioBridge: MobileVLCKit 3.6.0 does not expose the C
// video-callback API, so we call libvlc's C functions on the private libvlc_media_player_t
// inside VLCMediaPlayer. Returns NO if that handle moves in a future pod (then libVLC keeps
// drawing to its view — the old behaviour).

#import <Foundation/Foundation.h>

@class VLCMediaPlayer;

NS_ASSUME_NONNULL_BEGIN

/// The negotiated frame format: BGRA, `width`×`height`, `pitch` bytes per row. Called on
/// libVLC's thread when a stream's size is known (and again if it changes).
typedef void (*ShardVideoFormatCb)(void *ctx, unsigned width, unsigned height, unsigned pitch);
/// One decoded frame: `bgra` holds pitch×height bytes, valid only for the duration of the
/// call (copy it out). `timeMs` is libVLC's playback clock at display — the frame's
/// presentation time on the same media timeline as the audio pts.
typedef void (*ShardVideoFrameCb)(void *ctx, const uint8_t *bgra,
                                  unsigned width, unsigned height, unsigned pitch, int64_t timeMs);

/// Route `player`'s video into `ctx` as BGRA frames instead of drawing to a view. Call
/// BEFORE the first play(). `ctx` is retained by the caller for the player's lifetime.
/// Returns NO if the private libvlc handle could not be reached.
BOOL ShardRouteVLCVideo(VLCMediaPlayer *player,
                        void *ctx,
                        ShardVideoFormatCb format,
                        ShardVideoFrameCb frame);

NS_ASSUME_NONNULL_END

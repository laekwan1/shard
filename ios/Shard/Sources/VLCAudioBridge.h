// Route libVLC's DECODED audio to our own AVAudioEngine output instead of libVLC's
// built-in one.
//
// Why: libVLC's iOS audio output crackles over a Bluetooth link an Apple Watch
// jitters, and adds start/seek latency. AVFoundation's output (what AVPlayer uses)
// rides that jitter cleanly. So we keep libVLC only as the DECODER — its C audio
// callbacks hand us raw PCM (S16 interleaved stereo @ 48kHz) — and play that PCM
// through AVAudioEngine ourselves. This lets Opus/VP9/AV1 stay at best quality AND
// play cleanly, without forcing AAC/AVPlayer.
//
// MobileVLCKit does not expose the C audio-callback API, so this reaches the private
// libvlc_media_player_t handle inside VLCMediaPlayer (a fixed pod version, 3.6.0) and
// calls the libvlc C functions directly — the framework exports those symbols (proven
// by a link spike).

#import <Foundation/Foundation.h>

@class VLCMediaPlayer;

NS_ASSUME_NONNULL_BEGIN

/// Called once, before playback, with the negotiated format (always 48000, 2).
typedef void (*ShardAudioSetupCb)(void *ctx, unsigned rate, unsigned channels);
/// Called on libVLC's audio thread with `count` frames of S16 interleaved stereo.
typedef void (*ShardAudioPlayCb)(void *ctx, const int16_t *samples, unsigned count, int64_t pts);
/// Called on flush/drain (e.g. a seek) — the consumer must drop buffered audio.
typedef void (*ShardAudioFlushCb)(void *ctx);

/// Install audio callbacks on `player`. Call BEFORE the first play(). `ctx` is passed
/// back to every callback (retained by the caller for the player's lifetime). Returns
/// NO if the private libvlc handle could not be reached (then keep libVLC's own audio).
BOOL ShardRouteVLCAudio(VLCMediaPlayer *player,
                        void *ctx,
                        ShardAudioSetupCb setup,
                        ShardAudioPlayCb play,
                        ShardAudioFlushCb flush);

NS_ASSUME_NONNULL_END

#import "VLCAudioBridge.h"
#import <objc/runtime.h>
#include <stdint.h>
#include <stdlib.h>

// ---- libvlc C prototypes (declared here; MobileVLCKit exports the symbols) --------

typedef struct libvlc_media_player_t libvlc_media_player_t;

typedef void (*libvlc_audio_play_cb)(void *data, const void *samples, unsigned count, int64_t pts);
typedef void (*libvlc_audio_pause_cb)(void *data, int64_t pts);
typedef void (*libvlc_audio_resume_cb)(void *data, int64_t pts);
typedef void (*libvlc_audio_flush_cb)(void *data, int64_t pts);
typedef void (*libvlc_audio_drain_cb)(void *data);

extern void libvlc_audio_set_callbacks(libvlc_media_player_t *mp,
                                       libvlc_audio_play_cb play,
                                       libvlc_audio_pause_cb pause,
                                       libvlc_audio_resume_cb resume,
                                       libvlc_audio_flush_cb flush,
                                       libvlc_audio_drain_cb drain,
                                       void *opaque);
extern int libvlc_audio_set_format(libvlc_media_player_t *mp,
                                   const char *format, unsigned rate, unsigned channels);

// ---- routing -------------------------------------------------------------------

// One route's Swift target, handed to every libvlc callback as `opaque`.
typedef struct {
    void *ctx;               // Unmanaged pointer to the Swift sink
    ShardAudioPlayCb play;
    ShardAudioFlushCb flush;
} ShardAudioRoute;

static void shard_play(void *data, const void *samples, unsigned count, int64_t pts) {
    ShardAudioRoute *r = (ShardAudioRoute *)data;
    if (r && r->play) r->play(r->ctx, (const int16_t *)samples, count, pts);
}
static void shard_pause(void *data, int64_t pts) { (void)data; (void)pts; }
static void shard_resume(void *data, int64_t pts) { (void)data; (void)pts; }
static void shard_flush(void *data, int64_t pts) {
    (void)pts;
    ShardAudioRoute *r = (ShardAudioRoute *)data;
    if (r && r->flush) r->flush(r->ctx);
}
static void shard_drain(void *data) {
    ShardAudioRoute *r = (ShardAudioRoute *)data;
    if (r && r->flush) r->flush(r->ctx);
}

// VLCKit 3.x keeps the C player in a private ivar `_playerInstance`. Reach it by the
// ivar's offset — object_getIvar only works for object ivars, and this is a raw C
// pointer. Tied to the pinned pod version; if a future VLCKit renames it this returns
// NULL and we fall back to libVLC's own audio (no crash, just the old behaviour).
static libvlc_media_player_t *handle_of(VLCMediaPlayer *player) {
    if (!player) return NULL;
    // object_getClass, not [player class]: VLCMediaPlayer is only forward-declared here
    // (we avoid importing MobileVLCKit's headers), so we cannot send it messages — the
    // runtime reads its class without the interface.
    Ivar iv = class_getInstanceVariable(object_getClass(player), "_playerInstance");
    if (!iv) return NULL;
    ptrdiff_t off = ivar_getOffset(iv);
    void *self_ptr = (__bridge void *)player;
    return *(libvlc_media_player_t **)((char *)self_ptr + off);
}

BOOL ShardRouteVLCAudio(VLCMediaPlayer *player,
                        void *ctx,
                        ShardAudioSetupCb setup,
                        ShardAudioPlayCb play,
                        ShardAudioFlushCb flush) {
    libvlc_media_player_t *mp = handle_of(player);
    if (!mp) return NO;

    ShardAudioRoute *r = calloc(1, sizeof(ShardAudioRoute));
    if (!r) return NO;
    r->ctx = ctx;
    r->play = play;
    r->flush = flush;

    // Fix the format so the play callback always gets S16 interleaved stereo at 48kHz
    // (libVLC resamples to it). 48k matches opus's native rate — no resample there.
    if (setup) setup(ctx, 48000, 2);
    libvlc_audio_set_format(mp, "S16N", 48000, 2);
    libvlc_audio_set_callbacks(mp, shard_play, shard_pause, shard_resume,
                               shard_flush, shard_drain, r);
    return YES;
}

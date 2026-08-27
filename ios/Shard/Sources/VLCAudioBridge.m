// LINK-FEASIBILITY SPIKE (temporary).
//
// The plan (b): stop letting libVLC drive the iOS audio output — its output
// crackles over a Bluetooth link an Apple Watch jitters, and it adds start/seek
// latency. Instead take libVLC's DECODED PCM through libvlc's C audio callbacks
// and play it through AVAudioEngine (Apple-native output, robust like AVPlayer),
// keeping libVLC only as the decoder so Opus/VP9/AV1 stay playable at best quality.
//
// That needs libvlc's C symbols, which MobileVLCKit's Objective-C wrapper does not
// expose. This file proves whether the framework at least EXPORTS those C symbols
// for us to call directly: it declares the prototype itself and references it from
// a `used` function so the linker must resolve `libvlc_audio_set_callbacks`. If the
// CI app link succeeds, the symbol is exported and plan (b) can proceed against
// MobileVLCKit; if it fails to link, we must add libvlc as a separate library.
//
// Nothing here runs — the reference exists only to force link resolution.

#include <stdint.h>

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

__attribute__((used))
void shard_vlc_audio_probe(libvlc_media_player_t *mp) {
    // Never called; the reference is what forces the linker to resolve the symbol.
    libvlc_audio_set_callbacks(mp, 0, 0, 0, 0, 0, 0);
}

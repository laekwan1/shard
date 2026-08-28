#import "VLCVideoBridge.h"
#import <objc/runtime.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// ---- libvlc C prototypes (declared here; MobileVLCKit exports the symbols) --------

typedef struct libvlc_media_player_t libvlc_media_player_t;

typedef void *(*libvlc_video_lock_cb)(void *opaque, void **planes);
typedef void (*libvlc_video_unlock_cb)(void *opaque, void *picture, void *const *planes);
typedef void (*libvlc_video_display_cb)(void *opaque, void *picture);
typedef unsigned (*libvlc_video_format_cb)(void **opaque, char *chroma,
                                           unsigned *width, unsigned *height,
                                           unsigned *pitches, unsigned *lines);
typedef void (*libvlc_video_cleanup_cb)(void *opaque);

extern void libvlc_video_set_callbacks(libvlc_media_player_t *mp,
                                       libvlc_video_lock_cb lock,
                                       libvlc_video_unlock_cb unlock,
                                       libvlc_video_display_cb display,
                                       void *opaque);
extern void libvlc_video_set_format_callbacks(libvlc_media_player_t *mp,
                                              libvlc_video_format_cb setup,
                                              libvlc_video_cleanup_cb cleanup);
extern int64_t libvlc_media_player_get_time(libvlc_media_player_t *mp);

// ---- routing -------------------------------------------------------------------

typedef struct {
    void *ctx;
    ShardVideoFormatCb format;
    ShardVideoFrameCb frame;
    libvlc_media_player_t *mp;
    uint8_t *buffer;          // one frame, BGRA, pitch*height
    unsigned width, height, pitch;
} ShardVideoRoute;

// libVLC negotiates the picture format. We force BGRA (one plane) so the frame drops
// straight into a 32BGRA CVPixelBuffer with no colour conversion of our own. A single
// buffer is enough: with one buffer libVLC will not begin decoding the next frame until
// display() returns, and we copy the frame out inside display().
static unsigned vformat(void **opaque, char *chroma,
                        unsigned *width, unsigned *height,
                        unsigned *pitches, unsigned *lines) {
    ShardVideoRoute *r = (ShardVideoRoute *)*opaque;
    unsigned w = *width, h = *height;
    memcpy(chroma, "BGRA", 4);
    unsigned pitch = w * 4;
    *pitches = pitch;
    *lines = h;
    r->width = w; r->height = h; r->pitch = pitch;
    free(r->buffer);
    r->buffer = malloc((size_t)pitch * h);
    if (!r->buffer) return 0;
    if (r->format) r->format(r->ctx, w, h, pitch);
    return 1;   // one buffer
}

static void vcleanup(void *opaque) {
    ShardVideoRoute *r = (ShardVideoRoute *)opaque;
    if (r) { free(r->buffer); r->buffer = NULL; }
}

static void *vlock(void *opaque, void **planes) {
    ShardVideoRoute *r = (ShardVideoRoute *)opaque;
    planes[0] = r->buffer;
    return NULL;   // no per-picture id needed
}

static void vunlock(void *opaque, void *picture, void *const *planes) {
    (void)opaque; (void)picture; (void)planes;
}

static void vdisplay(void *opaque, void *picture) {
    ShardVideoRoute *r = (ShardVideoRoute *)opaque;
    (void)picture;
    // libVLC calls display when its master clock reaches this frame's pts, so get_time
    // here IS (near enough) the frame's presentation time — on the same timeline as the
    // audio pts. The sink presents the frame when the audio at that time is heard.
    int64_t t = r->mp ? libvlc_media_player_get_time(r->mp) : 0;
    if (r->frame) r->frame(r->ctx, r->buffer, r->width, r->height, r->pitch, t);
}

// Same private-ivar reach as VLCAudioBridge (see there for why object_getClass, not
// [player class]).
static libvlc_media_player_t *handle_of(VLCMediaPlayer *player) {
    if (!player) return NULL;
    Ivar iv = class_getInstanceVariable(object_getClass(player), "_playerInstance");
    if (!iv) return NULL;
    ptrdiff_t off = ivar_getOffset(iv);
    void *self_ptr = (__bridge void *)player;
    return *(libvlc_media_player_t **)((char *)self_ptr + off);
}

BOOL ShardRouteVLCVideo(VLCMediaPlayer *player,
                        void *ctx,
                        ShardVideoFormatCb format,
                        ShardVideoFrameCb frame) {
    libvlc_media_player_t *mp = handle_of(player);
    if (!mp) return NO;

    ShardVideoRoute *r = calloc(1, sizeof(ShardVideoRoute));
    if (!r) return NO;
    r->ctx = ctx;
    r->format = format;
    r->frame = frame;
    r->mp = mp;

    // Order matters: format callbacks first (they set *opaque for the render callbacks),
    // then the render callbacks with our route as the opaque.
    libvlc_video_set_format_callbacks(mp, vformat, vcleanup);
    libvlc_video_set_callbacks(mp, vlock, vunlock, vdisplay, r);
    return YES;
}

// C ABI exported by libshard_mobile (crates/shard-mobile/src/download.rs).
//
// Swift links these symbols directly — the same interface Kotlin reaches over
// JNI. Nothing Rust-shaped crosses: plain C strings and function pointers only.
#ifndef SHARD_CORE_H
#define SHARD_CORE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Called as bytes arrive: (ctx, downloaded, total). total is 0 when unknown.
typedef void (*ShardProgress)(void *ctx, uint64_t done, uint64_t total);

// Polled between chunks: return non-zero to abort. ctx is passed through.
typedef int (*ShardCancel)(void *ctx);

// Save a segmented (HLS) stream. Returns an owned JSON C string
// {"ok":true,"path":"..."} or {"ok":false,"error":"..."}; free it with
// shard_string_free. referer/title may be NULL.
char *shard_download_hls(const char *manifest_url, const char *referer,
                         const char *into_dir, const char *title,
                         ShardProgress progress, ShardCancel cancel, void *ctx);

// Save a plain file URL (progressive MP4 / direct <video src>). Same contract.
char *shard_download_direct(const char *url, const char *referer,
                            const char *into_dir, const char *title,
                            ShardProgress progress, ShardCancel cancel,
                            void *ctx);

// Release a string returned by this library.
void shard_string_free(char *ptr);

#ifdef __cplusplus
}
#endif

#endif // SHARD_CORE_H

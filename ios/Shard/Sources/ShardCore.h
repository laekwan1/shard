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
                         const char *cookie, const char *user_agent,
                         const char *extra_headers, const char *into_dir,
                         const char *title, ShardProgress progress,
                         ShardCancel cancel, void *ctx);

// Save a plain file URL (progressive MP4 / direct <video src>). Same contract.
char *shard_download_direct(const char *url, const char *referer,
                            const char *cookie, const char *user_agent,
                            const char *extra_headers, const char *into_dir,
                            const char *title, ShardProgress progress,
                            ShardCancel cancel, void *ctx);

// Quality rows for a captured YouTube offer (the ytInitialPlayerResponse the
// page script built). Returns owned JSON: {"ok":true,"rows":[{itag,label,detail}]}
// or {"ok":false,"error":"..."}. itag 4294967295 is the audio-only row. Free
// with shard_string_free.
char *shard_youtube_qualities(const char *offer_json);

// Pickable qualities for an HLS stream. Returns owned JSON:
// {"ok":true,"rows":[{"url","label","detail"}]} (highest first, one per
// resolution) or {"ok":false,"error":"..."}. Empty rows means a plain media
// playlist with nothing to choose — download it directly. referer/cookie may be
// NULL. Free with shard_string_free.
char *shard_hls_qualities(const char *manifest_url, const char *referer,
                          const char *cookie, const char *user_agent,
                          const char *extra_headers);

// Download a YouTube video (or its audio alone) from a captured offer. itag
// names the wanted format, or 4294967295 for audio only. Same contract as
// shard_download_hls.
char *shard_download_youtube(const char *offer_json, uint32_t itag,
                             const char *into_dir, ShardProgress progress,
                             ShardCancel cancel, void *ctx);

// ── iOS 자체 서명 엔진 (libshard_mobile을 --features resign로 빌드할 때만) ──────────
// 2FA 코드를 돌려주는 콜백. 반환 문자열은 콜백이 리턴할 때까지 유효하면 되고 Rust가 복사한다.
typedef const char *(*ShardTfa)(void *ctx);
// 진행 로그 한 줄(화면에 표시). line은 이 호출 동안만 유효.
typedef void (*ShardLog)(void *ctx, const char *line);

// 로그인 → 재서명 → (device_addr+pairing_path가 있으면) 설치를 이 스레드에서 동기로 실행.
// 소유 JSON C 문자열 {"ok":true,"path":"<서명된 .ipa>"} 또는 {"ok":false,"error":"..."} 반환;
// shard_string_free로 해제. device_addr/pairing_path는 NULL 가능(그러면 서명만).
char *shard_resign_run(const char *email, const char *password,
                       const char *bundle_id, const char *app_name,
                       const char *ipa_path, const char *state_dir,
                       const char *work_dir, const char *device_addr,
                       const char *pairing_path, ShardTfa tfa, ShardLog log,
                       void *ctx);

// 첫 테스트: .ipa/설치 없이 로그인→인증서→App ID→프로파일 발급(②③)만 검증. 소유 JSON 반환
// {"ok":true,"path":"team=...; appId=...; profile=...B"} 또는 {"ok":false,"error":"..."};
// shard_string_free로 해제.
char *shard_resign_verify(const char *email, const char *password,
                          const char *bundle_id, const char *app_name,
                          const char *state_dir, ShardTfa tfa, ShardLog log,
                          void *ctx);

// ④ 1단계 스모크 테스트: 페어링 파일 + 터널 주소(예: 10.7.0.1)로 폰 lockdownd에 붙는지 확인.
// 소유 JSON {"ok":true,"path":"lockdownd 통과 — iOS ..."} 또는 {"ok":false,"error":"..."} 반환;
// shard_string_free로 해제. log로 단계 진행을 화면에 스트리밍.
char *shard_resign_probe(const char *pairing_path, const char *addr,
                         ShardLog log, void *ctx);

// ④+⑤ 자기 자신 갱신: 실행 중 앱 번들을 재서명해 자기 자신에 업그레이드 설치까지. bundle_id는 실행
// 중 번들의 CFBundleIdentifier(같아야 in-place), app_bundle_path는 Bundle.main.bundlePath.
// device_addr(터널)+pairing_path가 있으면 설치까지, NULL이면 서명만. 소유 JSON 반환, shard_string_free.
char *shard_resign_selfupdate(const char *email, const char *password,
                              const char *bundle_id, const char *app_name,
                              const char *app_bundle_path, const char *state_dir,
                              const char *work_dir, const char *device_addr,
                              const char *pairing_path, ShardTfa tfa,
                              ShardLog log, void *ctx);

// The bypass engine: a local HTTP/CONNECT proxy that applies the DPI/SNI
// desync. shard_start binds 127.0.0.1:<port> (0 = any free port) and returns the
// bound port, or a negative error. The WebView is then pointed at that port.
int shard_start(const char *config_dir, uint16_t port);
void shard_stop(void);
int shard_is_running(void);

// Release a string returned by this library.
void shard_string_free(char *ptr);

#ifdef __cplusplus
}
#endif

#endif // SHARD_CORE_H

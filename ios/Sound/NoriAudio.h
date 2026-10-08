// The audio unit the Rust output drives (crates/ios/src/output.rs). The callbacks are Rust.
// NoriGrant is also declared there as AudioGrant; the two must agree.

#ifndef NORI_AUDIO_H
#define NORI_AUDIO_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint32_t rate;
    uint32_t io_ms;
    uint64_t latency_us;
    int32_t port;
    char name[128];
} NoriGrant;

/// 0 on success. `err` receives a NUL-terminated message otherwise.
int nori_audio_open(uint32_t rate, uint32_t channels, uint32_t io_ms, NoriGrant *out, char *err, uint32_t err_len);
int nori_audio_start(char *err, uint32_t err_len);
void nori_audio_stop(void);
int nori_audio_set_io_ms(uint32_t io_ms, uint32_t *granted_ms, char *err, uint32_t err_len);
void nori_audio_route(NoriGrant *out);
void nori_audio_close(void);

/// Rust, called from the render callback. `out` is interleaved float, `frames` × the opened channels;
/// `ahead_us` is how long after the call the buffer's first frame leaves the unit.
void nori_ios_render(uint32_t frames, float *out, uint64_t ahead_us);
/// `unavailable` when the previous device is gone (headphones pulled).
void nori_ios_route(int32_t port, const char *name, uint64_t latency_us, int unavailable);
void nori_ios_media_reset(void);
/// A line for the core's log.
void nori_ios_audio_log(const char *line);
void nori_ios_interruption(int began, int resume);

#ifdef __cplusplus
}
#endif

#endif

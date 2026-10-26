#ifndef SAPFLOW_SIGNET_FFI_H
#define SAPFLOW_SIGNET_FFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define SAPFLOW_SIGNET_ABI_VERSION 1

#define SAPFLOW_SIGNET_INVALID_INPUT 1
#define SAPFLOW_SIGNET_INVALID_CREDENTIAL 2
#define SAPFLOW_SIGNET_INVALID_PROOF 3
#define SAPFLOW_SIGNET_BUSY 4
#define SAPFLOW_SIGNET_DEADLINE 5
#define SAPFLOW_SIGNET_CANCELLED 6
#define SAPFLOW_SIGNET_REFUSED 7
#define SAPFLOW_SIGNET_UNAVAILABLE 8
#define SAPFLOW_SIGNET_CLOSED 9

#define SAPFLOW_SIGNET_EVENT_NONE 0
#define SAPFLOW_SIGNET_EVENT_READY 1
#define SAPFLOW_SIGNET_EVENT_PROOF 2
#define SAPFLOW_SIGNET_EVENT_FAILED 3

typedef struct {
    uint8_t *ptr;
    size_t len;
} SapflowSignetBuffer;

typedef struct {
    uint32_t tag;
    uint64_t request_id;
    int32_t error;
    SapflowSignetBuffer bytes;
    void *credential;
} SapflowSignetEvent;

/* Every non-null input pointer must address len readable bytes for the call.
 * Every output pointer must be writable. Returned buffers, events and handles
 * are owned by the caller and freed exactly once with their matching function.
 * Error returns are fixed SAPFLOW_SIGNET_* codes and contain no secret text. */
uint32_t sapflow_signet_abi_version(void);
size_t sapflow_signet_buffer_size(void);
size_t sapflow_signet_event_size(void);
void sapflow_signet_buffer_free(SapflowSignetBuffer buffer);
void sapflow_signet_secret_free(SapflowSignetBuffer buffer);
void sapflow_signet_event_free(SapflowSignetEvent event);

int32_t sapflow_signet_credential_restore(const uint8_t *input, size_t input_len, void **out);
void sapflow_signet_credential_free(void *credential);
int32_t sapflow_signet_credential_encoded(const void *credential, SapflowSignetBuffer *out);

int32_t sapflow_signet_native_pair_bunker(const uint8_t *uri, size_t uri_len, uint32_t deadline_ms, void **out);
int32_t sapflow_signet_native_restore(const void *credential, uint32_t deadline_ms, void **out);
void sapflow_signet_native_free(void *owner);
int32_t sapflow_signet_native_connect(const void *owner, uint64_t request_id);
int32_t sapflow_signet_native_sign_login(const void *owner, uint64_t request_id,
    const uint8_t *challenge, size_t challenge_len,
    const uint8_t *origin, size_t origin_len,
    const uint8_t *app_name, size_t app_name_len,
    uint64_t created_at_seconds);
int32_t sapflow_signet_native_wait_event(const void *owner, uint32_t timeout_ms, SapflowSignetEvent *out);
int32_t sapflow_signet_native_request_close(const void *owner);
int32_t sapflow_signet_native_wait_closed(const void *owner, uint32_t timeout_ms, bool *out);
int32_t sapflow_signet_native_is_closed(const void *owner, bool *out);

#ifdef __cplusplus
}
#endif
#endif

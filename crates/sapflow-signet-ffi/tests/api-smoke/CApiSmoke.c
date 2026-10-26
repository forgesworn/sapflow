#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "SignetAuthFFI.h"

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "c-api-smoke failed at line %d\n", __LINE__); return 1; \
} } while (0)
#define STATUS(call) CHECK((call) == 0)

static const char *uri = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid";
static const char *persona = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

static int close_owner(void *owner) {
    STATUS(sapflow_signet_native_request_close(owner));
    bool closed = false;
    STATUS(sapflow_signet_native_wait_closed(owner, 2000, &closed));
    CHECK(closed);
    STATUS(sapflow_signet_native_is_closed(owner, &closed));
    CHECK(closed);
    return 0;
}

int main(void) {
    CHECK(sapflow_signet_abi_version() == SAPFLOW_SIGNET_ABI_VERSION);
    CHECK(sapflow_signet_buffer_size() == sizeof(SapflowSignetBuffer));
    CHECK(sapflow_signet_event_size() == sizeof(SapflowSignetEvent));

    char credential[1024];
    int credential_len = snprintf(credential, sizeof(credential),
        "{\"version\":1,\"persona\":\"%s\",\"app_secret_key\":\"%064d\",\"bunker_uri\":\"%s\"}",
        persona, 1, uri);
    CHECK(credential_len > 0 && (size_t)credential_len < sizeof(credential));
    char expected[1024];
    memcpy(expected, credential, (size_t)credential_len);

    void *saved = NULL;
    STATUS(sapflow_signet_credential_restore((const uint8_t *)credential, (size_t)credential_len, &saved));
    memset(credential, 0, (size_t)credential_len);
    SapflowSignetBuffer encoded = {0};
    STATUS(sapflow_signet_credential_encoded(saved, &encoded));
    CHECK(encoded.len == (size_t)credential_len && memcmp(encoded.ptr, expected, encoded.len) == 0);
    sapflow_signet_secret_free(encoded);

    const char *bad = "{}";
    CHECK(sapflow_signet_credential_restore((const uint8_t *)bad, 2, &saved) == SAPFLOW_SIGNET_INVALID_CREDENTIAL);
    CHECK(sapflow_signet_credential_restore(NULL, 1, &saved) == SAPFLOW_SIGNET_INVALID_INPUT);

    void *owner = NULL;
    CHECK(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 0, &owner) == SAPFLOW_SIGNET_INVALID_INPUT);
    CHECK(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 120001, &owner) == SAPFLOW_SIGNET_INVALID_INPUT);
    const char *insecure = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=ws://relay.example.invalid";
    CHECK(sapflow_signet_native_pair_bunker((const uint8_t *)insecure, strlen(insecure), 1000, &owner) == SAPFLOW_SIGNET_INVALID_INPUT);

    STATUS(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 120000, &owner));
    void *other = NULL;
    CHECK(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 1000, &other) == SAPFLOW_SIGNET_BUSY);
    CHECK(other == NULL);
    CHECK(sapflow_signet_native_connect(owner, 0) == SAPFLOW_SIGNET_INVALID_INPUT);
    const char *challenge = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const char *origin = "https://fixture.example.invalid";
    const char *app = "Generated offline fixture";
    CHECK(sapflow_signet_native_sign_login(owner, 1,
        (const uint8_t *)challenge, strlen(challenge),
        (const uint8_t *)origin, strlen(origin),
        (const uint8_t *)app, strlen(app), 1) == SAPFLOW_SIGNET_INVALID_INPUT);
    SapflowSignetEvent event = {0};
    CHECK(sapflow_signet_native_wait_event(owner, 1001, &event) == SAPFLOW_SIGNET_INVALID_INPUT);
    bool closed = false;
    CHECK(sapflow_signet_native_wait_closed(owner, 2001, &closed) == SAPFLOW_SIGNET_INVALID_INPUT);
    CHECK(close_owner(owner) == 0);
    CHECK(sapflow_signet_native_connect(owner, 1) == SAPFLOW_SIGNET_CLOSED);
    CHECK(close_owner(owner) == 0);
    sapflow_signet_native_free(owner);

    owner = NULL;
    STATUS(sapflow_signet_native_restore(saved, 120000, &owner));
    CHECK(close_owner(owner) == 0);
    sapflow_signet_native_free(owner);

    owner = NULL;
    STATUS(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 30, &owner));
    do {
        STATUS(sapflow_signet_native_wait_event(owner, 100, &event));
    } while (event.tag == SAPFLOW_SIGNET_EVENT_NONE);
    CHECK(event.tag == SAPFLOW_SIGNET_EVENT_FAILED && event.request_id == 0 && event.error == SAPFLOW_SIGNET_DEADLINE);
    sapflow_signet_event_free(event);
    STATUS(sapflow_signet_native_wait_closed(owner, 2000, &closed));
    CHECK(closed);
    memset(&event, 0, sizeof(event));
    STATUS(sapflow_signet_native_wait_event(owner, 0, &event));
    CHECK(event.tag == SAPFLOW_SIGNET_EVENT_NONE);
    CHECK(sapflow_signet_native_connect(owner, 1) == SAPFLOW_SIGNET_CLOSED);
    sapflow_signet_native_free(owner);

    owner = NULL;
    STATUS(sapflow_signet_native_pair_bunker((const uint8_t *)uri, strlen(uri), 120000, &owner));
    CHECK(close_owner(owner) == 0);
    sapflow_signet_native_free(owner);
    sapflow_signet_credential_free(saved);

    puts("c-api-smoke: ok");
    return 0;
}

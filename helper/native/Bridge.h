#ifndef MACS_FAN_HELPER_BRIDGE_H
#define MACS_FAN_HELPER_BRIDGE_H
#include <stddef.h>
#include <stdint.h>
// Opaque handle lives for the process lifetime; calls are serialized by the
// native host queue. Owner bytes come only from its authenticated session.
void *mf_helper_create(uint8_t fake);
// Always read-only; failure is reported by backend_open_error in responses.
void *mf_helper_create_smc_readonly(void);
// Internal profile admission; production service intentionally does not call it.
void *mf_helper_create_smc_control(void);
char *mf_helper_request(void *handle, const uint8_t owner[32], const uint8_t *json, size_t size);
char *mf_helper_disconnect(void *handle, const uint8_t owner[32]);
char *mf_helper_tick(void *handle);
char *mf_helper_shutdown(void *handle);
// Trusted native events: 1=sleep, 2=wake (remains locked), 3=shutdown.
// No wire request can invoke these functions.
char *mf_helper_lifecycle_begin(void *handle, uint8_t event);
char *mf_helper_lifecycle_poll(void *handle);
#ifdef MF_TEST_HARNESS
void *mf_helper_test_create(uint8_t failed_automatic_writes);
// Fixture-only; disconnect/shutdown and stop all concurrent calls before freeing.
void mf_helper_test_destroy(void *handle);
#endif
void mf_helper_free(char *response);
#endif

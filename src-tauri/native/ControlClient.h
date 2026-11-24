#ifndef MF_CONTROL_CLIENT_H
#define MF_CONTROL_CLIENT_H
#include <stddef.h>
#include <stdint.h>
// Owned UTF-8 JSON. Free each non-NULL result exactly once. Input is only a
// closed array of helper plan entries, never transport identity or credentials.
char *mf_control_start(const uint8_t *entries_json, size_t length, uint64_t intent_id);
char *mf_control_status(void);
char *mf_control_stop(uint64_t intent_id);
void mf_control_free(char *json);
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
#import <Foundation/Foundation.h>
typedef struct {
    BOOL (*self_matches)(NSString *requirement);
    id (*connection)(NSString *name, NSXPCConnectionOptions options);
    BOOL allow_fake;
} MFControlClientTestBridge;
void MFControlClientTestInstallBridge(MFControlClientTestBridge bridge);
void MFControlClientTestSetSequence(uint64_t sequence);
#endif
#endif

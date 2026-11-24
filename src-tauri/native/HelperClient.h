#ifndef MF_HELPER_CLIENT_H
#define MF_HELPER_CLIENT_H
// Owned UTF-8 JSON; release exactly once. No identity, service or request input.
char *mf_helper_inspect(void);
void mf_helper_client_free(char *json);
#ifdef MF_HELPER_CLIENT_TEST_HARNESS
#import <Foundation/Foundation.h>
// Test builds NEVER fall through to the production connection constructor.
// Security remains real unless a test explicitly installs self_matches.
typedef struct {
    BOOL (*self_matches)(NSString *requirement);
    id (*connection)(NSString *name, NSXPCConnectionOptions options);
} MFHelperClientTestBridge;
void MFHelperClientTestInstallBridge(MFHelperClientTestBridge bridge);
#endif
#endif

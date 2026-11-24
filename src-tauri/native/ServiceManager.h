#ifndef MF_SERVICE_MANAGER_H
#define MF_SERVICE_MANAGER_H

// Owned UTF-8 JSON. Call mf_service_free exactly once for each non-NULL result.
char *mf_service_status(void);
char *mf_service_register(void);
char *mf_service_unregister(void);
void mf_service_free(char *json);

#ifdef MF_SERVICE_TEST_HARNESS
#import <Foundation/Foundation.h>
// Compiled out of production. A probe can exercise state mapping and count
// mutations without ever loading or calling the real ServiceManagement API.
typedef struct {
    NSInteger (*status)(void);
    BOOL (*register_service)(NSError **error);
    BOOL (*unregister_service)(NSError **error);
} MFServiceTestBridge;
void MFServiceTestInstallBridge(MFServiceTestBridge bridge);
#endif

#endif

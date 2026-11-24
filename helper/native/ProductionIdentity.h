#import <Foundation/Foundation.h>
// Immutable build-time production identity. Never exported over XPC and never
// populated from an external Team, environment, plist, or client argument.
FOUNDATION_EXPORT NSString *MFProductionClientBundleIdentifier(void);
FOUNDATION_EXPORT NSString *MFProductionHelperBundleIdentifier(void);
FOUNDATION_EXPORT NSString *MFProductionMachServiceName(void);
FOUNDATION_EXPORT NSString *MFProductionConfiguredTeamIdentifier(void);
// nil until a reviewed Team is configured; keys are "client" and "helper".
FOUNDATION_EXPORT NSDictionary *MFProductionConfiguredRequirements(void);
// Identity validation only: this function never starts or probes a listener.
FOUNDATION_EXPORT NSDictionary *MFProductionIdentityCheck(void);
#ifdef MF_TEST_HARNESS
FOUNDATION_EXPORT NSDictionary *MFProductionIdentityProbe(void);
#endif

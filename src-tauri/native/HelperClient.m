#import "HelperClient.h"
#import "../../helper/native/ProductionIdentity.h"
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <objc/message.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <stdint.h>

@protocol MFObservationHelper
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end

static const NSUInteger MFMaximumReplyBytes = 1024 * 1024;
static uint64_t MFNow(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return UINT64_MAX;
    return (uint64_t)now.tv_sec * NSEC_PER_SEC + (uint64_t)now.tv_nsec;
}
static uint64_t MFBudget(void) {
#if defined(MF_HELPER_CLIENT_TEST_HARNESS) && !defined(MF_HELPER_CLIENT_TEST_FULL_TIMEOUT)
    return 30 * NSEC_PER_MSEC;
#else
    return 3 * NSEC_PER_SEC;
#endif
}
#ifdef MF_HELPER_CLIENT_TEST_HARNESS
static MFHelperClientTestBridge MFTestBridge;
void MFHelperClientTestInstallBridge(MFHelperClientTestBridge bridge) { MFTestBridge = bridge; }
#endif
static BOOL MFSupported(void) {
#if defined(MF_HELPER_CLIENT_TEST_HARNESS) && defined(MF_HELPER_CLIENT_TEST_OS_MAJOR)
    return MF_HELPER_CLIENT_TEST_OS_MAJOR >= 13;
#else
    return NSProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 13;
#endif
}
static BOOL MFBoolean(id value, BOOL expected) {
    return value && CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID() && [value boolValue] == expected;
}
static BOOL MFUnsigned(id value) {
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return NO;
    return strchr("cCsSiIlLqQ", [value objCType][0]) && [value compare:@0] != NSOrderedAscending && [value compare:@(UINT64_MAX)] != NSOrderedDescending;
}
static BOOL MFVersion(id value) { return MFUnsigned(value) && [value isEqual:@1]; }
static NSMutableDictionary *MFResult(NSString *result, NSString *reason, BOOL authenticated, NSDictionary *response) {
    NSMutableDictionary *value = [@{@"schema_version":@1, @"operation":@"inspect_hardware",
        @"hardware_write_enabled":@NO, @"production_control_ready":@NO,
        @"authenticated_response":@(authenticated), @"result":result, @"reason":reason} mutableCopy];
    if (response) value[@"response"] = response;
    return value;
}
static char *MFEncode(NSDictionary *value) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:value options:NSJSONWritingSortedKeys error:NULL];
    if (!data) return NULL;
    char *bytes = malloc(data.length + 1);
    if (!bytes) return NULL;
    memcpy(bytes, data.bytes, data.length); bytes[data.length] = 0;
    return bytes;
}
static char *MFBoundary(BOOL busy) {
    return strdup(busy ?
        "{\"schema_version\":1,\"operation\":\"inspect_hardware\",\"hardware_write_enabled\":false,\"production_control_ready\":false,\"authenticated_response\":false,\"result\":\"blocked\",\"reason\":\"helper_client_busy\"}" :
        "{\"schema_version\":1,\"operation\":\"inspect_hardware\",\"hardware_write_enabled\":false,\"production_control_ready\":false,\"authenticated_response\":false,\"result\":\"unavailable\",\"reason\":\"helper_client_exception\"}");
}
static BOOL MFCompileRequirement(NSString *text) {
    SecRequirementRef requirement = NULL;
    BOOL valid = [text isKindOfClass:NSString.class] && text.length &&
        SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement) == errSecSuccess;
    if (requirement) CFRelease(requirement);
    return valid;
}
static BOOL MFSelfMatches(NSString *text) {
#ifdef MF_HELPER_CLIENT_TEST_HARNESS
    if (MFTestBridge.self_matches) return MFTestBridge.self_matches(text);
#endif
    SecRequirementRef requirement = NULL;
    SecCodeRef code = NULL;
    BOOL valid = NO;
    if (![text isKindOfClass:NSString.class] || SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement) != errSecSuccess) goto done;
    if (SecCodeCopySelf(kSecCSDefaultFlags, &code) != errSecSuccess) goto done;
    valid = SecCodeCheckValidity(code, kSecCSStrictValidate, requirement) == errSecSuccess;
done:
    if (requirement) CFRelease(requirement);
    if (code) CFRelease(code);
    return valid;
}
static NSXPCConnection *MFConnection(void) {
#ifdef MF_HELPER_CLIENT_TEST_HARNESS
    return MFTestBridge.connection ? MFTestBridge.connection(MFProductionMachServiceName(), NSXPCConnectionPrivileged) : nil;
#else
    return [[NSXPCConnection alloc] initWithMachServiceName:MFProductionMachServiceName() options:NSXPCConnectionPrivileged];
#endif
}
// A callback owns this state independently of the caller stack. The first
// terminal event wins, including timeout; late XPC blocks cannot use freed data.
@interface MFObservationCompletion : NSObject
@property(nonatomic, strong) NSLock *lock;
@property(nonatomic, strong) NSDictionary *value;
@property(nonatomic, strong) dispatch_semaphore_t done;
@property(nonatomic) uint64_t deadline;
- (void)finish:(NSDictionary *)value;
- (NSDictionary *)snapshot;
@end
@implementation MFObservationCompletion
- (instancetype)init {
    self = [super init];
    if (self) {
        _lock = [NSLock new]; _done = dispatch_semaphore_create(0);
        uint64_t now = MFNow();
        _deadline = now > UINT64_MAX - MFBudget() ? 0 : now + MFBudget();
    }
    return self;
}
- (void)finish:(NSDictionary *)value {
    [self.lock lock];
    BOOL first = self.value == nil;
    if (first) {
        // Claim-time enforcement closes the wait-timeout/late-reply race even
        // if the waiting thread is descheduled before it records timeout.
        self.value = MFNow() >= self.deadline ? MFResult(@"unavailable", @"helper_client_timeout", NO, nil) : value;
    }
    [self.lock unlock];
    if (first) dispatch_semaphore_signal(self.done);
}
- (NSDictionary *)snapshot {
    [self.lock lock]; NSDictionary *value = self.value; [self.lock unlock]; return value;
}
@end

static NSDictionary *MFValidateReply(id payload) {
    if (![payload isKindOfClass:NSData.class]) return MFResult(@"unavailable", @"helper_reply_invalid", NO, nil);
    if ([payload length] > MFMaximumReplyBytes) return MFResult(@"unavailable", @"helper_reply_too_large", NO, nil);
    id object = [NSJSONSerialization JSONObjectWithData:payload options:0 error:NULL];
    if (![object isKindOfClass:NSDictionary.class]) return MFResult(@"unavailable", @"helper_reply_invalid", NO, nil);
    NSDictionary *reply = object;
    NSSet *allowed = [NSSet setWithArray:@[@"version", @"dry_run", @"hardware_write_enabled", @"ok", @"error", @"result",
        @"helper_process_id", @"watchdog_ticks", @"watchdog_expirations", @"lifecycle", @"backend", @"hardware_reads",
        @"hardware_write_attempts", @"hardware_write_attempts_scope", @"hardware_inspection_observed_at_ms", @"backend_open_error", @"statuses"]];
    BOOL readonly = [reply[@"backend"] isEqual:@"smc_readonly"] && MFBoolean(reply[@"dry_run"], YES) && MFBoolean(reply[@"hardware_write_enabled"], NO);
    BOOL control = [reply[@"backend"] isEqual:@"smc_control"] && MFBoolean(reply[@"dry_run"], NO) && MFBoolean(reply[@"hardware_write_enabled"], YES);
    BOOL unavailable = [reply[@"backend"] isEqual:@"unavailable"] && MFBoolean(reply[@"dry_run"], YES) && MFBoolean(reply[@"hardware_write_enabled"], NO);
    BOOL valid = MFVersion(reply[@"version"]) && (readonly || control || unavailable);
    for (id key in reply) if (![allowed containsObject:key]) valid = NO;
    id writes = reply[@"hardware_write_attempts"];
    if (writes && writes != NSNull.null && (!MFUnsigned(writes) || ((readonly || unavailable) && ![writes isEqual:@0]))) valid = NO;
    if (reply[@"hardware_reads"] && !MFBoolean(reply[@"hardware_reads"], (BOOL)!unavailable)) valid = NO;
    if (MFBoolean(reply[@"ok"], YES)) {
        NSDictionary *result = reply[@"result"];
        valid = valid && !reply[@"error"] && (readonly || control) && MFBoolean(reply[@"hardware_reads"], YES) &&
            [result isKindOfClass:NSDictionary.class] && result.count == 2 && [result[@"kind"] isEqual:@"hardware_inspection"] &&
            [result[@"inspection"] isKindOfClass:NSDictionary.class];
        if (valid) {
            NSDictionary *inspection = result[@"inspection"];
            // These are observation markers, never a grant to enable controls.
            id verified=inspection[@"hardwareProfileVerified"], attempts=inspection[@"hardwareWriteAttempts"];
            valid = MFUnsigned(attempts) && MFUnsigned(reply[@"hardware_write_attempts"]) &&
                [attempts isEqual:reply[@"hardware_write_attempts"]] &&
                (control ? (MFBoolean(verified, YES) || MFBoolean(verified, NO)) :
                    (MFBoolean(verified, NO) && [attempts isEqual:@0]));
        }
        if (valid) return MFResult(@"observed", @"authenticated_hardware_inspection", YES, reply);
    } else if (MFBoolean(reply[@"ok"], NO)) {
        NSDictionary *error = reply[@"error"];
        valid = valid && !reply[@"result"] && [error isKindOfClass:NSDictionary.class] && error.count == 1 &&
            [error[@"code"] isKindOfClass:NSString.class] && [error[@"code"] length] > 0 && [error[@"code"] length] <= 128 &&
            (readonly || control || unavailable);
        if (valid) return MFResult(@"helper_error", @"authenticated_helper_error", YES, reply);
    }
    return MFResult(@"unavailable", @"helper_reply_invalid", NO, nil);
}
static void MFFinish(MFObservationCompletion *completion, NSString *reason) {
    @try { [completion finish:MFResult(@"unavailable", reason, NO, nil)]; }
    @catch (NSException *exception) { (void)exception; }
}
static NSDictionary *MFObserve(void) {
    // This gate is intentionally before identity or any new API invocation.
    if (!MFSupported()) return MFResult(@"blocked", @"macos_13_required", NO, nil);
    NSDictionary *requirements = MFProductionConfiguredRequirements();
    if (!requirements || !MFProductionConfiguredTeamIdentifier().length) return MFResult(@"blocked", @"production_team_unconfigured", NO, nil);
    if (!MFSelfMatches(requirements[@"client"])) return MFResult(@"blocked", @"production_client_signature_requirement_failed", NO, nil);
    if (!MFCompileRequirement(requirements[@"helper"])) return MFResult(@"blocked", @"production_helper_requirement_invalid", NO, nil);
    MFObservationCompletion *completion = [MFObservationCompletion new];
    NSXPCConnection *connection = nil;
    @try {
        connection = MFConnection();
        if (!connection) return MFResult(@"unavailable", @"helper_connection_unavailable", NO, nil);
        // MFSupported above gates this public macOS 13 API. Dynamic dispatch
        // avoids compiler availability-runtime linkage in Rust's -nodefaultlibs.
        SEL requirementSelector = NSSelectorFromString(@"setCodeSigningRequirement:");
        if (![connection respondsToSelector:requirementSelector])
            return MFResult(@"unavailable", @"helper_signing_requirement_api_unavailable", NO, nil);
        ((void (*)(id, SEL, NSString *))objc_msgSend)(connection, requirementSelector, requirements[@"helper"]);
        [connection setRemoteObjectInterface:[NSXPCInterface interfaceWithProtocol:@protocol(MFObservationHelper)]];
        [connection setInterruptionHandler:^{ MFFinish(completion, @"helper_connection_interrupted"); }];
        [connection setInvalidationHandler:^{ MFFinish(completion, @"helper_connection_invalidated"); }];
        [connection resume];
        id<MFObservationHelper> proxy = [connection remoteObjectProxyWithErrorHandler:^(NSError *error) {
            (void)error; MFFinish(completion, @"helper_xpc_error");
        }];
        NSData *request = [@"{\"version\":1,\"command\":{\"op\":\"inspect_hardware\"}}" dataUsingEncoding:NSUTF8StringEncoding];
        [proxy request:request reply:^(NSData *reply) {
            @try { [completion finish:MFValidateReply(reply)]; }
            @catch (NSException *exception) { (void)exception; MFFinish(completion, @"helper_client_exception"); }
        }];
        // NSXPC replies run on its background queue; no main runloop is required.
        uint64_t now = MFNow();
        uint64_t remaining = completion.deadline > now ? completion.deadline - now : 0;
        if (dispatch_semaphore_wait(completion.done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)remaining))) MFFinish(completion, @"helper_client_timeout");
        return [completion snapshot] ?: MFResult(@"unavailable", @"helper_client_exception", NO, nil);
    } @catch (NSException *exception) {
        (void)exception;
        return [completion snapshot] ?: MFResult(@"unavailable", @"helper_client_exception", NO, nil);
    } @finally {
        // A broken invalidation must not overwrite the first terminal response.
        @try { [connection invalidate]; } @catch (NSException *exception) { (void)exception; }
    }
}
char *mf_helper_inspect(void) {
    static pthread_mutex_t operation = PTHREAD_MUTEX_INITIALIZER;
    if (pthread_mutex_trylock(&operation) != 0) return MFBoundary(YES);
    @try {
        @autoreleasepool { return MFEncode(MFObserve()); }
    } @catch (NSException *exception) {
        (void)exception; return MFBoundary(NO);
    } @finally { pthread_mutex_unlock(&operation); }
}
void mf_helper_client_free(char *json) { free(json); }

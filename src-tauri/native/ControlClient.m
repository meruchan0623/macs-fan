#import "ControlClient.h"
#import "../../helper/native/ProductionIdentity.h"
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <objc/message.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

@protocol MFControlHelper
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end
static uint64_t Now(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) return UINT64_MAX;
    return (uint64_t)t.tv_sec * NSEC_PER_SEC + (uint64_t)t.tv_nsec;
}
static uint64_t Budget(void) {
#if defined(MF_CONTROL_CLIENT_TEST_HARNESS) && defined(MF_CONTROL_CLIENT_TEST_SHORT_TIMEOUT)
    return 200 * NSEC_PER_MSEC;
#else
    return 3 * NSEC_PER_SEC;
#endif
}
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
static MFControlClientTestBridge Bridge;
void MFControlClientTestInstallBridge(MFControlClientTestBridge bridge) { Bridge = bridge; }
#endif
static BOOL Bool(id x) { return x && CFGetTypeID((__bridge CFTypeRef)x) == CFBooleanGetTypeID(); }
static BOOL Number(id x) { return [x isKindOfClass:NSNumber.class] && !Bool(x) && isfinite([x doubleValue]); }
static BOOL Unsigned(id x, uint64_t max) {
    if (!Number(x)) return NO;
    // JSON u64 stays integer, never converted through double for seq/deadlines.
    const char *t = [x objCType];
    if (strchr("cCsSiIlLqQ", t[0]) == NULL || [x compare:@0] == NSOrderedAscending) return NO;
    return [x compare:@(max)] != NSOrderedDescending;
}
static BOOL Keys(id x, NSArray *required, NSArray *optional) {
    if (![x isKindOfClass:NSDictionary.class]) return NO;
    for (id k in required) if (!x[k]) return NO;
    for (id k in x) if (![required containsObject:k] && ![optional containsObject:k]) return NO;
    return YES;
}
static BOOL Enum(id x, NSArray *values) { return [x isKindOfClass:NSString.class] && [values containsObject:x]; }
static BOOL NullableEnum(id x, NSArray *values) { return x == NSNull.null || Enum(x, values); }
static BOOL Text(id x, NSUInteger limit) {
    return [x isKindOfClass:NSString.class] && [x length] > 0 && [x lengthOfBytesUsingEncoding:NSUTF8StringEncoding] <= limit &&
        [x rangeOfCharacterFromSet:NSCharacterSet.controlCharacterSet].location == NSNotFound;
}
static NSArray *Errors(void) {
    return @[@"unknown_fan",@"invalid_inventory",@"unverified_capabilities",@"invalid_rpm",@"invalid_ttl",@"invalid_sequence",@"deadline_overflow",@"owner_conflict",@"invalid_token",@"no_active_lease",@"recovery_pending",@"entropy_unavailable",@"clock_rollback",@"clock_untrusted",@"suspended",@"external_controller",@"actuator_unavailable",@"control_fault",@"plan_busy",@"invalid_plan",@"invalid_sensor_id",@"duplicate_sample",@"invalid_policy_config",@"sensor_fault",@"sequence_exhausted",@"unexpected_response",@"invalid_host_command",@"sensor_source_unavailable",@"no_active_plan",@"invalid_schema",@"readonly_backend",@"backend_unavailable",@"readonly_policy",@"no_reviewed_profile",@"profile_validation_failed",@"backend_open_failed"];
}
static BOOL Entries(id entries) {
    if (![entries isKindOfClass:NSArray.class] || [entries count] == 0 || [entries count] > 16) return NO;
    NSMutableSet *seen = [NSMutableSet new];
    for (id e in entries) {
        if (!Keys(e, @[@"fan",@"strategy"], @[]) || !Unsigned(e[@"fan"], UINT16_MAX) || [seen containsObject:e[@"fan"]]) return NO;
        [seen addObject:e[@"fan"]];
        id s = e[@"strategy"];
        if (![s isKindOfClass:NSDictionary.class]) return NO;
        if ([s[@"type"] isEqual:@"automatic"]) { if (!Keys(s, @[@"type"], @[])) return NO; }
        else if ([s[@"type"] isEqual:@"fixed"]) {
            if (!Keys(s, @[@"type",@"rpm"], @[]) || !Number(s[@"rpm"]) || [s[@"rpm"] doubleValue] <= 0 || [s[@"rpm"] doubleValue] > 100000) return NO;
        } else if ([s[@"type"] isEqual:@"sensor"]) {
            if (!Keys(s, @[@"type",@"sensor_id",@"min_rpm",@"max_rpm",@"lower_temp",@"upper_temp"], @[]) || !Text(s[@"sensor_id"],256)) return NO;
            for (id k in @[@"min_rpm",@"max_rpm",@"lower_temp",@"upper_temp"]) if (!Number(s[k])) return NO;
            if ([s[@"min_rpm"] doubleValue] <= 0 || [s[@"max_rpm"] doubleValue] > 100000 || [s[@"max_rpm"] doubleValue] < [s[@"min_rpm"] doubleValue] || [s[@"lower_temp"] doubleValue] >= [s[@"upper_temp"] doubleValue]) return NO;
        } else return NO;
    }
    return YES;
}
static BOOL Plan(id p) {
    if (!Keys(p,@[@"phase",@"fans",@"watchdog_expirations",@"authorized",@"consent_expires_at_ms",@"lease_ttl_ms",@"authorization_end",@"error",@"stop_reason"],@[]) ||
        !Enum(p[@"phase"],@[@"idle",@"preparing",@"running",@"stopped"]) || !Bool(p[@"authorized"]) ||
        !Unsigned(p[@"watchdog_expirations"],UINT64_MAX) || !Unsigned(p[@"lease_ttl_ms"],5000) ||
        !(p[@"consent_expires_at_ms"] == NSNull.null || Unsigned(p[@"consent_expires_at_ms"],UINT64_MAX)) ||
        !NullableEnum(p[@"authorization_end"],@[@"expired",@"stopped",@"fault",@"disconnected",@"sleep",@"shutdown"]) ||
        !NullableEnum(p[@"error"],Errors()) || !NullableEnum(p[@"stop_reason"],@[@"explicit",@"disconnected",@"sleep",@"shutdown"])) return NO;
    id fans = p[@"fans"];
    if (![fans isKindOfClass:NSArray.class] || [fans count] > 16) return NO;
    NSMutableSet *seen = [NSMutableSet new];
    for (id f in fans) {
        BOOL active = [f isKindOfClass:NSDictionary.class] && [f[@"state"] isEqual:@"active"];
        if (!Keys(f, active ? @[@"fan",@"state",@"target_rpm",@"lease_expires_at_ms"] : @[@"fan",@"state"],@[]) ||
            !Unsigned(f[@"fan"],UINT16_MAX) || [seen containsObject:f[@"fan"]] ||
            !Enum(f[@"state"],@[@"unobserved",@"automatic_confirmed",@"recovery_pending",@"recovery_exhausted",@"yielded",@"unknown",@"active"])) return NO;
        [seen addObject:f[@"fan"]];
        if (active && (!Number(f[@"target_rpm"]) || [f[@"target_rpm"] doubleValue] <= 0 || !Unsigned(f[@"lease_expires_at_ms"],UINT64_MAX))) return NO;
    }
    return YES;
}
static BOOL AdapterError(id e) {
    if (e==NSNull.null) return YES;
    if (![e isKindOfClass:NSDictionary.class]) return NO;
    id kind=e[@"kind"];
    NSArray *fields=nil;
    if (Enum(kind,@[@"unsupported_platform",@"identity_changed",@"timeout"])) fields=@[@"kind"];
    else if (Enum(kind,@[@"unavailable",@"denied"])) fields=@[@"kind",@"reason"];
    else if ([kind isEqual:@"io_kit"]) fields=@[@"kind",@"code"];
    else if ([kind isEqual:@"smc"]) fields=@[@"kind",@"key",@"code"];
    else if (Enum(kind,@[@"missing_key",@"metadata_changed",@"readback_mismatch"])) fields=@[@"kind",@"key"];
    else if ([kind isEqual:@"malformed"]) fields=@[@"kind",@"key",@"reason"];
    else if ([kind isEqual:@"unknown_fan"]) fields=@[@"kind",@"fan"];
    if (!fields || !Keys(e,fields,@[])) return NO;
    return (!e[@"reason"] || Text(e[@"reason"],256)) && (!e[@"key"] || Text(e[@"key"],32)) &&
        (!e[@"code"] || Unsigned(e[@"code"],UINT32_MAX)) && (!e[@"fan"] || Unsigned(e[@"fan"],UINT16_MAX));
}
static BOOL Statuses(id statuses) {
    if (![statuses isKindOfClass:NSArray.class] || [statuses count]>16) return NO;
    for (id status in statuses) {
        if (!Keys(status,@[@"fan",@"phase"],@[]) || !Unsigned(status[@"fan"],UINT16_MAX)) return NO;
        id p=status[@"phase"]; if (![p isKindOfClass:NSDictionary.class]) return NO;
        if ([p[@"state"] isEqual:@"active"]) {
            if (!Keys(p,@[@"state",@"target_rpm",@"expires_at_ms",@"seq"],@[]) || !Number(p[@"target_rpm"]) || !Unsigned(p[@"expires_at_ms"],UINT64_MAX) || !Unsigned(p[@"seq"],UINT64_MAX)) return NO;
        } else if ([p[@"state"] isEqual:@"automatic_confirmed"]) {
            if (!Keys(p,@[@"state",@"cause"],@[]) || !(p[@"cause"]==NSNull.null || Text(p[@"cause"],128))) return NO;
        } else if ([p[@"state"] isEqual:@"recovering"]) {
            if (!Keys(p,@[@"state",@"cause",@"attempts",@"next_retry_at_ms",@"exhausted",@"last_error"],@[]) || !Text(p[@"cause"],128) || !Text(p[@"last_error"],128) || !Unsigned(p[@"attempts"],UINT64_MAX) || !Bool(p[@"exhausted"]) || ([p[@"exhausted"] boolValue] ? p[@"next_retry_at_ms"]!=NSNull.null : !Unsigned(p[@"next_retry_at_ms"],UINT64_MAX))) return NO;
        } else if ([p[@"state"] isEqual:@"yielded"]) {
            if (!Keys(p,@[@"state",@"cause",@"observed_mode",@"observed_target_rpm"],@[]) || !Text(p[@"cause"],128) || !Enum(p[@"observed_mode"],@[@"Automatic",@"Manual",@"Unknown"]) || !(p[@"observed_target_rpm"]==NSNull.null || Number(p[@"observed_target_rpm"]))) return NO;
        } else return NO;
    }
    return YES;
}
// Closed constructor identity: flags alone never establish execution ability.
static BOOL Control(id r) {
    id c=r[@"control"];
    if (!Keys(c,@[@"version",@"execution",@"admission"],@[]) || !Unsigned(c[@"version"],1) || ![c[@"version"] isEqual:@1]) return NO;
    if ([r[@"backend"] isEqual:@"smc_readonly"] && [r[@"dry_run"] boolValue] && ![r[@"hardware_write_enabled"] boolValue])
        return [c[@"execution"] isEqual:@"readonly"] && Enum(c[@"admission"],@[@"readonly_policy",@"no_reviewed_profile",@"profile_validation_failed"]);
    if ([r[@"backend"] isEqual:@"unavailable"] && [r[@"dry_run"] boolValue] && ![r[@"hardware_write_enabled"] boolValue])
        return [c[@"execution"] isEqual:@"readonly"] && Enum(c[@"admission"],@[@"backend_open_failed",@"backend_unavailable"]);
    if ([r[@"backend"] isEqual:@"smc_control"] && ![r[@"dry_run"] boolValue] && [r[@"hardware_write_enabled"] boolValue])
        return [c[@"execution"] isEqual:@"profile_gated"] && [c[@"admission"] isEqual:@"admitted"];
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
    if (Bridge.allow_fake && [r[@"backend"] isEqual:@"fake"] && [r[@"dry_run"] boolValue] && ![r[@"hardware_write_enabled"] boolValue])
        return [c[@"execution"] isEqual:@"test_fake"] && [c[@"admission"] isEqual:@"test_only"];
#endif
    return NO;
}
static NSDictionary *Fingerprint(NSDictionary *r) {
    return @{@"backend":r[@"backend"],@"dry_run":r[@"dry_run"],@"hardware_write_enabled":r[@"hardware_write_enabled"],@"control":r[@"control"]};
}
static BOOL Writable(NSDictionary *fingerprint) {
    return [fingerprint[@"backend"] isEqual:@"smc_control"] && [fingerprint[@"control"][@"execution"] isEqual:@"profile_gated"] && [fingerprint[@"control"][@"admission"] isEqual:@"admitted"];
}
static NSDictionary *Reply(id data, NSString *op) {
    if (![data isKindOfClass:NSData.class] || [data length] > 1024 * 1024) return nil;
    id r = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
    if (!Keys(r,@[@"version",@"dry_run",@"hardware_write_enabled",@"ok",@"backend",@"control"],
        @[@"result",@"error",@"plan",@"helper_process_id",@"watchdog_ticks",@"watchdog_expirations",@"lifecycle",@"hardware_reads",@"hardware_write_attempts",@"hardware_write_attempts_scope",@"hardware_inspection_observed_at_ms",@"backend_open_error",@"statuses"]) ||
        !Unsigned(r[@"version"],1) || ![r[@"version"] isEqual:@1] || !Bool(r[@"dry_run"]) || !Bool(r[@"hardware_write_enabled"]) || !Bool(r[@"ok"]) ||
        !Enum(r[@"backend"],@[@"smc_readonly",@"smc_control",@"unavailable",@"fake"]) || !Control(r)) return nil;
    for (id k in @[@"helper_process_id",@"watchdog_ticks",@"watchdog_expirations"]) if (r[k] && !Unsigned(r[k],UINT64_MAX)) return nil;
    for (id k in @[@"hardware_write_attempts",@"hardware_inspection_observed_at_ms"]) if (r[k] && r[k]!=NSNull.null && !Unsigned(r[k],UINT64_MAX)) return nil;
    if ((r[@"hardware_reads"] && (!Bool(r[@"hardware_reads"]) || [r[@"hardware_reads"] boolValue] != (BOOL)([r[@"backend"] isEqual:@"smc_readonly"] || [r[@"backend"] isEqual:@"smc_control"]))) ||
        (r[@"lifecycle"] && !Enum(r[@"lifecycle"],@[@"running",@"sleep_locked",@"wake_requires_new_session",@"shutting_down"])) ||
        (r[@"hardware_write_attempts_scope"] && ![r[@"hardware_write_attempts_scope"] isEqual:@"latest_hardware_inspection"]) ||
        (r[@"backend_open_error"] && !AdapterError(r[@"backend_open_error"])) ||
        (r[@"statuses"] && !Statuses(r[@"statuses"]))) return nil;
    if ([r[@"ok"] boolValue]) {
        id result = r[@"result"];
        BOOL start = [op isEqual:@"start_plan"];
        if (r[@"error"] || r[@"plan"] || !Keys(result,start ? @[@"kind",@"plan",@"token"] : @[@"kind",@"plan"],@[]) || ![result[@"kind"] isEqual:@"plan"] || !Plan(result[@"plan"])) return nil;
        if (start) {
            id t = result[@"token"];
            if (![t isKindOfClass:NSString.class] || [t length] != 64 || [t rangeOfCharacterFromSet:[[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"] invertedSet]].location != NSNotFound || [t isEqual:[@"" stringByPaddingToLength:64 withString:@"0" startingAtIndex:0]]) return nil;
        }
    } else if (r[@"result"] || !Keys(r[@"error"],@[@"code"],@[]) || !Enum(r[@"error"][@"code"], Errors()) || !Plan(r[@"plan"])) return nil;
    return r;
}
static BOOL Matches(NSDictionary *plan, NSArray *entries) {
    if ([plan[@"fans"] count]!=entries.count) return NO;
    BOOL preparing=[plan[@"phase"] isEqual:@"preparing"], hasSensor=NO;
    if (preparing && (![plan[@"authorized"] boolValue] || plan[@"consent_expires_at_ms"]==NSNull.null ||
        [plan[@"lease_ttl_ms"] unsignedLongLongValue]==0 || plan[@"authorization_end"]!=NSNull.null ||
        plan[@"error"]!=NSNull.null || plan[@"stop_reason"]!=NSNull.null)) return NO;
    for (NSDictionary *entry in entries) {
        NSDictionary *found=nil, *strategy=entry[@"strategy"];
        hasSensor |= [strategy[@"type"] isEqual:@"sensor"];
        for (NSDictionary *fan in plan[@"fans"]) if ([fan[@"fan"] isEqual:entry[@"fan"]]) found=fan;
        if (!found) return NO;
        // Preparing proves consent only: no fan may already claim a write.
        if (preparing && !Enum(found[@"state"],@[@"unobserved",@"automatic_confirmed"])) return NO;
        if ([plan[@"phase"] isEqual:@"running"] && [plan[@"authorized"] boolValue] &&
            ![found[@"state"] isEqual:[strategy[@"type"] isEqual:@"automatic"] ? @"automatic_confirmed" : @"active"]) return NO;
        if ([found[@"state"] isEqual:@"active"]) {
            if ([strategy[@"type"] isEqual:@"fixed"]) {
                if (![found[@"target_rpm"] isEqual:strategy[@"rpm"]]) return NO;
            } else if ([strategy[@"type"] isEqual:@"sensor"]) {
                double rpm=[found[@"target_rpm"] doubleValue];
                if (rpm<[strategy[@"min_rpm"] doubleValue] || rpm>[strategy[@"max_rpm"] doubleValue]) return NO;
            } else return NO;
        }
    }
    return !preparing || hasSensor;
}
static NSString *Identity(void) {
#if defined(MF_CONTROL_CLIENT_TEST_HARNESS) && defined(MF_CONTROL_CLIENT_TEST_OS_MAJOR)
    if (MF_CONTROL_CLIENT_TEST_OS_MAJOR < 13) return @"macos_13_required";
#else
    if (NSProcessInfo.processInfo.operatingSystemVersion.majorVersion < 13) return @"macos_13_required";
#endif
    NSDictionary *r = MFProductionConfiguredRequirements();
    if (!r || !MFProductionConfiguredTeamIdentifier().length) return @"production_team_unconfigured";
    SecRequirementRef req = NULL; SecCodeRef code = NULL; BOOL valid = NO;
    if (SecRequirementCreateWithString((__bridge CFStringRef)r[@"client"],0,&req) == errSecSuccess) {
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
        if (Bridge.self_matches) valid = Bridge.self_matches(r[@"client"]); else
#endif
        valid = SecCodeCopySelf(0,&code) == errSecSuccess && SecCodeCheckValidity(code,kSecCSStrictValidate,req) == errSecSuccess;
    }
    if (code) CFRelease(code); if (req) CFRelease(req);
    if (!valid) return @"production_client_signature_requirement_failed";
    req = NULL;
    valid = SecRequirementCreateWithString((__bridge CFStringRef)r[@"helper"],0,&req) == errSecSuccess;
    if (req) CFRelease(req);
    return valid ? nil : @"production_helper_requirement_invalid";
}
@interface MFControlCall : NSObject
@property(nonatomic,strong) dispatch_semaphore_t done;
@property(nonatomic,strong) NSDictionary *value;
@property(nonatomic,copy) NSString *operation;
@property(nonatomic) uint64_t deadline, intent;
@end
@implementation MFControlCall
- (instancetype)init { if ((self=[super init])) _done=dispatch_semaphore_create(0); return self; }
@end
@interface MFControlSession : NSObject
@property(nonatomic,strong) dispatch_queue_t queue;
@property(nonatomic,strong) dispatch_source_t timer;
@property(nonatomic,strong) NSXPCConnection *connection;
@property(nonatomic,strong) MFControlCall *call;
@property(nonatomic,copy) NSString *token;
@property(nonatomic,copy) NSString *state;
@property(nonatomic,copy) NSString *reason;
@property(nonatomic,strong) NSDictionary *plan;
@property(nonatomic,strong) NSDictionary *fingerprint;
@property(nonatomic,strong) NSArray *entries;
@property(nonatomic) uint64_t generation, requestID, sequence, deadline, completed, renewAt, highestIntent;
@property(nonatomic) BOOL inflight, authenticated, stoppedConfirmed;
- (NSDictionary *)snapshot:(NSString *)operation;
- (void)close:(NSString *)reason;
- (void)send:(NSDictionary *)command deadline:(uint64_t)deadline then:(void (^)(NSDictionary *))then;
@end
@implementation MFControlSession
- (instancetype)init {
    if ((self=[super init])) {
        _queue=dispatch_queue_create("local.macsfan.control-client",DISPATCH_QUEUE_SERIAL);
        _state=@"idle"; _reason=@"no_plan_started";
        _timer=dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER,0,0,_queue);
        dispatch_source_set_timer(_timer,dispatch_time(DISPATCH_TIME_NOW,50*NSEC_PER_MSEC),50*NSEC_PER_MSEC,5*NSEC_PER_MSEC);
        __weak MFControlSession *weak=self;
        dispatch_source_set_event_handler(_timer, ^{ @autoreleasepool { @try {
            MFControlSession *s=weak; if (!s) return;
            uint64_t now=Now();
            if (s.inflight && now>=s.deadline) { [s close:@"helper_client_timeout"]; return; }
            if (s.token && !s.inflight && now>=s.renewAt) {
                if (s.sequence==UINT64_MAX) { [s close:@"sequence_exhausted"]; return; }
                s.sequence++;
                [s send:@{@"op":@"renew_plan",@"token":s.token,@"seq":@(s.sequence),@"ttl_ms":@5000} deadline:now+Budget() then:^(NSDictionary *r) {
                    [s accept:r];
                    if (![r[@"ok"] boolValue] || ![s.plan[@"authorized"] boolValue] || (![s.state isEqual:@"active"] && ![s.state isEqual:@"automatic_confirmed"] && ![s.state isEqual:@"preparing"])) [s close:@"authorization_ended"];
                }];
            }
        } @catch (NSException *e) { (void)e; [weak close:@"helper_client_exception"]; } } });
        dispatch_resume(_timer);
    }
    return self;
}
- (NSDictionary *)snapshot:(NSString *)op {
    uint64_t now=Now();
    BOOL fresh=self.completed && now>=self.completed && now-self.completed<=1500*NSEC_PER_MSEC;
    BOOL current=self.authenticated && fresh && (self.connection || self.stoppedConfirmed);
    BOOL live=current && self.connection && [self.plan[@"authorized"] boolValue] && self.token;
    NSString *gate=Identity();
    NSString *state=self.state, *reason=self.reason;
    if (gate && !self.connection && !self.completed) { state=@"blocked"; reason=gate; }
    if (([state isEqual:@"active"] || [state isEqual:@"preparing"]) && !live) { state=@"unknown"; reason=@"response_stale"; }
    return @{@"schema_version":@1,@"operation":op,@"state":state,@"reason":reason,
        @"hardware_write_enabled":@((BOOL)(current && self.connection && Writable(self.fingerprint))),@"authenticated_response":@(self.authenticated),@"current":@(current),
        @"authorization_live":@(live),@"request_in_flight":@(self.inflight),
        @"can_attempt_start":@((BOOL)(!gate && !self.connection && !self.inflight)),@"can_stop":@((BOOL)(self.connection!=nil || self.inflight)),
        @"last_response_age_ms":self.completed && now>=self.completed ? @((now-self.completed)/NSEC_PER_MSEC) : NSNull.null,
        @"plan":self.plan ?: (id)NSNull.null};
}
- (void)finishCall {
    MFControlCall *call=self.call; self.call=nil;
    if (call) { call.value=[self snapshot:call.operation]; dispatch_semaphore_signal(call.done); }
}
- (void)disconnect {
    self.generation++; self.inflight=NO; self.token=nil; self.fingerprint=nil;
    NSXPCConnection *c=self.connection; self.connection=nil;
    @try { [c invalidate]; } @catch (NSException *e) { (void)e; }
}
- (void)close:(NSString *)reason {
    BOOL mayHaveControlled=self.token!=nil || self.inflight;
    [self disconnect]; self.stoppedConfirmed=NO;
    self.state=mayHaveControlled || self.plan ? @"recovery_pending" : @"unknown"; self.reason=reason;
    [self finishCall];
}
- (void)accept:(NSDictionary *)r {
    self.authenticated=YES; self.completed=Now(); self.renewAt=self.completed+NSEC_PER_SEC;
    self.plan=r[@"result"] ? r[@"result"][@"plan"] : r[@"plan"];
    BOOL allAutomatic=[self.plan[@"fans"] count]>0, allConfirmed=allAutomatic, anyActive=NO;
    for (NSDictionary *f in self.plan[@"fans"]) {
        allAutomatic &= [f[@"state"] isEqual:@"automatic_confirmed"];
        anyActive |= [f[@"state"] isEqual:@"active"];
        allConfirmed &= [f[@"state"] isEqual:@"automatic_confirmed"] || [f[@"state"] isEqual:@"active"];
    }
    if ([self.plan[@"phase"] isEqual:@"preparing"] && [self.plan[@"authorized"] boolValue]) self.state=@"preparing";
    else if (allAutomatic) self.state=@"automatic_confirmed";
    else if (allConfirmed && anyActive && [self.plan[@"authorized"] boolValue] && [self.plan[@"phase"] isEqual:@"running"]) self.state=@"active";
    else if ([self.plan[@"phase"] isEqual:@"idle"]) self.state=@"idle";
    else self.state=@"recovery_pending";
    self.reason=[r[@"ok"] boolValue] ? @"authenticated_plan_response" : r[@"error"][@"code"];
}
- (void)send:(NSDictionary *)command deadline:(uint64_t)deadline then:(void (^)(NSDictionary *))then {
    if (self.inflight || !self.connection || Now()>=deadline) { [self close:@"helper_client_timeout"]; return; }
    self.inflight=YES; self.deadline=deadline;
    uint64_t generation=self.generation, request=++self.requestID;
    __weak MFControlSession *weak=self;
    id<MFControlHelper> proxy=[self.connection remoteObjectProxyWithErrorHandler:^(NSError *e) {
        (void)e; dispatch_async(weak.queue, ^{ @try { MFControlSession *s=weak; if (s.generation==generation && s.requestID==request && s.inflight) [s close:@"helper_xpc_error"]; } @catch(NSException *e) { (void)e; } });
    }];
    NSData *data=[NSJSONSerialization dataWithJSONObject:@{@"version":@1,@"command":command} options:0 error:NULL];
    [proxy request:data reply:^(NSData *data) {
        dispatch_async(weak.queue, ^{ @autoreleasepool { @try {
            MFControlSession *s=weak;
            if (!s || s.generation!=generation || s.requestID!=request || !s.inflight) return;
            if (Now()>=s.deadline) { [s close:@"helper_client_timeout"]; return; }
            NSDictionary *r=Reply(data,command[@"op"]);
            // Deadline is checked again after parsing, at the state claim.
            if (Now()>=s.deadline) { [s close:@"helper_client_timeout"]; return; }
            if (!r || (![command[@"op"] isEqual:@"plan_status"] && [r[@"ok"] boolValue] && !Matches(r[@"result"][@"plan"],s.entries))) { [s close:@"helper_reply_invalid"]; return; }
            NSDictionary *fingerprint=Fingerprint(r);
            if (s.fingerprint && ![s.fingerprint isEqual:fingerprint]) { [s close:@"helper_capability_changed"]; return; }
            if (!s.fingerprint) {
                if (![command[@"op"] isEqual:@"plan_status"]) { [s close:@"helper_reply_invalid"]; return; }
                s.fingerprint=fingerprint;
            }
            s.inflight=NO; then(r);
        } @catch(NSException *e) { (void)e; [weak close:@"helper_client_exception"]; } } });
    }];
}
- (BOOL)claim:(MFControlCall *)call {
    if (!call.intent || (call.intent==UINT64_MAX && [call.operation isEqual:@"start_plan"]) || call.intent<=self.highestIntent) {
        NSMutableDictionary *v=[[self snapshot:call.operation] mutableCopy]; v[@"reason"]=@"stale_intent"; call.value=v; dispatch_semaphore_signal(call.done); return NO;
    }
    self.highestIntent=call.intent; return YES;
}
- (void)start:(NSArray *)entries call:(MFControlCall *)call {
    if (![self claim:call]) return;
    if (self.connection || self.inflight) { call.value=[self snapshot:call.operation]; dispatch_semaphore_signal(call.done); return; }
    self.call=call; self.entries=entries; self.stoppedConfirmed=NO; self.authenticated=NO; self.completed=0; self.plan=nil;
    NSString *gate=Identity();
    if (gate) { self.state=@"blocked"; self.reason=gate; [self finishCall]; return; }
    uint64_t now=Now(); if (now>UINT64_MAX-Budget()) { [self close:@"clock_unavailable"]; return; }
    uint64_t deadline=call.deadline; self.generation++; uint64_t generation=self.generation;
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
    self.connection=Bridge.connection ? Bridge.connection(MFProductionMachServiceName(),NSXPCConnectionPrivileged) : nil;
#else
    self.connection=[[NSXPCConnection alloc] initWithMachServiceName:MFProductionMachServiceName() options:NSXPCConnectionPrivileged];
#endif
    if (!self.connection) { [self close:@"helper_connection_unavailable"]; return; }
    SEL selector=NSSelectorFromString(@"setCodeSigningRequirement:");
    if (![self.connection respondsToSelector:selector]) { [self close:@"helper_signing_requirement_api_unavailable"]; return; }
    ((void(*)(id,SEL,NSString *))objc_msgSend)(self.connection,selector,MFProductionConfiguredRequirements()[@"helper"]);
    self.connection.remoteObjectInterface=[NSXPCInterface interfaceWithProtocol:@protocol(MFControlHelper)];
    __weak MFControlSession *weak=self;
    void (^lost)(void)=^{ dispatch_async(weak.queue, ^{ @try { if (weak.generation==generation) [weak close:@"helper_connection_lost"]; } @catch(NSException *e) { (void)e; } }); };
    self.connection.interruptionHandler=lost; self.connection.invalidationHandler=lost;
    [self.connection resume];
    [self send:@{@"op":@"plan_status"} deadline:deadline then:^(NSDictionary *r) {
        [self accept:r];
        BOOL allow=Writable(self.fingerprint);
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
        allow=allow || (Bridge.allow_fake && [r[@"backend"] isEqual:@"fake"]);
#endif
        if (!allow || ![r[@"ok"] boolValue]) {
            NSString *reason=allow ? r[@"error"][@"code"] : r[@"control"][@"admission"];
            [self disconnect]; self.state=@"blocked"; self.reason=reason; [self finishCall]; return;
        }
        self.sequence=1;
        [self send:@{@"op":@"start_plan",@"entries":entries,@"seq":@1,@"ttl_ms":@5000} deadline:deadline then:^(NSDictionary *r) {
            [self accept:r];
            if ([r[@"ok"] boolValue] && [self.plan[@"authorized"] boolValue] && ([self.state isEqual:@"active"] || [self.state isEqual:@"automatic_confirmed"] || [self.state isEqual:@"preparing"])) {
                self.token=r[@"result"][@"token"]; [self finishCall];
            } else { NSString *reason=self.reason; [self close:reason]; }
        }];
    }];
}
- (void)stop:(MFControlCall *)call {
    if (![self claim:call]) return;
    if (self.inflight) {
        [self close:@"stop_cancelled_inflight_request"];
        call.value=[self snapshot:call.operation]; dispatch_semaphore_signal(call.done); return;
    }
    self.call=call;
    if (!self.connection || !self.token) { [self disconnect]; [self finishCall]; return; }
    if (self.sequence==UINT64_MAX) { [self close:@"sequence_exhausted"]; return; }
    self.sequence++;
    [self send:@{@"op":@"stop_plan",@"token":self.token,@"seq":@(self.sequence)} deadline:call.deadline then:^(NSDictionary *r) {
        [self accept:r];
        BOOL confirmed=[r[@"ok"] boolValue] && ![self.plan[@"authorized"] boolValue] && [self.state isEqual:@"automatic_confirmed"];
        [self disconnect]; self.stoppedConfirmed=confirmed;
        if (!confirmed) { self.state=@"recovery_pending"; self.reason=@"automatic_recovery_unconfirmed"; }
        [self finishCall];
    }];
}
@end
static MFControlSession *Session(void) { static MFControlSession *s; static dispatch_once_t once; dispatch_once(&once, ^{ s=[MFControlSession new]; }); return s; }
static char *Encode(NSDictionary *value) {
    NSData *d=[NSJSONSerialization dataWithJSONObject:value options:NSJSONWritingSortedKeys error:NULL];
    if (!d) return NULL; char *p=malloc(d.length+1); if (!p) return NULL;
    memcpy(p,d.bytes,d.length); p[d.length]=0; return p;
}
static char *Exception(void) { return strdup("{\"schema_version\":1,\"operation\":\"control_status\",\"state\":\"unknown\",\"reason\":\"helper_client_exception\",\"hardware_write_enabled\":false,\"authenticated_response\":false,\"current\":false,\"authorization_live\":false,\"request_in_flight\":false,\"can_attempt_start\":false,\"can_stop\":true,\"last_response_age_ms\":null,\"plan\":null}"); }
static char *Invoke(NSString *op, const uint8_t *bytes, size_t length, uint64_t intent) {
    uint64_t entered=Now();
    @try { @autoreleasepool {
        MFControlSession *s=Session();
        if ([op isEqual:@"control_status"]) {
            __block NSDictionary *v; dispatch_sync(s.queue, ^{ @try { v=[s snapshot:op]; } @catch(NSException *e) { (void)e; [s close:@"helper_client_exception"]; } });
            return v ? Encode(v) : Exception();
        }
        NSArray *entries=nil;
        if ([op isEqual:@"start_plan"]) {
            if (bytes && length>0 && length<=16384) entries=[NSJSONSerialization JSONObjectWithData:[NSData dataWithBytes:bytes length:length] options:0 error:NULL];
            if (!Entries(entries)) {
                __block NSMutableDictionary *v; dispatch_sync(s.queue, ^{ @try { v=[[s snapshot:op] mutableCopy]; v[@"reason"]=@"invalid_plan_input"; } @catch(NSException *e) { (void)e; } });
                return v ? Encode(v) : Exception();
            }
        }
        MFControlCall *call=[MFControlCall new]; call.operation=op; call.intent=intent; call.deadline=entered>UINT64_MAX-Budget() ? 0 : entered+Budget();
        dispatch_async(s.queue, ^{ @autoreleasepool { @try {
            if ([op isEqual:@"start_plan"]) [s start:entries call:call]; else [s stop:call];
        } @catch(NSException *e) { (void)e; [s close:@"helper_client_exception"]; if (!call.value) { call.value=[s snapshot:op]; dispatch_semaphore_signal(call.done); } } } });
        dispatch_semaphore_wait(call.done,DISPATCH_TIME_FOREVER);
        return Encode(call.value);
    } } @catch(NSException *e) { (void)e; return Exception(); }
}
char *mf_control_start(const uint8_t *bytes,size_t length,uint64_t intent) { return Invoke(@"start_plan",bytes,length,intent); }
char *mf_control_status(void) { return Invoke(@"control_status",NULL,0,0); }
char *mf_control_stop(uint64_t intent) { return Invoke(@"stop_plan",NULL,0,intent); }
void mf_control_free(char *json) { free(json); }
#ifdef MF_CONTROL_CLIENT_TEST_HARNESS
void MFControlClientTestSetSequence(uint64_t sequence) { MFControlSession *s=Session(); dispatch_sync(s.queue, ^{ s.sequence=sequence; }); }
#endif

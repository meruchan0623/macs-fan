#!/usr/bin/env python3
"""Nonroot isolated ControlClient ABI/state-machine regression; no hardware or launchd."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
NATIVE = ROOT.parent / 'src-tauri/native'
EVIDENCE = ROOT.parents[2] / 'work/control-native-0.16'


def run(command):
    print('$ ' + shlex.join(map(str, command)), flush=True)
    result = subprocess.run(list(map(str, command)), stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, timeout=60)
    print(result.stdout, end='', flush=True)
    with (EVIDENCE/'control.log').open('a') as log:
        log.write('$ ' + shlex.join(map(str, command)) + '\n' + result.stdout)
    result.check_returncode()
    return result.stdout


HARNESS = r'''
#import <Foundation/Foundation.h>
#import "ControlClient.h"
#include <unistd.h>
static NSString *mode=@"valid";
static NSMutableArray *checks;
static NSMutableDictionary *records;
static unsigned requests, starts, renews, stops, invalidations, factories;
static BOOL missingTeam, denySelf, requirementSet, resumed;
static uint64_t previousSequence;
static NSString *secret=@"1111111111111111111111111111111111111111111111111111111111111111";
static void (^late)(NSData *);
static void (^interrupt)(void);
static uint64_t intent;
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
extern void *mf_helper_create(uint8_t fake);
extern void mf_helper_test_destroy(void *);
extern char *mf_helper_tick(void *);
extern char *mf_helper_request(void *, const uint8_t *, const uint8_t *, size_t);
extern char *mf_helper_disconnect(void *, const uint8_t *);
extern void mf_helper_free(char *);
static void *host;
static NSLock *hostLock;
static uint8_t owner[32]={1};
#endif
static void check(NSString *name, BOOL okay) {
    if (!okay) { fprintf(stderr,"FAIL %s\n",name.UTF8String); abort(); }
    [checks addObject:name];
}
NSString *MFProductionConfiguredTeamIdentifier(void) { return missingTeam ? @"" : @"ABCDE12345"; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSDictionary *MFProductionConfiguredRequirements(void) {
    return missingTeam ? nil : @{@"client":@"identifier \"local.macsfan.desktop\"",@"helper":@"identifier \"local.macsfan.helper.control\""};
}
static BOOL selfMatches(NSString *r) { return !denySelf && [r isEqual:MFProductionConfiguredRequirements()[@"client"]]; }
static NSData *encode(id x) { return [NSJSONSerialization dataWithJSONObject:x options:0 error:NULL]; }
static NSDictionary *decode(char *p) {
    if (!p) abort();
    NSString *text=[NSString stringWithUTF8String:p];
    check(@"secret_never_public",![text containsString:secret] && ![text containsString:@"\"token\""]);
    id v=[NSJSONSerialization JSONObjectWithData:[text dataUsingEncoding:NSUTF8StringEncoding] options:0 error:NULL];
    mf_control_free(p); return v;
}
static NSDictionary *startID(uint64_t id) {
    const char *e="[{\"fan\":0,\"strategy\":{\"type\":\"fixed\",\"rpm\":3000}}]";
#ifdef MF_CONTROL_CLIENT_REAL_SENSOR
    e="[{\"fan\":0,\"strategy\":{\"type\":\"sensor\",\"sensor_id\":\"test:temperature\",\"min_rpm\":1000,\"max_rpm\":5000,\"lower_temp\":40,\"upper_temp\":80}}]";
#endif
    if ([mode hasPrefix:@"preparing"]) e="[{\"fan\":0,\"strategy\":{\"type\":\"sensor\",\"sensor_id\":\"test:temperature\",\"min_rpm\":2000,\"max_rpm\":5000,\"lower_temp\":40,\"upper_temp\":80}}]";
    return decode(mf_control_start((const uint8_t *)e,strlen(e),id));
}
static NSDictionary *start(void) { return startID(++intent); }
static NSDictionary *stop(void) { return decode(mf_control_stop(++intent)); }
static NSDictionary *status(void) { return decode(mf_control_status()); }
static NSMutableDictionary *reply(NSString *op) {
    BOOL idle=[op isEqual:@"plan_status"], stopped=[op isEqual:@"stop_plan"];
    id fan=stopped ? @{@"fan":@0,@"state":@"automatic_confirmed"} : @{@"fan":@0,@"state":@"active",@"target_rpm":@3000,@"lease_expires_at_ms":@10000};
    NSMutableDictionary *p=[@{@"phase":idle?@"idle":stopped?@"stopped":@"running",@"fans":idle?@[]:@[fan],@"watchdog_expirations":@0,@"authorized":@((BOOL)(!idle&&!stopped)),@"consent_expires_at_ms":idle||stopped?(id)NSNull.null:@5000,@"lease_ttl_ms":@1000,@"authorization_end":stopped?@"stopped":NSNull.null,@"error":NSNull.null,@"stop_reason":stopped?@"explicit":NSNull.null} mutableCopy];
    NSMutableDictionary *result=[@{@"kind":@"plan",@"plan":p} mutableCopy];
    if ([op isEqual:@"start_plan"]) result[@"token"]=secret;
        BOOL readonly=[mode isEqual:@"readonly"] || [mode hasPrefix:@"denied_"], control=[mode hasPrefix:@"control"];
    NSString *admission=[mode hasPrefix:@"denied_"] ? [mode substringFromIndex:7] : @"readonly_policy";
    return [@{@"version":@1,@"dry_run":@((BOOL)!control),@"hardware_write_enabled":@(control),@"ok":@YES,@"backend":readonly?([admission hasPrefix:@"backend_"]?@"unavailable":@"smc_readonly"):control?@"smc_control":@"fake",
        @"control":@{@"version":@1,@"execution":readonly?@"readonly":control?@"profile_gated":@"test_fake",@"admission":readonly?admission:control?@"admitted":@"test_only"},@"result":result} mutableCopy];
}
@interface Mock : NSObject
@property(nonatomic,copy) void (^interruptionHandler)(void);
@property(nonatomic,copy) void (^invalidationHandler)(void);
@property(nonatomic,strong) NSXPCInterface *remoteObjectInterface;
- (void)setCodeSigningRequirement:(NSString *)r;
- (void)resume;
- (void)invalidate;
- (id)remoteObjectProxyWithErrorHandler:(void (^)(NSError *))handler;
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end
@implementation Mock
- (void)setCodeSigningRequirement:(NSString *)r { requirementSet=[r isEqual:MFProductionConfiguredRequirements()[@"helper"]]; }
- (void)resume { check(@"requirement_before_resume",requirementSet); resumed=YES; }
- (void)invalidate {
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
    [hostLock lock];
    if (host) { mf_helper_free(mf_helper_disconnect(host,owner)); mf_helper_test_destroy(host); host=NULL; }
    [hostLock unlock];
#endif
    invalidations++; if (self.invalidationHandler) self.invalidationHandler(); if ([mode isEqual:@"throw_invalidate"]) [NSException raise:@"test" format:@"test"]; }
- (id)remoteObjectProxyWithErrorHandler:(void (^)(NSError *))handler { (void)handler; return self; }
- (void)request:(NSData *)payload reply:(void (^)(NSData *))callback {
    check(@"resumed_before_request",resumed); requests++;
    NSDictionary *r=[NSJSONSerialization JSONObjectWithData:payload options:0 error:NULL];
    NSDictionary *c=r[@"command"]; NSString *op=c[@"op"];
    if ([op isEqual:@"start_plan"]) { starts++; previousSequence=1; check(@"exact_start_fields",c.count==4 && [c[@"ttl_ms"] isEqual:@5000] && [c[@"seq"] isEqual:@1]); }
    if ([op isEqual:@"renew_plan"] || [op isEqual:@"stop_plan"]) {
        #ifndef MF_CONTROL_CLIENT_REAL_HELPER
        check(@"same_connection_secret",[c[@"token"] isEqual:secret]);
#endif
        check(@"strict_integer_sequence",[c[@"seq"] unsignedLongLongValue]>previousSequence);
        previousSequence=[c[@"seq"] unsignedLongLongValue];
        if ([op isEqual:@"renew_plan"]) renews++; else stops++;
    }
    interrupt=self.interruptionHandler;
    if ([mode isEqual:@"throw_request"]) [NSException raise:@"test" format:@"test"];
    if (([mode isEqual:@"hold_start"] && [op isEqual:@"start_plan"]) || ([mode isEqual:@"hold_renew"] && [op isEqual:@"renew_plan"])) { late=[callback copy]; return; }
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
    [hostLock lock];
    char *raw=mf_helper_request(host,owner,payload.bytes,payload.length);
    NSData *real=[NSData dataWithBytes:raw length:strlen(raw)]; mf_helper_free(raw); [hostLock unlock]; callback(real); return;
#endif
    NSMutableDictionary *out=reply(op);
    if ([mode hasPrefix:@"preparing"] && ![op isEqual:@"plan_status"] && ![op isEqual:@"stop_plan"]) {
        NSMutableDictionary *plan=out[@"result"][@"plan"];
        if (![mode isEqual:@"preparing_transition"] || [op isEqual:@"start_plan"]) {
            plan[@"phase"]=@"preparing";
            plan[@"fans"]=@[@{@"fan":@0,@"state":[mode isEqual:@"preparing_automatic"]?@"automatic_confirmed":@"unobserved"}];
        }
        if ([mode isEqual:@"preparing_active"]) plan[@"fans"]=@[@{@"fan":@0,@"state":@"active",@"target_rpm":@3000,@"lease_expires_at_ms":@10000}];
        if ([mode isEqual:@"preparing_wrong_fan"]) plan[@"fans"]=@[@{@"fan":@1,@"state":@"unobserved"}];
        if ([mode isEqual:@"preparing_missing_fan"]) plan[@"fans"]=@[];
        if ([mode isEqual:@"preparing_no_consent"]) plan[@"consent_expires_at_ms"]=NSNull.null;
        if ([mode isEqual:@"preparing_ended"]) plan[@"authorization_end"]=@"expired";
        if ([mode isEqual:@"preparing_false_running"] && [op isEqual:@"renew_plan"]) {
            plan[@"phase"]=@"running"; plan[@"fans"]=@[@{@"fan":@0,@"state":@"automatic_confirmed"}];
        }
        if ([mode isEqual:@"preparing_unauthorized"]) plan[@"authorized"]=@NO;
        if ([mode isEqual:@"preparing_extra_fan"]) plan[@"fans"]=@[@{@"fan":@0,@"state":@"unobserved"},@{@"fan":@1,@"state":@"unobserved"}];
        if ([mode isEqual:@"preparing_out_of_range"] && [op isEqual:@"renew_plan"]) {
            plan[@"phase"]=@"running"; plan[@"fans"]=@[@{@"fan":@0,@"state":@"active",@"target_rpm":@6000,@"lease_expires_at_ms":@10000}];
        }
        if ([mode isEqual:@"preparing_hold_renew"] && [op isEqual:@"renew_plan"]) { late=[callback copy]; return; }
    }
    if (([mode isEqual:@"control_change_start"] && [op isEqual:@"start_plan"]) ||
        ([mode isEqual:@"control_change_renew"] && [op isEqual:@"renew_plan"]) ||
        ([mode isEqual:@"control_change_stop"] && [op isEqual:@"stop_plan"])) {
        out[@"backend"]=@"smc_readonly"; out[@"dry_run"]=@YES; out[@"hardware_write_enabled"]=@NO;
        out[@"control"]=@{@"version":@1,@"execution":@"readonly",@"admission":@"no_reviewed_profile"};
    }
    if ([mode isEqual:@"control_hold_renew"] && [op isEqual:@"renew_plan"]) { late=[callback copy]; return; }
    if ([mode isEqual:@"invalid"] && [op isEqual:@"start_plan"]) out[@"extra"]=@1;
    if ([mode isEqual:@"wrong_kind"] && [op isEqual:@"start_plan"]) out[@"result"][@"kind"]=@"statuses";
    if ([mode isEqual:@"fake_authority"] && [op isEqual:@"plan_status"]) out[@"hardware_write_enabled"]=@YES;
    if ([mode isEqual:@"stop_unconfirmed"] && [op isEqual:@"stop_plan"]) out[@"result"][@"plan"][@"fans"]=@[@{@"fan":@0,@"state":@"recovery_pending"}];
    if (([mode isEqual:@"stop_exhausted"] || [mode isEqual:@"stop_inconsistent"]) && [op isEqual:@"stop_plan"]) {
        out[@"result"][@"plan"][@"fans"]=@[@{@"fan":@0,@"state":@"recovery_exhausted"}];
        out[@"statuses"]=@[@{@"fan":@0,@"phase":@{@"state":@"recovering",@"cause":@"Disconnected",@"attempts":@3,@"next_retry_at_ms":NSNull.null,@"exhausted":@((BOOL)[mode isEqual:@"stop_exhausted"]),@"last_error":@"Write(Io)"}}];
    }
    if ([mode isEqual:@"sync_late"] && [op isEqual:@"start_plan"]) usleep(250000);
    callback(encode(out));
}
@end
static id connection(NSString *name, NSXPCConnectionOptions options) {
    check(@"fixed_service",[name isEqual:@"local.macsfan.helper.control"] && options==NSXPCConnectionPrivileged);
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
    [hostLock lock]; host=mf_helper_create(1); [hostLock unlock];
#ifdef MF_CONTROL_CLIENT_REAL_SENSOR
    // Wait for the first real worker publication; never inject samples/time.
    usleep(120000);
#endif
#endif
    factories++; requirementSet=NO; resumed=NO; return [Mock new];
}
int main(void) { @autoreleasepool {
    checks=[NSMutableArray new]; records=[NSMutableDictionary new];
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
    hostLock=[NSLock new];
#endif
    MFControlClientTestInstallBridge((MFControlClientTestBridge){selfMatches,connection,YES});
#ifdef MF_CONTROL_CLIENT_REAL_HELPER
    NSDictionary *v=start();
#ifdef MF_CONTROL_CLIENT_REAL_SENSOR
    check(@"real_sensor_start_preparing_with_authorization",[v[@"state"] isEqual:@"preparing"] && [v[@"authorization_live"] boolValue] && [v[@"plan"][@"phase"] isEqual:@"preparing"] && [v[@"plan"][@"authorized"] boolValue]);
    records[@"sensor_start"]=v;
    check(@"real_sensor_preparing_has_no_write_claim",[v[@"plan"][@"fans"] count]==1 && [v[@"plan"][@"fans"][0][@"state"] isEqual:@"unobserved"] && ![v[@"hardware_write_enabled"] boolValue]);
    // Only the host-local sampler advances. Tick consumes a distinct batch.
    usleep(120000); [hostLock lock]; mf_helper_free(mf_helper_tick(host)); [hostLock unlock];
    for (int i=0;i<100;i++) { if ([status()[@"state"] isEqual:@"active"]) break; usleep(20000); }
    NSDictionary *running=status();
    records[@"sensor_running"]=running;
    check(@"real_sensor_native_renew_observes_running",renews>0 && [running[@"state"] isEqual:@"active"] && [running[@"authorization_live"] boolValue] && [running[@"plan"][@"fans"][0][@"target_rpm"] isEqual:@3000] && ![running[@"hardware_write_enabled"] boolValue]);
#else
    check(@"real_helper_plan_schema",[v[@"state"] isEqual:@"active"] && [v[@"authenticated_response"] boolValue]);
#endif
    NSDictionary *stopped=stop(); records[@"stop"]=stopped;
    check(@"real_helper_stop_schema",[stopped[@"state"] isEqual:@"automatic_confirmed"] && ![stopped[@"authorization_live"] boolValue] && [stopped[@"plan"][@"fans"][0][@"state"] isEqual:@"automatic_confirmed"]);
    [hostLock lock]; BOOL released=host==NULL; [hostLock unlock];
    check(@"fixture_host_destroyed_after_disconnect",released);
#elif defined(MF_CONTROL_CLIENT_FRESHNESS_ONLY)
    mode=@"control_hold_renew"; NSDictionary *live=start();
    check(@"admitted_fresh_write_claim",[live[@"hardware_write_enabled"] boolValue] && [live[@"authorization_live"] boolValue]);
    usleep(1650000); NSDictionary *stale=status(); records[@"stale"]=stale;
    check(@"stale_connected_reply_cannot_claim_write",[stale[@"request_in_flight"] boolValue] && [stale[@"can_stop"] boolValue] && ![stale[@"current"] boolValue] && ![stale[@"hardware_write_enabled"] boolValue] && ![stale[@"authorization_live"] boolValue]);
    stop();
#elif defined(MF_CONTROL_CLIENT_TEST_OS_MAJOR)
    check(@"old_os_denied",[status()[@"reason"] isEqual:@"macos_13_required"] && factories==0);
#else
    NSArray *rows=@[
        @[@"smc_readonly",@YES,@NO,@"readonly",@"readonly_policy"],
        @[@"smc_readonly",@YES,@NO,@"readonly",@"no_reviewed_profile"],
        @[@"smc_readonly",@YES,@NO,@"readonly",@"profile_validation_failed"],
        @[@"unavailable",@YES,@NO,@"readonly",@"backend_open_failed"],
        @[@"unavailable",@YES,@NO,@"readonly",@"backend_unavailable"],
        @[@"smc_control",@NO,@YES,@"profile_gated",@"admitted"],
        @[@"fake",@YES,@NO,@"test_fake",@"test_only"]];
    for (NSArray *row in rows) {
        for (NSString *op in @[@"plan_status",@"start_plan",@"renew_plan",@"stop_plan"]) {
            NSMutableDictionary *r=reply(op); r[@"backend"]=row[0]; r[@"dry_run"]=row[1]; r[@"hardware_write_enabled"]=row[2];
            r[@"control"]=@{@"version":@1,@"execution":row[3],@"admission":row[4]};
            check(@"seven_rows_every_success_operation",Reply(encode(r),op)!=nil);
            NSMutableDictionary *failure=[r mutableCopy]; failure[@"ok"]=@NO; failure[@"plan"]=r[@"result"][@"plan"]; [failure removeObjectForKey:@"result"];
            failure[@"error"]=@{@"code":[row[3] isEqual:@"readonly"]?row[4]:@"control_fault"};
            check(@"seven_rows_every_business_failure_operation",Reply(encode(failure),op)!=nil);
            for (NSString *key in @[@"backend",@"dry_run",@"hardware_write_enabled",@"control"]) {
                NSMutableDictionary *bad=[r mutableCopy]; [bad removeObjectForKey:key];
                check(@"missing_capability_field_rejected",!Reply(encode(bad),op));
            }
            for (NSString *key in @[@"backend",@"dry_run",@"hardware_write_enabled"]) {
                NSMutableDictionary *bad=[r mutableCopy]; bad[key]=[key isEqual:@"backend"]?@"unknown":@(![r[key] boolValue]);
                check(@"single_field_flip_rejected",!Reply(encode(bad),op));
            }
            for (NSString *key in @[@"version",@"execution",@"admission",@"unknown"]) {
                NSMutableDictionary *bad=[r mutableCopy], *c=[r[@"control"] mutableCopy];
                c[key]=[key isEqual:@"version"]?@2:@"unknown"; bad[@"control"]=c;
                check(@"closed_control_rejected",!Reply(encode(bad),op));
                [c removeObjectForKey:key];
                if (![key isEqual:@"unknown"]) check(@"missing_control_key_rejected",!Reply(encode(bad),op));
            }
            NSMutableDictionary *bad=[r mutableCopy], *c=[r[@"control"] mutableCopy]; c[@"version"]=@YES; bad[@"control"]=c;
            check(@"boolean_control_version_rejected",!Reply(encode(bad),op));
            bad=[r mutableCopy]; bad[@"version"]=@YES; check(@"boolean_envelope_version_rejected",!Reply(encode(bad),op));
            bad=[failure mutableCopy]; bad[@"token"]=secret; check(@"failure_token_rejected",!Reply(encode(bad),op));
            bad=[r mutableCopy]; bad[@"error"]=failure[@"error"]; check(@"success_error_rejected",!Reply(encode(bad),op));
        }
    }
    unsigned acceptedCombinations=0, combinations=0;
    for (NSString *backend in @[@"smc_readonly",@"unavailable",@"smc_control",@"fake"]) for (NSNumber *dry in @[@YES,@NO]) for (NSNumber *write in @[@YES,@NO])
    for (NSString *execution in @[@"readonly",@"profile_gated",@"test_fake"]) for (NSString *admission in @[@"readonly_policy",@"no_reviewed_profile",@"profile_validation_failed",@"backend_open_failed",@"backend_unavailable",@"admitted",@"test_only"]) {
        NSArray *candidate=@[backend,dry,write,execution,admission];
        NSMutableDictionary *r=reply(@"plan_status"); r[@"backend"]=backend; r[@"dry_run"]=dry; r[@"hardware_write_enabled"]=write;
        r[@"control"]=@{@"version":@1,@"execution":execution,@"admission":admission};
        BOOL accepted=Reply(encode(r),@"plan_status")!=nil; combinations++; acceptedCombinations+=accepted;
        check(@"complete_control_cross_product",accepted==[rows containsObject:candidate]);
    }
    check(@"exactly_seven_of_336_combinations",combinations==336 && acceptedCombinations==7);
    MFControlClientTestInstallBridge((MFControlClientTestBridge){selfMatches,connection,NO});
    check(@"fake_requires_compiletime_bridge_allow",!Reply(encode(reply(@"plan_status")),@"plan_status"));
    MFControlClientTestInstallBridge((MFControlClientTestBridge){selfMatches,connection,YES});
    missingTeam=YES;
    check(@"status_preflight",[status()[@"state"] isEqual:@"blocked"] && ![status()[@"can_attempt_start"] boolValue]);
    check(@"missing_team_no_connection",[start()[@"reason"] isEqual:@"production_team_unconfigured"] && factories==0);
    missingTeam=NO; denySelf=YES;
    check(@"self_signature_denied",[start()[@"reason"] isEqual:@"production_client_signature_requirement_failed"] && factories==0);
    denySelf=NO; mode=@"readonly"; unsigned before=starts;
    check(@"readonly_preflight_never_starts",[start()[@"reason"] isEqual:@"readonly_policy"] && starts==before);
    for (NSString *reason in @[@"readonly_policy",@"no_reviewed_profile",@"profile_validation_failed",@"backend_open_failed",@"backend_unavailable"]) {
        mode=[@"denied_" stringByAppendingString:reason]; unsigned deniedBefore=starts; NSDictionary *denied=start();
        check(@"five_denials_are_authenticated_never_start",[denied[@"reason"] isEqual:reason] && [denied[@"state"] isEqual:@"blocked"] && [denied[@"authenticated_response"] boolValue] && ![denied[@"current"] boolValue] && ![denied[@"hardware_write_enabled"] boolValue] && starts==deniedBefore);
    }
    mode=@"control"; NSDictionary *admitted=start(); records[@"admitted"]=admitted;
    check(@"admitted_stub_write_capability_separate_authorization",[admitted[@"hardware_write_enabled"] boolValue] && [admitted[@"authorization_live"] boolValue]);
    NSDictionary *confirmed=stop(); records[@"confirmed"]=confirmed;
    check(@"confirmed_stop_disconnect_clears_write",[confirmed[@"current"] boolValue] && ![confirmed[@"hardware_write_enabled"] boolValue] && ![confirmed[@"authorization_live"] boolValue]);
    mode=@"control_change_start"; NSDictionary *changed=start();
    check(@"changed_start_fingerprint_revokes",[changed[@"reason"] isEqual:@"helper_capability_changed"] && ![changed[@"hardware_write_enabled"] boolValue] && ![changed[@"authorization_live"] boolValue]);
    mode=@"control_change_renew"; start(); usleep(1150000);
    check(@"changed_renew_fingerprint_revokes",[status()[@"reason"] isEqual:@"helper_capability_changed"] && ![status()[@"hardware_write_enabled"] boolValue]);
    mode=@"control_change_stop"; start(); changed=stop();
    check(@"changed_stop_cannot_confirm_recovery",[changed[@"reason"] isEqual:@"helper_capability_changed"] && [changed[@"state"] isEqual:@"recovery_pending"] && ![changed[@"hardware_write_enabled"] boolValue]);
    mode=@"control"; start(); interrupt(); usleep(10000);
    check(@"admitted_disconnect_clears_write",![status()[@"hardware_write_enabled"] boolValue] && ![status()[@"authorization_live"] boolValue]);
    mode=@"valid";
    mode=@"preparing";
    NSDictionary *prepared=start();
    check(@"preparing_retains_live_connection",[prepared[@"state"] isEqual:@"preparing"] && [prepared[@"authorization_live"] boolValue] && [prepared[@"can_stop"] boolValue] && ![prepared[@"hardware_write_enabled"] boolValue]);
    before=renews; usleep(1150000);
    check(@"preparing_renews_without_writes",renews>before && [status()[@"state"] isEqual:@"preparing"]);
    check(@"preparing_stop_confirmed",[stop()[@"state"] isEqual:@"automatic_confirmed"]);
    mode=@"preparing_automatic"; check(@"preparing_prior_automatic_not_running",[start()[@"state"] isEqual:@"preparing"]); stop();
    mode=@"preparing_transition"; check(@"sensor_start_waits_for_samples",[start()[@"state"] isEqual:@"preparing"]); usleep(1150000);
    check(@"sensor_renew_confirms_running",[status()[@"state"] isEqual:@"active"]); stop();
    for (NSString *bad in @[@"preparing_active",@"preparing_wrong_fan",@"preparing_extra_fan",@"preparing_missing_fan",@"preparing_no_consent",@"preparing_ended",@"preparing_unauthorized"]) {
        mode=bad; NSDictionary *v=start();
        check(@"invalid_preparing_disconnects",![v[@"authorization_live"] boolValue] && ![status()[@"can_stop"] boolValue]);
    }
    mode=@"preparing_false_running"; start(); usleep(1150000);
    check(@"running_requires_requested_outcomes",[status()[@"reason"] isEqual:@"helper_reply_invalid"] && ![status()[@"authorization_live"] boolValue]);
    mode=@"preparing_out_of_range"; start(); usleep(1150000);
    check(@"sensor_target_bounds_enforced",[status()[@"reason"] isEqual:@"helper_reply_invalid"] && ![status()[@"authorization_live"] boolValue]);
    mode=@"preparing"; start(); interrupt(); usleep(10000);
    check(@"preparing_disconnect_never_revives",[status()[@"state"] isEqual:@"recovery_pending"] && ![status()[@"authorization_live"] boolValue]);
    mode=@"preparing_hold_renew"; start(); usleep(1300000);
    check(@"preparing_timeout_revokes",![status()[@"authorization_live"] boolValue] && [status()[@"state"] isEqual:@"recovery_pending"]);
    mode=@"valid"; late(encode(reply(@"renew_plan"))); usleep(10000);
    check(@"preparing_late_reply_inert",[status()[@"state"] isEqual:@"recovery_pending"]);
    mode=@"valid";
    check(@"successful_start_confirmed",[start()[@"state"] isEqual:@"active"] && [status()[@"authorization_live"] boolValue]);
    unsigned requestBefore=requests;
    for (int i=0;i<100;i++) status();
    check(@"status_does_not_renew",requests==requestBefore);
    before=renews; usleep(1150000);
    check(@"independent_renewal",renews==before+1 && [status()[@"state"] isEqual:@"active"]);
    MFControlClientTestSetSequence(9007199254740992ULL); previousSequence=9007199254740992ULL;
    check(@"stop_confirmed",[stop()[@"state"] isEqual:@"automatic_confirmed"] && previousSequence==9007199254740993ULL);
    check(@"stop_disconnects",![status()[@"can_stop"] boolValue] && ![status()[@"authorization_live"] boolValue]);
    uint64_t stale=intent; stop();
    before=starts; check(@"stop_before_prepared_start",[startID(stale)[@"reason"] isEqual:@"stale_intent"] && starts==before);
    start(); check(@"old_stop_cannot_kill_new_start",[decode(mf_control_stop(stale))[@"state"] isEqual:@"active"]); stop();
    for (NSString *bad in @[@"invalid",@"wrong_kind",@"fake_authority",@"throw_request"]) {
        mode=bad; NSDictionary *v=start(); check(@"invalid_or_exception_disconnects",![v[@"authorization_live"] boolValue] && ![status()[@"can_stop"] boolValue]);
    }
    mode=@"hold_start"; NSDictionary *timed=start();
    check(@"timeout_closes",[timed[@"reason"] isEqual:@"helper_client_timeout"] && ![status()[@"can_stop"] boolValue]);
    void (^old)(NSData *)=late; mode=@"valid"; start(); old(encode(reply(@"start_plan"))); usleep(10000);
    check(@"late_reply_cannot_replace_generation",[status()[@"state"] isEqual:@"active"]); stop();
    mode=@"sync_late"; check(@"claim_time_deadline",[start()[@"reason"] isEqual:@"helper_client_timeout"]);
    mode=@"hold_start"; dispatch_semaphore_t done=dispatch_semaphore_create(0); uint64_t pending=++intent;
    dispatch_async(dispatch_get_global_queue(0,0),^{ startID(pending); dispatch_semaphore_signal(done); });
    for (int i=0;i<100&&! [status()[@"request_in_flight"] boolValue];i++) usleep(1000);
    check(@"stop_cancels_inflight_start",[stop()[@"reason"] isEqual:@"stop_cancelled_inflight_request"]);
    check(@"cancel_unblocks_start",dispatch_semaphore_wait(done,dispatch_time(DISPATCH_TIME_NOW,NSEC_PER_SEC))==0);
    mode=@"valid"; start(); interrupt(); usleep(10000);
    check(@"disconnect_revokes",![status()[@"authorization_live"] boolValue] && [status()[@"state"] isEqual:@"recovery_pending"]);
    start(); mode=@"stop_unconfirmed"; check(@"recovery_not_faked",[stop()[@"state"] isEqual:@"recovery_pending"]);
    mode=@"valid"; start(); mode=@"stop_exhausted"; NSDictionary *exhausted=stop();
    check(@"exhausted_recovery_null_is_valid",[exhausted[@"authenticated_response"] boolValue] && [exhausted[@"state"] isEqual:@"recovery_pending"] && [exhausted[@"plan"][@"fans"][0][@"state"] isEqual:@"recovery_exhausted"]);
    mode=@"valid"; start(); mode=@"stop_inconsistent";
    check(@"inconsistent_retry_is_rejected",[stop()[@"reason"] isEqual:@"helper_reply_invalid"]);
    mode=@"valid"; start(); MFControlClientTestSetSequence(UINT64_MAX);
    check(@"sequence_exhaustion_revokes",[stop()[@"reason"] isEqual:@"sequence_exhausted"]);
    mode=@"hold_renew"; start(); usleep(1050000); before=requests;
    for(int i=0;i<100;i++) status();
    check(@"renewal_single_inflight",requests==before && [status()[@"request_in_flight"] boolValue]);
    usleep(250000); check(@"renewal_timeout_revokes",![status()[@"can_stop"] boolValue]);
    mode=@"throw_invalidate"; start(); check(@"invalidation_exception_contained",[stop()[@"state"] isEqual:@"automatic_confirmed"]);
    mode=@"valid"; before=starts; check(@"max_reserved_for_stop",[startID(UINT64_MAX)[@"reason"] isEqual:@"stale_intent"] && starts==before);
    const char *bad="[{\"fan\":0,\"strategy\":{\"type\":\"fixed\",\"rpm\":true},\"token\":\"forged\"}]";
    check(@"closed_input",[decode(mf_control_start((const uint8_t*)bad,strlen(bad),++intent))[@"reason"] isEqual:@"invalid_plan_input"]);
#endif
    puts([[NSString alloc] initWithData:encode(@{@"passed":@YES,@"checks":[NSOrderedSet orderedSetWithArray:checks].array,@"requests":@(requests),@"invalidations":@(invalidations),@"real_hardware_access":@NO,@"records":records}) encoding:NSUTF8StringEncoding].UTF8String);
} }
'''
DRIVER = r'''
#import <Foundation/Foundation.h>
#import "ControlClient.h"
int main(void) { @autoreleasepool {
    NSDictionary *plan=@{@"phase":@"idle",@"fans":@[],@"watchdog_expirations":@0,@"authorized":@NO,@"consent_expires_at_ms":NSNull.null,@"lease_ttl_ms":@0,@"authorization_end":NSNull.null,@"error":NSNull.null,@"stop_reason":NSNull.null};
    NSMutableDictionary *r=[@{@"version":@1,@"backend":@"fake",@"dry_run":@YES,@"hardware_write_enabled":@NO,@"ok":@YES,@"control":@{@"version":@1,@"execution":@"test_fake",@"admission":@"test_only"},@"result":@{@"kind":@"plan",@"plan":plan}} mutableCopy];
    if (Reply([NSJSONSerialization dataWithJSONObject:r options:0 error:NULL],@"plan_status")) return 2;
    r[@"backend"]=@"smc_control"; r[@"dry_run"]=@NO; r[@"hardware_write_enabled"]=@YES; r[@"control"]=@{@"version":@1,@"execution":@"profile_gated",@"admission":@"admitted"};
    if (!Reply([NSJSONSerialization dataWithJSONObject:r options:0 error:NULL],@"plan_status")) return 3;
    char *p=mf_control_status(); puts(p); mf_control_free(p);
} }
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--stub-only', action='store_true', help='Skip Rust helper integration until the host wire contract lands')
    options = parser.parse_args()
    assert os.geteuid() != 0
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    (EVIDENCE/'control.log').write_text('')
    evidence = {'passed': False, 'scope': 'Real production identity denial and isolated mock transport state-machine regression; no real helper write channel or hardware access', 'runs': {}, 'response_parser_boundary': 'Parsed schema/types/closed capability combinations use existing NSJSONSerialization semantics; no raw JSON duplicate-key detector added; host serde replies emit unique fields'}
    try:
        with tempfile.TemporaryDirectory(prefix='mf-control-client-', dir=EVIDENCE) as temp:
            d = Path(temp)
            (d/'Harness.m').write_text('#include "' + str(NATIVE/'ControlClient.m') + '"\n' + HARNESS)
            (d/'Driver.m').write_text('#include "' + str(NATIVE/'ControlClient.m') + '"\n' + DRIVER)
            flags = ['xcrun','clang','-fobjc-arc','-fobjc-arc-exceptions','-fblocks','-Wall','-Wextra','-Werror','-mmacosx-version-min=11.0','-framework','Foundation','-framework','Security','-I',NATIVE]
            binary=d/'production'
            run(flags+[ROOT/'native/ProductionIdentity.m',d/'Driver.m','-o',binary])
            evidence['runs']['production']=json.loads(run([binary]))
            evidence['normal_parser']={'fake_rejected':True,'admitted_grammar_accepted':True}
            assert evidence['runs']['production']['reason']=='production_team_unconfigured'
            nm=run(['nm','-g',binary])
            assert 'MFControlClientTest' not in nm and 'isPlatformVersionAtLeast' not in nm and 'availability_version_check' not in nm
            for name,extra in [('state_machine',[]),('freshness',['-DMF_CONTROL_CLIENT_FRESHNESS_ONLY','-Wno-unused-function','-Wno-unused-variable']),('old_os',['-DMF_CONTROL_CLIENT_TEST_OS_MAJOR=12','-Wno-unused-function','-Wno-unused-variable'])]:
                binary=d/name
                run(flags+['-DMF_CONTROL_CLIENT_TEST_HARNESS']+([] if name=='freshness' else ['-DMF_CONTROL_CLIENT_TEST_SHORT_TIMEOUT'])+extra+[d/'Harness.m','-o',binary])
                evidence['runs'][name]=json.loads(run([binary]))
            if options.stub_only:
                evidence['integration'] = 'deferred_by_explicit_stub_only_option'
                evidence['source_sha256']={str(p.relative_to(ROOT.parent)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [NATIVE/'ControlClient.m',NATIVE/'HelperClient.m',Path(__file__).resolve()]}
                evidence['passed']=True
                return
            target=d/'cargo-target'
            run(['cargo','build','--offline','--features','test-harness','--manifest-path',ROOT/'Cargo.toml','--target-dir',target])
            for name, variant in [('real_helper_schema', []), ('real_sensor_schema', ['-DMF_CONTROL_CLIENT_REAL_SENSOR'])]:
                binary=d/name
                run(flags+variant+['-DMF_CONTROL_CLIENT_TEST_HARNESS','-DMF_CONTROL_CLIENT_REAL_HELPER','-Wno-unused-function','-Wno-unused-variable','-framework','IOKit']+[d/'Harness.m',target/'debug/libmacs_fan_helper_bridge.a','-o',binary])
                evidence['runs'][name]=json.loads(run([binary]))
            evidence['source_sha256']={str(p.relative_to(ROOT.parent)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [NATIVE/'ControlClient.h',NATIVE/'ControlClient.m',Path(__file__).resolve()]}
            evidence['passed']=True
    finally:
        (EVIDENCE/'verification.json').write_text(json.dumps(evidence,indent=2)+'\n')
        print('Evidence: '+str(EVIDENCE/'verification.json'))


if __name__=='__main__':
    main()

#!/usr/bin/env python3
"""Nonroot production-client denial and mocked transport regression. Never registers services."""
import hashlib
import json
import os
import plistlib
import re
from pathlib import Path
import shlex
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
NATIVE = ROOT.parent / 'src-tauri/native'
EVIDENCE = ROOT.parents[2] / 'work/control-native-0.16/inspection-verification.json'


def run(command):
    print('$ ' + shlex.join(map(str, command)), flush=True)
    result = subprocess.run(list(map(str, command)), stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, timeout=60)
    if result.stdout:
        print(result.stdout, end='', flush=True)
    with (EVIDENCE.parent/'inspection.log').open('a') as log:
        log.write('$ ' + shlex.join(map(str, command)) + '\n' + result.stdout)
    result.check_returncode()
    return result.stdout


HARNESS = r'''
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#include <stdatomic.h>
#import "HelperClient.h"
static NSMutableArray *checks, *events;
static NSString *mode, *throwAt;
static BOOL missingTeam, invalidHelper, reenter, hold;
static unsigned identityCalls, factories, invalidations;
static _Atomic(unsigned) liveConnections;
static void (^lateReply)(NSData *);
static dispatch_semaphore_t entered, releaseRequest;
static NSDictionary *reentrant;
static void check(NSString *name, BOOL condition) {
    if (!condition) { fprintf(stderr, "FAIL %s\n", name.UTF8String); abort(); }
    [checks addObject:name];
}
static void waitFor(dispatch_semaphore_t semaphore) {
    if (dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC))) abort();
}
static void maybeThrow(NSString *where) {
    [events addObject:where];
    if ([throwAt isEqual:where]) [NSException raise:@"Injected" format:@"test only"];
}
NSString *MFProductionConfiguredTeamIdentifier(void) { identityCalls++; return missingTeam ? @"" : @"ABCDE12345"; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSDictionary *MFProductionConfiguredRequirements(void) {
    identityCalls++;
    if (missingTeam) return nil;
    return @{@"client":@"anchor apple generic and identifier \"local.macsfan.desktop\" and certificate leaf[subject.OU] = \"ABCDE12345\"",
        @"helper": invalidHelper ? @"this is not a requirement" : @"anchor apple generic and identifier \"local.macsfan.helper.control\" and certificate leaf[subject.OU] = \"ABCDE12345\""};
}
static NSDictionary *decode(char *bytes) {
    if (!bytes) abort();
    NSData *data = [NSData dataWithBytes:bytes length:strlen(bytes)];
    mf_helper_client_free(bytes);
    id value = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
    if (![value isKindOfClass:NSDictionary.class]) abort();
    check(@"wrapper_invariants", [value[@"schema_version"] isEqual:@1] && [value[@"operation"] isEqual:@"inspect_hardware"] &&
        [value[@"hardware_write_enabled"] isEqual:@NO] && [value[@"production_control_ready"] isEqual:@NO]);
    return value;
}
static NSDictionary *invoke(void) { @autoreleasepool { return decode(mf_helper_inspect()); } }
static NSMutableDictionary *validReply(void) {
    return [@{@"version":@1,@"dry_run":@YES,@"hardware_write_enabled":@NO,@"ok":@YES,
        @"backend":@"smc_readonly",@"hardware_reads":@YES,@"hardware_write_attempts":@0,
        @"result":@{@"kind":@"hardware_inspection",@"inspection":@{@"hardwareProfileVerified":@NO,@"hardwareWriteAttempts":@0}}} mutableCopy];
}
static NSData *encode(id value) { return [NSJSONSerialization dataWithJSONObject:value options:0 error:NULL]; }
@interface MockConnection : NSObject
@property(nonatomic,copy) void (^interruptionHandler)(void);
@property(nonatomic,copy) void (^invalidationHandler)(void);
@property(nonatomic,copy) void (^errorHandler)(NSError *);
@property(nonatomic,strong) NSXPCInterface *remoteObjectInterface;
- (void)setCodeSigningRequirement:(NSString *)requirement;
- (void)resume;
- (void)invalidate;
- (id)remoteObjectProxyWithErrorHandler:(void (^)(NSError *))handler;
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end
@implementation MockConnection
- (instancetype)init { self = [super init]; if (self) atomic_fetch_add(&liveConnections, 1); return self; }
- (void)dealloc { atomic_fetch_sub(&liveConnections, 1); }
- (BOOL)respondsToSelector:(SEL)selector {
    if ([mode isEqual:@"no_api"] && selector == NSSelectorFromString(@"setCodeSigningRequirement:")) return NO;
    return [super respondsToSelector:selector];
}
- (void)setCodeSigningRequirement:(NSString *)requirement {
    check(@"exact_helper_requirement", [requirement isEqual:MFProductionConfiguredRequirements()[@"helper"]]);
    maybeThrow(@"requirement");
}
- (void)setRemoteObjectInterface:(NSXPCInterface *)value { _remoteObjectInterface = value; maybeThrow(@"interface"); }
- (void)setInterruptionHandler:(void (^)(void))value { _interruptionHandler = value; maybeThrow(@"interruption_handler"); }
- (void)setInvalidationHandler:(void (^)(void))value { _invalidationHandler = value; maybeThrow(@"invalidation_handler"); }
- (void)resume { maybeThrow(@"resume"); }
- (void)invalidate { invalidations++; if (self.invalidationHandler) self.invalidationHandler(); maybeThrow(@"invalidate"); }
- (id)remoteObjectProxyWithErrorHandler:(void (^)(NSError *))handler {
    self.errorHandler = handler; maybeThrow(@"proxy"); return self;
}
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply {
    maybeThrow(@"request");
    check(@"fixed_exact_request", [[[NSString alloc] initWithData:payload encoding:NSUTF8StringEncoding] isEqual:@"{\"version\":1,\"command\":{\"op\":\"inspect_hardware\"}}"]);
    if (reenter) { reenter = NO; reentrant = invoke(); }
    if (hold) { hold = NO; dispatch_semaphore_signal(entered); waitFor(releaseRequest); }
    if ([mode isEqual:@"async_valid"] || [mode isEqual:@"late35"]) {
        int64_t delay = [mode isEqual:@"late35"] ? 35 * NSEC_PER_MSEC : 5 * NSEC_PER_MSEC;
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, delay), dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), ^{ reply(encode(validReply())); });
        return;
    }
    if ([mode isEqual:@"sync_late"]) {
        [NSThread sleepForTimeInterval:0.05]; reply(encode(validReply())); return;
    }
    if ([mode isEqual:@"timeout"]) { lateReply = [reply copy]; return; }
    if ([mode isEqual:@"error"]) { self.errorHandler([NSError errorWithDomain:@"mock" code:1 userInfo:nil]); return; }
    if ([mode isEqual:@"invalidated"]) { self.invalidationHandler(); return; }
    if ([mode isEqual:@"interrupted"]) { self.interruptionHandler(); return; }
    if ([mode isEqual:@"malformed"]) { reply([@"{bad" dataUsingEncoding:NSUTF8StringEncoding]); return; }
    if ([mode isEqual:@"not_data"]) { reply((NSData *)(id)@"wrong_type"); return; }
    if ([mode isEqual:@"oversized"]) { reply([NSMutableData dataWithLength:1024 * 1024 + 1]); return; }
    NSMutableDictionary *value = validReply();
    if ([mode isEqual:@"wrong_op"]) value[@"result"] = @{@"kind":@"fans",@"fans":@[]};
    if ([mode isEqual:@"write_claim"]) value[@"hardware_write_enabled"] = @YES;
    if ([mode hasPrefix:@"control"]) {
        value[@"backend"]=@"smc_control"; value[@"dry_run"]=@NO; value[@"hardware_write_enabled"]=@YES; value[@"hardware_write_attempts"]=@7;
        value[@"result"]=@{@"kind":@"hardware_inspection",@"inspection":@{@"hardwareProfileVerified":@YES,@"hardwareWriteAttempts":@7}};
        if ([mode isEqual:@"control_unverified"]) value[@"result"]=@{@"kind":@"hardware_inspection",@"inspection":@{@"hardwareProfileVerified":@NO,@"hardwareWriteAttempts":@7}};
        if ([mode isEqual:@"control_wrong_flags"]) value[@"dry_run"]=@YES;
        if ([mode isEqual:@"control_negative_count"]) value[@"hardware_write_attempts"]=@(-1);
        if ([mode isEqual:@"control_boolean_count"]) value[@"hardware_write_attempts"]=@YES;
        if ([mode isEqual:@"control_fractional_count"]) value[@"hardware_write_attempts"]=@1.5;
        if ([mode isEqual:@"control_mismatch_count"]) value[@"hardware_write_attempts"]=@8;
        if ([mode isEqual:@"control_nonbool_profile"]) value[@"result"]=@{@"kind":@"hardware_inspection",@"inspection":@{@"hardwareProfileVerified":@1,@"hardwareWriteAttempts":@7}};
        if ([mode isEqual:@"control_error"]) { value[@"ok"]=@NO; [value removeObjectForKey:@"result"]; value[@"error"]=@{@"code":@"actuator_unavailable"}; }
    }
    if ([mode isEqual:@"write_count"]) value[@"hardware_write_attempts"] = @1;
    if ([mode isEqual:@"fake"]) value[@"backend"]=@"fake";
    if ([mode isEqual:@"missing_backend"]) [value removeObjectForKey:@"backend"];
    if ([mode isEqual:@"unavailable_success"]) value[@"backend"]=@"unavailable";
    if ([mode isEqual:@"profile_claim"]) value[@"result"] = @{@"kind":@"hardware_inspection",@"inspection":@{@"hardwareProfileVerified":@YES,@"hardwareWriteAttempts":@0}};
    if ([mode isEqual:@"unexpected"]) value[@"production_control_ready"] = @YES;
    if ([mode isEqual:@"bool_version"]) value[@"version"] = @YES;
    if ([mode isEqual:@"nonbool_marker"]) value[@"dry_run"] = @1;
    if ([mode isEqual:@"helper_error"]) value = [@{@"version":@1,@"dry_run":@YES,@"hardware_write_enabled":@NO,@"backend":@"unavailable",@"ok":@NO,@"error":@{@"code":@"actuator_unavailable"}} mutableCopy];
    if ([mode isEqual:@"invalid_error"]) { value[@"ok"] = @NO; value[@"error"] = @{@"code":@"error"}; }
    reply(encode(value));
    if ([mode isEqual:@"duplicate"]) { reply([@"bad" dataUsingEncoding:NSUTF8StringEncoding]); self.errorHandler([NSError errorWithDomain:@"mock" code:2 userInfo:nil]); self.interruptionHandler(); }
    if ([mode isEqual:@"reply_then_throw"]) [NSException raise:@"AfterReply" format:@"test only"];
}
@end
static BOOL selfMatches(NSString *text) { (void)text; maybeThrow(@"self"); return YES; }
static id connection(NSString *name, NSXPCConnectionOptions options) {
    factories++; maybeThrow(@"factory");
    check(@"fixed_privileged_service", [name isEqual:@"local.macsfan.helper.control"] && options == NSXPCConnectionPrivileged);
    return [MockConnection new];
}
static void install(BOOL stubSecurity) {
    MFHelperClientTestInstallBridge((MFHelperClientTestBridge){stubSecurity ? selfMatches : NULL, connection});
}
static NSDictionary *one(NSString *value) {
    mode = value; events = [NSMutableArray new]; NSDictionary *result = invoke();
    check(@"connection_released_after_terminal_path", atomic_load(&liveConnections) == 0);
    return result;
}
static void expect(NSString *name, NSDictionary *value, NSString *reason, BOOL authenticated) {
    check(name, [value[@"reason"] isEqual:reason] && [value[@"authenticated_response"] isEqual:@(authenticated)] &&
        (authenticated || !value[@"response"]));
}
int main(void) {
    @autoreleasepool {
        checks = [NSMutableArray new]; events = [NSMutableArray new]; install(YES);
#if defined(MF_HELPER_CLIENT_TEST_OS_MAJOR) && MF_HELPER_CLIENT_TEST_OS_MAJOR < 13
        expect(@"old_os_gate", invoke(), @"macos_13_required", NO);
        check(@"old_os_no_identity_or_new_api", !identityCalls && !factories && !events.count);
#elif defined(MF_HELPER_CLIENT_DEADLINE_RACE)
        expect(@"expired_reply_during_waiter_pause_stays_timeout", one(@"late35"), @"helper_client_timeout", NO);
#else
        missingTeam = YES;
        expect(@"unconfigured_team_denied", invoke(), @"production_team_unconfigured", NO);
        check(@"unconfigured_no_connection", factories == 0);
        missingTeam = NO; install(NO);
        expect(@"fake_team_rejected_by_real_security", invoke(), @"production_client_signature_requirement_failed", NO);
        check(@"real_security_denial_before_connection", factories == 0);
        install(YES); invalidHelper = YES;
        expect(@"invalid_helper_requirement_denied", invoke(), @"production_helper_requirement_invalid", NO);
        check(@"invalid_requirement_no_connection", factories == 0);
        invalidHelper = NO;
        expect(@"mock_valid_readonly_reply", one(@"valid"), @"authenticated_hardware_inspection", YES);
        check(@"requirement_precedes_resume", [events indexOfObject:@"requirement"] < [events indexOfObject:@"resume"]);
        expect(@"missing_requirement_api_fails_closed", one(@"no_api"), @"helper_signing_requirement_api_unavailable", NO);
        expect(@"background_reply_without_main_runloop", one(@"async_valid"), @"authenticated_hardware_inspection", YES);
        expect(@"synchronous_expired_reply_rejected", one(@"sync_late"), @"helper_client_timeout", NO);
        MFHelperClientTestInstallBridge((MFHelperClientTestBridge){selfMatches, NULL});
        unsigned beforeMissingFactory = factories;
        expect(@"test_missing_factory_no_production_fallthrough", invoke(), @"helper_connection_unavailable", NO);
        check(@"test_missing_factory_did_not_create_connection", factories == beforeMissingFactory);
        install(YES);
        for (NSString *name in @[@"control",@"control_unverified"]) expect(@"control_host_read_rpc_no_authorization", one(name), @"authenticated_hardware_inspection", YES);
        expect(@"control_host_error_authenticated",one(@"control_error"),@"authenticated_helper_error",YES);
        expect(@"mock_helper_error_is_authenticated", one(@"helper_error"), @"authenticated_helper_error", YES);
        NSDictionary *transport = @{@"error":@"helper_xpc_error",@"invalidated":@"helper_connection_invalidated",@"interrupted":@"helper_connection_interrupted",@"timeout":@"helper_client_timeout",@"oversized":@"helper_reply_too_large"};
        for (NSString *name in transport) {
            unsigned before = invalidations;
            expect([@"mock_" stringByAppendingString:name], one(name), transport[name], NO);
            check(@"terminal_path_invalidated_once", invalidations == before + 1);
            if ([name isEqual:@"timeout"]) { lateReply(encode(validReply())); lateReply(nil); lateReply = nil; }
        }
        for (NSString *name in @[@"malformed",@"not_data",@"wrong_op",@"write_claim",@"write_count",@"profile_claim",@"unexpected",@"bool_version",@"nonbool_marker",@"invalid_error",@"control_wrong_flags",@"control_negative_count",@"control_boolean_count",@"control_fractional_count",@"control_mismatch_count",@"control_nonbool_profile",@"fake",@"missing_backend",@"unavailable_success"]) {
            expect([@"reject_" stringByAppendingString:name], one(name), @"helper_reply_invalid", NO);
        }
        expect(@"first_reply_wins_duplicate_callbacks", one(@"duplicate"), @"authenticated_hardware_inspection", YES);
        expect(@"first_reply_wins_send_exception", one(@"reply_then_throw"), @"authenticated_hardware_inspection", YES);
        for (NSString *where in @[@"self",@"factory",@"requirement",@"interface",@"interruption_handler",@"invalidation_handler",@"resume",@"proxy",@"request"]) {
            throwAt = where;
            expect([@"exception_" stringByAppendingString:where], one(@"valid"), @"helper_client_exception", NO);
            throwAt = nil;
            expect(@"recovered_after_exception", one(@"valid"), @"authenticated_hardware_inspection", YES);
        }
        throwAt = @"invalidate";
        expect(@"invalidation_exception_preserves_reply", one(@"valid"), @"authenticated_hardware_inspection", YES);
        throwAt = nil; reenter = YES;
        expect(@"reentrant_owner_finishes", one(@"valid"), @"authenticated_hardware_inspection", YES);
        expect(@"reentrant_fails_promptly", reentrant, @"helper_client_busy", NO);
        entered = dispatch_semaphore_create(0); releaseRequest = dispatch_semaphore_create(0);
        dispatch_semaphore_t done = dispatch_semaphore_create(0); hold = YES; mode = @"valid";
        __block NSDictionary *owner;
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), ^{
            @autoreleasepool { owner = invoke(); dispatch_semaphore_signal(done); }
        });
        waitFor(entered);
        expect(@"concurrent_caller_fails_promptly", invoke(), @"helper_client_busy", NO);
        dispatch_semaphore_signal(releaseRequest); waitFor(done);
        expect(@"concurrent_owner_finishes", owner, @"authenticated_hardware_inspection", YES);
        expect(@"post_late_callback_state_reusable", one(@"valid"), @"authenticated_hardware_inspection", YES);
#endif
        NSData *data = encode(@{@"checks":checks,@"factory_calls":@(factories),@"invalidations":@(invalidations)});
        fwrite(data.bytes, 1, data.length, stdout); fputc('\n', stdout);
    }
}
'''

DRIVER = r'''
#include <stdio.h>
#include "HelperClient.h"
int main(void) { char *result = mf_helper_inspect(); if (!result) return 1; puts(result); mf_helper_client_free(result); }
'''


SERVER = r'''
#import <Foundation/Foundation.h>
@protocol FixtureHelper
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end
@interface Fixture : NSObject <NSXPCListenerDelegate, FixtureHelper>
@end
@implementation Fixture
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    (void)listener;
    // Only this temporary fixture reads a mutable admission policy, enabling
    // the same wrong client bytes to prove rejection/acceptance causality.
    NSString *requirement = [NSString stringWithContentsOfFile:@"FIXTURE_POLICY_PATH" encoding:NSUTF8StringEncoding error:NULL];
    if (![@[@"identifier \"io.macsfan.client.fixture\"", @"identifier \"io.macsfan.client.fixture.wrong\""] containsObject:requirement]) return NO;
    [connection setCodeSigningRequirement:requirement];
    connection.exportedInterface = [NSXPCInterface interfaceWithProtocol:@protocol(FixtureHelper)];
    connection.exportedObject = self;
    [connection resume]; return YES;
}
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply {
    NSString *expected = @"{\"version\":1,\"command\":{\"op\":\"inspect_hardware\"}}";
    BOOL exact = [[[NSString alloc] initWithData:payload encoding:NSUTF8StringEncoding] isEqual:expected];
    NSDictionary *value = @{@"version":@1,@"dry_run":@YES,@"hardware_write_enabled":@NO,@"backend":@"unavailable",@"ok":@NO,
        @"error":@{@"code":exact ? @"fixture_readonly_no_hardware" : @"fixture_wrong_request"}};
    reply([NSJSONSerialization dataWithJSONObject:value options:0 error:NULL]);
}
@end
int main(void) {
    @autoreleasepool {
        Fixture *delegate = [Fixture new]; NSXPCListener *listener = NSXPCListener.serviceListener;
        listener.delegate = delegate; [listener resume]; dispatch_main();
    }
}
'''

REAL_CLIENT = r'''
#import <Foundation/Foundation.h>
#import "HelperClient.h"
NSString *MFProductionConfiguredTeamIdentifier(void) { return @"FIXTURE_ONLY"; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSDictionary *MFProductionConfiguredRequirements(void) {
    return @{@"client":@"identifier \"SELF_ID\"", @"helper":@"cdhash H\"HELPER_HASH\""};
}
static id fixture(NSString *name, NSXPCConnectionOptions options) {
    if (![name isEqual:@"local.macsfan.helper.control"] || options != NSXPCConnectionPrivileged) abort();
    return [[NSXPCConnection alloc] initWithServiceName:@"io.macsfan.client.fixture.helper"];
}
int main(void) {
    @autoreleasepool {
        // NULL means actual SecCodeCopySelf + SecCodeCheckValidity, no self stub.
        MFHelperClientTestInstallBridge((MFHelperClientTestBridge){NULL, fixture});
        char *value = mf_helper_inspect(); if (!value) return 1; puts(value); mf_helper_client_free(value);
    }
}
'''


def actual_xpc(build, flags):
    """New client selector and completion path over actual nonprivileged NSXPC.

    The service is an explicit fixed-error fixture: no helper engine, SMC or
    production Team. Identifier-only client identity is local test policy;
    server identity is an exact CDHash compiled into each test client.
    """
    result = {'scope': 'Real embedded NSXPC, fixed readonly error fixture, actual Security self check; no hardware or production identity',
              'responses': {}}
    service_id = 'io.macsfan.client.fixture.helper'
    app = build / 'ConnectorFixture.app'
    client = app / 'Contents/MacOS/Client'
    service = app / f'Contents/XPCServices/{service_id}.xpc'
    helper = service / 'Contents/MacOS/FixtureHelper'
    client.parent.mkdir(parents=True)
    helper.parent.mkdir(parents=True)
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps({
        'CFBundleIdentifier': 'io.macsfan.client.fixture', 'CFBundleExecutable': 'Client',
        'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1'}))
    (service / 'Contents/Info.plist').write_bytes(plistlib.dumps({
        'CFBundleIdentifier': service_id, 'CFBundleExecutable': 'FixtureHelper',
        'CFBundlePackageType': 'XPC!', 'CFBundleVersion': '1', 'LSMinimumSystemVersion': '13.0',
        'XPCService': {'ServiceType': 'Application'}}))
    server = build / 'FixtureServer.m'
    policy = build / 'FixtureClientPolicy.txt'
    policy.write_text('identifier "io.macsfan.client.fixture"')
    server.write_text(SERVER.replace('FIXTURE_POLICY_PATH', str(policy)))
    run(flags + ['-mmacosx-version-min=13.0', server, '-o', helper])
    # This generated scratch bundle lives in CloudStorage; strip Finder/provider
    # detritus only from this fixture before its disposable ad-hoc signature.
    run(['xattr', '-cr', service])
    run(['codesign', '--force', '--sign', '-', service])
    signature = run(['codesign', '-d', '--verbose=4', helper])
    digest = re.search(r'^CDHash=([0-9a-f]{40})$', signature, re.M).group(1)
    result['server_cdhash'] = digest
    result['server_sha256'] = hashlib.sha256(helper.read_bytes()).hexdigest()
    result['clients'] = {}
    for name, self_id, server_hash in [
        ('matching', 'io.macsfan.client.fixture', digest),
        ('wrong_server_pin', 'io.macsfan.client.fixture', '0' * 40),
        ('wrong_real_client', 'io.macsfan.client.fixture.wrong', digest)]:
        source = build / (name + '.m')
        source.write_text(REAL_CLIENT.replace('SELF_ID', self_id).replace('HELPER_HASH', server_hash))
        run(flags + ['-DMF_HELPER_CLIENT_TEST_HARNESS', '-DMF_HELPER_CLIENT_TEST_FULL_TIMEOUT',
                     NATIVE / 'HelperClient.m', source, '-o', client])
        run(['codesign', '--force', '--sign', '-', '--identifier', self_id, client])
        result['clients'][name] = {'sha256': hashlib.sha256(client.read_bytes()).hexdigest(),
                                   'self_requirement': 'identifier "' + self_id + '"',
                                   'compiled_helper_cdhash': server_hash}
        response = json.loads(run([client]))
        result['responses'][name] = response
        if name == 'matching':
            assert response['authenticated_response'] is True and response['result'] == 'helper_error'
            assert response['response']['error']['code'] == 'fixture_readonly_no_hardware'
        else:
            assert response['authenticated_response'] is False and 'response' not in response
            assert response['reason'] in {'helper_xpc_error', 'helper_connection_invalidated', 'helper_connection_interrupted'}
    wrong_sha = hashlib.sha256(client.read_bytes()).hexdigest()
    policy.write_text('identifier "io.macsfan.client.fixture.wrong"')
    counterfactual = json.loads(run([client]))
    assert hashlib.sha256(client.read_bytes()).hexdigest() == wrong_sha == result['clients']['wrong_real_client']['sha256']
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == result['server_sha256']
    assert counterfactual['authenticated_response'] is True and counterfactual['result'] == 'helper_error'
    assert counterfactual['response']['error']['code'] == 'fixture_readonly_no_hardware'
    result['responses']['identical_wrong_client_allowed_by_fixture_policy'] = counterfactual
    result['counterfactual'] = {'identical_client_sha256': wrong_sha, 'identical_server_sha256': result['server_sha256'],
                               'changed_only': 'temporary fixture server client-admission file'}
    result['passed'] = True
    return result


def manifest():
    paths = [NATIVE / 'HelperClient.h', NATIVE / 'HelperClient.m',
             ROOT / 'native/ProductionIdentity.h', ROOT / 'native/ProductionIdentity.m', Path(__file__).resolve()]
    return {str(p.relative_to(ROOT.parent)): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}


def main():
    EVIDENCE.parent.mkdir(parents=True, exist_ok=True)
    (EVIDENCE.parent/'inspection.log').write_text('')
    evidence = {'passed': False, 'started_at_unix': time.time(), 'hardware_write_enabled': False,
                'production_control_ready': False, 'scope': 'Mocked callback/lifecycle regression, real Security denials, real embedded NSXPC fixed-error fixture; no authenticated production channel or hardware I/O',
                'source_sha256': manifest(), 'runs': {}, 'response_parser_boundary': 'Parsed schema/types/closed capability combinations use existing NSJSONSerialization semantics; no raw JSON duplicate-key detector added; host serde replies emit unique fields'}
    try:
        assert os.geteuid() != 0, 'Probe must run as ordinary user'
        with tempfile.TemporaryDirectory(prefix='macs-fan-production-client-', dir=EVIDENCE.parent) as temp:
            build = Path(temp)
            harness = build / 'ClientHarness.m'
            harness.write_text(HARNESS)
            driver = build / 'Driver.m'
            driver.write_text(DRIVER)
            flags = ['xcrun', 'clang', '-fobjc-arc', '-fobjc-arc-exceptions', '-fblocks', '-Wall', '-Wextra', '-Werror',
                     '-mmacosx-version-min=11.0', '-framework', 'Foundation', '-framework', 'Security', '-I', NATIVE]
            binary = build / 'ProductionClient'
            run(flags + [NATIVE / 'HelperClient.m', ROOT / 'native/ProductionIdentity.m', driver, '-o', binary])
            run(['codesign', '--force', '--sign', '-', '--identifier', 'local.macsfan.desktop', binary])
            production = json.loads(run([binary]))
            assert production['reason'] == 'production_team_unconfigured' and production['authenticated_response'] is False
            symbols = run(['nm', '-g', binary])
            assert 'MFHelperClientTestInstallBridge' not in symbols
            assert 'isPlatformVersionAtLeast' not in symbols and 'availability_version_check' not in symbols
            evidence['runs']['production_unconfigured_real_identity'] = production
            evidence['production_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
            for name, definitions in [('mock_current', []), ('mock_old_os', ['-DMF_HELPER_CLIENT_TEST_OS_MAJOR=12'])]:
                binary = build / name
                command = flags + ['-DMF_HELPER_CLIENT_TEST_HARNESS'] + definitions
                # The old-OS variant intentionally compiles out main's modern test cases.
                if definitions:
                    command += ['-Wno-unused-function', '-Wno-unused-variable']
                run(command + [NATIVE / 'HelperClient.m', harness, '-o', binary])
                run(['codesign', '--force', '--sign', '-', '--identifier', 'local.macsfan.desktop', binary])
                evidence['runs'][name] = json.loads(run([binary]))
            race = build / 'DeadlineRace.m'
            race.write_text('#import <Foundation/Foundation.h>\n#include <unistd.h>\n'
                'static long MFDelayedWait(dispatch_semaphore_t s, dispatch_time_t t) {\n'
                'long result = dispatch_semaphore_wait(s, t); if (result) usleep(80000); return result; }\n'
                '#define dispatch_semaphore_wait MFDelayedWait\n'
                '#include "' + str(NATIVE / 'HelperClient.m') + '"\n'
                '#undef dispatch_semaphore_wait\n' + HARNESS)
            binary = build / 'deadline_race'
            run(flags + ['-DMF_HELPER_CLIENT_TEST_HARNESS', '-DMF_HELPER_CLIENT_DEADLINE_RACE',
                         '-Wno-unused-function', '-Wno-unused-variable', race, '-o', binary])
            evidence['runs']['deterministic_deadline_race'] = json.loads(run([binary]))
            evidence['runs']['actual_nonprivileged_xpc'] = actual_xpc(build, flags)
            evidence['all_inputs_unchanged'] = manifest() == evidence['source_sha256']
            assert evidence['all_inputs_unchanged']
            evidence['passed'] = True
    finally:
        EVIDENCE.write_text(json.dumps(evidence, indent=2) + '\n')
        print('evidence=' + str(EVIDENCE), flush=True)


if __name__ == '__main__':
    main()

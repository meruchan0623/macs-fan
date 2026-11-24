#!/usr/bin/env python3
"""Nonroot SMAppService wrapper denial tests; every service call is a stub.

This script never calls a real register/unregister, invokes launchctl, installs
files, opens an SMC device, or writes hardware. Build artifacts are temporary.
"""
import json
import os
from pathlib import Path
import plistlib
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
NATIVE = ROOT.parent / "src-tauri/native"


def run(command):
    print("$ " + shlex.join(map(str, command)), flush=True)
    completed = subprocess.run(list(map(str, command)), text=True, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT)
    if completed.stdout:
        print(completed.stdout, end="", flush=True)
    completed.check_returncode()
    return completed.stdout


HARNESS = r'''
#import <Foundation/Foundation.h>
#import "ServiceManager.h"
static NSInteger state;
static unsigned statusCalls, registerCalls, unregisterCalls;
static BOOL throwStatus, reenterStatus, holdStatus;
static NSArray *reentrant;
static dispatch_semaphore_t entered, releaseStatus;
static NSDictionary *reply(char *json);
static void waitFor(dispatch_semaphore_t semaphore) {
    if (dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC))) abort();
}
static NSInteger statusStub(void) {
    statusCalls++;
    if (throwStatus) {
        throwStatus = NO;
        [NSException raise:@"MFInjectedServiceException" format:@"probe only"];
    }
    if (reenterStatus) {
        reenterStatus = NO;
        reentrant = @[reply(mf_service_status()), reply(mf_service_register()), reply(mf_service_unregister())];
    }
    if (holdStatus) {
        holdStatus = NO;
        dispatch_semaphore_signal(entered);
        waitFor(releaseStatus);
    }
    return state;
}
static BOOL registerStub(NSError **error) { (void)error; registerCalls++; return YES; }
static BOOL unregisterStub(NSError **error) { (void)error; unregisterCalls++; return YES; }
static NSDictionary *reply(char *json) {
    if (!json) abort();
    NSData *data = [NSData dataWithBytes:json length:strlen(json)];
    mf_service_free(json);
    id result = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
    if (![result isKindOfClass:NSDictionary.class]) abort();
    return result;
}
int main(void) {
    @autoreleasepool {
        MFServiceTestInstallBridge((MFServiceTestBridge){statusStub, registerStub, unregisterStub});
        NSMutableArray *observed = [NSMutableArray new];
        for (NSNumber *value in @[@0, @1, @2, @3, @99]) {
            state = value.integerValue;
            [observed addObject:reply(mf_service_status())];
        }
        NSDictionary *registered = reply(mf_service_register());
        NSDictionary *unregistered = reply(mf_service_unregister());
        NSMutableDictionary *boundary = [NSMutableDictionary new];
#if !defined(MF_SERVICE_TEST_OS_MAJOR) || MF_SERVICE_TEST_OS_MAJOR >= 13
        if (NSProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 13) {
            char *(*operations[])(void) = {mf_service_status, mf_service_register, mf_service_unregister};
            NSMutableArray *exceptions = [NSMutableArray new], *recoveries = [NSMutableArray new];
            for (unsigned index = 0; index < 3; index++) {
                throwStatus = YES;
                [exceptions addObject:reply(operations[index]())];
                [recoveries addObject:reply(mf_service_status())];
            }
            boundary[@"exceptions"] = exceptions;
            boundary[@"recoveries"] = recoveries;
            unsigned before = statusCalls;
            reenterStatus = YES;
            boundary[@"reentrant_completed"] = reply(mf_service_status());
            boundary[@"reentrant"] = reentrant;
            boundary[@"reentrant_status_delta"] = @(statusCalls - before);
            entered = dispatch_semaphore_create(0);
            releaseStatus = dispatch_semaphore_create(0);
            dispatch_semaphore_t done = dispatch_semaphore_create(0);
            __block NSDictionary *completed;
            before = statusCalls;
            holdStatus = YES;
            dispatch_async(dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), ^{
                @autoreleasepool {
                    completed = reply(mf_service_status());
                    dispatch_semaphore_signal(done);
                }
            });
            waitFor(entered);
            boundary[@"concurrent"] = @[reply(mf_service_status()), reply(mf_service_register()), reply(mf_service_unregister())];
            dispatch_semaphore_signal(releaseStatus);
            waitFor(done);
            boundary[@"concurrent_completed"] = completed;
            boundary[@"concurrent_status_delta"] = @(statusCalls - before);
            boundary[@"after_concurrent"] = reply(mf_service_status());
        }
#endif
        NSDictionary *result = @{ @"observed": observed, @"register": registered,
            @"unregister": unregistered, @"status_calls": @(statusCalls),
            @"register_calls": @(registerCalls), @"unregister_calls": @(unregisterCalls),
            @"boundary": boundary };
        NSData *data = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingSortedKeys error:NULL];
        puts([[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding].UTF8String);
    }
    return 0;
}
'''

# Test-only configured identity with a syntactically valid, fictitious Team.
# The real local executable must still fail Apple's actual signature check.
CONFIGURED_FIXTURE = r'''
#import "ProductionIdentity.h"
NSString *MFProductionClientBundleIdentifier(void) { return @"local.macsfan.desktop"; }
NSString *MFProductionHelperBundleIdentifier(void) { return @"local.macsfan.helper.control"; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSString *MFProductionConfiguredTeamIdentifier(void) { return @"ABCDE12345"; }
NSDictionary *MFProductionConfiguredRequirements(void) {
    return @{@"client": @"anchor apple generic and identifier \"local.macsfan.desktop\" and certificate leaf[subject.OU] = \"ABCDE12345\"",
             @"helper": @"anchor apple generic and identifier \"local.macsfan.helper.control\" and certificate leaf[subject.OU] = \"ABCDE12345\""};
}
'''


def validate(result, reason):
    assert result["register_calls"] == result["unregister_calls"] == 0, result
    expected = ["not_registered", "enabled", "requires_approval", "not_found", "unavailable"]
    assert [item["service_state"] for item in result["observed"]] == expected, result
    for index, item in enumerate(result["observed"]):
        assert item["operation"] == "status" and item["result"] == "observed", item
        assert item["registration_enabled"] is (index == 1), item
        assert item["requires_approval"] is (index == 2), item
    for operation in ["register", "unregister"]:
        assert result[operation]["result"] == "blocked", result[operation]
        assert result[operation]["operation"] == operation, result[operation]
    for item in [*result["observed"], result["register"], result["unregister"]]:
        assert item["schema_version"] == 1, item
        assert item["reason"] == reason, item
        assert item["production_identity_valid"] is False, item
        assert item["production_ready"] is False, item
        assert item["hardware_write_enabled"] is False, item
    boundary = result["boundary"]
    for group, expected_result, expected_reason in [
        ("exceptions", "error", "service_management_exception"),
        ("reentrant", "blocked", "service_operation_busy"),
        ("concurrent", "blocked", "service_operation_busy")]:
        assert len(boundary[group]) == 3, boundary
        for operation, item in zip(["status", "register", "unregister"], boundary[group]):
            assert item["schema_version"] == 1 and item["operation"] == operation, item
            assert item["result"] == expected_result and item["reason"] == expected_reason, item
            assert item["hardware_write_enabled"] is False and item["production_ready"] is False, item
    assert boundary["reentrant_status_delta"] == boundary["concurrent_status_delta"] == 1, boundary
    for item in [*boundary["recoveries"], boundary["reentrant_completed"],
                 boundary["concurrent_completed"], boundary["after_concurrent"]]:
        assert item["result"] == "observed" and item["reason"] == reason, item


def main():
    assert os.geteuid() != 0, "Run as an ordinary user; this probe requires no privilege."
    expected = {"Label": "local.macsfan.helper.control",
                "BundleProgram": "Contents/Library/LaunchServices/MacsFanHelper",
                "ProgramArguments": ["MacsFanHelper", "--serve-production"],
                "MachServices": {"local.macsfan.helper.control": True}}
    assert plistlib.loads((ROOT / "packaging/local.macsfan.helper.control.plist").read_bytes()) == expected
    flags = ["clang", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra", "-Werror",
             "-mmacosx-version-min=11.0", "-I", NATIVE, "-I", ROOT / "native"]
    # Check the exact production compilation separately from the injected build.
    run([*flags, "-fsyntax-only", NATIVE / "ServiceManager.m"])
    with tempfile.TemporaryDirectory(prefix="macs-fan-service-probe-") as temporary:
        build = Path(temporary)
        harness = build / "main.m"
        fixture = build / "ConfiguredFixture.m"
        harness.write_text(HARNESS)
        fixture.write_text(CONFIGURED_FIXTURE)
        results = {}
        for name, identity, reason in [
            ("unconfigured", ROOT / "native/ProductionIdentity.m", "production_team_unconfigured"),
            ("adhoc_rejected", fixture, "production_client_signature_requirement_failed")]:
            binary = build / name
            run([*flags, "-DMF_SERVICE_TEST_HARNESS", harness,
                 NATIVE / "ServiceManager.m", identity,
                 "-framework", "Foundation", "-framework", "Security", "-o", binary])
            output = run([binary])
            result = json.loads(output)
            validate(result, reason)
            results[name] = {"reason": reason, "register_calls": result["register_calls"],
                             "unregister_calls": result["unregister_calls"],
                             "states_checked": len(result["observed"]),
                             "exception_recovery": True, "concurrency_and_reentry": True}
        unsupported = build / "unsupported"
        run([*flags, "-DMF_SERVICE_TEST_HARNESS", "-DMF_SERVICE_TEST_OS_MAJOR=12", harness,
             NATIVE / "ServiceManager.m", ROOT / "native/ProductionIdentity.m",
             "-framework", "Foundation", "-framework", "Security", "-o", unsupported])
        result = json.loads(run([unsupported]))
        assert result["status_calls"] == result["register_calls"] == result["unregister_calls"] == 0
        for item in [*result["observed"], result["register"], result["unregister"]]:
            assert item["result"] == "unsupported" and item["reason"] == "macos_13_required", item
            assert item["platform_supported"] is False and item["hardware_write_enabled"] is False, item
        results["macos_12"] = {"all_service_calls": 0, "result": "unsupported"}
        # The production object must have no hard SMAppService class dependency.
        obj = build / "ServiceManager.o"
        run([*flags, "-c", NATIVE / "ServiceManager.m", "-o", obj])
        symbols = run(["nm", "-u", obj])
        assert "_OBJC_CLASS_$_SMAppService" not in symbols
    print(json.dumps({"passed": True, "real_service_mutations": 0,
                      "hardware_write_enabled": False, "checks": results,
                      "limits": ["No Developer ID identity or genuine registered-service execution",
                                 "No root, installation, approval, hardware or recovery test"]}, sort_keys=True))


if __name__ == "__main__":
    main()

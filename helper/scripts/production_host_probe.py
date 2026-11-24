#!/usr/bin/env python3
"""Nonprivileged production-host gates; no launchd registration or SMC access.

Build ordinary Helper.m with a bridge that exits before any backend can open.
The simulated-UID/identity binaries are temporary unit fixtures, never products.
Real Apple/Team/root listener startup is explicitly outside this verification.
"""
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BUILD = Path(tempfile.mkdtemp(prefix="macs-fan-production-host-"))
CHECKS = []


def run(args, expected=0, env=None):
    print("+", " ".join(map(str, args)), flush=True)
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True, env=env, timeout=30)
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", flush=True)
    if result.returncode != expected:
        raise RuntimeError(f"exit={result.returncode}, expected={expected}")
    return result


def check(name, condition):
    if not condition:
        raise AssertionError(name)
    CHECKS.append(name)
    print("PASS", name, flush=True)


def compile_host(name, identity, root_uid=False):
    binary = BUILD / name
    args = ["xcrun", "clang", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra", "-Werror",
            "-mmacosx-version-min=13.0", "-I", ROOT / "native", "-framework", "Foundation",
            "-framework", "Security", "-framework", "IOKit", "-framework", "AppKit"]
    if root_uid:
        args += ["-Dgeteuid=MFProbeEffectiveUID", BUILD / "RootUID.m"]
    run(args + [ROOT / "native/Helper.m", identity, BUILD / "BridgeStub.m", "-o", binary])
    run(["codesign", "--force", "--sign", "-", "--identifier", "local.macsfan.helper.control", binary])
    return binary


def main():
    if os.geteuid() == 0:
        raise RuntimeError("probe refuses real root")
    (BUILD / "RootUID.m").write_text('#include <unistd.h>\nuid_t MFProbeEffectiveUID(void) { return 0; }\n')
    (BUILD / "BridgeStub.m").write_text('''#include "Bridge.h"
#include <stdio.h>
#include <stdlib.h>
__attribute__((noreturn)) static void stop(void) { fputs("unexpected_bridge_call\\n", stderr); exit(90); }
void *mf_helper_create(uint8_t fake) { (void)fake; stop(); }
void *mf_helper_create_smc_readonly(void) { fputs("stub_readonly_constructor_reached_no_hardware\\n", stderr); exit(91); }
char *mf_helper_request(void *h, const uint8_t o[32], const uint8_t *j, size_t n) { (void)h; (void)o; (void)j; (void)n; stop(); }
char *mf_helper_disconnect(void *h, const uint8_t o[32]) { (void)h; (void)o; stop(); }
char *mf_helper_tick(void *h) { (void)h; stop(); }
char *mf_helper_shutdown(void *h) { (void)h; stop(); }
char *mf_helper_lifecycle_begin(void *h, uint8_t e) { (void)h; (void)e; stop(); }
char *mf_helper_lifecycle_poll(void *h) { (void)h; stop(); }
void mf_helper_free(char *r) { (void)r; stop(); }
''')
    (BUILD / "IdentityStub.m").write_text('''#import "ProductionIdentity.h"
NSString *MFProductionClientBundleIdentifier(void) { return @"local.macsfan.desktop"; }
NSString *MFProductionHelperBundleIdentifier(void) { return @"local.macsfan.helper.control"; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSString *MFProductionConfiguredTeamIdentifier(void) { return @"ABCDE12345"; }
NSDictionary *MFProductionConfiguredRequirements(void) { return @{@"client":@"stub-client", @"helper":@"stub-helper"}; }
NSDictionary *MFProductionIdentityCheck(void) { return @{@"production_identity_valid":@YES,
    @"production_channel_available":@NO, @"production_listener_resumed":@NO,
    @"production_control_ready":@NO, @"identity_check_only":@YES, @"hardware_write_enabled":@NO}; }
''')
    ordinary = compile_host("Ordinary", ROOT / "native/ProductionIdentity.m")
    for flag in ["--production-check", "--serve-production"]:
        response = json.loads(run([ordinary, flag], 78).stdout)
        check(flag[2:] + "_fails_before_any_bridge_call", response["reason"] == "production_team_unconfigured"
              and not response["production_identity_valid"] and not response["production_listener_resumed"]
              and not response["production_control_ready"] and not response["hardware_write_enabled"])
    spoofed = dict(os.environ, MF_PRODUCTION_TEAM_ID="ABCDE12345", MFBackend="fake", MFDryRun="true",
                   MFClientRequirement="anchor trusted", MF_MACH_SERVICE="attacker.control")
    response = json.loads(run([ordinary, "--serve-production"], 78, spoofed).stdout)
    check("environment_cannot_supply_identity_or_backend", response["reason"] == "production_team_unconfigured"
          and response["mach_service_name"] == "local.macsfan.helper.control")
    # Ordinary signed Info.plist data likewise cannot select production policy.
    app = BUILD / "Spoof.app"
    executable = app / "Contents/MacOS/Ordinary"
    executable.parent.mkdir(parents=True)
    executable.write_bytes(ordinary.read_bytes())
    executable.chmod(0o755)
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier":"local.macsfan.helper.control",
        "CFBundleExecutable":"Ordinary", "CFBundlePackageType":"APPL", "MFProductionTeamID":"ABCDE12345",
        "MFDryRun":True, "MFBackend":"fake", "MFClientRequirement":"anchor trusted"}))
    run(["codesign", "--force", "--sign", "-", app])
    response = json.loads(run([executable, "--serve-production"], 78).stdout)
    check("signed_plist_cannot_supply_production_team", response["reason"] == "production_team_unconfigured")
    symbols = run(["nm", "-gU", ordinary]).stdout
    check("ordinary_build_exports_no_team_template_or_test_probe", "_MFProductionTemplate" not in symbols
          and "_MFProductionIdentityProbe" not in symbols)
    root_stub = compile_host("SimulatedRoot", ROOT / "native/ProductionIdentity.m", True)
    response = json.loads(run([root_stub, "--serve-production"], 78).stdout)
    check("simulated_root_still_requires_fixed_identity", response["reason"] == "production_team_unconfigured")
    for args in [[], ["client", "untrusted", "untrusted"], ["--validate-config"], ["--identity-probe"],
                 ["--lifecycle-probe", "sleep_wake"], ["--serve-production", "--backend=fake"]]:
        output = run([root_stub] + args, 77)
        check("simulated_root_rejects_debug_" + str(len(CHECKS)), "dry_run_refuses_root" in output.stderr)
    identity_stub = compile_host("SimulatedIdentity", BUILD / "IdentityStub.m")
    response = json.loads(run([identity_stub, "--production-check"]).stdout)
    check("identity_success_does_not_start_listener", response["identity_check_only"]
          and not response["production_listener_resumed"] and not response["production_control_ready"])
    response = json.loads(run([identity_stub, "--serve-production"], 77).stdout)
    check("identity_success_nonroot_stops_before_bridge", response["reason"] == "production_requires_root")
    gated_stub = compile_host("SimulatedIdentityAndRoot", BUILD / "IdentityStub.m", True)
    reached = run([gated_stub, "--serve-production"], 91)
    check("simulated_gates_choose_only_readonly_constructor", "stub_readonly_constructor_reached_no_hardware" in reached.stderr)
    evidence = {"checks": CHECKS, "hardware_write_enabled": False, "real_root_used": False,
                "service_registered": False, "real_smc_access": False, "production_listener_positive_path_verified": False,
                "scope": "negative paths and isolated identity/UID/constructor fixtures", "build_directory": str(BUILD)}
    path = BUILD / "production-host-verification.json"
    path.write_text(json.dumps(evidence, indent=2) + "\n")
    print("VERIFIED", len(CHECKS), "production-host checks; evidence=", path, flush=True)


if __name__ == "__main__":
    main()

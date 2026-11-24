#!/usr/bin/env python3
"""Compile-gated native lifecycle tests. Never sleeps the user's computer."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BUILD = Path(tempfile.mkdtemp(prefix="macs-fan-helper-lifecycle-"))
TARGET = Path("/tmp/macs-fan-helper-lifecycle-target")
CHECKS = []


def run(args, expected=0, capture=False, env=None):
    print("+", " ".join(map(str, args)), flush=True)
    result = subprocess.run(list(map(str, args)), text=True, capture_output=capture, env=env)
    if capture:
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


def main():
    if os.geteuid() == 0:
        raise RuntimeError("test refuses root")
    env = dict(os.environ, CARGO_TARGET_DIR=str(TARGET), CARGO_INCREMENTAL="0", MACOSX_DEPLOYMENT_TARGET="13.0")
    run(["cargo", "build", "--offline", "--features", "test-harness", "--manifest-path", ROOT / "Cargo.toml"], env=env)
    binary = BUILD / "LifecycleProbe"
    run(["xcrun", "clang", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra", "-Werror", "-DMF_TEST_HARNESS=1",
         "-mmacosx-version-min=13.0", "-framework", "Foundation", "-framework", "Security", "-framework", "IOKit",
         "-framework", "AppKit", ROOT / "native/Helper.m", ROOT / "native/ProductionIdentity.m",
         TARGET / "debug/libmacs_fan_helper_bridge.a", "-o", binary])
    # Match the fixed helper identifier, so ad-hoc rejection is not merely an
    # identifier mismatch. The Apple certificate/Team constraints still fail.
    run(["codesign", "--force", "--sign", "-", "--identifier", "local.macsfan.helper.control", binary])
    identity = json.loads(run([binary, "--identity-probe"], capture=True).stdout)
    check("fixed_identity_templates_compile", identity["template_compiles"] is True and
          'identifier "local.macsfan.desktop"' in identity["client_requirement"] and
          'identifier "local.macsfan.helper.control"' in identity["helper_requirement"] and
          'anchor apple generic' in identity["helper_requirement"] and
          'certificate leaf[subject.OU] = "ABCDE12345"' in identity["helper_requirement"])
    check("invalid_team_inputs_rejected", identity["invalid_team_inputs_rejected"] is True)
    check("adhoc_code_cannot_satisfy_production_requirement", identity["adhoc_self_rejected"] is True)
    for flag in ["--production-check", "--serve-production"]:
        result = json.loads(run([binary, flag], expected=78, capture=True).stdout)
        check(flag[2:] + "_refuses_unconfigured_team", result["production_identity_valid"] is False and
              result["production_channel_available"] is False and result["hardware_write_enabled"] is False and
              result["reason"] == "production_team_unconfigured")
    spoofed_env = dict(os.environ, MF_PRODUCTION_TEAM_ID="ABCDE12345", MFBackend="smc", MFDryRun="false")
    spoofed = json.loads(run([binary, "--serve-production"], expected=78, capture=True, env=spoofed_env).stdout)
    check("environment_cannot_enable_production", spoofed["reason"] == "production_team_unconfigured" and
          spoofed["production_channel_available"] is False and spoofed["hardware_write_enabled"] is False)
    for scenario in ["sleep_wake", "sleep_failure_wake", "shutdown", "shutdown_failure", "timeout"]:
        response = json.loads(run([binary, "--lifecycle-probe", scenario], capture=True).stdout)
        check("native_" + scenario, response["ok"] is True)
    for scenario, code in [("exit_success", 0), ("exit_failure", 74)]:
        result = run([binary, "--lifecycle-probe", scenario], expected=code, capture=True)
        events = [json.loads(line) for line in result.stderr.splitlines() if line.startswith('{')]
        report = events[-1]
        check("sigterm_" + scenario, report["report"]["recovery_confirmed"] is (code == 0)
              and report["report"]["wait_ms"] <= 1200)
    evidence = {"checks":CHECKS,"test_binary":str(binary),"test_compile_flag":"MF_TEST_HARNESS=1",
                "rust_test_feature":"test-harness","hardware_write_enabled":False,"real_sleep_triggered":False}
    path = BUILD / "lifecycle-verification.json"
    path.write_text(json.dumps(evidence, indent=2) + "\n")
    print("VERIFIED", len(CHECKS), "lifecycle/identity checks; evidence=", path, flush=True)


if __name__ == "__main__":
    main()

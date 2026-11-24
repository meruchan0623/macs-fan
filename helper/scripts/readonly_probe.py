#!/usr/bin/env python3
"""Real nonroot, pinned NSXPC → AppleSMC reads; never authorizes hardware writes.

Separate from probe.py's pure fake regression. Uses its actual native build,
bundle, signing and stdin client helpers without changing that regression.
"""
import hashlib
import json
import math
import os
from pathlib import Path
import plistlib
import shutil
import tempfile
import time

# Set before importing the shared helper so this probe never takes its build lock.
os.environ["MF_HELPER_TARGET"] = tempfile.mkdtemp(prefix="macs-fan-readonly-target-")
import probe as shared

EVIDENCE = shared.ROOT / "docs/readonly-verification.json"


def source_preflight():
    source = (shared.ROOT / "src/lib.rs").read_text()
    adapter = (shared.ROOT.parent / "smc-adapter/src/lib.rs").read_text()
    # Deliberately refuse to run an acquire if a later implementation replaces
    # the permanently read-only constructor with production profile admission.
    readonly = source[source.index("fn smc_readonly()"):source.index("fn smc_control()")]
    native = (shared.ROOT / "native/Helper.m").read_text()
    shared.check("readonly_probe_and_production_use_readonly_constructor",
                 "SmcAdapter::open_readonly()" in readonly
                 and "SmcAdapter::open()" not in readonly
                 and "mf_helper_create_smc_control(" not in native
                 and "mf_helper_create_smc_readonly()" in native)
    start = adapter.index("pub fn open_readonly()")
    end = adapter.index("pub fn last_error", start)
    shared.check("readonly_constructor_permanently_clears_profile",
                 "adapter.inner.profile = None;" in adapter[start:end])
    shared.check("adapter_profile_is_private", "pub profile:" not in adapter)
    return {str(path.relative_to(shared.ROOT.parent)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in [shared.ROOT / "src/lib.rs", shared.ROOT / "native/Helper.m",
                         shared.ROOT.parent / "smc-adapter/src/lib.rs"]}



def build_input_manifest():
    """Record project inputs; this does not claim SDK/toolchain reproducibility."""
    paths = set()
    for crate in ["helper", "control-core", "control-runtime", "smc-adapter"]:
        for path in (shared.ROOT.parent / crate).rglob("*"):
            relative = path.relative_to(shared.ROOT.parent)
            if any(part in {"target", "docs", "__pycache__"} for part in relative.parts):
                continue
            if path.is_file() and (path.suffix in {".rs", ".m", ".h", ".inc"}
                                   or path.name in {"Cargo.toml", "Cargo.lock"}):
                paths.add(path)
    paths.update([shared.ROOT / "scripts/probe.py", Path(__file__).resolve()])
    return {str(path.relative_to(shared.ROOT.parent)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(paths)}


def binary_record(path, digest):
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "cdhash": digest}


def markers(reply):
    return (reply.get("dry_run") is True and reply.get("backend") == "smc_readonly"
            and reply.get("hardware_reads") is True
            and reply.get("hardware_write_enabled") is False)


def record(evidence, name, reply):
    # No lease credential may enter logs, even when an unexpected grant fails.
    def scrub(value):
        if isinstance(value, dict):
            return {key: "<redacted>" if key == "token" else scrub(item) for key, item in value.items()}
        if isinstance(value, list):
            return [scrub(item) for item in value]
        return value
    evidence["responses"][name] = scrub(reply)
    print(name + "=" + json.dumps(scrub(reply), sort_keys=True), flush=True)


def main():
    evidence = {"passed": False, "started_at_unix": time.time(), "checks": shared.CHECKS,
                "temporary_build": str(shared.BUILD), "cargo_target": str(shared.TARGET),
                "dry_run": True, "hardware_write_enabled": False, "production_ready": False,
                "production_identity": False, "responses": {},
                "limits": ["No verified hardware write profile or production identity",
                           "Synchronous IOKit reads have no hard cancellation deadline",
                           "No independently verified hardware recovery mechanism"]}
    try:
        shared.check("nonroot_probe", os.geteuid() != 0)
        evidence["build_input_sha256"] = build_input_manifest()
        evidence["build_record_scope"] = "Project source/Cargo/probe input and signed output records for this run; not a reproducible-build proof"
        evidence["source_sha256"] = source_preflight()
        env = dict(os.environ, CARGO_TARGET_DIR=str(shared.TARGET), CARGO_INCREMENTAL="0",
                   MACOSX_DEPLOYMENT_TARGET="13.0")
        shared.run(["cargo", "build", "--offline", "--manifest-path", shared.ROOT / "Cargo.toml"], env=env)
        raw = shared.BUILD / "ReadonlyProbeClient"
        shared.run(["xcrun", "clang", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra", "-Werror",
                    "-mmacosx-version-min=13.0", "-framework", "Foundation", "-framework", "Security",
                    "-framework", "IOKit", "-framework", "AppKit", shared.ROOT / "native/Helper.m",
                    shared.ROOT / "native/ProductionIdentity.m",
                    shared.TARGET / "debug/libmacs_fan_helper_bridge.a", "-o", raw])
        shared.check("all_project_inputs_unchanged_after_native_compile",
                     build_input_manifest() == evidence["build_input_sha256"])
        shared.run(["codesign", "--force", "--sign", "-", "--identifier", "io.macsfan.probe.client", raw])
        digest = shared.cdhash(raw)
        app, client, service, helper, helper_pin = shared.make_bundle("Readonly", raw, digest, "smc_readonly")
        evidence.update(temporary_bundle=str(app), client_cdhash=digest, helper_requirement=helper_pin)
        evidence["signed_binaries"] = {
            "client": binary_record(client, digest),
            "helper": binary_record(helper, helper_pin.removeprefix('cdhash H"').removesuffix('"')),
        }
        driver = shared.Driver(client, helper_pin)
        try:
            driver.command("connect")
            reply = driver.request({"op": "enumerate"})
            record(evidence, "enumerate", reply)
            shared.check("real_smc_enumeration_with_readonly_markers", markers(reply) and reply.get("ok") is True)
            shared.check("helper_is_a_distinct_process", reply["helper_process_id"] not in [driver.process.pid, os.getpid()])
            shared.check("preinspection_counter_and_timestamp_are_unobserved",
                         reply["hardware_write_attempts"] is None
                         and reply["hardware_inspection_observed_at_ms"] is None
                         and reply["hardware_write_attempts_scope"] == "latest_hardware_inspection")
            fans = reply["result"]["fans"]
            shared.check("bounded_nonempty_real_fan_inventory", isinstance(fans, list) and 0 < len(fans) <= 16
                         and len(set(fans)) == len(fans) and all(type(fan) is int and 0 <= fan < 16 for fan in fans))
            inspected = driver.request({"op": "inspect_hardware"})
            record(evidence, "inspection_before_denial", inspected)
            shared.check("inspection_has_real_readonly_report", markers(inspected) and inspected.get("ok") is True
                         and inspected["result"]["kind"] == "hardware_inspection")
            shared.check("inspection_counter_and_timestamp_become_observed",
                         inspected["hardware_write_attempts"] == 0
                         and inspected["hardware_write_attempts_scope"] == "latest_hardware_inspection"
                         and type(inspected["hardware_inspection_observed_at_ms"]) is int
                         and inspected["hardware_inspection_observed_at_ms"] >= 0)
            report = inspected["result"]["inspection"]
            shared.check("inspection_inventory_matches_enumeration", report["inventoryError"] is None
                         and [fan["fan"] for fan in report["fans"]] == fans)
            shared.check("inspection_has_no_profile_or_hardware_writes", report["hardwareProfileVerified"] is False
                         and report["hardwareWriteAttempts"] == 0 and bool(report["writeRefusal"]))
            for observed in report["fans"]:
                fan = observed["fan"]
                caps = observed["capabilities"]
                shared.check(f"fan_{fan}_real_bounds_are_unverified", caps is not None and caps["validated"] is False
                             and caps["stagesTargetWhileAutomatic"] is False and caps["modeControl"] == "Unknown"
                             and all(type(caps[key]) in [float, int] and math.isfinite(caps[key]) for key in ["minRpm", "maxRpm", "stepRpm"])
                             and 0 <= caps["minRpm"] <= caps["maxRpm"] <= 30000 and caps["stepRpm"] > 0)
                keys = {key["key"]: key for key in observed["keys"]}
                shared.check(f"fan_{fan}_real_metadata_and_mode_are_exposed", observed["mode"] in ["automatic", "manual", "unknown"]
                             and all(keys[f"F{fan}{suffix}"]["metadata"] is not None for suffix in ["Mn", "Mx", "Tg"]))
                caps_reply = driver.request({"op": "capabilities", "fan": fan})
                record(evidence, f"capabilities_denied_{fan}", caps_reply)
                shared.check(f"fan_{fan}_core_capabilities_keep_verification_gate", markers(caps_reply)
                             and caps_reply.get("ok") is False and caps_reply.get("error", {}).get("code") == "unverified_capabilities")
                denied = driver.request({"op": "acquire", "fan": fan, "rpm": caps["minRpm"], "ttl_ms": 100, "seq": 1})
                record(evidence, f"acquire_denied_{fan}", denied)
                shared.check(f"fan_{fan}_unverified_acquire_is_denied", markers(denied) and denied.get("ok") is False
                             and denied.get("error", {}).get("code") == "unverified_capabilities" and "result" not in denied)
            for fan in fans:
                denied = driver.request({"op":"set_target","fan":fan,"token":"0" * 64,"rpm":3000,"ttl_ms":100,"seq":1})
                record(evidence, f"set_target_denied_{fan}", denied)
                shared.check(f"fan_{fan}_target_update_without_owned_lease_is_denied",
                             markers(denied) and denied.get("ok") is False
                             and denied.get("error", {}).get("code") == "no_active_lease")
            for fan in fans:
                denied = driver.request({"op":"start_plan","entries":[{"fan":fan,"strategy":{"type":"fixed","rpm":3000}}],"ttl_ms":5000,"seq":1})
                record(evidence, f"plan_denied_{fan}", denied)
                shared.check(f"fan_{fan}_plan_cannot_bypass_readonly_policy", markers(denied) and denied.get("ok") is False
                             and denied.get("error", {}).get("code") == "readonly_policy"
                             and denied.get("control") == {"version":1,"execution":"readonly","admission":"readonly_policy"}
                             and denied.get("plan", {}).get("authorized") is False)
            after = driver.request({"op": "inspect_hardware"})
            record(evidence, "inspection_after_denial", after)
            shared.check("denial_creates_no_lease_or_hardware_write", markers(after) and after.get("ok") is True
                         and after.get("statuses") == []
                         and after["result"]["inspection"]["hardwareWriteAttempts"] == 0
                         and after["result"]["inspection"]["hardwareProfileVerified"] is False)
            shared.check("second_inspection_has_current_counter_and_monotonic_timestamp",
                         after["hardware_write_attempts"] == 0
                         and after["hardware_write_attempts_scope"] == "latest_hardware_inspection"
                         and type(after["hardware_inspection_observed_at_ms"]) is int
                         and after["hardware_inspection_observed_at_ms"] >= inspected["hardware_inspection_observed_at_ms"])
            malformed = driver.request({"op": "inspect_hardware", "fan": fans[0]})
            record(evidence, "inspection_extra_field_rejected", malformed)
            shared.check("inspection_schema_rejects_extra_fields", malformed.get("ok") is False
                         and malformed.get("error", {}).get("code") == "invalid_schema")
        finally:
            driver.close()
        wrong = shared.Driver(client, shared.pin("0" * 40))
        try:
            wrong.command("connect")
            denied = wrong.request({"op": "enumerate"})
            record(evidence, "wrong_helper_identity", denied)
            shared.check("client_rejects_wrong_real_helper_pin", denied.get("transport_error") == "xpc_rejected"
                         and denied.get("error_code") == 4102 and denied.get("signing_requirement_failure") is True)
        finally:
            wrong.close()
        wrong_raw = shared.BUILD / "WrongReadonlyClient"
        shutil.copy2(raw, wrong_raw)
        shared.run(["codesign", "--force", "--sign", "-", "--options", "runtime", "--identifier", "io.macsfan.probe.client", wrong_raw])
        wrong_digest = shared.cdhash(wrong_raw)
        shared.check("wrong_client_has_different_real_cdhash", wrong_digest != digest)
        wrong_app = shared.BUILD / "WrongReadonly.app"
        shutil.copytree(app, wrong_app)
        wrong_client = wrong_app / "Contents/MacOS/Probe"
        shutil.copy2(wrong_raw, wrong_client)
        wrong = shared.Driver(wrong_client, helper_pin)
        try:
            wrong.command("connect")
            denied = wrong.request({"op": "enumerate"})
            record(evidence, "wrong_client_identity", denied)
            shared.check("helper_rejects_wrong_real_client_pin", denied.get("transport_error") == "xpc_rejected" and "result" not in denied)
        finally:
            wrong.close()
        # The same runtime-signed executable must work when its actual CDHash
        # is allowed. Otherwise generic XPC 4097 could mask launch incompatibility.
        _, matched_client, _, matched_helper, matched_pin = shared.make_bundle(
            "MatchedRuntimeReadonly", raw, wrong_digest, "smc_readonly")
        shutil.copy2(wrong_raw, matched_client)
        evidence["signed_binaries"].update({
            "runtime_counterfactual_client": binary_record(matched_client, wrong_digest),
            "runtime_counterfactual_helper": binary_record(matched_helper, matched_pin.removeprefix('cdhash H"').removesuffix('"')),
        })
        shared.check("runtime_counterfactual_uses_identical_rejected_client_bytes",
                     matched_client.read_bytes() == wrong_client.read_bytes())
        matched = shared.Driver(matched_client, matched_pin)
        try:
            matched.command("connect")
            accepted = matched.request({"op": "enumerate"})
            record(evidence, "matched_runtime_client_enumerate", accepted)
            shared.check("same_runtime_client_enumerates_when_real_pin_is_allowed",
                         markers(accepted) and accepted.get("ok") is True
                         and accepted["result"]["fans"] == fans)
            inspected = matched.request({"op": "inspect_hardware"})
            record(evidence, "matched_runtime_client_inspection", inspected)
            shared.check("same_runtime_client_inspects_real_hardware_without_writes",
                         markers(inspected) and inspected.get("ok") is True
                         and inspected["result"]["kind"] == "hardware_inspection"
                         and inspected["result"]["inspection"]["inventoryError"] is None
                         and [fan["fan"] for fan in inspected["result"]["inspection"]["fans"]] == fans
                         and inspected["result"]["inspection"]["hardwareWriteAttempts"] == 0
                         and inspected["result"]["inspection"]["hardwareProfileVerified"] is False
                         and inspected["hardware_write_attempts"] == 0
                         and inspected["statuses"] == [])
        finally:
            matched.close()
        invalid = shared.BUILD / "StillRejectsSmc.xpc"
        shutil.copytree(service, invalid)
        info_path = invalid / "Contents/Info.plist"
        info = plistlib.loads(info_path.read_bytes())
        info["MFBackend"] = "smc"
        info_path.write_bytes(plistlib.dumps(info))
        shared.run(["codesign", "--force", "--sign", "-", invalid])
        shared.run([invalid / "Contents/MacOS/MacsFanHelper", "--validate-config"], expected=78)
        shared.check("smc_backend_still_rejected_after_valid_resigning", True)
        evidence["build_input_sha256_after_verification"] = build_input_manifest()
        shared.check("all_project_inputs_unchanged_after_verification",
                     evidence["build_input_sha256_after_verification"] == evidence["build_input_sha256"])
        evidence["passed"] = True
    except Exception as error:
        evidence["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        evidence["finished_at_unix"] = time.time()
        EVIDENCE.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
        print("evidence=" + str(EVIDENCE), flush=True)
    print(f"VERIFIED {len(shared.CHECKS)} read-only integration checks", flush=True)


if __name__ == "__main__":
    main()

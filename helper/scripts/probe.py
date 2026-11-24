#!/usr/bin/env python3
"""Build and exercise real, nonprivileged embedded NSXPC in a temporary app.

No launchctl, installation, certificate/keychain mutation, or SMC access.
Ad-hoc CDHash pins are test-only and are not production authentication policy.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import select
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BUILD = Path(tempfile.mkdtemp(prefix="macs-fan-xpc-verify-"))
TARGET = Path(os.environ.get("MF_HELPER_TARGET", "/tmp/macs-fan-helper-verify"))
SERVICE_ID = "io.macsfan.helper.dryrun"
CHECKS = []


def run(args, *, expected=0, capture=False, env=None):
    print("+", " ".join(map(str, args)), flush=True)
    result = subprocess.run(list(map(str, args)), text=True, capture_output=capture, env=env)
    if capture:
        # Only build/signing/config output is printed. IPC lease tokens are
        # deliberately kept within the test driver and never logged.
        if result.stdout:
            print(result.stdout, end="", flush=True)
        if result.stderr:
            print(result.stderr, end="", flush=True)
    if result.returncode != expected:
        raise RuntimeError(f"command exited {result.returncode}, expected {expected}")
    return result


def check(name, condition):
    if not condition:
        raise AssertionError(name)
    CHECKS.append(name)
    print("PASS", name, flush=True)


def cdhash(path):
    result = run(["codesign", "-d", "--verbose=4", path], capture=True)
    match = re.search(r"^CDHash=([0-9a-f]{40})$", result.stderr, re.M)
    if not match:
        raise RuntimeError("missing CDHash")
    return match.group(1)


def pin(digest):
    return 'cdhash H"' + digest + '"'


def make_bundle(name, binary, client_hash, backend="fake"):
    app = BUILD / f"{name}.app"
    client = app / "Contents/MacOS/Probe"
    service = app / f"Contents/XPCServices/{SERVICE_ID}.xpc"
    helper = service / "Contents/MacOS/MacsFanHelper"
    client.parent.mkdir(parents=True)
    helper.parent.mkdir(parents=True)
    shutil.copy2(binary, client)
    shutil.copy2(binary, helper)
    app_info = {"CFBundleIdentifier": "io.macsfan.probe.client", "CFBundleExecutable": "Probe",
                "CFBundlePackageType": "APPL", "CFBundleVersion": "1"}
    service_info = {"CFBundleIdentifier": SERVICE_ID, "CFBundleExecutable": "MacsFanHelper",
                    "CFBundlePackageType": "XPC!", "CFBundleVersion": "1",
                    "MFDryRun": True, "MFBackend": backend, "MFClientRequirement": pin(client_hash),
                    "LSMinimumSystemVersion": "13.0", "XPCService": {"ServiceType": "Application"}}
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(app_info))
    (service / "Contents/Info.plist").write_bytes(plistlib.dumps(service_info))
    run(["codesign", "--force", "--sign", "-", service])
    run([helper, "--validate-config"])
    return app, client, service, helper, pin(cdhash(helper))


class Driver:
    def __init__(self, executable, requirement):
        print("+", executable, "client", SERVICE_ID, requirement, flush=True)
        self.process = subprocess.Popen([str(executable), "client", SERVICE_ID, requirement],
                                        text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE)

    def command(self, action, connection="a", payload=None):
        value = {"action": action, "connection": connection}
        if payload is not None:
            value["payload_base64"] = base64.b64encode(payload).decode("ascii")
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()
        readable, _, _ = select.select([self.process.stdout], [], [], 8)
        if not readable:
            raise RuntimeError("driver response timeout")
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(f"driver exited {self.process.poll()}")
        return json.loads(line)

    def request(self, command=None, *, raw=None, connection="a"):
        payload = raw if raw is not None else json.dumps({"version": 1, "command": command}).encode()
        return self.command("request", connection, payload)

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.write('{"action":"exit"}\n')
            self.process.stdin.flush()
            self.process.wait(timeout=8)
        if self.process.returncode != 0:
            raise RuntimeError("driver exit failure")


def main():
    if os.geteuid() == 0:
        raise RuntimeError("dry-run verification must not run as root")
    env = dict(os.environ, CARGO_TARGET_DIR=str(TARGET), CARGO_INCREMENTAL="0", MACOSX_DEPLOYMENT_TARGET="13.0")
    run(["cargo", "build", "--offline", "--manifest-path", ROOT / "Cargo.toml"], env=env)
    raw = BUILD / "ProbeClient"
    run(["xcrun", "clang", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra", "-Werror",
         "-mmacosx-version-min=13.0", "-framework", "Foundation", "-framework", "Security",
         "-framework", "IOKit", "-framework", "AppKit", ROOT / "native/Helper.m",
         ROOT / "native/ProductionIdentity.m", TARGET / "debug/libmacs_fan_helper_bridge.a", "-o", raw])
    run(["codesign", "--force", "--sign", "-", "--identifier", "io.macsfan.probe.client", raw])
    # Native test entrypoints must be absent from this ordinary build.
    run([raw, "--lifecycle-probe", "sleep_wake"], expected=78)
    client_hash = cdhash(raw)
    app, client, service, helper, helper_pin = make_bundle("Accepted", raw, client_hash)
    driver = Driver(client, helper_pin)
    try:
        driver.command("connect", "a")
        response = driver.request({"op": "enumerate"})
        check("real_xpc_accepts_pinned_client_and_helper", response.get("ok") is True and response["result"]["fans"] == [0])
        check("helper_runs_in_a_distinct_real_process", response["helper_process_id"] != driver.process.pid and response["helper_process_id"] != os.getpid())
        check("fake_is_explicit_and_hardware_writes_are_disabled", response["dry_run"] is True and response["backend"] == "fake" and response["hardware_write_enabled"] is False)
        driver.command("connect", "observer")
        malformed = [b"", b"null", b"[]", b"{}", b"\xff", b"{} {}",
                     b'{"version":1,"version":1,"command":{"op":"enumerate"}}',
                     b'{"version":1,"command":{"op":"enumerate","op":"enumerate"}}',
                     b'{"version":1,"command":{"op":"enumerate","pid":1}}',
                     b'{"version":1,"owner":"forged","command":{"op":"enumerate"}}',
                     b'{"version":1,"command":{"op":"tick"}}',
                     b'{"version":1,"command":{"op":"capabilities","fan":65536}}',
                     b'{"version":1,"command":{"op":"acquire","fan":0,"rpm":NaN,"ttl_ms":5,"seq":1}}',
                     b'{"version":1,"command":{"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5,"seq":18446744073709551616}}',
                     b'{"version":1,"command":{"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5,"seq":1.5}}']
        malformed_results = [driver.request(raw=raw_request) for raw_request in malformed]
        check("closed_schema_rejects_15_malformed_or_forged_requests", all(result.get("error", {}).get("code") == "invalid_schema" for result in malformed_results))
        check("malformed_and_empty_payload_replies_keep_dry_run_markers", all(result.get("version") == 1 and result.get("dry_run") is True and result.get("hardware_write_enabled") is False for result in malformed_results))
        check("request_larger_than_4096_bytes_rejected", driver.request(raw=b" " * 4097)["error"]["code"] == "request_too_large")
        exact_limit = b'{"version":1,"command":{"op":"enumerate"}}'
        exact_limit += b" " * (4096 - len(exact_limit))
        check("request_exactly_4096_bytes_is_accepted", driver.request(raw=exact_limit)["ok"] is True)
        for ttl in [0, 5001]:
            check(f"ttl_{ttl}_rejected", driver.request({"op":"acquire", "fan":0,"rpm":3000,"ttl_ms":ttl,"seq":1})["error"]["code"] == "invalid_ttl")
        check("initial_sequence_zero_rejected", driver.request({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":0})["error"]["code"] == "invalid_sequence")
        acquired = driver.request({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":1})
        check("fake_lease_is_acquired_over_real_xpc", acquired.get("ok") is True and acquired["result"]["kind"] == "lease")
        token = acquired["result"]["token"]
        check("owner_is_bound_to_authenticated_connection", driver.request({"op":"renew","fan":0,"token":token,"ttl_ms":5000,"seq":2}, connection="observer")["error"]["code"] == "owner_conflict")
        check("replayed_sequence_rejected", driver.request({"op":"renew","fan":0,"token":token,"ttl_ms":5000,"seq":1})["error"]["code"] == "invalid_sequence")
        target_update = {"op":"set_target","fan":0,"token":token,"rpm":3600,"ttl_ms":5000,"seq":2}
        check("target_update_rejects_other_authenticated_owner", driver.request(target_update, connection="observer")["error"]["code"] == "owner_conflict")
        changed = driver.request(target_update)
        check("target_update_retains_lease_secret_and_reads_back_fake_target", changed.get("ok") is True and changed["result"]["token"] == token and changed["result"]["target_rpm"] == 3600 and changed["result"]["seq"] == 2 and changed["hardware_write_enabled"] is False)
        check("target_update_sequence_replay_rejected", driver.request(target_update)["error"]["code"] == "invalid_sequence")
        check("target_update_cannot_supply_hardware_key", driver.request({**target_update,"seq":3,"key":"F0Tg"})["error"]["code"] == "invalid_schema")
        check("u64_max_sequence_preserved_exactly", driver.request({"op":"renew","fan":0,"token":token,"ttl_ms":5000,"seq":2**64-1})["result"]["seq"] == 2**64-1)
        driver.command("disconnect", "a")
        deadline = time.monotonic() + 2
        observed = None
        while time.monotonic() < deadline:
            response = driver.request({"op":"enumerate"}, connection="observer")
            statuses = response.get("statuses", [])
            if statuses and statuses[0]["phase"]["state"] == "automatic_confirmed":
                observed = statuses[0]["phase"]
                break
            time.sleep(0.02)
        check("disconnect_revokes_before_5_second_ttl_with_confirmed_fake_automatic", observed is not None and observed["cause"] == "Disconnected")
        check("old_secret_cannot_be_renewed_after_disconnect", driver.request({"op":"renew","fan":0,"token":token,"ttl_ms":100,"seq":2}, connection="observer")["error"]["code"] == "no_active_lease")
        short = driver.request({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":150,"seq":1}, connection="observer")
        check("fresh_lease_after_disconnect", short.get("ok") is True)
        before_expirations = short["watchdog_expirations"]
        time.sleep(1.25)
        response = driver.request({"op":"enumerate"}, connection="observer")
        check("independent_timer_expires_lease_before_next_client_request", response["statuses"][0]["phase"]["state"] == "automatic_confirmed" and response["statuses"][0]["phase"]["cause"] == "Expired" and response["watchdog_expirations"] > before_expirations)
        capabilities = driver.request({"op":"capabilities","fan":0}, connection="observer")
        check("typed_capabilities_are_from_fake_adapter", capabilities["result"]["min_rpm"] == 1000 and capabilities["result"]["max_rpm"] == 5000 and capabilities["backend"] == "fake")
        final_lease = driver.request({"op":"acquire","fan":0,"rpm":3500,"ttl_ms":5000,"seq":1}, connection="observer")
        release_token = final_lease["result"]["token"]
        check("token_encoding_rejected_over_real_xpc", driver.request({"op":"release","fan":0,"token":"A"*64,"seq":2}, connection="observer")["error"]["code"] == "invalid_token_encoding")
        check("release_sequence_replay_rejected_over_real_xpc", driver.request({"op":"release","fan":0,"token":release_token,"seq":1}, connection="observer")["error"]["code"] == "invalid_sequence")
        released = driver.request({"op":"release","fan":0,"token":release_token,"seq":2}, connection="observer")
        check("typed_release_confirms_fake_automatic_over_real_xpc", released.get("ok") is True and released["result"]["kind"] == "automatic_confirmed" and released["statuses"][0]["phase"]["state"] == "automatic_confirmed")

        plan_start = {"op":"start_plan","entries":[{"fan":0,"strategy":{"type":"fixed","rpm":3200}}],"ttl_ms":5000,"seq":1}
        begun = driver.request(plan_start, connection="observer")
        check("plan_starts_through_same_authenticated_host", begun.get("ok") is True and begun["result"]["plan"]["phase"] == "running" and begun["hardware_write_enabled"] is False)
        plan_token = begun["result"]["token"]
        initial_deadline = begun["result"]["plan"]["fans"][0]["lease_expires_at_ms"]
        time.sleep(1.25)
        running = driver.request({"op":"plan_status"}, connection="observer")
        check("independent_timer_renews_core_lease_without_client_traffic", running["result"]["plan"]["phase"] == "running" and running["result"]["plan"]["fans"][0]["lease_expires_at_ms"] > initial_deadline)
        check("plan_status_never_discloses_authorization_secret", plan_token not in json.dumps(running))
        driver.command("connect", "other_plan_owner")
        renew_plan = {"op":"renew_plan","token":plan_token,"ttl_ms":5000,"seq":2}
        check("plan_renew_rejects_other_authenticated_connection", driver.request(renew_plan, connection="other_plan_owner")["error"]["code"] == "owner_conflict")
        driver.command("disconnect", "other_plan_owner")
        check("unrelated_disconnect_keeps_plan_running", driver.request({"op":"plan_status"}, connection="observer")["result"]["plan"]["phase"] == "running")
        check("plan_authorization_renew_requires_monotonic_sequence", driver.request(renew_plan, connection="observer")["ok"] is True and driver.request(renew_plan, connection="observer")["error"]["code"] == "invalid_sequence")
        check("raw_acquire_cannot_compete_with_plan", driver.request({"op":"acquire","fan":0,"rpm":4000,"ttl_ms":5000,"seq":1}, connection="observer")["error"]["code"] == "plan_busy")
        stopped = driver.request({"op":"stop_plan","token":plan_token,"seq":3}, connection="observer")
        check("plan_stop_reports_confirmed_fake_restoration", stopped.get("ok") is True and stopped["result"]["plan"]["fans"][0]["state"] == "automatic_confirmed")
        brief = driver.request({**plan_start,"ttl_ms":150}, connection="observer")
        before_plan_expiry = brief["result"]["plan"]["watchdog_expirations"]
        time.sleep(1.25)
        expired = driver.request({"op":"plan_status"}, connection="observer")
        check("independent_timer_revokes_expired_client_consent", expired["result"]["plan"]["watchdog_expirations"] > before_plan_expiry and expired["result"]["plan"]["phase"] == "stopped" and expired["result"]["plan"]["authorization_end"] == "expired" and expired["result"]["plan"]["fans"][0]["state"] == "automatic_confirmed")
        check("late_plan_heartbeat_cannot_restart_control", driver.request({"op":"renew_plan","token":brief["result"]["token"],"ttl_ms":5000,"seq":2}, connection="observer")["error"]["code"] == "no_active_plan")
        check("client_supplied_sample_cannot_become_trusted", driver.request({**plan_start,"samples":[{"quality":"verified","celsius":60,"sampled_at_ms":0}]}, connection="observer")["error"]["code"] == "invalid_schema")
        sensor_plan = {**plan_start,"entries":[{"fan":0,"strategy":{"type":"sensor","sensor_id":"smc:TCMb","min_rpm":1000,"max_rpm":5000,"lower_temp":40,"upper_temp":80}}]}
        check("missing_local_sensor_provider_is_explicitly_rejected", driver.request(sensor_plan, connection="observer")["error"]["code"] == "sensor_source_unavailable")

        sensor_plan["entries"][0]["strategy"]["sensor_id"] = "test:temperature"
        sensor_started = driver.request(sensor_plan, connection="observer")
        sensor_snapshot = sensor_started.get("result", {}).get("plan", {})
        check("fake_local_sensor_plan_is_authorized_over_real_xpc", sensor_started.get("ok") is True and sensor_snapshot.get("authorized") is True and sensor_snapshot.get("phase") in ("preparing", "running") and sensor_started["hardware_write_enabled"] is False)
        check("sensor_start_outcomes_match_observed_phase", (sensor_snapshot["phase"] == "preparing" and sensor_snapshot["fans"] == [{"fan": 0, "state": "automatic_confirmed"}]) or (sensor_snapshot["phase"] == "preparing" and sensor_snapshot["fans"] == [{"fan": 0, "state": "unobserved"}]) or (sensor_snapshot["phase"] == "running" and len(sensor_snapshot["fans"]) == 1 and sensor_snapshot["fans"][0]["state"] == "active" and sensor_snapshot["fans"][0]["target_rpm"] == 3000))
        sensor_token = sensor_started["result"]["token"]
        driver.command("connect", "a")
        check("sensor_plan_rejects_other_authenticated_owner", driver.request({"op":"renew_plan","token":sensor_token,"ttl_ms":5000,"seq":2}, connection="a").get("error", {}).get("code") == "owner_conflict")
        # No requests while independent host timer consumes a genuinely new batch.
        time.sleep(0.3)
        sensor_running = driver.request({"op":"plan_status"}, connection="observer")
        snapshot = sensor_running["result"]["plan"]
        check("independent_timer_runs_fake_local_sensor_target", snapshot["phase"] == "running" and snapshot["authorized"] is True and len(snapshot["fans"]) == 1 and snapshot["fans"][0]["state"] == "active" and snapshot["fans"][0]["target_rpm"] == 3000 and sensor_running["hardware_write_enabled"] is False)
        check("sensor_status_hides_authorization_secret", sensor_token not in json.dumps(sensor_running))
        check("sensor_plan_renews_with_same_owner_and_token", driver.request({"op":"renew_plan","token":sensor_token,"ttl_ms":5000,"seq":2}, connection="observer")["ok"] is True)
        sensor_stopped = driver.request({"op":"stop_plan","token":sensor_token,"seq":3}, connection="observer")
        snapshot = sensor_stopped["result"]["plan"]
        check("sensor_plan_stop_confirms_fake_automatic_readback", sensor_stopped["ok"] is True and snapshot["authorized"] is False and snapshot["phase"] == "stopped" and snapshot["fans"] == [{"fan":0,"state":"automatic_confirmed"}] and sensor_stopped["hardware_write_enabled"] is False)

        wrong_raw = BUILD / "WrongClient"
        shutil.copy2(raw, wrong_raw)
        # Preserve bundle identifier and executable behavior; changing signed
        # runtime flags changes the CDHash without breaking bundle lookup.
        run(["codesign", "--force", "--sign", "-", "--options", "runtime", "--identifier", "io.macsfan.probe.client", wrong_raw])
        wrong_hash = cdhash(wrong_raw)
        check("wrong_client_has_distinct_real_cdhash", wrong_hash != client_hash)
        wrong_app = BUILD / "Wrong.app"
        shutil.copytree(app, wrong_app)
        wrong_client = wrong_app / "Contents/MacOS/Probe"
        shutil.copy2(wrong_raw, wrong_client)
        wrong = Driver(wrong_client, helper_pin)
        try:
            wrong.command("connect")
            denied = wrong.request({"op":"enumerate"})
            print("wrong_client_rejection=", json.dumps(denied), flush=True)
            check("actual_wrong_code_identity_is_rejected_by_xpc", denied.get("transport_error") == "xpc_rejected" and "result" not in denied)
        finally:
            wrong.close()
        _, matched_client, _, _, matched_pin = make_bundle("MatchedWrongIdentity", wrong_raw, wrong_hash)
        matched = Driver(matched_client, matched_pin)
        try:
            matched.command("connect")
            allowed = matched.request({"op":"enumerate"})
            check("same_wrong_identity_connects_when_its_real_pin_is_explicitly_allowed", allowed.get("ok") is True and allowed["result"]["fans"] == [0])
        finally:
            matched.close()
        wrong_server = Driver(client, pin("0" * 40))
        try:
            wrong_server.command("connect")
            denied = wrong_server.request({"op":"enumerate"})
            print("wrong_helper_rejection=", json.dumps(denied), flush=True)
            check("client_rejects_wrong_helper_identity", denied.get("transport_error") == "xpc_rejected")
        finally:
            wrong_server.close()
    finally:
        driver.close()

    run([raw, "--validate-config"], expected=78)
    check("missing_signing_configuration_refuses_start", True)
    for name, replacement in [("TamperedBackend", {"MFBackend":"unavailable"}),
                              ("TamperedClientPin", {"MFClientRequirement":pin("0"*40)})]:
        tampered = BUILD / (name + ".xpc")
        shutil.copytree(service, tampered)
        executable = tampered / "Contents/MacOS/MacsFanHelper"
        run([executable, "--validate-config"])
        info_path = tampered / "Contents/Info.plist"
        info = plistlib.loads(info_path.read_bytes())
        info.update(replacement)
        info_path.write_bytes(plistlib.dumps(info))
        # Deliberately no codesign after a still-well-formed config mutation.
        run([executable], expected=78)
        check(name + "_without_resigning_refuses_start", True)
    for name, replacement in [("BroadRequirement", {"MFClientRequirement":"true"}),
                              ("MalformedRequirement", {"MFClientRequirement":"invalid requirement"}),
                              ("UnknownBackend", {"MFBackend":"smc"}),
                              ("ProductionFlag", {"MFDryRun":False})]:
        invalid = BUILD / (name + ".xpc")
        shutil.copytree(service, invalid)
        info_path = invalid / "Contents/Info.plist"
        info = plistlib.loads(info_path.read_bytes())
        info.update(replacement)
        info_path.write_bytes(plistlib.dumps(info))
        run(["codesign", "--force", "--sign", "-", invalid])
        run([invalid / "Contents/MacOS/MacsFanHelper"], expected=78)
        check(name + "_refuses_server_start", True)
    _, unavailable_client, _, _, unavailable_pin = make_bundle("Unavailable", raw, client_hash, "unavailable")
    unavailable = Driver(unavailable_client, unavailable_pin)
    try:
        unavailable.command("connect")
        response = unavailable.request({"op":"enumerate"})
        check("unavailable_backend_returns_explicit_failure_over_xpc", response.get("ok") is False and response["backend"] == "unavailable" and response["error"]["code"] == "actuator_unavailable" and response["hardware_write_enabled"] is False)
    finally:
        unavailable.close()
    print(f"VERIFIED {len(CHECKS)} real-IPC checks; temporary_bundle={app}", flush=True)
    evidence = {"checks": CHECKS, "temporary_bundle": str(app), "dry_run": True,
                "hardware_write_enabled": False, "production_identity": False,
                "native_test_injection_disabled": True,
                "sensor_plan_records": {"start": sensor_snapshot, "running": sensor_running["result"]["plan"], "stop": sensor_stopped["result"]["plan"]},
                "probe_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    (BUILD / "verification.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
    output = ROOT.parents[2] / "work/helper-xpc-0.12"
    output.mkdir(parents=True, exist_ok=True)
    (output / "verification.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
    print("evidence=", BUILD / "verification.json", flush=True)


if __name__ == "__main__":
    main()

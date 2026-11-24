// Included only when native MF_TEST_HARNESS and Rust test-harness are enabled.
// These injections are local CLI tests, never exported NSXPC methods.
static int MFLifecycleProbe(NSString *scenario) {
    BOOL failure = [scenario containsString:@"failure"] || [scenario isEqual:@"timeout"];
    BOOL exitSignal = [scenario hasPrefix:@"exit_"];
    if (![@[@"sleep_wake", @"sleep_failure_wake", @"shutdown", @"shutdown_failure", @"timeout", @"exit_success", @"exit_failure"] containsObject:scenario]) return 64;
    MFHost *host = [[MFHost alloc] initWithEngine:mf_helper_test_create(failure ? 3 : 0) observePower:NO];
    if (!host) return 70;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block int result = 1;
    dispatch_async(host.queue, ^{
        NSMutableData *owner = [NSMutableData dataWithLength:32]; memset(owner.mutableBytes, 0x5a, owner.length);
        NSData *acquire = [@"{\"version\":1,\"command\":{\"op\":\"acquire\",\"fan\":0,\"rpm\":3000,\"ttl_ms\":5000,\"seq\":1}}" dataUsingEncoding:NSUTF8StringEncoding];
        NSDictionary *grant = MFDecode(mf_helper_request(host.engine, owner.bytes, acquire.bytes, acquire.length));
        if (![grant[@"ok"] isEqual:@YES]) { dispatch_semaphore_signal(done); return; }
        if (exitSignal) {
            // Signals only this temporary child process, never any user app.
            kill(getpid(), SIGTERM);
            return;
        }
        if ([scenario hasPrefix:@"sleep_"]) {
            MFRunPowerTransition(host, kIOMessageSystemWillSleep, ^(NSDictionary *sleep) {
                NSDictionary *renewCommand = @{@"version":@1,@"command":@{@"op":@"renew",@"fan":@0,
                    @"token":grant[@"result"][@"token"],@"ttl_ms":@5000,@"seq":@2}};
                NSData *renew = [NSJSONSerialization dataWithJSONObject:renewCommand options:0 error:NULL];
                NSDictionary *sleepRenew = MFDecode(mf_helper_request(host.engine, owner.bytes, renew.bytes, renew.length));
                NSDictionary *sleepAcquire = MFDecode(mf_helper_request(host.engine, owner.bytes, acquire.bytes, acquire.length));
                MFRunPowerTransition(host, kIOMessageSystemHasPoweredOn, ^(NSDictionary *wake) {
                    NSDictionary *wakeRenew = MFDecode(mf_helper_request(host.engine, owner.bytes, renew.bytes, renew.length));
                    NSDictionary *wakeAcquire = MFDecode(mf_helper_request(host.engine, owner.bytes, acquire.bytes, acquire.length));
                    BOOL locked = [sleepRenew[@"error"][@"code"] isEqual:@"suspended"] &&
                        [sleepAcquire[@"error"][@"code"] isEqual:@"suspended"] &&
                        [wakeRenew[@"error"][@"code"] isEqual:@"suspended"] &&
                        [wakeAcquire[@"error"][@"code"] isEqual:@"suspended"];
                    BOOL confirmed = [sleep[@"recovery_confirmed"] isEqual:@YES] && [wake[@"recovery_confirmed"] isEqual:@YES];
                    BOOL recoveryExpected = failure ? (!confirmed && [wake[@"statuses"][0][@"phase"][@"attempts"] isEqual:@3]
                        && [wake[@"statuses"][0][@"phase"][@"exhausted"] isEqual:@YES]) : confirmed;
                    BOOL freshRequired = [wake[@"lifecycle"] isEqual:@"wake_requires_new_session"];
                    result = locked && recoveryExpected && freshRequired ? 0 : 1;
                    MFPrint(@{@"test":scenario,@"ok":result == 0 ? @YES : @NO,
                        @"acquire_and_renew_locked":locked ? @YES : @NO,@"sleep":sleep,@"wake":wake});
                    dispatch_semaphore_signal(done);
                });
            });
        } else {
            [host transition:3 budget:[scenario isEqual:@"timeout"] ? 0.1 : 1.0 completion:^(NSDictionary *report) {
                BOOL confirmed = [report[@"recovery_confirmed"] isEqual:@YES];
                BOOL bounded = [report[@"wait_ms"] doubleValue] <= 1200.0;
                BOOL expected = failure ? !confirmed : confirmed;
                if ([scenario isEqual:@"timeout"]) expected = expected && [report[@"deadline_exceeded"] isEqual:@YES];
                if ([scenario isEqual:@"shutdown_failure"]) {
                    NSDictionary *phase = report[@"statuses"][0][@"phase"];
                    expected = expected && [phase[@"exhausted"] isEqual:@YES] && [phase[@"attempts"] isEqual:@3];
                }
                result = expected && bounded ? 0 : 1;
                MFPrint(@{@"test":scenario,@"ok":result == 0 ? @YES : @NO,@"report":report});
                dispatch_semaphore_signal(done);
            }];
        }
    });
    if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC))) return 2;
    return result;
}

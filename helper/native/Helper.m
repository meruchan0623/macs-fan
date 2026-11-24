#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <AppKit/NSWorkspace.h>
#import <IOKit/IOMessage.h>
#import <IOKit/pwr_mgt/IOPMLib.h>
#import "Bridge.h"
#import "ProductionIdentity.h"
#include <signal.h>
#include <unistd.h>

// The sole exported selector. Identity and host events are not wire arguments.
@protocol MFFanHelper
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply;
@end

static const NSUInteger MFMaxRequestBytes = 4096;
static NSData *MFError(NSString *code) {
    return [NSJSONSerialization dataWithJSONObject:@{@"version":@1, @"dry_run":@YES,
        @"hardware_write_enabled":@NO, @"ok":@NO, @"error":@{@"code":code}} options:0 error:NULL];
}
static NSData *MFConsume(char *json) {
    if (!json) return MFError(@"internal_failure");
    NSData *data = [NSData dataWithBytes:json length:strlen(json)];
    mf_helper_free(json);
    return data;
}

// Exact CDHash pins are exclusively for this local dry-run harness. There is
// no production signing profile or hardware write actuator in that harness.
static BOOL MFValidatePin(NSString *text) {
    if (![text isKindOfClass:NSString.class]) return NO;
    NSRegularExpression *expression = [NSRegularExpression regularExpressionWithPattern:
        @"^cdhash H\"(?:[0-9a-f]{40}|[0-9a-f]{64})\"$" options:0 error:NULL];
    if ([expression numberOfMatchesInString:text options:0 range:NSMakeRange(0, text.length)] != 1) return NO;
    SecRequirementRef requirement = NULL;
    OSStatus status = SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement);
    if (requirement) CFRelease(requirement);
    return status == errSecSuccess;
}
static NSDictionary *MFConfiguration(void) {
    NSDictionary *config = NSBundle.mainBundle.infoDictionary;
    if (![config[@"MFDryRun"] isEqual:@YES] || !MFValidatePin(config[@"MFClientRequirement"])) return nil;
    if (![@[@"fake", @"unavailable", @"smc_readonly"] containsObject:config[@"MFBackend"]]) return nil;
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopySelf(kSecCSDefaultFlags, &code);
    if (status == errSecSuccess && code) status = SecCodeCheckValidity(code, kSecCSDefaultFlags, NULL);
    else status = errSecCSUnsigned;
    if (code) CFRelease(code);
    return status == errSecSuccess ? config : nil;
}

@interface MFHost : NSObject
@property(nonatomic) void *engine;
@property(nonatomic, strong) dispatch_queue_t queue;
@property(nonatomic, strong) dispatch_source_t timer;
@property(nonatomic, strong) dispatch_source_t termination;
@property(nonatomic, strong) dispatch_source_t interruption;
@property(nonatomic) io_connect_t powerConnection;
@property(nonatomic) io_object_t powerNotifier;
@property(nonatomic) IONotificationPortRef powerPort;
@property(nonatomic, strong) id powerOffObserver;
@property(nonatomic, strong) NSMutableSet<NSNumber *> *pendingSleepAcks;
@property(nonatomic) BOOL stopping;
- (instancetype)initWithFake:(BOOL)fake;
- (instancetype)initWithEngine:(void *)engine observePower:(BOOL)observePower;
- (instancetype)initWithEngine:(void *)engine observePower:(BOOL)observePower observeWorkspace:(BOOL)observeWorkspace;
- (void)transition:(uint8_t)event budget:(NSTimeInterval)budget completion:(void (^)(NSDictionary *))completion;
- (void)requestExit;
@end

static NSDictionary *MFDecode(char *response) {
    NSDictionary *decoded = [NSJSONSerialization JSONObjectWithData:MFConsume(response) options:0 error:NULL];
    return [decoded isKindOfClass:NSDictionary.class] ? decoded : @{};
}
static void MFLogLifecycle(NSString *event, NSDictionary *report) {
    NSData *line = [NSJSONSerialization dataWithJSONObject:@{@"event":event, @"report":report} options:0 error:NULL];
    fwrite(line.bytes, 1, line.length, stderr); fputc('\n', stderr); fflush(stderr);
}
static void MFRunPowerTransition(MFHost *host, natural_t type, void (^completion)(NSDictionary *)) {
    // Shared mapping for the real callback and compile-gated local tests.
    [host transition:type == kIOMessageSystemWillSleep ? 1 : 2 budget:1.0 completion:completion];
}
static void MFAcknowledgePower(MFHost *host, intptr_t notification) {
    IOReturn status = IOAllowPowerChange(host.powerConnection, notification);
    if (status != kIOReturnSuccess) fprintf(stderr, "event=power_ack_failed code=%d\n", status);
}
static void MFPowerCallback(void *refcon, io_service_t service, natural_t type, void *argument) {
    (void)service;
    MFHost *host = (__bridge MFHost *)refcon;
    // Delivered on the same serial queue as requests and all engine ticks.
    if (type == kIOMessageCanSystemSleep) {
        MFAcknowledgePower(host, (intptr_t)argument);
    } else if (type == kIOMessageSystemWillSleep) {
        NSNumber *notification = @((intptr_t)argument);
        [host.pendingSleepAcks addObject:notification];
        MFRunPowerTransition(host, type, ^(NSDictionary *report) {
            MFLogLifecycle(@"system_will_sleep_recovery", report);
            // Mandatory acknowledgement even when recovery remains unconfirmed.
            // Never hold up a user's forced sleep for the system's 30s timeout.
            if ([host.pendingSleepAcks containsObject:notification] && host.powerConnection) {
                MFAcknowledgePower(host, notification.longLongValue);
                [host.pendingSleepAcks removeObject:notification];
            }
        });
    } else if (type == kIOMessageSystemHasPoweredOn) {
        MFRunPowerTransition(host, type, ^(NSDictionary *report) {
            MFLogLifecycle(@"system_woke_locked", report);
        });
    }
}
@implementation MFHost
- (instancetype)initWithFake:(BOOL)fake {
    return [self initWithEngine:mf_helper_create(fake ? 1 : 0) observePower:YES];
}
- (instancetype)initWithEngine:(void *)engine observePower:(BOOL)observePower {
    return [self initWithEngine:engine observePower:observePower observeWorkspace:YES];
}
- (instancetype)initWithEngine:(void *)engine observePower:(BOOL)observePower observeWorkspace:(BOOL)observeWorkspace {
    self = [super init];
    if (self) {
        _engine = engine;
        if (!_engine) return nil;
        _queue = dispatch_queue_create("local.macsfan.helper.engine", DISPATCH_QUEUE_SERIAL);
        _pendingSleepAcks = [NSMutableSet new];
        if (observePower) {
            _powerConnection = IORegisterForSystemPower((__bridge void *)self, &_powerPort, MFPowerCallback, &_powerNotifier);
            if (_powerConnection == IO_OBJECT_NULL || !_powerPort) {
                fprintf(stderr, "refused_power_observation_unavailable\n");
                if (_powerNotifier) IODeregisterForSystemPower(&_powerNotifier);
                if (_powerPort) IONotificationPortDestroy(_powerPort);
                if (_powerConnection) IOServiceClose(_powerConnection);
                _powerPort = NULL; _powerConnection = IO_OBJECT_NULL; _powerNotifier = IO_OBJECT_NULL;
                return nil;
            }
            IONotificationPortSetDispatchQueue(_powerPort, _queue);
            // NSWorkspace belongs to a user GUI session. A LaunchDaemon uses
            // IOKit sleep/wake plus launchd's SIGTERM, without WindowServer.
            if (observeWorkspace) {
                __weak MFHost *weakHost = self;
                _powerOffObserver = [NSWorkspace.sharedWorkspace.notificationCenter
                    addObserverForName:NSWorkspaceWillPowerOffNotification object:nil queue:nil usingBlock:^(NSNotification *notification) {
                        (void)notification;
                        MFHost *host = weakHost;
                        if (host) dispatch_async(host.queue, ^{ [host requestExit]; });
                    }];
            }
        }
        _timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, _queue);
        dispatch_source_set_timer(_timer, DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC, 5 * NSEC_PER_MSEC);
        dispatch_source_set_event_handler(_timer, ^{ mf_helper_free(mf_helper_tick(self.engine)); });
        dispatch_resume(_timer);
        signal(SIGTERM, SIG_IGN);
        signal(SIGINT, SIG_IGN);
        _termination = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0, _queue);
        dispatch_source_set_event_handler(_termination, ^{
            [self requestExit];
        });
        dispatch_resume(_termination);
        _interruption = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGINT, 0, _queue);
        dispatch_source_set_event_handler(_interruption, ^{ [self requestExit]; });
        dispatch_resume(_interruption);
    }
    return self;
}
- (void)pollRecoveryUntil:(NSTimeInterval)deadline started:(NSTimeInterval)started completion:(void (^)(NSDictionary *))completion {
    NSMutableDictionary *report = [MFDecode(mf_helper_lifecycle_poll(self.engine)) mutableCopy];
    BOOL confirmed = [report[@"recovery_confirmed"] isEqual:@YES];
    BOOL pending = [report[@"recovery_pending"] isEqual:@YES];
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    if (confirmed || !pending || now >= deadline) {
        report[@"wait_ms"] = @((now - started) * 1000.0);
        report[@"deadline_exceeded"] = (!confirmed && pending && now >= deadline) ? @YES : @NO;
        completion(report);
        return;
    }
    NSTimeInterval delay = MIN(0.05, MAX(0.0, deadline - now));
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(delay * NSEC_PER_SEC)), self.queue, ^{
        [self pollRecoveryUntil:deadline started:started completion:completion];
    });
}
- (void)transition:(uint8_t)event budget:(NSTimeInterval)budget completion:(void (^)(NSDictionary *))completion {
    // Internal native host API only; no NSXPC protocol exposes lifecycle events.
    NSTimeInterval started = NSProcessInfo.processInfo.systemUptime;
    mf_helper_free(mf_helper_lifecycle_begin(self.engine, event));
    [self pollRecoveryUntil:started + MIN(MAX(budget, 0.0), 1.0) started:started completion:completion];
}
- (void)requestExit {
    if (self.stopping) return; // Repeated signals cannot extend the deadline.
    self.stopping = YES;
    [self transition:3 budget:1.0 completion:^(NSDictionary *report) {
        BOOL confirmed = [report[@"recovery_confirmed"] isEqual:@YES];
        MFLogLifecycle(confirmed ? @"shutdown_recovery_confirmed" : @"shutdown_recovery_unconfirmed", report);
        for (NSNumber *notification in self.pendingSleepAcks) {
            if (self.powerConnection) MFAcknowledgePower(self, notification.longLongValue);
        }
        [self.pendingSleepAcks removeAllObjects];
        if (self.powerOffObserver) [NSWorkspace.sharedWorkspace.notificationCenter removeObserver:self.powerOffObserver];
        if (self.powerNotifier) IODeregisterForSystemPower(&self->_powerNotifier);
        if (self.powerPort) { IONotificationPortDestroy(self.powerPort); self.powerPort = NULL; }
        if (self.powerConnection) { IOServiceClose(self.powerConnection); self.powerConnection = IO_OBJECT_NULL; }
        exit(confirmed ? 0 : 74);
    }];
}
@end

@interface MFSession : NSObject <MFFanHelper>
@property(nonatomic, strong) MFHost *host;
@property(nonatomic, copy) NSData *owner;
@property(nonatomic) BOOL closed; // Accessed only on host.queue.
- (void)close;
@end
@implementation MFSession
- (void)request:(NSData *)payload reply:(void (^)(NSData *))reply {
    // NSXPC has checked the real peer requirement before invoking this method.
    if (payload.length > MFMaxRequestBytes) { reply(MFError(@"request_too_large")); return; }
    __block NSData *response;
    // Backpressure: no application-owned unbounded async request queue.
    dispatch_sync(self.host.queue, ^{
        response = self.closed ? MFError(@"connection_closed") :
            MFConsume(mf_helper_request(self.host.engine, self.owner.bytes, payload.bytes, payload.length));
    });
    reply(response);
}
- (void)close {
    dispatch_async(self.host.queue, ^{
        if (self.closed) return;
        self.closed = YES;
        mf_helper_free(mf_helper_disconnect(self.host.engine, self.owner.bytes));
        fprintf(stderr, "event=authenticated_connection_revoked hardware_write_enabled=false\n");
    });
}
@end

@interface MFListener : NSObject <NSXPCListenerDelegate>
@property(nonatomic, copy) NSString *requirement;
@property(nonatomic, strong) MFHost *host;
@end
@implementation MFListener
- (BOOL)listener:(NSXPCListener *)listener shouldAcceptNewConnection:(NSXPCConnection *)connection {
    (void)listener;
    if (!self.requirement.length || !self.host) return NO;
    uint8_t owner[32];
    if (SecRandomCopyBytes(kSecRandomDefault, sizeof(owner), owner) != errSecSuccess) return NO;
    MFSession *session = [MFSession new];
    session.host = self.host;
    session.owner = [NSData dataWithBytes:owner length:sizeof(owner)];
    // Public macOS 13+ enforcement binds every message to its actual peer.
    // No processIdentifier/getpeereid substitution or private auditToken KVC.
    [connection setCodeSigningRequirement:self.requirement];
    connection.exportedInterface = [NSXPCInterface interfaceWithProtocol:@protocol(MFFanHelper)];
    connection.exportedObject = session;
    connection.interruptionHandler = ^{ [session close]; };
    connection.invalidationHandler = ^{ [session close]; };
    [connection resume];
    return YES;
}
@end

static void MFPrint(NSDictionary *object) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:object options:NSJSONWritingSortedKeys error:NULL];
    fwrite(data.bytes, 1, data.length, stdout); fputc('\n', stdout); fflush(stdout);
}
#ifdef MF_TEST_HARNESS
#include "LifecycleTests.inc"
#endif
static NSXPCConnection *MFClientConnection(NSString *service, NSString *pin) {
    NSXPCConnection *connection = [[NSXPCConnection alloc] initWithServiceName:service];
    [connection setCodeSigningRequirement:pin];
    connection.remoteObjectInterface = [NSXPCInterface interfaceWithProtocol:@protocol(MFFanHelper)];
    [connection resume];
    return connection;
}
static int MFClient(NSString *service, NSString *pin) {
    if (!MFValidatePin(pin)) { fprintf(stderr, "invalid_server_requirement\n"); return 78; }
    NSMutableDictionary<NSString *, NSXPCConnection *> *connections = [NSMutableDictionary new];
    char *line = NULL; size_t capacity = 0;
    while (getline(&line, &capacity, stdin) >= 0) {
        NSData *data = [[NSString stringWithUTF8String:line] dataUsingEncoding:NSUTF8StringEncoding];
        NSDictionary *command = [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
        if (![command isKindOfClass:NSDictionary.class]) { MFPrint(@{@"transport_error":@"bad_driver_input"}); continue; }
        NSString *action = command[@"action"], *name = command[@"connection"];
        if ([action isEqual:@"exit"]) break;
        if (![name isKindOfClass:NSString.class]) { MFPrint(@{@"transport_error":@"missing_connection"}); continue; }
        if ([action isEqual:@"connect"]) {
            if (connections[name]) { MFPrint(@{@"transport_error":@"duplicate_connection"}); continue; }
            connections[name] = MFClientConnection(service, pin);
            MFPrint(@{@"transport":@"connected"});
        } else if ([action isEqual:@"disconnect"]) {
            [connections[name] invalidate]; [connections removeObjectForKey:name];
            MFPrint(@{@"transport":@"disconnected"});
        } else if ([action isEqual:@"request"] && connections[name]) {
            NSData *payload = [[NSData alloc] initWithBase64EncodedString:command[@"payload_base64"] options:0];
            if (!payload) { MFPrint(@{@"transport_error":@"invalid_payload"}); continue; }
            dispatch_semaphore_t done = dispatch_semaphore_create(0);
            NSLock *resultLock = [NSLock new];
            __block NSDictionary *result = nil;
            id<MFFanHelper> proxy = [connections[name] remoteObjectProxyWithErrorHandler:^(NSError *error) {
                [resultLock lock];
                if (!result) result = @{@"transport_error":@"xpc_rejected", @"error_code":@(error.code),
                    @"signing_requirement_failure":error.code == NSXPCConnectionCodeSigningRequirementFailure ? @YES : @NO};
                [resultLock unlock];
                dispatch_semaphore_signal(done);
            }];
            [proxy request:payload reply:^(NSData *reply) {
                NSDictionary *decoded = [NSJSONSerialization JSONObjectWithData:reply options:0 error:NULL];
                [resultLock lock];
                if (!result) result = [decoded isKindOfClass:NSDictionary.class] ? decoded : @{@"transport_error":@"invalid_reply"};
                [resultLock unlock];
                dispatch_semaphore_signal(done);
            }];
            if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC))) {
                [resultLock lock];
                if (!result) result = @{@"transport_error":@"timeout"};
                [resultLock unlock];
                [connections[name] invalidate]; [connections removeObjectForKey:name];
            }
            [resultLock lock];
            NSDictionary *finalResult = result ?: @{@"transport_error":@"missing_reply"};
            [resultLock unlock];
            MFPrint(finalResult);
        } else { MFPrint(@{@"transport_error":@"unknown_driver_action"}); }
    }
    free(line);
    for (NSXPCConnection *connection in connections.allValues) [connection invalidate];
    return 0;
}

// LaunchDaemon entrypoint. No command-line/configuration value selects its
// identity, Mach name, requirement, or backend. Identity is checked before root
// status, engine construction, power registration, or listener creation.
static int MFServeProduction(void) {
    NSDictionary *identity = MFProductionIdentityCheck();
    if (![identity[@"production_identity_valid"] isEqual:@YES]) {
        MFPrint(identity); return 78;
    }
    if (geteuid() != 0) {
        NSMutableDictionary *failure = [identity mutableCopy];
        failure[@"reason"] = @"production_requires_root";
        failure[@"identity_check_only"] = @NO;
        MFPrint(failure); return 77;
    }
    NSDictionary *requirements = MFProductionConfiguredRequirements();
    if (!requirements[@"client"] || !requirements[@"helper"]) {
        // Defensive fail closed even if the fixed identity provider changes.
        MFPrint(@{@"reason":@"production_requirements_unavailable",
            @"production_channel_available":@NO, @"production_listener_resumed":@NO,
            @"production_control_ready":@NO, @"hardware_write_enabled":@NO});
        return 78;
    }
    __attribute__((objc_precise_lifetime)) MFListener *delegate = [MFListener new];
    delegate.requirement = requirements[@"client"];
    delegate.host = [[MFHost alloc] initWithEngine:mf_helper_create_smc_readonly()
        observePower:YES observeWorkspace:NO];
    if (!delegate.host) {
        MFPrint(@{@"reason":@"production_readonly_host_unavailable",
            @"production_channel_available":@NO, @"production_listener_resumed":@NO,
            @"production_control_ready":@NO, @"hardware_write_enabled":@NO});
        return 70;
    }
    __attribute__((objc_precise_lifetime)) NSXPCListener *listener =
        [[NSXPCListener alloc] initWithMachServiceName:MFProductionMachServiceName()];
    // Reject wrong identities before allocating a session, and retain the
    // per-connection requirement above for enforcement on every message.
    [listener setConnectionCodeSigningRequirement:requirements[@"client"]];
    listener.delegate = delegate;
    [listener resume];
    MFPrint(@{@"event":@"production_readonly_listener_resumed",
        @"production_identity_valid":@YES, @"identity_check_only":@NO,
        @"production_listener_resumed":@YES, @"production_control_ready":@NO,
        @"hardware_write_enabled":@NO, @"backend":@"smc_readonly",
        @"mach_service_name":MFProductionMachServiceName(),
        @"channel_authentication":@"fixed_client_requirement_each_message",
        @"reason":@"listener_resumed_control_unavailable"});
    // Mach-service listeners do not run a serviceListener's implicit run loop.
    // Keep an input source so a run loop without pending timers cannot spin.
    [NSRunLoop.currentRunLoop addPort:[NSMachPort port] forMode:NSDefaultRunLoopMode];
    // objc_precise_lifetime retains both listener and delegate across this loop.
    for (;;) {
        @autoreleasepool {
            [NSRunLoop.currentRunLoop runUntilDate:[NSDate distantFuture]];
        }
    }
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc == 2 && strcmp(argv[1], "--production-check") == 0) {
            NSDictionary *identity = MFProductionIdentityCheck();
            MFPrint(identity);
            return [identity[@"production_identity_valid"] isEqual:@YES] ? 0 : 78;
        }
        if (argc == 2 && strcmp(argv[1], "--serve-production") == 0)
            return MFServeProduction();
        if (geteuid() == 0) { fprintf(stderr, "dry_run_refuses_root\n"); return 77; }
#ifdef MF_TEST_HARNESS
        if (argc == 3 && strcmp(argv[1], "--lifecycle-probe") == 0)
            return MFLifecycleProbe([NSString stringWithUTF8String:argv[2]]);
        if (argc == 2 && strcmp(argv[1], "--identity-probe") == 0) {
            MFPrint(MFProductionIdentityProbe()); return 0;
        }
#endif
        if (argc == 4 && strcmp(argv[1], "client") == 0)
            return MFClient([NSString stringWithUTF8String:argv[2]], [NSString stringWithUTF8String:argv[3]]);
        NSDictionary *configuration = MFConfiguration();
        if (!configuration) { fprintf(stderr, "refused_missing_or_invalid_dry_run_signing_configuration\n"); return 78; }
        if (argc == 2 && strcmp(argv[1], "--validate-config") == 0) {
            printf("validated_dry_run_configuration hardware_write_enabled=false\n"); return 0;
        }
        if (argc != 1) return 64;
        MFListener *delegate = [MFListener new];
        delegate.requirement = configuration[@"MFClientRequirement"];
        if ([configuration[@"MFBackend"] isEqual:@"smc_readonly"]) {
            delegate.host = [[MFHost alloc] initWithEngine:mf_helper_create_smc_readonly() observePower:YES];
        } else {
            delegate.host = [[MFHost alloc] initWithFake:[configuration[@"MFBackend"] isEqual:@"fake"]];
        }
        if (!delegate.host) return 70;
        NSXPCListener *listener = NSXPCListener.serviceListener;
        listener.delegate = delegate;
        fprintf(stderr, "event=dry_run_xpc_started backend=%s hardware_write_enabled=false\n", [configuration[@"MFBackend"] UTF8String]);
        [listener resume];
    }
    return 0;
}

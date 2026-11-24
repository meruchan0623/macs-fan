#import "ServiceManager.h"
#import "../../helper/native/ProductionIdentity.h"
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <objc/message.h>
#import <dlfcn.h>
#import <sys/stat.h>
#import <stdlib.h>
#import <string.h>
#import <stdio.h>

static NSString * const MFServicePlist = @"local.macsfan.helper.control.plist";
static NSString * const MFHelperProgram = @"Contents/Library/LaunchServices/MacsFanHelper";
static NSString * const MFHelperArgumentZero = @"MacsFanHelper";

#ifdef MF_SERVICE_TEST_HARNESS
static MFServiceTestBridge MFTestBridge;
void MFServiceTestInstallBridge(MFServiceTestBridge bridge) { MFTestBridge = bridge; }
#endif

static BOOL MFSupported(void) {
#if defined(MF_SERVICE_TEST_HARNESS) && defined(MF_SERVICE_TEST_OS_MAJOR)
    return MF_SERVICE_TEST_OS_MAJOR >= 13;
#else
    return NSProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 13;
#endif
}

// No static SMAppService symbol or hard framework dependency: the app still
// launches on its macOS 11 minimum. All new API dispatch is guarded at runtime.
#ifndef MF_SERVICE_TEST_HARNESS
static id MFService(void) {
    if (!MFSupported()) return nil;
    static void *framework;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        framework = dlopen("/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement", RTLD_LAZY | RTLD_LOCAL);
    });
    if (!framework) return nil;
    Class cls = NSClassFromString(@"SMAppService");
    SEL selector = NSSelectorFromString(@"daemonServiceWithPlistName:");
    if (!cls || ![cls respondsToSelector:selector]) return nil;
    return ((id (*)(id, SEL, id))objc_msgSend)(cls, selector, MFServicePlist);
}
#endif

static NSInteger MFStatus(id service) {
#ifdef MF_SERVICE_TEST_HARNESS
    (void)service;
    if (MFTestBridge.status) return MFTestBridge.status();
    return -1; // A test build never falls through to the real service manager.
#else
    SEL selector = NSSelectorFromString(@"status");
    if (!service || ![service respondsToSelector:selector]) return -1;
    return ((NSInteger (*)(id, SEL))objc_msgSend)(service, selector);
#endif
}

static id MFGetService(void) {
#ifdef MF_SERVICE_TEST_HARNESS
    return nil;
#else
    return MFService();
#endif
}

static BOOL MFMutate(id service, BOOL registering, NSError **error) {
#ifdef MF_SERVICE_TEST_HARNESS
    (void)service;
    if (registering) return MFTestBridge.register_service ? MFTestBridge.register_service(error) : NO;
    return MFTestBridge.unregister_service ? MFTestBridge.unregister_service(error) : NO;
#else
    SEL selector = NSSelectorFromString(registering ? @"registerAndReturnError:" : @"unregisterAndReturnError:");
    if (!service || ![service respondsToSelector:selector]) return NO;
    return ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(service, selector, error);
#endif
}

static NSString *MFState(NSInteger status) {
    // SMAppServiceStatus values are stable ABI; unknown future values fail shut.
    switch (status) {
        case 0: return @"not_registered";
        case 1: return @"enabled";
        case 2: return @"requires_approval";
        case 3: return @"not_found";
        default: return @"unavailable";
    }
}

static BOOL MFSelfMatches(NSString *text) {
    SecRequirementRef requirement = NULL;
    SecCodeRef code = NULL;
    BOOL valid = NO;
    if (!text || SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement) != errSecSuccess) goto done;
    if (SecCodeCopySelf(kSecCSDefaultFlags, &code) != errSecSuccess) goto done;
    valid = SecCodeCheckValidity(code, kSecCSStrictValidate, requirement) == errSecSuccess;
done:
    if (requirement) CFRelease(requirement);
    if (code) CFRelease(code);
    return valid;
}

static BOOL MFStaticMatches(NSURL *url, NSString *text, BOOL nested, BOOL validateResources) {
    SecRequirementRef requirement = NULL;
    SecStaticCodeRef code = NULL;
    BOOL valid = NO;
    if (!text || SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement) != errSecSuccess) goto done;
    if (SecStaticCodeCreateWithPath((__bridge CFURLRef)url, kSecCSDefaultFlags, &code) != errSecSuccess) goto done;
    SecCSFlags flags = kSecCSStrictValidate | kSecCSCheckAllArchitectures;
    if (nested) flags |= kSecCSCheckNestedCode;
    if (!validateResources) flags |= kSecCSDoNotValidateResources;
    valid = SecStaticCodeCheckValidity(code, flags, requirement) == errSecSuccess;
done:
    if (requirement) CFRelease(requirement);
    if (code) CFRelease(code);
    return valid;
}

static BOOL MFRealPath(NSURL *url, BOOL directory, BOOL executable) {
    struct stat value;
    if (lstat(url.fileSystemRepresentation, &value) != 0 || S_ISLNK(value.st_mode)) return NO;
    if (directory ? !S_ISDIR(value.st_mode) : !S_ISREG(value.st_mode)) return NO;
    return !executable || (value.st_mode & 0111) != 0;
}

static BOOL MFChildPath(NSURL *bundle, NSString *relative, BOOL executable) {
    NSArray<NSString *> *parts = relative.pathComponents;
    NSURL *current = bundle;
    for (NSUInteger index = 0; index < parts.count; index++) {
        current = [current URLByAppendingPathComponent:parts[index]];
        if (!MFRealPath(current, index + 1 < parts.count, executable && index + 1 == parts.count)) return NO;
    }
    return YES;
}

static BOOL MFValidPlist(NSURL *url) {
    NSData *data = [NSData dataWithContentsOfURL:url];
    if (!data || data.length > 64 * 1024) return NO;
    id plist = [NSPropertyListSerialization propertyListWithData:data options:NSPropertyListImmutable format:NULL error:NULL];
    if (![plist isKindOfClass:NSDictionary.class]) return NO;
    NSDictionary *expected = @{
        @"Label": MFProductionMachServiceName(),
        @"BundleProgram": MFHelperProgram,
        @"ProgramArguments": @[MFHelperArgumentZero, @"--serve-production"],
        @"MachServices": @{MFProductionMachServiceName(): @YES}
    };
    // Exact reviewed schema. No alternate executable, user, session, argument,
    // environment, or additional service can be introduced through packaging.
    return [plist isEqual:expected];
}

static NSString *MFPreflight(BOOL registering, NSMutableDictionary *result) {
    NSDictionary *requirements = MFProductionConfiguredRequirements();
    if (!requirements || MFProductionConfiguredTeamIdentifier().length == 0) return @"production_team_unconfigured";
    if (!MFSelfMatches(requirements[@"client"])) return @"production_client_signature_requirement_failed";
    result[@"production_identity_valid"] = @YES;
    NSURL *bundle = NSBundle.mainBundle.bundleURL;
    if (!bundle || ![bundle.pathExtension isEqual:@"app"] || !MFRealPath(bundle, YES, NO)) return @"app_bundle_required";
    // The real running image, app seal and nested signatures must agree. The
    // bundle identifier in Info.plist alone is never authentication evidence.
    // Cleanup authenticates the current executable without requiring removed
    // helper resources to retain a valid outer seal. It still pins the daemon
    // descriptor below; it cannot unregister a caller-selected service.
    if (!MFStaticMatches(bundle, requirements[@"client"], registering, registering)) return @"app_bundle_signature_requirement_failed";
    NSString *plistRelative = [@"Contents/Library/LaunchDaemons" stringByAppendingPathComponent:MFServicePlist];
    if (!MFChildPath(bundle, plistRelative, NO)) return @"service_plist_missing_or_unsafe";
    if (!MFValidPlist([bundle URLByAppendingPathComponent:plistRelative])) return @"service_plist_invalid";
    if (registering) {
        if (!MFChildPath(bundle, MFHelperProgram, YES)) return @"embedded_helper_missing_or_unsafe";
        if (!MFStaticMatches([bundle URLByAppendingPathComponent:MFHelperProgram], requirements[@"helper"], NO, YES)) return @"embedded_helper_signature_requirement_failed";
    }
    result[@"package_valid"] = @YES;
    return nil;
}

static char *MFEncode(NSDictionary *result) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingSortedKeys error:NULL];
    if (!data) return strdup("{\"result\":\"error\",\"reason\":\"json_encoding_failed\",\"hardware_write_enabled\":false}");
    char *bytes = malloc(data.length + 1);
    if (!bytes) return NULL;
    memcpy(bytes, data.bytes, data.length);
    bytes[data.length] = 0;
    return bytes;
}

static char *MFPerform(NSString *operation) {
    @autoreleasepool {
        BOOL supported = MFSupported();
        NSMutableDictionary *result = [@{
            @"schema_version": @1, @"operation": operation,
            @"platform_supported": supported ? @YES : @NO, @"minimum_macos": @"13.0",
            @"service_name": MFProductionMachServiceName(),
            @"service_state": @"unavailable", @"registration_enabled": @NO,
            @"requires_approval": @NO, @"production_identity_valid": @NO,
            @"package_valid": @NO, @"production_ready": @NO,
            @"hardware_write_enabled": @NO, @"result": @"blocked"
        } mutableCopy];
        if (!supported) {
            result[@"result"] = @"unsupported";
            result[@"reason"] = @"macos_13_required";
            return MFEncode(result);
        }
        id service = MFGetService();
        NSInteger status = MFStatus(service);
        result[@"service_state"] = MFState(status);
        result[@"registration_enabled"] = status == 1 ? @YES : @NO;
        result[@"requires_approval"] = status == 2 ? @YES : @NO;
        BOOL registering = [operation isEqual:@"register"];
        NSString *blocked = MFPreflight(registering || [operation isEqual:@"status"], result);
        if ([operation isEqual:@"status"]) {
            result[@"result"] = @"observed";
            result[@"reason"] = blocked ?: ((status < 0 || status > 3) ? @"service_management_unavailable" : @"registration_is_not_authenticated_readiness");
            return MFEncode(result);
        }
        if (blocked) {
            result[@"reason"] = blocked;
            return MFEncode(result);
        }
        if (status < 0 || status > 3) {
            result[@"reason"] = @"service_management_unavailable";
            return MFEncode(result);
        }
        NSError *error = nil;
        BOOL success = MFMutate(service, registering, &error);
        status = MFStatus(service);
        result[@"service_state"] = MFState(status);
        result[@"registration_enabled"] = status == 1 ? @YES : @NO;
        result[@"requires_approval"] = status == 2 ? @YES : @NO;
        result[@"result"] = success ? @"succeeded" : @"error";
        result[@"reason"] = success ? (registering ? @"registration_requested" : @"unregistration_requested") : @"service_management_operation_failed";
        if (error) {
            result[@"error_domain"] = [error.domain substringToIndex:MIN(error.domain.length, 256u)];
            result[@"error_code"] = @(error.code);
            NSString *message = error.localizedDescription;
            result[@"error_message"] = [message substringToIndex:MIN(message.length, 2048u)];
        }
        return MFEncode(result);
    }
}

// Exception and contention replies use only libc, including from the catch
// path: no exception detail or Foundation serializer can escape the C ABI.
static char *MFBoundaryReply(const char *operation, BOOL busy) {
    char json[512];
    int length = snprintf(json, sizeof(json),
        "{\"schema_version\":1,\"operation\":\"%s\",\"hardware_write_enabled\":false,"
        "\"production_ready\":false,\"result\":\"%s\",\"reason\":\"%s\"}",
        operation, busy ? "blocked" : "error",
        busy ? "service_operation_busy" : "service_management_exception");
    if (length < 0 || (size_t)length >= sizeof(json)) return NULL;
    return strdup(json);
}

static char *MFInvoke(const char *operation) {
    @autoreleasepool {
        static NSLock *lock;
        static dispatch_once_t once;
        BOOL acquired = NO;
        @try {
            dispatch_once(&once, ^{ lock = [NSLock new]; });
            // Serialize observations with mutations too. Reentrant callbacks
            // and concurrent callers fail promptly instead of deadlocking.
            if (![lock tryLock]) return MFBoundaryReply(operation, YES);
            acquired = YES;
            return MFPerform([NSString stringWithUTF8String:operation]);
        } @catch (NSException *exception) {
            (void)exception;
            return MFBoundaryReply(operation, NO);
        } @finally {
            if (acquired) [lock unlock];
        }
    }
}

char *mf_service_status(void) { return MFInvoke("status"); }
char *mf_service_register(void) { return MFInvoke("register"); }
char *mf_service_unregister(void) { return MFInvoke("unregister"); }
void mf_service_free(char *json) { free(json); }

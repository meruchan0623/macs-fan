#import "ProductionIdentity.h"
#import <Security/Security.h>

// Independent of the original vendor. Keep these as reviewed source constants.
static NSString * const MFProductionClientID = @"local.macsfan.desktop";
static NSString * const MFProductionHelperID = @"local.macsfan.helper.control";
// Deliberately unconfigured: do not fill from IPC, Info.plist, environment,
// client-supplied Team ID, or credentials discovered on the host.
static NSString * const MFProductionTeamID = @"";

static NSDictionary *MFProductionTemplate(NSString *teamID) {
    if (![teamID isKindOfClass:NSString.class]) return nil;
    NSRegularExpression *format = [NSRegularExpression regularExpressionWithPattern:@"^[A-Z0-9]{10}$" options:0 error:NULL];
    if ([format numberOfMatchesInString:teamID options:0 range:NSMakeRange(0, teamID.length)] != 1) return nil;
    NSMutableDictionary *requirements = [NSMutableDictionary new];
    for (NSString *role in @[@"client", @"helper"]) {
        NSString *identifier = [role isEqual:@"client"] ? MFProductionClientID : MFProductionHelperID;
        NSString *text = [NSString stringWithFormat:@"anchor apple generic and identifier \"%@\" and certificate leaf[subject.OU] = \"%@\"", identifier, teamID];
        SecRequirementRef compiled = NULL;
        OSStatus status = SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &compiled);
        if (compiled) CFRelease(compiled);
        if (status != errSecSuccess) return nil;
        requirements[role] = text;
    }
    return [requirements copy];
}

NSString *MFProductionClientBundleIdentifier(void) { return MFProductionClientID; }
NSString *MFProductionHelperBundleIdentifier(void) { return MFProductionHelperID; }
NSString *MFProductionMachServiceName(void) { return @"local.macsfan.helper.control"; }
NSString *MFProductionConfiguredTeamIdentifier(void) { return MFProductionTeamID; }
NSDictionary *MFProductionConfiguredRequirements(void) {
    return MFProductionTemplate(MFProductionTeamID);
}

static BOOL MFSelfSatisfies(NSString *text) {
    SecRequirementRef requirement = NULL;
    SecCodeRef selfCode = NULL;
    BOOL valid = NO;
    if (SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &requirement) != errSecSuccess || !requirement) goto finish;
    if (SecCodeCopySelf(kSecCSDefaultFlags, &selfCode) != errSecSuccess || !selfCode) goto finish;
    valid = SecCodeCheckValidity(selfCode, kSecCSDefaultFlags, requirement) == errSecSuccess;
finish:
    if (requirement) CFRelease(requirement);
    if (selfCode) CFRelease(selfCode);
    return valid;
}

NSDictionary *MFProductionIdentityCheck(void) {
    NSDictionary *requirements = MFProductionConfiguredRequirements();
    BOOL valid = requirements && MFSelfSatisfies(requirements[@"helper"]);
    NSString *reason = !requirements ? @"production_team_unconfigured" :
        (!valid ? @"production_signature_requirement_failed" : @"identity_valid_listener_not_started");
    // This is a validation prerequisite, never permission to open hardware.
    return @{@"production_identity_valid":valid ? @YES : @NO,
        @"production_channel_available":@NO, @"production_listener_resumed":@NO,
        @"production_control_ready":@NO, @"identity_check_only":@YES,
        @"hardware_write_enabled":@NO, @"mach_service_name":MFProductionMachServiceName(),
        @"client_bundle_id":MFProductionClientID, @"helper_bundle_id":MFProductionHelperID,
        @"reason":reason};
}

#ifdef MF_TEST_HARNESS
NSDictionary *MFProductionIdentityProbe(void) {
    // A syntax fixture, not a real account/team and never the configured ID.
    NSDictionary *example = MFProductionTemplate(@"ABCDE12345");
    BOOL invalidRejected = YES;
    for (NSString *bad in @[@"", @"short", @"abcdefghij", @"ABCDE12345\" or true"]) {
        if (MFProductionTemplate(bad)) invalidRejected = NO;
    }
    return @{@"template_compiles":example ? @YES : @NO,
        @"invalid_team_inputs_rejected":@(invalidRejected),
        @"adhoc_self_rejected":example && !MFSelfSatisfies(example[@"helper"]) ? @YES : @NO,
        @"client_requirement":example[@"client"] ?: @"",
        @"helper_requirement":example[@"helper"] ?: @"",
        @"configured_identity":MFProductionIdentityCheck()};
}
#endif

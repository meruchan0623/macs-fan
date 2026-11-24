#include "DiskTemperature.h"
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/storage/ata/ATASMARTLib.h>
#include <IOKit/storage/nvme/NVMeSMARTLibExternal.h>
#include <string.h>

_Static_assert(sizeof(ATASMARTData) == 512, "SDK ATA data");
_Static_assert(sizeof(NVMeSMARTData) == 512, "SDK NVMe data");
_Static_assert(offsetof(NVMeSMARTData, TEMPERATURE) == 1, "SDK NVMe temperature");
/* Internal dependency seam: tests supply fake COM interfaces; production always uses SDK. */
typedef struct {
    kern_return_t (*create)(io_service_t, CFUUIDRef, CFUUIDRef, IOCFPlugInInterface ***, SInt32 *);
    IOReturn (*destroy)(IOCFPlugInInterface **);
} DiskAPI;
static const DiskAPI sdk_api = { IOCreatePlugInInterfaceForService, IODestroyPlugInInterface };

static int boolean_true(io_service_t service, CFStringRef key) {
    CFTypeRef value = IORegistryEntryCreateCFProperty(service, key, kCFAllocatorDefault, 0);
    int yes = value && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value);
    if (value) CFRelease(value);
    return yes;
}
static void model_name(io_service_t service, char output[128]) {
    CFTypeRef value = IORegistryEntryCreateCFProperty(service, CFSTR("Device Characteristics"), kCFAllocatorDefault, 0);
    if (value && CFGetTypeID(value) == CFDictionaryGetTypeID()) {
        CFTypeRef name = CFDictionaryGetValue(value, CFSTR("Product Name"));
        if (name && CFGetTypeID(name) == CFStringGetTypeID()) {
            if (!CFStringGetCString(name, output, 128, kCFStringEncodingUTF8)) output[0] = 0;
        }
    }
    if (value) CFRelease(value);
}
static void read_service(io_service_t service, MFDiskReading *out, const DiskAPI *api) {
    IOCFPlugInInterface **plugin = NULL;
    IOATASMARTInterface **ata = NULL;
    IONVMeSMARTInterface **nvme = NULL;
    SInt32 score = 0;
    out->stage = 1;
    out->validation_status = (int32_t)kIOReturnNotReady;
    out->status = api->create(service, out->transport == 2 ? kIONVMeSMARTUserClientTypeID : kIOATASMARTUserClientTypeID,
        kIOCFPlugInInterfaceID, &plugin, &score);
    if (out->status != kIOReturnSuccess) goto cleanup;
    if (!plugin || !*plugin || !(*plugin)->QueryInterface) { out->status = (int32_t)kIOReturnBadArgument; goto cleanup; }
    out->stage = 2;
    HRESULT query = (*plugin)->QueryInterface(plugin,
        CFUUIDGetUUIDBytes(out->transport == 2 ? kIONVMeSMARTInterfaceID : kIOATASMARTInterfaceID),
        out->transport == 2 ? (void **)&nvme : (void **)&ata);
    out->status = (int32_t)query;
    if (FAILED(query)) goto cleanup;
    if (out->transport == 2) {
        if (!nvme || !*nvme || !(*nvme)->SMARTReadData || !(*nvme)->Release) { out->status = (int32_t)kIOReturnBadArgument; goto cleanup; }
        NVMeSMARTData data = {0};
        out->stage = 3;
        out->status = (*nvme)->SMARTReadData(nvme, &data);
        if (out->status == kIOReturnSuccess) {
            memcpy(out->raw, &data, sizeof(data)); out->raw_valid = 1; out->stage = 5;
        }
    } else {
        if (!ata || !*ata || !(*ata)->SMARTReadData || !(*ata)->SMARTValidateReadData || !(*ata)->Release) { out->status = (int32_t)kIOReturnBadArgument; goto cleanup; }
        ATASMARTData data = {0};
        out->stage = 3;
        out->status = (*ata)->SMARTReadData(ata, &data);
        if (out->status == kIOReturnSuccess) {
            out->stage = 4;
            out->validation_status = (*ata)->SMARTValidateReadData(ata, &data);
            if (out->validation_status == kIOReturnSuccess) {
                memcpy(out->raw, &data, sizeof(data)); out->raw_valid = 1; out->stage = 5;
            }
        }
    }
cleanup:
    if (ata && *ata && (*ata)->Release) (*ata)->Release(ata);
    if (nvme && *nvme && (*nvme)->Release) (*nvme)->Release(nvme);
    if (plugin) api->destroy(plugin);
}
/* False means stop enumeration; success+NULL is an empty successful result. */
static int enumeration_ready(MFDiskBatch *out, io_iterator_t iterator) {
    if (out->enumeration_status != kIOReturnSuccess || !iterator) {
        if (iterator) IOObjectRelease(iterator);
        return 0;
    }
    return 1;
}
int32_t mf_disk_sample(MFDiskBatch *out, uint32_t capacity) {
    if (!out || capacity != sizeof(*out)) return (int32_t)kIOReturnBadArgument;
    memset(out, 0, sizeof(*out));
    out->abi_version = MF_DISK_ABI_VERSION; out->size = sizeof(*out);
    out->reading_size = sizeof(MFDiskReading); out->supported = 1;
    CFMutableDictionaryRef matching = IOServiceMatching("IOBlockStorageDevice");
    if (!matching) { out->enumeration_status = (int32_t)kIOReturnNoMemory; return 0; }
    io_iterator_t iterator = IO_OBJECT_NULL;
    out->enumeration_status = IOServiceGetMatchingServices(kIOMasterPortDefault, matching, &iterator);
    if (!enumeration_ready(out, iterator)) return 0;
    io_service_t service;
    while ((service = IOIteratorNext(iterator))) {
        if (out->service_count >= MF_DISK_MAX_SERVICES) { out->truncated = 1; IOObjectRelease(service); break; }
        out->service_count++;
        uint32_t transport = boolean_true(service, CFSTR("NVMe SMART Capable")) ? 2 :
            boolean_true(service, CFSTR("SMART Capable")) ? 1 : 0;
        if (transport) {
            uint64_t identity = 0;
            if (IORegistryEntryGetRegistryEntryID(service, &identity) != kIOReturnSuccess || !identity) out->truncated = 1;
            else {
                int duplicate = 0;
                for (uint32_t i = 0; i < out->reading_count; i++) if (out->readings[i].registry_id == identity) duplicate = 1;
                if (!duplicate) {
                    if (out->reading_count >= MF_DISK_MAX_READINGS) out->truncated = 1;
                    else {
                        MFDiskReading *record = &out->readings[out->reading_count++];
                        record->registry_id = identity; record->transport = transport;
                        model_name(service, record->model);
                        read_service(service, record, &sdk_api);
                    }
                }
            }
        }
        IOObjectRelease(service);
    }
    if (!IOIteratorIsValid(iterator)) { out->truncated = 1; out->enumeration_status = (int32_t)kIOReturnOffline; }
    IOObjectRelease(iterator);
    return 0;
}
#ifdef MF_DISK_TEST_MAIN
#include <assert.h>
/* Injected open failure must never expose partially filled read bytes. */
static kern_return_t fail_open(io_service_t s, CFUUIDRef a, CFUUIDRef b, IOCFPlugInInterface ***p, SInt32 *v) {
    (void)s; (void)a; (void)b; (void)p; (void)v; return kIOReturnNotPrivileged;
}
static int released, destroyed, query_fails;
static IONVMeSMARTInterface fake_smart_table;
static IONVMeSMARTInterface *fake_smart = &fake_smart_table;
static IOCFPlugInInterface fake_plugin_table;
static IOCFPlugInInterface *fake_plugin = &fake_plugin_table;
static ULONG fake_release(void *self) { (void)self; released++; return 0; }
static IOReturn fake_read(void *self, NVMeSMARTData *data) {
    (void)self; memset(data, 0xff, sizeof(*data)); return kIOReturnOffline;
}
static HRESULT fake_query(void *self, REFIID uuid, LPVOID *output) {
    (void)self; (void)uuid; if (query_fails) return E_NOINTERFACE; *output = &fake_smart; return S_OK;
}
static kern_return_t fake_create(io_service_t s, CFUUIDRef a, CFUUIDRef b, IOCFPlugInInterface ***p, SInt32 *v) {
    (void)s; (void)a; (void)b; (void)v; *p = &fake_plugin; return kIOReturnSuccess;
}
static IOReturn fake_destroy(IOCFPlugInInterface **p) { (void)p; destroyed++; return kIOReturnSuccess; }
int main(void) {
    MFDiskReading reading = {0}; reading.transport = 2;
    DiskAPI fake = {fail_open, IODestroyPlugInInterface};
    read_service(0, &reading, &fake);
    assert(reading.stage == 1 && reading.status == (int32_t)kIOReturnNotPrivileged && !reading.raw_valid);
    for (unsigned i=0;i<512;i++) assert(reading.raw[i] == 0);
    memset(&reading, 0, sizeof(reading)); reading.transport = 2;
    fake_plugin_table.QueryInterface = fake_query;
    fake_smart_table.Release = fake_release;
    fake_smart_table.SMARTReadData = fake_read;
    fake.create = fake_create; fake.destroy = fake_destroy;
    read_service(0, &reading, &fake);
    assert(reading.stage == 3 && reading.status == (int32_t)kIOReturnOffline && !reading.raw_valid);
    assert(released == 1 && destroyed == 1);
    for (unsigned i=0;i<512;i++) assert(reading.raw[i] == 0);
    memset(&reading, 0, sizeof(reading)); reading.transport = 2; query_fails = 1;
    read_service(0, &reading, &fake);
    assert(reading.stage == 2 && reading.status == (int32_t)E_NOINTERFACE && !reading.raw_valid);
    assert(released == 1 && destroyed == 2);
    assert(mf_disk_sample(NULL, sizeof(MFDiskBatch)) != 0);
    MFDiskBatch batch = {0};
    assert(!enumeration_ready(&batch, IO_OBJECT_NULL));
    assert(batch.enumeration_status == kIOReturnSuccess && batch.reading_count == 0 && batch.service_count == 0);
    batch.enumeration_status = (int32_t)kIOReturnNotPrivileged;
    assert(!enumeration_ready(&batch, IO_OBJECT_NULL));
    assert(batch.enumeration_status == (int32_t)kIOReturnNotPrivileged);
    assert(mf_disk_sample(&batch, sizeof(batch)-1) != 0);
    return 0;
}
#endif

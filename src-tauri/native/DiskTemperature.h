#ifndef MF_DISK_TEMPERATURE_H
#define MF_DISK_TEMPERATURE_H
#include <stdint.h>
#include <stddef.h>
#define MF_DISK_ABI_VERSION 1u
#define MF_DISK_MAX_READINGS 16u
#define MF_DISK_MAX_SERVICES 256u
/* transport: 1 ATA, 2 NVMe; stage: 1 open, 2 query, 3 read, 4 validate, 5 complete */
typedef struct {
    uint64_t registry_id;
    uint32_t transport, stage;
    int32_t status, validation_status;
    uint32_t raw_valid, reserved;
    char model[128];
    uint8_t raw[512];
} MFDiskReading;
typedef struct {
    uint32_t abi_version, size, reading_size, supported;
    uint32_t service_count, reading_count, truncated;
    int32_t enumeration_status;
    MFDiskReading readings[MF_DISK_MAX_READINGS];
} MFDiskBatch;
_Static_assert(sizeof(MFDiskReading) == 672, "disk reading ABI");
_Static_assert(offsetof(MFDiskReading, model) == 32, "model ABI");
_Static_assert(offsetof(MFDiskReading, raw) == 160, "raw ABI");
_Static_assert(sizeof(MFDiskBatch) == 10784, "disk batch ABI");
_Static_assert(offsetof(MFDiskBatch, readings) == 32, "batch ABI");
int32_t mf_disk_sample(MFDiskBatch *output, uint32_t capacity);
#endif

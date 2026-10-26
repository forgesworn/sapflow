#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "../../include/SapflowFFI.h"

static void put_u64(uint8_t *out, uint64_t value) {
    for (unsigned i = 0; i < 8; i++) out[i] = (uint8_t)(value >> (i * 8));
}

int main(void) {
    assert(sapflow_abi_version() == SAPFLOW_ABI_VERSION);
    uint8_t input[11] = {3, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3};
    SapflowBuffer created = {0}, error = {0};
    assert(sapflow_call(2, NULL, input, sizeof(input), &created, &error) == 0);
    assert(created.len == 8);
    uint64_t raw;
    memcpy(&raw, created.ptr, 8);
    sapflow_buffer_free(created);
    void *handle = (void *)(uintptr_t)raw;
    SapflowBuffer length = {0};
    assert(sapflow_call(3, handle, NULL, 0, &length, &error) == 0);
    uint64_t value;
    memcpy(&value, length.ptr, 8);
    assert(value == 3);
    sapflow_buffer_free(length);
    uint8_t index[8];
    put_u64(index, 1);
    SapflowBuffer byte = {0};
    assert(sapflow_call(5, handle, index, sizeof(index), &byte, &error) == 0);
    assert(byte.len == 1 && byte.ptr[0] == 2);
    sapflow_buffer_free(byte);
    sapflow_object_free(SAPFLOW_OBJECT_ENCODED_BUFFER, handle);

    uint8_t direct_input[5] = {9, 1, 2, 3, 9};
    assert(sapflow_encoded_buffer_new(direct_input + 1, 3, &handle, &error) == 0);
    memset(direct_input, 0, sizeof(direct_input));
    assert(sapflow_encoded_buffer_scalar(handle, SAPFLOW_OP_ENCODED_BUFFER_LEN, 0,
        &value, &error) == 0);
    assert(value == 3);
    assert(sapflow_encoded_buffer_scalar(handle, SAPFLOW_OP_ENCODED_BUFFER_BYTE_AT, 1,
        &value, &error) == 0);
    assert(value == 2);
    sapflow_object_free(SAPFLOW_OBJECT_ENCODED_BUFFER, handle);

    uint8_t codec_input[10] = {2, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x88};
    assert(sapflow_call(SAPFLOW_OP_MEDIA_CODEC_AAC, NULL, codec_input,
        sizeof(codec_input), &created, &error) == 0);
    memcpy(&raw, created.ptr, 8);
    sapflow_buffer_free(created);
    void *codec = (void *)(uintptr_t)raw;
    uint8_t payload[5] = {9, 1, 2, 3, 9};
    void *sample = NULL;
    SapflowMediaSampleResult metadata = {0};
    assert(sapflow_media_codec_sample(codec, payload + 1, 3, 0, 0, 1024,
        48000, false, &sample, &error) == 0);
    memset(payload, 0, sizeof(payload));
    assert(sample != NULL);
    assert(sapflow_media_sample_metadata(sample, &metadata, &error) == 0);
    assert(metadata.len == 3 && metadata.pts == 0 && metadata.dts == 0);
    assert(metadata.duration == 1024 && metadata.timescale == 48000);
    assert(!metadata.is_video && !metadata.is_keyframe);
    sapflow_object_free(SAPFLOW_OBJECT_MEDIA_SAMPLE, sample);
    sapflow_object_free(SAPFLOW_OBJECT_MEDIA_CODEC, codec);
    puts("C ABI smoke passed");
    return 0;
}

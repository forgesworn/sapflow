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
    puts("C ABI smoke passed");
    return 0;
}

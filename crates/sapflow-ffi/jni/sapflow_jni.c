#include <jni.h>
#include <stdint.h>
#include <stdlib.h>

#include "../include/SapflowFFI.h"

#define JNI_NAME(name) Java_dev_forgesworn_sapflow_Native_##name

static void throw_native(JNIEnv *env, SapflowBuffer error) {
    jclass cls = (*env)->FindClass(env, "dev/forgesworn/sapflow/NativeException");
    if (cls == NULL) {
        sapflow_buffer_free(error);
        return;
    }
    jmethodID constructor = (*env)->GetMethodID(env, cls, "<init>", "(Ljava/lang/String;)V");
    jstring message = NULL;
    jobject exception = NULL;
    char *utf = malloc(error.len + 1);
    if (utf == NULL) {
        (*env)->DeleteLocalRef(env, cls);
        sapflow_buffer_free(error);
        (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/OutOfMemoryError"), "cannot allocate native error");
        return;
    }
    for (size_t i = 0; i < error.len; i++) utf[i] = (char)error.ptr[i];
    utf[error.len] = '\0';
    sapflow_buffer_free(error);
    if (constructor != NULL) message = (*env)->NewStringUTF(env, utf);
    free(utf);
    if (message != NULL) exception = (*env)->NewObject(env, cls, constructor, message);
    if (exception != NULL) (*env)->Throw(env, (jthrowable)exception);
    if (exception != NULL) (*env)->DeleteLocalRef(env, exception);
    if (message != NULL) (*env)->DeleteLocalRef(env, message);
    (*env)->DeleteLocalRef(env, cls);
}

JNIEXPORT jint JNICALL JNI_NAME(abiVersion)(JNIEnv *env, jclass cls) {
    (void)env; (void)cls;
    return (jint)sapflow_abi_version();
}

static int valid_input(JNIEnv *env, jlong address, jlong len) {
    if (len < 0 || (len > 0 && address == 0)) {
        (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/IllegalArgumentException"), "invalid direct buffer address or length");
        return 0;
    }
    return 1;
}

static jlong handle_result(JNIEnv *env, int32_t rc, void *out, SapflowBuffer error) {
    if (rc != 0) {
        throw_native(env, error);
        return 0;
    }
    return (jlong)(intptr_t)out;
}

JNIEXPORT jlong JNICALL JNI_NAME(directBufferAddress)(JNIEnv *env, jclass cls, jobject buffer) {
    (void)cls;
    return (jlong)(intptr_t)(*env)->GetDirectBufferAddress(env, buffer);
}

JNIEXPORT jlong JNICALL JNI_NAME(encodedBufferNew)(JNIEnv *env, jclass cls, jlong address, jlong len) {
    (void)cls;
    if (!valid_input(env, address, len)) return 0;
    void *out = NULL;
    SapflowBuffer error = {0};
    int32_t rc = sapflow_encoded_buffer_new((const uint8_t *)(intptr_t)address, (size_t)len, &out, &error);
    return handle_result(env, rc, out, error);
}

JNIEXPORT jlong JNICALL JNI_NAME(encodedBufferScalar)(JNIEnv *env, jclass cls,
    jlong buffer, jint operation, jlong index) {
    (void)cls;
    uint64_t out = 0;
    SapflowBuffer error = {0};
    int32_t rc = sapflow_encoded_buffer_scalar((const void *)(intptr_t)buffer,
        (uint32_t)operation, (uint64_t)index, &out, &error);
    if (rc != 0) {
        throw_native(env, error);
        return 0;
    }
    return (jlong)out;
}

JNIEXPORT jlong JNICALL JNI_NAME(mediaCodecSample)(JNIEnv *env, jclass cls,
    jlong codec, jlong address, jlong len, jlong pts, jlong dts,
    jint duration, jint timescale, jboolean keyframe) {
    (void)cls;
    if (!valid_input(env, address, len)) return 0;
    void *out = NULL;
    SapflowBuffer error = {0};
    int32_t rc = sapflow_media_codec_sample((const void *)(intptr_t)codec,
        (const uint8_t *)(intptr_t)address, (size_t)len, pts, dts,
        (uint32_t)duration, (uint32_t)timescale, keyframe, &out, &error);
    return handle_result(env, rc, out, error);
}

JNIEXPORT jlong JNICALL JNI_NAME(mediaSampleScalar)(JNIEnv *env, jclass cls,
    jlong sample, jint operation) {
    (void)cls;
    uint64_t out = 0;
    SapflowBuffer error = {0};
    int32_t rc = sapflow_media_sample_scalar((const void *)(intptr_t)sample,
        (uint32_t)operation, &out, &error);
    if (rc != 0) {
        throw_native(env, error);
        return 0;
    }
    return (jlong)out;
}

JNIEXPORT jlongArray JNICALL JNI_NAME(sampleFanoutPush)(JNIEnv *env, jclass cls,
    jlong fanout, jlong sample, jlong now_ns, jboolean aac_views) {
    (void)cls;
    SapflowFanoutOutcome out = {0};
    SapflowBuffer error = {0};
    int32_t rc;
    if (aac_views) {
        rc = sapflow_sample_fanout_push_aac_timing_views((const void *)(intptr_t)fanout,
            (const void *)(intptr_t)sample, (uint64_t)now_ns, &out, &error);
    } else {
        rc = sapflow_sample_fanout_push((const void *)(intptr_t)fanout,
            (const void *)(intptr_t)sample, (uint64_t)now_ns, &out, &error);
    }
    if (rc != 0) {
        throw_native(env, error);
        return NULL;
    }
    jlong values[] = {out.publishing_queued, out.recording_queued,
        out.publishing_waiting_keyframe, (jlong)out.publishing_overflows, out.recording_failed};
    jlongArray result = (*env)->NewLongArray(env, 5);
    if (result != NULL) {
        (*env)->SetLongArrayRegion(env, result, 0, 5, values);
        if ((*env)->ExceptionCheck(env)) {
            (*env)->DeleteLocalRef(env, result);
            result = NULL;
        }
    }
    return result;
}

JNIEXPORT jlong JNICALL JNI_NAME(sampleFanoutPop)(JNIEnv *env, jclass cls,
    jlong fanout, jboolean recording, jlong now_ns) {
    (void)cls;
    void *out = NULL;
    SapflowBuffer error = {0};
    int32_t rc = sapflow_sample_fanout_pop((const void *)(intptr_t)fanout,
        recording, (uint64_t)now_ns, &out, &error);
    return handle_result(env, rc, out, error);
}

JNIEXPORT jbyteArray JNICALL JNI_NAME(call)(JNIEnv *env, jclass cls,
    jint operation, jlong handle, jbyteArray input) {
    (void)cls;
    if (input == NULL) {
        (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/IllegalArgumentException"), "input must not be null");
        return NULL;
    }
    jsize len = (*env)->GetArrayLength(env, input);
    jbyte *elements = len == 0 ? NULL : (*env)->GetByteArrayElements(env, input, NULL);
    if (len != 0 && elements == NULL) return NULL;
    SapflowBuffer out = {0}, error = {0};
    int32_t rc = sapflow_call((uint32_t)operation, (const void *)(intptr_t)handle,
        (const uint8_t *)elements, (size_t)len, &out, &error);
    if (elements != NULL) (*env)->ReleaseByteArrayElements(env, input, elements, JNI_ABORT);
    if (rc != 0) {
        throw_native(env, error);
        return NULL;
    }
    if (out.len > INT32_MAX) {
        sapflow_buffer_free(out);
        (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/IllegalStateException"), "native response is too large");
        return NULL;
    }
    jbyteArray result = (*env)->NewByteArray(env, (jsize)out.len);
    if (result != NULL && out.len != 0) {
        (*env)->SetByteArrayRegion(env, result, 0, (jsize)out.len, (const jbyte *)out.ptr);
        if ((*env)->ExceptionCheck(env)) {
            (*env)->DeleteLocalRef(env, result);
            result = NULL;
        }
    }
    sapflow_buffer_free(out);
    return result;
}

JNIEXPORT void JNICALL JNI_NAME(objectFree)(JNIEnv *env, jclass cls, jint kind, jlong handle) {
    (void)env; (void)cls;
    sapflow_object_free((uint32_t)kind, (void *)(intptr_t)handle);
}

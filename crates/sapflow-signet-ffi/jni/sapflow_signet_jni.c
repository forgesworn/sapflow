#include <jni.h>
#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "../include/SignetAuthFFI.h"

#define JNI_NAME(name) Java_lab_signet_Native_##name

typedef struct {
    jbyteArray array;
    jbyte *elements;
    const uint8_t *ptr;
    size_t len;
} Input;

static void throw_code(JNIEnv *env, int32_t code) {
    const char *name;
    switch (code) {
        case SAPFLOW_SIGNET_INVALID_INPUT: name = "InvalidInput"; break;
        case SAPFLOW_SIGNET_INVALID_CREDENTIAL: name = "InvalidCredential"; break;
        case SAPFLOW_SIGNET_INVALID_PROOF: name = "InvalidProof"; break;
        case SAPFLOW_SIGNET_BUSY: name = "Busy"; break;
        case SAPFLOW_SIGNET_DEADLINE: name = "Deadline"; break;
        case SAPFLOW_SIGNET_CANCELLED: name = "Cancelled"; break;
        case SAPFLOW_SIGNET_REFUSED: name = "Refused"; break;
        case SAPFLOW_SIGNET_CLOSED: name = "Closed"; break;
        default: name = "Unavailable"; break;
    }
    if ((*env)->ExceptionCheck(env)) return;
    char class_name[64] = "lab/signet/SignetException$";
    size_t prefix = sizeof("lab/signet/SignetException$") - 1;
    size_t i = 0;
    while (name[i] != '\0' && prefix + i + 1 < sizeof(class_name)) {
        class_name[prefix + i] = name[i];
        i++;
    }
    class_name[prefix + i] = '\0';
    jclass cls = (*env)->FindClass(env, class_name);
    if (cls == NULL) return;
    jmethodID constructor = (*env)->GetMethodID(env, cls, "<init>", "()V");
    jobject exception = constructor == NULL ? NULL : (*env)->NewObject(env, cls, constructor);
    if (exception != NULL) (*env)->Throw(env, (jthrowable)exception);
    if (exception != NULL) (*env)->DeleteLocalRef(env, exception);
    (*env)->DeleteLocalRef(env, cls);
}

static int borrow_input(JNIEnv *env, jbyteArray array, Input *input) {
    input->array = array;
    input->elements = NULL;
    input->ptr = NULL;
    input->len = 0;
    if (array == NULL) {
        throw_code(env, SAPFLOW_SIGNET_INVALID_INPUT);
        return 0;
    }
    jsize len = (*env)->GetArrayLength(env, array);
    input->len = (size_t)len;
    if (len == 0) return 1;
    input->elements = (*env)->GetByteArrayElements(env, array, NULL);
    if (input->elements == NULL) return 0;
    input->ptr = (const uint8_t *)input->elements;
    return 1;
}

static void release_input(JNIEnv *env, Input *input) {
    if (input->elements != NULL) {
        (*env)->ReleaseByteArrayElements(env, input->array, input->elements, JNI_ABORT);
    }
}

static jbyteArray copy_buffer(JNIEnv *env, SapflowSignetBuffer buffer) {
    if (buffer.len > INT32_MAX || (buffer.len != 0 && buffer.ptr == NULL)) {
        throw_code(env, SAPFLOW_SIGNET_UNAVAILABLE);
        return NULL;
    }
    jbyteArray result = (*env)->NewByteArray(env, (jsize)buffer.len);
    if (result == NULL) return NULL;
    if (buffer.len != 0) {
        (*env)->SetByteArrayRegion(env, result, 0, (jsize)buffer.len, (const jbyte *)buffer.ptr);
        if ((*env)->ExceptionCheck(env)) {
            (*env)->DeleteLocalRef(env, result);
            return NULL;
        }
    }
    return result;
}

static jbyteArray buffer_result(JNIEnv *env, int32_t rc, SapflowSignetBuffer out, int secret) {
    if (rc != 0) {
        throw_code(env, rc);
        return NULL;
    }
    jbyteArray result = copy_buffer(env, out);
    if (secret) sapflow_signet_secret_free(out);
    else sapflow_signet_buffer_free(out);
    return result;
}

static jlong handle_result(JNIEnv *env, int32_t rc, void *out) {
    if (rc != 0) {
        throw_code(env, rc);
        return 0;
    }
    return (jlong)(intptr_t)out;
}

static void unit_result(JNIEnv *env, int32_t rc) {
    if (rc != 0) throw_code(env, rc);
}

static jobject box_long(JNIEnv *env, jlong value) {
    jclass cls = (*env)->FindClass(env, "java/lang/Long");
    if (cls == NULL) return NULL;
    jmethodID method = (*env)->GetStaticMethodID(env, cls, "valueOf", "(J)Ljava/lang/Long;");
    jobject result = method == NULL ? NULL : (*env)->CallStaticObjectMethod(env, cls, method, value);
    (*env)->DeleteLocalRef(env, cls);
    return result;
}

static jobject box_int(JNIEnv *env, jint value) {
    jclass cls = (*env)->FindClass(env, "java/lang/Integer");
    if (cls == NULL) return NULL;
    jmethodID method = (*env)->GetStaticMethodID(env, cls, "valueOf", "(I)Ljava/lang/Integer;");
    jobject result = method == NULL ? NULL : (*env)->CallStaticObjectMethod(env, cls, method, value);
    (*env)->DeleteLocalRef(env, cls);
    return result;
}

static int set_element(JNIEnv *env, jobjectArray array, jsize index, jobject value) {
    (*env)->SetObjectArrayElement(env, array, index, value);
    return !(*env)->ExceptionCheck(env);
}

static jstring copy_text(JNIEnv *env, SapflowSignetBuffer buffer) {
    if (buffer.len == SIZE_MAX || (buffer.len != 0 && buffer.ptr == NULL)) {
        throw_code(env, SAPFLOW_SIGNET_UNAVAILABLE);
        return NULL;
    }
    char *text = malloc(buffer.len + 1);
    if (text == NULL) return NULL;
    if (buffer.len != 0) memcpy(text, buffer.ptr, buffer.len);
    text[buffer.len] = '\0';
    jstring result = (*env)->NewStringUTF(env, text);
    free(text);
    return result;
}

static jobjectArray event_result(JNIEnv *env, int32_t rc, SapflowSignetEvent out) {
    if (rc != 0) {
        throw_code(env, rc);
        return NULL;
    }
    if (out.tag == SAPFLOW_SIGNET_EVENT_NONE) {
        sapflow_signet_event_free(out);
        return NULL;
    }
    jclass cls = (*env)->FindClass(env, "java/lang/Object");
    jobjectArray result = NULL;
    jobject tag = NULL;
    jobject id = NULL;
    jobject third = NULL;
    jobject fourth = NULL;
    jstring persona = NULL;
    jsize count = out.tag == SAPFLOW_SIGNET_EVENT_READY ? 4 : 3;
    if (cls != NULL) {
        result = (*env)->NewObjectArray(env, count, cls, NULL);
        (*env)->DeleteLocalRef(env, cls);
    }
    if (result == NULL) goto done;
    tag = box_int(env, (jint)out.tag);
    id = box_long(env, (jlong)out.request_id);
    if (out.tag == SAPFLOW_SIGNET_EVENT_READY) {
        persona = copy_text(env, out.bytes);
        third = persona;
        fourth = box_long(env, (jlong)(intptr_t)out.credential);
    } else if (out.tag == SAPFLOW_SIGNET_EVENT_PROOF) {
        third = copy_buffer(env, out.bytes);
    } else if (out.tag == SAPFLOW_SIGNET_EVENT_FAILED) {
        third = box_int(env, out.error);
    }
    if (tag == NULL || id == NULL || third == NULL ||
        (out.tag == SAPFLOW_SIGNET_EVENT_READY && fourth == NULL) ||
        !set_element(env, result, 0, tag) ||
        !set_element(env, result, 1, id) ||
        !set_element(env, result, 2, third) ||
        (out.tag == SAPFLOW_SIGNET_EVENT_READY && !set_element(env, result, 3, fourth))) {
        (*env)->DeleteLocalRef(env, result);
        result = NULL;
    }

done:
    if (result == NULL && !(*env)->ExceptionCheck(env)) {
        throw_code(env, SAPFLOW_SIGNET_UNAVAILABLE);
    }
    if (tag != NULL) (*env)->DeleteLocalRef(env, tag);
    if (id != NULL) (*env)->DeleteLocalRef(env, id);
    if (third != NULL) (*env)->DeleteLocalRef(env, third);
    if (fourth != NULL) (*env)->DeleteLocalRef(env, fourth);
    sapflow_signet_event_free(out);
    return result;
}

JNIEXPORT jint JNICALL JNI_NAME(abiVersion)(JNIEnv *env, jclass cls) {
    (void)env; (void)cls;
    return (jint)sapflow_signet_abi_version();
}

JNIEXPORT jlong JNICALL JNI_NAME(credentialRestore)(JNIEnv *env, jclass cls, jbyteArray bytes) {
    (void)cls;
    Input input;
    if (!borrow_input(env, bytes, &input)) return 0;
    void *out = NULL;
    int32_t rc = sapflow_signet_credential_restore(input.ptr, input.len, &out);
    release_input(env, &input);
    return handle_result(env, rc, out);
}

JNIEXPORT void JNICALL JNI_NAME(credentialFree)(JNIEnv *env, jclass cls, jlong handle) {
    (void)env; (void)cls;
    sapflow_signet_credential_free((void *)(intptr_t)handle);
}

JNIEXPORT jbyteArray JNICALL JNI_NAME(credentialEncoded)(JNIEnv *env, jclass cls, jlong handle) {
    (void)cls;
    SapflowSignetBuffer out = {0};
    int32_t rc = sapflow_signet_credential_encoded((void *)(intptr_t)handle, &out);
    return buffer_result(env, rc, out, 1);
}

JNIEXPORT jlong JNICALL JNI_NAME(nativePairBunker)(JNIEnv *env, jclass cls, jbyteArray uri, jint deadline_ms) {
    (void)cls;
    Input input;
    if (!borrow_input(env, uri, &input)) return 0;
    void *out = NULL;
    int32_t rc = sapflow_signet_native_pair_bunker(input.ptr, input.len, (uint32_t)deadline_ms, &out);
    release_input(env, &input);
    return handle_result(env, rc, out);
}

JNIEXPORT jlong JNICALL JNI_NAME(nativeRestore)(JNIEnv *env, jclass cls, jlong credential, jint deadline_ms) {
    (void)cls;
    void *out = NULL;
    int32_t rc = sapflow_signet_native_restore((void *)(intptr_t)credential, (uint32_t)deadline_ms, &out);
    return handle_result(env, rc, out);
}

JNIEXPORT void JNICALL JNI_NAME(nativeFree)(JNIEnv *env, jclass cls, jlong handle) {
    (void)env; (void)cls;
    sapflow_signet_native_free((void *)(intptr_t)handle);
}

JNIEXPORT void JNICALL JNI_NAME(nativeConnect)(JNIEnv *env, jclass cls, jlong handle, jlong request_id) {
    (void)cls;
    unit_result(env, sapflow_signet_native_connect((void *)(intptr_t)handle, (uint64_t)request_id));
}

JNIEXPORT void JNICALL JNI_NAME(nativeSignLogin)(JNIEnv *env, jclass cls, jlong handle, jlong request_id,
    jbyteArray challenge, jbyteArray origin, jbyteArray app_name, jlong created_at_seconds) {
    (void)cls;
    Input challenge_input, origin_input, app_input;
    if (!borrow_input(env, challenge, &challenge_input)) return;
    if (!borrow_input(env, origin, &origin_input)) {
        release_input(env, &challenge_input);
        return;
    }
    if (!borrow_input(env, app_name, &app_input)) {
        release_input(env, &origin_input);
        release_input(env, &challenge_input);
        return;
    }
    int32_t rc = sapflow_signet_native_sign_login(
        (void *)(intptr_t)handle, (uint64_t)request_id,
        challenge_input.ptr, challenge_input.len,
        origin_input.ptr, origin_input.len,
        app_input.ptr, app_input.len,
        (uint64_t)created_at_seconds
    );
    release_input(env, &app_input);
    release_input(env, &origin_input);
    release_input(env, &challenge_input);
    unit_result(env, rc);
}

JNIEXPORT jobjectArray JNICALL JNI_NAME(nativeWaitEvent)(JNIEnv *env, jclass cls, jlong handle, jint timeout_ms) {
    (void)cls;
    SapflowSignetEvent out = {0};
    int32_t rc = sapflow_signet_native_wait_event((void *)(intptr_t)handle, (uint32_t)timeout_ms, &out);
    return event_result(env, rc, out);
}

JNIEXPORT void JNICALL JNI_NAME(nativeRequestClose)(JNIEnv *env, jclass cls, jlong handle) {
    (void)cls;
    unit_result(env, sapflow_signet_native_request_close((void *)(intptr_t)handle));
}

JNIEXPORT jboolean JNICALL JNI_NAME(nativeWaitClosed)(JNIEnv *env, jclass cls, jlong handle, jint timeout_ms) {
    (void)cls;
    bool out = false;
    int32_t rc = sapflow_signet_native_wait_closed((void *)(intptr_t)handle, (uint32_t)timeout_ms, &out);
    if (rc != 0) {
        throw_code(env, rc);
        return JNI_FALSE;
    }
    return out ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL JNI_NAME(nativeIsClosed)(JNIEnv *env, jclass cls, jlong handle) {
    (void)cls;
    bool out = false;
    int32_t rc = sapflow_signet_native_is_closed((void *)(intptr_t)handle, &out);
    if (rc != 0) {
        throw_code(env, rc);
        return JNI_FALSE;
    }
    return out ? JNI_TRUE : JNI_FALSE;
}

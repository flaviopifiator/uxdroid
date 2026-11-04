// Built with the Android NDK against its libmediandk stub, then bound to uxdroid's libmediandk
// table. media_check() is what an app's codec probe does: look for a codec, find none, and keep a
// format. The JNI functions are NativeCall's (runtime/art/samples/NativeCall.java), so that
// media_smoke.sh runs the same checks from DEX through ART and the native bridge.
#include <jni.h>
#include <media/NdkMediaCodec.h>
#include <media/NdkMediaFormat.h>
#include <stdint.h>
#include <string.h>

static const char *const EXPECTED =
    "mime: string(video/avc), width: int32(1280), durationUs: int64(1099511627776), "
    "frame-rate: float(30.500000), csd-0: data}";

// 0 when every check holds, else the number of the first that failed.
__attribute__((visibility("default"))) int media_check(void) {
    if (AMediaCodec_createDecoderByType("video/avc") != NULL) return 1;
    if (AMediaCodec_createEncoderByType("video/avc") != NULL) return 2;
    if (AMediaCodec_createCodecByName("c2.android.avc.decoder") != NULL) return 3;
    if (AMediaCodec_start(NULL) != AMEDIA_ERROR_INVALID_OBJECT) return 4;
    if (AMediaCodec_dequeueInputBuffer(NULL, 0) != AMEDIA_ERROR_INVALID_OBJECT) return 5;

    AMediaFormat *format = AMediaFormat_new();
    if (format == NULL) return 6;
    static const uint8_t csd[] = {0, 0, 0, 1};
    AMediaFormat_setString(format, AMEDIAFORMAT_KEY_MIME, "video/avc");
    AMediaFormat_setInt32(format, AMEDIAFORMAT_KEY_WIDTH, 1280);
    AMediaFormat_setInt64(format, AMEDIAFORMAT_KEY_DURATION, INT64_C(1) << 40);
    AMediaFormat_setFloat(format, AMEDIAFORMAT_KEY_FRAME_RATE, 30.5f);
    AMediaFormat_setBuffer(format, AMEDIAFORMAT_KEY_CSD_0, csd, sizeof csd);
    int32_t width = 0;
    int64_t duration = 0;
    float rate = 0;
    const char *mime = NULL;
    void *data = NULL;
    size_t size = 0;
    if (!AMediaFormat_getInt32(format, "width", &width) || width != 1280) return 7;
    if (!AMediaFormat_getInt64(format, "durationUs", &duration) || duration != INT64_C(1) << 40)
        return 8;
    if (!AMediaFormat_getFloat(format, "frame-rate", &rate) || rate != 30.5f) return 9;
    if (!AMediaFormat_getString(format, "mime", &mime) || strcmp(mime, "video/avc") != 0) return 10;
    if (!AMediaFormat_getBuffer(format, "csd-0", &data, &size) || size != sizeof csd ||
        memcmp(data, csd, size) != 0)
        return 11;
    if (strcmp(AMediaFormat_toString(format), EXPECTED) != 0) return 12;
    if (AMediaFormat_delete(format) != AMEDIA_OK) return 13;
    return 0;
}

JNIEXPORT jint JNICALL Java_NativeCall_add(JNIEnv *env, jclass clazz, jint left, jint right) {
    int failed = media_check();
    return failed == 0 ? left + right : -failed;
}

JNIEXPORT jstring JNICALL Java_NativeCall_greet(JNIEnv *env, jclass clazz, jstring name) {
    const char *chars = (*env)->GetStringUTFChars(env, name, NULL);
    AMediaFormat *format = AMediaFormat_new();
    AMediaFormat_setString(format, AMEDIAFORMAT_KEY_MIME, chars);
    (*env)->ReleaseStringUTFChars(env, name, chars);
    jstring text = (*env)->NewStringUTF(env, AMediaFormat_toString(format));
    AMediaFormat_delete(format);
    return text;
}

JNIEXPORT jint JNICALL Java_NativeCall_callBack(JNIEnv *env, jclass clazz) {
    jmethodID method = (*env)->GetStaticMethodID(env, clazz, "fromNative", "(I)I");
    return AMediaCodec_createDecoderByType("video/hevc") == NULL
               ? (*env)->CallStaticIntMethod(env, clazz, method, 21)
               : -1;
}

JNIEXPORT void JNICALL Java_NativeCall_throwFromNative(JNIEnv *env, jclass clazz) {
    (*env)->ThrowNew(env, (*env)->FindClass(env, "java/lang/IllegalStateException"),
                     "no codec for video/hevc");
}

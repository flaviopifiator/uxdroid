// Built with the Android NDK against bionic, then loaded by ART through the uxdroid bridge.
#include <android/log.h>
#include <jni.h>
#include <stdlib.h>
#include <string>

static bool on_load_ran = false;
static const std::string prefix = std::string("hello ") + "from bionic, ";

extern "C" JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *, void *) {
    on_load_ran = true;
    return JNI_VERSION_1_6;
}

extern "C" JNIEXPORT jint JNICALL Java_NativeCall_add(JNIEnv *, jclass, jint left, jint right) {
    return on_load_ran ? left + right : -1;
}

extern "C" JNIEXPORT jstring JNICALL Java_NativeCall_greet(JNIEnv *env, jclass, jstring name) {
    const char *chars = env->GetStringUTFChars(name, nullptr);
    std::string text = prefix + chars;
    env->ReleaseStringUTFChars(name, chars);
    __android_log_print(ANDROID_LOG_INFO, "nativecall", "greeting %s", text.c_str());
    return env->NewStringUTF(text.c_str());
}

extern "C" JNIEXPORT jint JNICALL Java_NativeCall_callBack(JNIEnv *env, jclass clazz) {
    jmethodID method = env->GetStaticMethodID(clazz, "fromNative", "(I)I");
    return env->CallStaticIntMethod(clazz, method, 21);
}

extern "C" JNIEXPORT void JNICALL Java_NativeCall_throwFromNative(JNIEnv *env, jclass) {
    try {
        throw std::string("thrown in C++");
    } catch (const std::string &message) {
        env->ThrowNew(env->FindClass("java/lang/IllegalStateException"), message.c_str());
    }
}

// The native half of the test APK, built with the Android NDK against bionic.
#include <jni.h>
#include <string>

#ifndef LABEL
#define LABEL "bionic"
#endif

static const std::string text = std::string("hello from ") + LABEL;

extern "C" JNIEXPORT jint JNICALL Java_com_example_first_MainActivity_fillColor(JNIEnv *, jclass) {
    return static_cast<jint>(0xFF008080);
}

extern "C" JNIEXPORT jstring JNICALL Java_com_example_first_MainActivity_label(JNIEnv *env, jclass) {
    return env->NewStringUTF(text.c_str());
}

// The native half of NdkActivity, built by the NDK. It uses each Android system library the way an
// app does, and reports what it saw; ndk_smoke.sh checks the reports and the screenshots.
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <SLES/OpenSLES.h>
#include <SLES/OpenSLES_Android.h>
#include <android/asset_manager_jni.h>
#include <android/bitmap.h>
#include <android/log.h>
#include <android/looper.h>
#include <android/native_window_jni.h>
#include <jni.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define TAG "ndksample"

static int pipe_fds[2];
// The thread that registered the pipe: the Activity's, which runs the main Looper.
static pid_t looper_thread;

static void log_through_vprint(const char* format, ...) {
    va_list args;
    va_start(args, format);
    __android_log_vprint(ANDROID_LOG_INFO, TAG, format, args);
    va_end(args);
}

static int on_pipe(int fd, int events, void* data) {
    char byte = 0;
    read(fd, &byte, 1);
    __android_log_print(ANDROID_LOG_INFO, TAG, "looper callback fired: byte %c, events %d, data %ld, main thread %s",
                        byte, events, (long)data, syscall(SYS_gettid) == looper_thread ? "yes" : "no");
    close(fd);
    close(pipe_fds[1]);
    return 0;
}

static const char* check_assets(JNIEnv* env, jobject java_assets) {
    AAssetManager* manager = AAssetManager_fromJava(env, java_assets);
    if (manager == NULL || AAssetManager_fromJava(env, java_assets) != manager) return "no asset manager";
    if (AAssetManager_open(manager, "missing.txt", AASSET_MODE_BUFFER) != NULL) return "a missing asset opened";
    AAsset* asset = AAssetManager_open(manager, "hello.txt", AASSET_MODE_BUFFER);
    if (asset == NULL) return "hello.txt did not open";
    static const char expected[] = "Hello from an APK asset\n";
    const void* buffer = AAsset_getBuffer(asset);
    if (AAsset_getLength(asset) != (off_t)strlen(expected) || buffer == NULL ||
        memcmp(buffer, expected, strlen(expected)) != 0) {
        return "hello.txt has the wrong contents";
    }
    char word[6] = {0};
    if (AAsset_seek(asset, 6, SEEK_SET) != 6 || AAsset_read(asset, word, 4) != 4 || strcmp(word, "from") != 0 ||
        AAsset_getRemainingLength(asset) != (off_t)strlen(expected) - 10) {
        return "seeking and reading hello.txt failed";
    }
    off_t start = 0, length = 0;
    int fd = AAsset_openFileDescriptor(asset, &start, &length);
    if (fd < 0 || length != (off_t)strlen(expected)) return "no descriptor for a stored asset";
    char direct[32] = {0};
    if (pread(fd, direct, (size_t)length, start) != length || memcmp(direct, expected, strlen(expected)) != 0) {
        return "the asset's descriptor reads other bytes";
    }
    close(fd);
    AAsset_close(asset);
    AAssetDir* dir = AAssetManager_openDir(manager, "texts");
    if (dir == NULL) return "texts did not open";
    char names[64] = {0};
    const char* name;
    while ((name = AAssetDir_getNextFileName(dir)) != NULL) {
        strncat(names, name, sizeof(names) - strlen(names) - 2);
        strcat(names, " ");
    }
    AAssetDir_close(dir);
    if (strcmp(names, "a.txt b.txt ") != 0) {
        __android_log_print(ANDROID_LOG_ERROR, TAG, "texts lists \"%s\"", names);
        return "texts lists other files";
    }
    return NULL;
}

static const char* check_bitmap(JNIEnv* env, jobject bitmap) {
    AndroidBitmapInfo info;
    if (AndroidBitmap_getInfo(env, bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS) return "no bitmap info";
    if (info.width != 2 || info.height != 1 || info.format != ANDROID_BITMAP_FORMAT_RGBA_8888 || info.stride < 8) {
        return "wrong bitmap info";
    }
    unsigned char* pixels = NULL;
    if (AndroidBitmap_lockPixels(env, bitmap, (void**)&pixels) != ANDROID_BITMAP_RESULT_SUCCESS || !pixels) {
        return "the bitmap did not lock";
    }
    // Android keeps ARGB_8888 as R, G, B, A bytes.
    if (pixels[0] != 0x10 || pixels[1] != 0x20 || pixels[2] != 0x30 || pixels[3] != 0xFF) {
        __android_log_print(ANDROID_LOG_ERROR, TAG, "pixel 0 is %02x %02x %02x %02x", pixels[0], pixels[1],
                            pixels[2], pixels[3]);
        return "pixel 0 has the wrong bytes";
    }
    pixels[4] = 0x40;
    pixels[5] = 0x50;
    pixels[6] = 0x60;
    pixels[7] = 0xFF;
    if (AndroidBitmap_unlockPixels(env, bitmap) != ANDROID_BITMAP_RESULT_SUCCESS) return "the bitmap did not unlock";
    return NULL;
}

// RustDesk still uses OpenSL ES, which Android 30 deprecated in favor of AAudio.
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
static const char* check_opensl(void) {
    SLObjectItf engine = NULL;
    if (slCreateEngine(&engine, 0, NULL, 0, NULL, NULL) != SL_RESULT_SUCCESS || engine == NULL) return "no engine";
    if ((*engine)->Realize(engine, SL_BOOLEAN_FALSE) != SL_RESULT_SUCCESS) return "the engine did not realize";
    SLEngineItf itf = NULL;
    if ((*engine)->GetInterface(engine, SL_IID_ENGINE, &itf) != SL_RESULT_SUCCESS || itf == NULL) {
        return "no engine interface";
    }
    SLObjectItf mix = NULL;
    SLresult result = (*itf)->CreateOutputMix(itf, &mix, 0, NULL, NULL);
    (*engine)->Destroy(engine);
    if (result != SL_RESULT_FEATURE_UNSUPPORTED) return "an output mix did not fail cleanly";
    if (SL_IID_ENGINE == SL_IID_PLAY || SL_IID_ANDROIDSIMPLEBUFFERQUEUE == NULL) return "bad interface IDs";
    return NULL;
}

static const char* check_looper(void) {
    ALooper* looper = ALooper_forThread();
    if (looper == NULL) return "the main thread has no looper";
    if (pipe(pipe_fds) != 0) return "no pipe";
    looper_thread = (pid_t)syscall(SYS_gettid);
    if (ALooper_addFd(looper, pipe_fds[0], 0, ALOOPER_EVENT_INPUT, on_pipe, (void*)7) != 1) return "addFd failed";
    write(pipe_fds[1], "x", 1);
    return NULL;
}

JNIEXPORT jstring JNICALL Java_com_example_ndk_NdkActivity_check(JNIEnv* env, jclass class, jobject assets,
                                                                  jobject bitmap) {
    __android_log_print(ANDROID_LOG_INFO, TAG, "liblog print %d", 42);
    __android_log_write(ANDROID_LOG_WARN, TAG, "liblog write");
    __android_log_buf_write(LOG_ID_MAIN, ANDROID_LOG_ERROR, TAG, "liblog buf_write");
    log_through_vprint("liblog vprint %s", "ok");
    const char* failure = check_assets(env, assets);
    if (!failure) failure = check_bitmap(env, bitmap);
    if (!failure) failure = check_opensl();
    if (!failure) failure = check_looper();
    return (*env)->NewStringUTF(env, failure ? failure : "assets bitmap opensl looper ok");
}

JNIEXPORT jstring JNICALL Java_com_example_ndk_NdkActivity_drawCpu(JNIEnv* env, jclass class, jobject surface,
                                                                    jint color) {
    ANativeWindow* window = ANativeWindow_fromSurface(env, surface);
    if (window == NULL) return (*env)->NewStringUTF(env, "no window");
    ANativeWindow_setBuffersGeometry(window, 0, 0, WINDOW_FORMAT_RGBA_8888);
    ANativeWindow_Buffer buffer;
    if (ANativeWindow_lock(window, &buffer, NULL) != 0) {
        ANativeWindow_release(window);
        return (*env)->NewStringUTF(env, "the window did not lock");
    }
    unsigned char rgba[4] = {(color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF, (color >> 24) & 0xFF};
    for (int y = 0; y < buffer.height; y++) {
        unsigned char* row = (unsigned char*)buffer.bits + (size_t)y * buffer.stride * 4;
        for (int x = 0; x < buffer.width; x++) memcpy(row + x * 4, rgba, 4);
    }
    int posted = ANativeWindow_unlockAndPost(window);
    char report[96];
    int width = ANativeWindow_getWidth(window), height = ANativeWindow_getHeight(window);
    snprintf(report, sizeof(report), "%dx%d format %d, window %dx%d, %s", buffer.width, buffer.height, buffer.format,
             width, height, posted == 0 ? "posted" : "not posted");
    ANativeWindow_release(window);
    return (*env)->NewStringUTF(env, report);
}

JNIEXPORT jstring JNICALL Java_com_example_ndk_NdkActivity_drawGl(JNIEnv* env, jclass class, jobject surface,
                                                                   jfloat red, jfloat green, jfloat blue) {
    char report[256];
    EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    EGLint major = 0, minor = 0;
    if (display == EGL_NO_DISPLAY || !eglInitialize(display, &major, &minor)) {
        return (*env)->NewStringUTF(env, "eglInitialize failed");
    }
    const EGLint config_attributes[] = {EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                                        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
                                        EGL_NONE};
    EGLConfig config;
    EGLint count = 0;
    if (!eglChooseConfig(display, config_attributes, &config, 1, &count) || count < 1) {
        return (*env)->NewStringUTF(env, "no window config");
    }
    EGLint visual = 0, surface_type = 0;
    eglGetConfigAttrib(display, config, EGL_NATIVE_VISUAL_ID, &visual);
    eglGetConfigAttrib(display, config, EGL_SURFACE_TYPE, &surface_type);
    if (!(surface_type & EGL_WINDOW_BIT)) return (*env)->NewStringUTF(env, "the config cannot draw windows");
    ANativeWindow* window = ANativeWindow_fromSurface(env, surface);
    ANativeWindow_setBuffersGeometry(window, 0, 0, visual);
    EGLSurface egl_surface = eglCreateWindowSurface(display, config, window, NULL);
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
    if (egl_surface == EGL_NO_SURFACE || context == EGL_NO_CONTEXT ||
        !eglMakeCurrent(display, egl_surface, egl_surface, context)) {
        return (*env)->NewStringUTF(env, "no current window surface");
    }
    EGLint width = 0, height = 0;
    eglQuerySurface(display, egl_surface, EGL_WIDTH, &width);
    eglQuerySurface(display, egl_surface, EGL_HEIGHT, &height);
    glClearColor(red, green, blue, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    // GL's origin is the bottom left, so the upper half of y is the top of the window.
    glEnable(GL_SCISSOR_TEST);
    glScissor(0, height / 2, width / 2, height - height / 2);
    glClearColor(1.0f, 1.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDisable(GL_SCISSOR_TEST);
    unsigned char corner[4] = {0};
    glReadPixels(width - 1, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, corner);
    EGLBoolean swapped = eglSwapBuffers(display, egl_surface);
    snprintf(report, sizeof(report), "EGL %d.%d, %s, %dx%d, visual %d, corner %d %d %d, %s", major, minor,
             (const char*)glGetString(GL_RENDERER), width, height, visual, corner[0], corner[1], corner[2],
             swapped ? "swapped" : "not swapped");
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(display, context);
    eglDestroySurface(display, egl_surface);
    ANativeWindow_release(window);
    return (*env)->NewStringUTF(env, report);
}

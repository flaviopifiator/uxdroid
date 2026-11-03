// The native half of NdkActivity, built by the NDK. It uses each Android system library the way an
// app does, and reports what it saw; ndk_smoke.sh checks the reports and the screenshots.
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <SLES/OpenSLES.h>
#include <SLES/OpenSLES_Android.h>
#include <android/asset_manager_jni.h>
#include <android/bitmap.h>
#include <android/choreographer.h>
#include <android/log.h>
#include <android/looper.h>
#include <android/native_window_jni.h>
#include <dlfcn.h>
#include <jni.h>
#include <pthread.h>
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
    if (result != SL_RESULT_SUCCESS || (*mix)->Realize(mix, SL_BOOLEAN_FALSE) != SL_RESULT_SUCCESS) {
        (*engine)->Destroy(engine);
        return "no output mix";
    }
    (*mix)->Destroy(mix);
    // This app's manifest does not request RECORD_AUDIO, so a recorder must be refused.
    SLDataLocator_IODevice device = {SL_DATALOCATOR_IODEVICE, SL_IODEVICE_AUDIOINPUT, SL_DEFAULTDEVICEID_AUDIOINPUT, NULL};
    SLDataSource mic = {&device, NULL};
    SLDataLocator_AndroidSimpleBufferQueue queue = {SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 1};
    SLDataFormat_PCM pcm = {SL_DATAFORMAT_PCM, 1, SL_SAMPLINGRATE_16, SL_PCMSAMPLEFORMAT_FIXED_16,
                            SL_PCMSAMPLEFORMAT_FIXED_16, SL_SPEAKER_FRONT_CENTER, SL_BYTEORDER_LITTLEENDIAN};
    SLDataSink into = {&queue, &pcm};
    SLObjectItf recorder = NULL;
    result = (*itf)->CreateAudioRecorder(itf, &recorder, &mic, &into, 0, NULL, NULL);
    (*engine)->Destroy(engine);
    if (result != SL_RESULT_PERMISSION_DENIED) return "a recorder without RECORD_AUDIO was not refused";
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

// AChoreographer, as Flutter's engine uses it: a chain of frame callbacks, each posting the next, on
// the main thread and on a thread of its own with an ALooper; that thread also asks for a vsync
// callback, through dlsym as for functions newer than the app's minimum SDK, and the refresh rate.
#define FRAMES 30

struct chain {
    const char* name;
    int count;
    int64_t first, last, shortest, longest;
};

static struct chain main_chain = {"main", 0, 0, 0, INT64_MAX, 0};
static struct chain thread_chain = {"looper", 0, 0, 0, INT64_MAX, 0};

static void on_frame(int64_t time, void* data) {
    struct chain* chain = data;
    if (chain->count > 0) {
        int64_t interval = time - chain->last;
        if (interval < chain->shortest) chain->shortest = interval;
        if (interval > chain->longest) chain->longest = interval;
    } else {
        chain->first = time;
    }
    chain->last = time;
    if (++chain->count < FRAMES) {
        AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, chain);
    } else {
        __android_log_print(ANDROID_LOG_INFO, TAG, "choreographer %s: %d frames in %lld ms, intervals %lld to %lld us",
                            chain->name, chain->count, (long long)((chain->last - chain->first) / 1000000),
                            (long long)(chain->shortest / 1000), (long long)(chain->longest / 1000));
    }
}

typedef void (*vsync_callback)(const void* data, void* user);
static int64_t (*frame_time_of)(const void*);
static size_t (*timelines_of)(const void*);
static int64_t (*deadline_of)(const void*, size_t);

static void on_vsync(const void* data, void* user) {
    (void)user;
    __android_log_print(ANDROID_LOG_INFO, TAG, "choreographer vsync: %zu timeline, deadline %lld us after its frame",
                        timelines_of(data), (long long)((deadline_of(data, 0) - frame_time_of(data)) / 1000));
    AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, &thread_chain);
}

static void on_refresh(int64_t period, void* user) {
    (void)user;
    __android_log_print(ANDROID_LOG_INFO, TAG, "choreographer refresh period %lld ns", (long long)period);
}

static void* choreographer_thread(void* unused) {
    (void)unused;
    ALooper_prepare(0);
    AChoreographer* choreographer = AChoreographer_getInstance();
    void* android = dlopen("libandroid.so", RTLD_NOW);
    void (*post_vsync)(AChoreographer*, vsync_callback, void*) =
        android ? dlsym(android, "AChoreographer_postVsyncCallback") : NULL;
    frame_time_of = android ? dlsym(android, "AChoreographerFrameCallbackData_getFrameTimeNanos") : NULL;
    timelines_of = android ? dlsym(android, "AChoreographerFrameCallbackData_getFrameTimelinesLength") : NULL;
    deadline_of = android ? dlsym(android, "AChoreographerFrameCallbackData_getFrameTimelineDeadlineNanos") : NULL;
    if (!choreographer || !post_vsync || !frame_time_of || !timelines_of || !deadline_of) {
        __android_log_print(ANDROID_LOG_ERROR, TAG, "choreographer thread: missing %s", choreographer ? "dlsym" : "instance");
        return NULL;
    }
    AChoreographer_registerRefreshRateCallback(choreographer, on_refresh, NULL);
    post_vsync(choreographer, on_vsync, NULL);
    while (thread_chain.count < FRAMES) ALooper_pollOnce(100, NULL, NULL, NULL);
    AChoreographer_unregisterRefreshRateCallback(choreographer, on_refresh, NULL);
    return NULL;
}

static const char* check_choreographer(void) {
    AChoreographer* choreographer = AChoreographer_getInstance();
    if (choreographer == NULL) return "the main thread has no choreographer";
    AChoreographer_postFrameCallback64(choreographer, on_frame, &main_chain);
    pthread_t thread;
    if (pthread_create(&thread, NULL, choreographer_thread, NULL) != 0) return "no choreographer thread";
    pthread_detach(thread);
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
    if (!failure) failure = check_choreographer();
    return (*env)->NewStringUTF(env, failure ? failure : "assets bitmap opensl looper choreographer ok");
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
    if (window == NULL) return (*env)->NewStringUTF(env, "no native window");
    ANativeWindow_setBuffersGeometry(window, 0, 0, visual);
    EGLSurface egl_surface = eglCreateWindowSurface(display, config, window, NULL);
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
    if (egl_surface == EGL_NO_SURFACE || context == EGL_NO_CONTEXT ||
        !eglMakeCurrent(display, egl_surface, egl_surface, context)) {
        if (context != EGL_NO_CONTEXT) eglDestroyContext(display, context);
        if (egl_surface != EGL_NO_SURFACE) eglDestroySurface(display, egl_surface);
        ANativeWindow_release(window);
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

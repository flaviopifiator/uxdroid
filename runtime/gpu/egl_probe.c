// Reports which EGL vendor and GL renderer a headless GLES 2 context gets, through the EGL device
// platform the way uxdroid's GPU path will open it. Needs no EGL or GL headers: everything is looked
// up at run time, so it builds with only a C compiler.
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>

typedef void *EGLDisplay, *EGLDeviceEXT, *EGLConfig, *EGLContext, *EGLSurface;
typedef int32_t EGLint;
typedef unsigned EGLBoolean;

#define EGL_PLATFORM_DEVICE_EXT 0x313F
#define EGL_VENDOR 0x3053
#define EGL_SURFACE_TYPE 0x3033
#define EGL_PBUFFER_BIT 0x0001
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_WIDTH 0x3057
#define EGL_HEIGHT 0x3056
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_OPENGL_ES_API 0x30A0
#define EGL_NONE 0x3038
#define GL_VENDOR 0x1F00
#define GL_RENDERER 0x1F01

int main(void) {
    void *egl = dlopen("libEGL.so.1", RTLD_NOW);
    if (!egl) return printf("no libEGL.so.1: %s\n", dlerror()), 1;
    void *(*proc)(const char *) = dlsym(egl, "eglGetProcAddress");
    EGLBoolean (*query_devices)(EGLint, EGLDeviceEXT *, EGLint *) = proc("eglQueryDevicesEXT");
    EGLDisplay (*platform_display)(unsigned, void *, const EGLint *) = proc("eglGetPlatformDisplayEXT");
    if (!query_devices || !platform_display) return printf("no EGL device platform\n"), 1;
    EGLDeviceEXT devices[8];
    EGLint count = 0;
    if (!query_devices(8, devices, &count) || count == 0) return printf("no EGL device\n"), 1;
    EGLBoolean (*initialize)(EGLDisplay, EGLint *, EGLint *) = dlsym(egl, "eglInitialize");
    const char *(*query_string)(EGLDisplay, EGLint) = dlsym(egl, "eglQueryString");
    EGLBoolean (*choose)(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *) = dlsym(egl, "eglChooseConfig");
    EGLBoolean (*bind_api)(unsigned) = dlsym(egl, "eglBindAPI");
    EGLContext (*create_context)(EGLDisplay, EGLConfig, EGLContext, const EGLint *) = dlsym(egl, "eglCreateContext");
    EGLSurface (*create_pbuffer)(EGLDisplay, EGLConfig, const EGLint *) = dlsym(egl, "eglCreatePbufferSurface");
    EGLBoolean (*make_current)(EGLDisplay, EGLSurface, EGLSurface, EGLContext) = dlsym(egl, "eglMakeCurrent");
    int usable = 0;
    for (EGLint i = 0; i < count; i++) {
        EGLDisplay display = platform_display(EGL_PLATFORM_DEVICE_EXT, devices[i], NULL);
        EGLint major, minor;
        if (!display || !initialize(display, &major, &minor)) {
            printf("device %d: cannot initialize\n", i);
            continue;
        }
        const EGLint attributes[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
                                     EGL_OPENGL_ES2_BIT, EGL_NONE};
        EGLConfig config;
        EGLint configs = 0;
        if (!choose(display, attributes, &config, 1, &configs) || configs == 0) continue;
        bind_api(EGL_OPENGL_ES_API);
        const EGLint version[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
        EGLContext context = create_context(display, config, NULL, version);
        const EGLint size[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
        EGLSurface surface = create_pbuffer(display, config, size);
        if (!context || !surface || !make_current(display, surface, surface, context)) {
            printf("device %d: no GLES 2 context\n", i);
            continue;
        }
        usable++;
        const char *(*gl_string)(unsigned) = proc("glGetString");
        printf("device %d: EGL %d.%d vendor %s, GL vendor %s, renderer %s\n", i, major, minor,
               query_string(display, EGL_VENDOR), gl_string(GL_VENDOR), gl_string(GL_RENDERER));
    }
    return usable ? 0 : 1;
}

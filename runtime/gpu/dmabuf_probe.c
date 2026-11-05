// Reports what the GPU path of uxdroid can use on this host: the EGL platforms and extensions of
// each display, whether GBM opens the render node, and whether a frame drawn into a pbuffer and
// blitted upside down into a GBM buffer object, imported as an EGLImage, comes out as a dmabuf that
// EGL imports back with the same pixels, top row first: the path `crates/ndk/src/egl.rs` takes. Like
// egl_probe.c it needs no headers and looks every function up at run time.
//
// Lines start with a key the scripts grep for: "client:", "display <platform>:", "has <ext>",
// "gbm:", "frame:", "import:", "fence:", "ok:".
#include <dlfcn.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLImage, *EGLSync, *EGLDeviceEXT;
typedef int32_t EGLint;
typedef intptr_t EGLAttrib;
typedef unsigned EGLBoolean;
struct gbm_device;
struct gbm_bo;

#define EGL_EXTENSIONS 0x3055
#define EGL_VENDOR 0x3053
#define EGL_NONE 0x3038
#define EGL_SURFACE_TYPE 0x3033
#define EGL_WINDOW_BIT 0x0004
#define EGL_PBUFFER_BIT 0x0001
#define EGL_RENDERABLE_TYPE 0x3040
#define EGL_OPENGL_ES2_BIT 0x0004
#define EGL_RED_SIZE 0x3024
#define EGL_GREEN_SIZE 0x3023
#define EGL_BLUE_SIZE 0x3022
#define EGL_ALPHA_SIZE 0x3021
#define EGL_NATIVE_VISUAL_ID 0x302E
#define EGL_WIDTH 0x3057
#define EGL_HEIGHT 0x3056
#define EGL_CONTEXT_CLIENT_VERSION 0x3098
#define EGL_OPENGL_ES_API 0x30A0
#define EGL_PLATFORM_GBM_KHR 0x31D7
#define EGL_PLATFORM_DEVICE_EXT 0x313F
#define EGL_PLATFORM_SURFACELESS_MESA 0x31DD
#define EGL_LINUX_DMA_BUF_EXT 0x3270
#define EGL_LINUX_DRM_FOURCC_EXT 0x3271
#define EGL_DMA_BUF_PLANE0_FD_EXT 0x3272
#define EGL_DMA_BUF_PLANE0_OFFSET_EXT 0x3273
#define EGL_DMA_BUF_PLANE0_PITCH_EXT 0x3274
#define EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT 0x3443
#define EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT 0x3444
#define EGL_SYNC_FENCE_KHR 0x30F9
#define EGL_SYNC_NATIVE_FENCE_ANDROID 0x3144
#define EGL_SYNC_FLUSH_COMMANDS_BIT_KHR 0x0001
#define EGL_CONDITION_SATISFIED_KHR 0x30F6
#define GL_TEXTURE_2D 0x0DE1
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_RGBA 0x1908
#define GL_UNSIGNED_BYTE 0x1401
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_SCISSOR_TEST 0x0C11
#define GL_RENDERER 0x1F01
#define GBM_FORMAT_ARGB8888 0x34325241
#define GL_RENDERBUFFER 0x8D41
#define GL_READ_FRAMEBUFFER 0x8CA8
#define GL_DRAW_FRAMEBUFFER 0x8CA9
#define GL_NEAREST 0x2600
#define GL_VERSION 0x1F02
#define GBM_BO_USE_RENDERING 4
#define GBM_BO_USE_LINEAR 16
#define W 64
#define H 32

static void *egl, *gbm;
static void *(*proc)(const char *);
static EGLBoolean (*initialize)(EGLDisplay, EGLint *, EGLint *);
static const char *(*query_string)(EGLDisplay, EGLint);
static EGLDisplay (*platform_display)(unsigned, void *, const EGLAttrib *);

#define LOAD(lib, name) dlsym(lib, name)

static int has(const char *list, const char *name) {
    size_t n = strlen(name);
    for (const char *at = list; list && (at = strstr(at, name)); at += n)
        if ((at == list || at[-1] == ' ') && (at[n] == ' ' || at[n] == 0)) return 1;
    return 0;
}

static const char *wanted[] = {
    "EGL_EXT_image_dma_buf_import", "EGL_EXT_image_dma_buf_import_modifiers",
    "EGL_MESA_image_dma_buf_export", "EGL_KHR_fence_sync", "EGL_KHR_wait_sync",
    "EGL_ANDROID_native_fence_sync", "EGL_KHR_image_base", "EGL_KHR_platform_gbm",
    "EGL_MESA_platform_gbm", "EGL_EXT_platform_device", "EGL_MESA_platform_surfaceless",
    "EGL_KHR_surfaceless_context", "EGL_EXT_device_drm_render_node", "EGL_EXT_buffer_age",
    "EGL_KHR_swap_buffers_with_damage", "EGL_EXT_device_enumeration", 0};

static void report(const char *what, const char *list) {
    for (int i = 0; wanted[i]; i++) printf("%s %s %s\n", what, has(list, wanted[i]) ? "has" : "lacks", wanted[i]);
}

static EGLDisplay open_display(const char *name, unsigned platform, void *native) {
    EGLDisplay display = platform_display(platform, native, NULL);
    EGLint major, minor;
    if (!display || !initialize(display, &major, &minor)) {
        printf("display %s: none\n", name);
        return NULL;
    }
    printf("display %s: EGL %d.%d vendor %s\n", name, major, minor, query_string(display, EGL_VENDOR));
    char what[64];
    snprintf(what, sizeof what, "display %s:", name);
    report(what, query_string(display, EGL_EXTENSIONS));
    return display;
}

int main(void) {
    egl = dlopen("libEGL.so.1", RTLD_NOW);
    if (!egl) return printf("no libEGL.so.1\n"), 1;
    proc = LOAD(egl, "eglGetProcAddress");
    initialize = LOAD(egl, "eglInitialize");
    query_string = LOAD(egl, "eglQueryString");
    platform_display = LOAD(egl, "eglGetPlatformDisplay");
    const char *client = query_string(NULL, EGL_EXTENSIONS);
    report("client:", client);

    EGLBoolean (*query_devices)(EGLint, EGLDeviceEXT *, EGLint *) = proc("eglQueryDevicesEXT");
    EGLDeviceEXT devices[8];
    EGLint count = 0;
    if (query_devices && query_devices(8, devices, &count))
        for (EGLint i = 0; i < count; i++) {
            char name[16];
            snprintf(name, sizeof name, "device%d", i);
            open_display(name, EGL_PLATFORM_DEVICE_EXT, devices[i]);
        }
    open_display("surfaceless", EGL_PLATFORM_SURFACELESS_MESA, NULL);

    gbm = dlopen("libgbm.so.1", RTLD_NOW);
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (!gbm || fd < 0) return printf("gbm: no libgbm.so.1 or render node\n"), 0;
    struct gbm_device *(*create_device)(int) = LOAD(gbm, "gbm_create_device");
    const char *(*backend)(struct gbm_device *) = LOAD(gbm, "gbm_device_get_backend_name");
    int (*bo_fd)(struct gbm_bo *) = LOAD(gbm, "gbm_bo_get_fd");
    uint32_t (*bo_stride)(struct gbm_bo *) = LOAD(gbm, "gbm_bo_get_stride");
    uint32_t (*bo_offset)(struct gbm_bo *, int) = LOAD(gbm, "gbm_bo_get_offset");
    uint64_t (*bo_modifier)(struct gbm_bo *) = LOAD(gbm, "gbm_bo_get_modifier");
    int (*bo_planes)(struct gbm_bo *) = LOAD(gbm, "gbm_bo_get_plane_count");
    struct gbm_device *device = create_device(fd);
    if (!device) return printf("gbm: cannot create a device on renderD128\n"), 0;
    printf("gbm: backend %s\n", backend(device));
    EGLDisplay display = open_display("gbm", EGL_PLATFORM_GBM_KHR, device);
    if (!display) return 0;

    EGLBoolean (*choose)(EGLDisplay, const EGLint *, EGLConfig *, EGLint, EGLint *) = LOAD(egl, "eglChooseConfig");
    EGLBoolean (*bind_api)(unsigned) = LOAD(egl, "eglBindAPI");
    EGLContext (*create_context)(EGLDisplay, EGLConfig, EGLContext, const EGLint *) = LOAD(egl, "eglCreateContext");
    EGLSurface (*create_pbuffer)(EGLDisplay, EGLConfig, const EGLint *) = LOAD(egl, "eglCreatePbufferSurface");
    EGLBoolean (*make_current)(EGLDisplay, EGLSurface, EGLSurface, EGLContext) = LOAD(egl, "eglMakeCurrent");
    EGLint (*get_error)(void) = LOAD(egl, "eglGetError");
    // uxdroid's window surface: the app draws into a pbuffer, its framebuffer 0, as before.
    const EGLint want[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                           EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig config = NULL;
    EGLint found = 0;
    if (!choose(display, want, &config, 1, &found) || !found) return printf("frame: no pbuffer config on GBM\n"), 0;
    bind_api(EGL_OPENGL_ES_API);
    const EGLint version[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    const EGLint size[] = {EGL_WIDTH, W, EGL_HEIGHT, H, EGL_NONE};
    EGLContext context = create_context(display, config, NULL, version);
    EGLSurface surface = create_pbuffer(display, config, size);
    if (!context || !surface || !make_current(display, surface, surface, context))
        return printf("frame: no pbuffer context on GBM (error 0x%x)\n", get_error()), 0;
    const char *(*gl_string)(unsigned) = proc("glGetString");
    void (*clear_color)(float, float, float, float) = proc("glClearColor");
    void (*clear)(unsigned) = proc("glClear");
    void (*enable)(unsigned) = proc("glEnable");
    void (*disable)(unsigned) = proc("glDisable");
    void (*scissor)(int, int, int, int) = proc("glScissor");
    printf("frame: renderer %s, %s\n", gl_string(GL_RENDERER), gl_string(GL_VERSION));
    // GL's bottom half blue, top half red: a buffer whose first row is the top starts red.
    clear_color(0, 0, 1, 1);
    clear(GL_COLOR_BUFFER_BIT);
    enable(GL_SCISSOR_TEST);
    scissor(0, H / 2, W, H / 2);
    clear_color(1, 0, 0, 1);
    clear(GL_COLOR_BUFFER_BIT);
    disable(GL_SCISSOR_TEST);

    // A swapchain buffer: a GBM buffer object, imported as an EGLImage behind a renderbuffer.
    struct gbm_bo *(*bo_create)(struct gbm_device *, uint32_t, uint32_t, uint32_t, uint32_t) = LOAD(gbm, "gbm_bo_create");
    struct gbm_bo *bo = bo_create(device, W, H, GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING);
    if (!bo) return printf("frame: gbm_bo_create refused a rendering buffer\n"), 0;
    int dmabuf = bo_fd(bo);
    uint64_t modifier = bo_modifier(bo);
    uint32_t stride = bo_stride(bo), offset = bo_offset(bo, 0);
    printf("frame: dmabuf stride %u offset %u modifier 0x%llx planes %d\n", stride, offset,
           (unsigned long long)modifier, bo_planes(bo));
    EGLImage (*create_image)(EGLDisplay, EGLContext, unsigned, void *, const EGLint *) = proc("eglCreateImageKHR");
    void (*image_renderbuffer)(unsigned, void *) = proc("glEGLImageTargetRenderbufferStorageOES");
    void (*image_texture)(unsigned, void *) = proc("glEGLImageTargetTexture2DOES");
    if (!create_image || !image_renderbuffer || !image_texture) return printf("import: no EGLImage functions\n"), 0;
    EGLint list[] = {EGL_WIDTH, W, EGL_HEIGHT, H, EGL_LINUX_DRM_FOURCC_EXT, GBM_FORMAT_ARGB8888,
                     EGL_DMA_BUF_PLANE0_FD_EXT, dmabuf, EGL_DMA_BUF_PLANE0_OFFSET_EXT, (EGLint)offset,
                     EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)stride,
                     EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(modifier & 0xffffffff),
                     EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(modifier >> 32), EGL_NONE};
    EGLImage image = create_image(display, NULL, EGL_LINUX_DMA_BUF_EXT, NULL, list);
    if (!image) return printf("import: eglCreateImageKHR refused the buffer (0x%x)\n", get_error()), 0;
    void (*gen_renderbuffers)(int, unsigned *) = proc("glGenRenderbuffers");
    void (*bind_renderbuffer)(unsigned, unsigned) = proc("glBindRenderbuffer");
    void (*attach_renderbuffer)(unsigned, unsigned, unsigned, unsigned) = proc("glFramebufferRenderbuffer");
    void (*gen_textures)(int, unsigned *) = proc("glGenTextures");
    void (*bind_texture)(unsigned, unsigned) = proc("glBindTexture");
    void (*gen_framebuffers)(int, unsigned *) = proc("glGenFramebuffers");
    void (*bind_framebuffer)(unsigned, unsigned) = proc("glBindFramebuffer");
    void (*attach)(unsigned, unsigned, unsigned, unsigned, int) = proc("glFramebufferTexture2D");
    unsigned (*status)(unsigned) = proc("glCheckFramebufferStatus");
    void (*blit)(int, int, int, int, int, int, int, int, unsigned, unsigned) = proc("glBlitFramebuffer");
    void (*read)(int, int, int, int, unsigned, unsigned, void *) = proc("glReadPixels");
    void (*flush)(void) = proc("glFlush");
    unsigned renderbuffer, target;
    gen_renderbuffers(1, &renderbuffer);
    bind_renderbuffer(GL_RENDERBUFFER, renderbuffer);
    image_renderbuffer(GL_RENDERBUFFER, image);
    gen_framebuffers(1, &target);
    bind_framebuffer(GL_DRAW_FRAMEBUFFER, target);
    attach_renderbuffer(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, renderbuffer);
    if (status(GL_DRAW_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return printf("import: the buffer is not a complete framebuffer\n"), 0;
    if (!blit) return printf("frame: no glBlitFramebuffer\n"), 0;
    // Framebuffer 0 into the buffer, upside down: GL's top row becomes the buffer's first.
    bind_framebuffer(GL_READ_FRAMEBUFFER, 0);
    blit(0, 0, W, H, 0, H, W, 0, GL_COLOR_BUFFER_BIT, GL_NEAREST);

    EGLSync (*create_sync)(EGLDisplay, unsigned, const EGLint *) = proc("eglCreateSyncKHR");
    EGLint (*client_wait)(EGLDisplay, EGLSync, EGLint, uint64_t) = proc("eglClientWaitSyncKHR");
    EGLint (*dup_fence)(EGLDisplay, EGLSync) = proc("eglDupNativeFenceFDANDROID");
    if (create_sync && has(query_string(display, EGL_EXTENSIONS), "EGL_ANDROID_native_fence_sync")) {
        EGLSync native_fence = create_sync(display, EGL_SYNC_NATIVE_FENCE_ANDROID, NULL);
        flush();
        printf("fence: native fence fd %s\n", native_fence && dup_fence(display, native_fence) >= 0 ? "exported" : "refused");
    }
    EGLSync sync = create_sync ? create_sync(display, EGL_SYNC_FENCE_KHR, NULL) : NULL;
    printf("fence: KHR fence %s\n", sync && client_wait(display, sync, EGL_SYNC_FLUSH_COMMANDS_BIT_KHR,
                                                        1000000000ull) == EGL_CONDITION_SATISFIED_KHR ? "signaled" : "failed");

    // Import the dmabuf again as a texture and read it back, the way a compositor sees the frame.
    EGLImage view = create_image(display, NULL, EGL_LINUX_DMA_BUF_EXT, NULL, list);
    unsigned texture, framebuffer;
    gen_textures(1, &texture);
    bind_texture(GL_TEXTURE_2D, texture);
    image_texture(GL_TEXTURE_2D, view);
    gen_framebuffers(1, &framebuffer);
    bind_framebuffer(GL_FRAMEBUFFER, framebuffer);
    attach(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    if (status(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return printf("import: incomplete framebuffer\n"), 0;
    uint8_t row0[4], rowN[4];
    // Texture row 0 is the buffer's first row in memory.
    read(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, row0);
    read(0, H - 1, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, rowN);
    printf("import: first row rgba %u,%u,%u last row rgba %u,%u,%u\n", row0[0], row0[1], row0[2], rowN[0], rowN[1], rowN[2]);
    int top_first = row0[0] > 200 && row0[2] < 50 && rowN[2] > 200 && rowN[0] < 50;
    printf("%s\n", top_first ? "ok: a frame blitted into a GBM buffer is a dmabuf with its top row first"
                             : "FAIL: the imported frame does not start with its top row");
    return top_first ? 0 : 2;
}

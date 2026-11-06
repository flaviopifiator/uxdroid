// What Flutter's Impeller does at the end of each render pass on its window: a multisampled config
// with a stencil buffer, then glDiscardFramebufferEXT of the depth and stencil of framebuffer 0.
// Mesa 25.0 dereferences a null surface there and the process dies; a discard is only a hint.
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <GLES2/gl2ext.h>

static EGLDisplay display;
static EGLSurface surface;
static EGLContext context;

// Makes a 16x16 pbuffer of a config with `samples` samples and a stencil buffer current.
static int make_current(EGLint samples, EGLint version) {
    display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (display == EGL_NO_DISPLAY || !eglInitialize(display, NULL, NULL)) return 1;
    const EGLint config_attributes[] = {
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_DEPTH_SIZE, 0, EGL_STENCIL_SIZE, 8,
        EGL_SAMPLE_BUFFERS, samples > 0, EGL_SAMPLES, samples, EGL_NONE};
    EGLConfig config;
    EGLint count = 0;
    if (!eglChooseConfig(display, config_attributes, &config, 1, &count) || count != 1) return 2;
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, version, EGL_NONE};
    context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
    if (context == EGL_NO_CONTEXT) return 3;
    const EGLint surface_attributes[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
    surface = eglCreatePbufferSurface(display, config, surface_attributes);
    if (surface == EGL_NO_SURFACE) return 4;
    if (!eglMakeCurrent(display, surface, surface, context)) return 5;
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    return glGetError() == GL_NO_ERROR ? 0 : 6;
}

static void release(void) {
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(display, surface);
    eglDestroyContext(display, context);
}

// glDiscardFramebufferEXT, found as Impeller finds it, on framebuffer 0 of a 4x config.
int discard_default_framebuffer(void) {
    int failed = make_current(4, 2);
    if (failed) return failed;
    PFNGLDISCARDFRAMEBUFFEREXTPROC discard =
        (PFNGLDISCARDFRAMEBUFFEREXTPROC)eglGetProcAddress("glDiscardFramebufferEXT");
    if (!discard) return 10;
    const GLenum attachments[] = {GL_DEPTH_EXT, GL_STENCIL_EXT};
    discard(GL_FRAMEBUFFER, 2, attachments);
    if (glGetError() != GL_NO_ERROR) return 11;
    // A target GL does not know still reaches GL, which refuses it.
    discard(GL_TEXTURE_2D, 2, attachments);
    if (glGetError() != GL_INVALID_ENUM) return 12;
    release();
    return 0;
}

// glInvalidateFramebuffer, OpenGL ES 3's form of the same, on framebuffer 0 of a 4x config.
int invalidate_default_framebuffer(void) {
    int failed = make_current(4, 3);
    if (failed) return failed;
    const GLenum attachments[] = {GL_DEPTH, GL_STENCIL};
    glInvalidateFramebuffer(GL_FRAMEBUFFER, 2, attachments);
    if (glGetError() != GL_NO_ERROR) return 20;
    release();
    return 0;
}

// A framebuffer of the app's own is discarded as before.
int discard_own_framebuffer(void) {
    int failed = make_current(0, 3);
    if (failed) return failed;
    GLuint texture, renderbuffer, framebuffer;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 16, 16, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glGenRenderbuffers(1, &renderbuffer);
    glBindRenderbuffer(GL_RENDERBUFFER, renderbuffer);
    glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH24_STENCIL8, 16, 16);
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT, GL_RENDERBUFFER,
                              renderbuffer);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return 30;
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    const GLenum attachments[] = {GL_COLOR_ATTACHMENT0, GL_DEPTH_ATTACHMENT, GL_STENCIL_ATTACHMENT};
    glInvalidateFramebuffer(GL_FRAMEBUFFER, 3, attachments);
    if (glGetError() != GL_NO_ERROR) return 31;
    // An attachment a framebuffer of the app's cannot have still reaches GL, which refuses it.
    const GLenum wrong[] = {GL_COLOR};
    glInvalidateFramebuffer(GL_FRAMEBUFFER, 1, wrong);
    if (glGetError() != GL_INVALID_ENUM) return 32;
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDeleteFramebuffers(1, &framebuffer);
    glDeleteRenderbuffers(1, &renderbuffer);
    glDeleteTextures(1, &texture);
    release();
    return 0;
}

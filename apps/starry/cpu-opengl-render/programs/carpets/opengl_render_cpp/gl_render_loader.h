// GL 4.5 core RENDER entry-point loader via eglGetProcAddress (desktop GL through EGL, no GLEW, no
// GL_GLEXT_PROTOTYPES). glcorearb.h supplies every enum + PFNGL*PROC typedef but declares no callable
// symbol without GL_GLEXT_PROTOTYPES, so we resolve every entry point we use - including the GL 1.0/1.1
// core (glClear/glViewport/glReadPixels/glGenTextures/glDrawArrays/...) - at runtime from the current
// EGL desktop-GL context via eglGetProcAddress (Mesa returns core pointers too). This is the render
// counterpart of cpu-opengl-compute's gl_loader_egl.h and needs only libEGL at link time.
#ifndef GL_RENDER_LOADER_H
#define GL_RENDER_LOADER_H
#include <EGL/egl.h>
#include <GL/glcorearb.h>
#define GLRPROCS(X) \
  /* GL 1.0/1.1 core (resolved via eglGetProcAddress, not linked from libGL) */ \
  X(PFNGLGETSTRINGPROC,glGetString) X(PFNGLGETERRORPROC,glGetError) \
  X(PFNGLVIEWPORTPROC,glViewport) X(PFNGLCLEARPROC,glClear) X(PFNGLCLEARCOLORPROC,glClearColor) \
  X(PFNGLCLEARDEPTHPROC,glClearDepth) X(PFNGLREADPIXELSPROC,glReadPixels) \
  X(PFNGLDRAWARRAYSPROC,glDrawArrays) X(PFNGLENABLEPROC,glEnable) X(PFNGLDISABLEPROC,glDisable) \
  X(PFNGLBLENDFUNCPROC,glBlendFunc) X(PFNGLDEPTHFUNCPROC,glDepthFunc) X(PFNGLSCISSORPROC,glScissor) \
  X(PFNGLFINISHPROC,glFinish) X(PFNGLGENTEXTURESPROC,glGenTextures) X(PFNGLBINDTEXTUREPROC,glBindTexture) \
  X(PFNGLTEXIMAGE2DPROC,glTexImage2D) X(PFNGLTEXPARAMETERIPROC,glTexParameteri) \
  X(PFNGLTEXPARAMETERFPROC,glTexParameterf) X(PFNGLDELETETEXTURESPROC,glDeleteTextures) \
  /* extended render state / draws / queries for exhaustive per-API coverage */ \
  X(PFNGLDRAWELEMENTSPROC,glDrawElements) X(PFNGLBLENDEQUATIONPROC,glBlendEquation) \
  X(PFNGLBLENDFUNCSEPARATEPROC,glBlendFuncSeparate) X(PFNGLBLENDCOLORPROC,glBlendColor) \
  X(PFNGLCULLFACEPROC,glCullFace) X(PFNGLFRONTFACEPROC,glFrontFace) X(PFNGLDEPTHMASKPROC,glDepthMask) \
  X(PFNGLCOLORMASKPROC,glColorMask) X(PFNGLCLEARBUFFERFVPROC,glClearBufferfv) \
  X(PFNGLGETINTEGERVPROC,glGetIntegerv) X(PFNGLGETBOOLEANVPROC,glGetBooleanv) \
  X(PFNGLGETFLOATVPROC,glGetFloatv) X(PFNGLISENABLEDPROC,glIsEnabled) \
  X(PFNGLACTIVETEXTUREPROC,glActiveTexture) X(PFNGLGENERATEMIPMAPPROC,glGenerateMipmap) \
  X(PFNGLUNIFORM2FPROC,glUniform2f) X(PFNGLUNIFORM3FPROC,glUniform3f) X(PFNGLUNIFORM1IVPROC,glUniform1iv) \
  X(PFNGLBUFFERSUBDATAPROC,glBufferSubData) \
  /* shaders + program */ \
  X(PFNGLCREATESHADERPROC,glCreateShader) X(PFNGLSHADERSOURCEPROC,glShaderSource) \
  X(PFNGLCOMPILESHADERPROC,glCompileShader) X(PFNGLGETSHADERIVPROC,glGetShaderiv) \
  X(PFNGLGETSHADERINFOLOGPROC,glGetShaderInfoLog) X(PFNGLDELETESHADERPROC,glDeleteShader) \
  X(PFNGLCREATEPROGRAMPROC,glCreateProgram) X(PFNGLATTACHSHADERPROC,glAttachShader) \
  X(PFNGLLINKPROGRAMPROC,glLinkProgram) X(PFNGLGETPROGRAMIVPROC,glGetProgramiv) \
  X(PFNGLGETPROGRAMINFOLOGPROC,glGetProgramInfoLog) X(PFNGLUSEPROGRAMPROC,glUseProgram) \
  X(PFNGLDELETEPROGRAMPROC,glDeleteProgram) X(PFNGLGETUNIFORMLOCATIONPROC,glGetUniformLocation) \
  X(PFNGLUNIFORM1FPROC,glUniform1f) X(PFNGLUNIFORM4FPROC,glUniform4f) X(PFNGLUNIFORM1IPROC,glUniform1i) \
  /* VAO/VBO + vertex attributes */ \
  X(PFNGLGENBUFFERSPROC,glGenBuffers) X(PFNGLBINDBUFFERPROC,glBindBuffer) \
  X(PFNGLBUFFERDATAPROC,glBufferData) X(PFNGLDELETEBUFFERSPROC,glDeleteBuffers) \
  X(PFNGLGENVERTEXARRAYSPROC,glGenVertexArrays) X(PFNGLBINDVERTEXARRAYPROC,glBindVertexArray) \
  X(PFNGLDELETEVERTEXARRAYSPROC,glDeleteVertexArrays) \
  X(PFNGLVERTEXATTRIBPOINTERPROC,glVertexAttribPointer) \
  X(PFNGLENABLEVERTEXATTRIBARRAYPROC,glEnableVertexAttribArray) \
  X(PFNGLDISABLEVERTEXATTRIBARRAYPROC,glDisableVertexAttribArray) \
  /* FBO + renderbuffer */ \
  X(PFNGLGENFRAMEBUFFERSPROC,glGenFramebuffers) X(PFNGLBINDFRAMEBUFFERPROC,glBindFramebuffer) \
  X(PFNGLDELETEFRAMEBUFFERSPROC,glDeleteFramebuffers) \
  X(PFNGLFRAMEBUFFERTEXTURE2DPROC,glFramebufferTexture2D) \
  X(PFNGLCHECKFRAMEBUFFERSTATUSPROC,glCheckFramebufferStatus) \
  X(PFNGLGENRENDERBUFFERSPROC,glGenRenderbuffers) X(PFNGLBINDRENDERBUFFERPROC,glBindRenderbuffer) \
  X(PFNGLRENDERBUFFERSTORAGEPROC,glRenderbufferStorage) \
  X(PFNGLFRAMEBUFFERRENDERBUFFERPROC,glFramebufferRenderbuffer) \
  X(PFNGLDELETERENDERBUFFERSPROC,glDeleteRenderbuffers)
#define RDECL(t,n) static t n;
GLRPROCS(RDECL)
static int glr_load(void){ int ok=1;
#define RLOAD(t,n) n=(t)eglGetProcAddress(#n); if(!n) ok=0;
  GLRPROCS(RLOAD)
  return ok; }
#endif

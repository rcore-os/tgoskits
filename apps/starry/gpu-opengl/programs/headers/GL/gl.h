// Vendored desktop-GL entry header for the surfaceless EGL carpet. Alpine ships the desktop
// GL/glcorearb.h and GL/gl.h only in mesa-dev (which pulls the clang22-libs closure the runtime
// does not need), so - like the EGL/GLES/KHR headers - the arch-independent GL header is vendored.
// glcorearb.h supplies every type and enum; its GL 1.0 entry-point prototypes sit behind
// GL_GLEXT_PROTOTYPES, which the carpet must not define (the loader declares same-named static
// function pointers for the modern compute entry points). So this shim declares only the four
// GL 1.0 calls the carpet makes directly against libGL; the rest resolve via eglGetProcAddress.
#ifndef __gl_h_
#define __gl_h_ 1
#include <GL/glcorearb.h>
#ifndef GL_GLEXT_PROTOTYPES
GLAPI const GLubyte *APIENTRY glGetString (GLenum name);
GLAPI GLenum APIENTRY glGetError (void);
GLAPI void APIENTRY glGetIntegerv (GLenum pname, GLint *data);
GLAPI void APIENTRY glFinish (void);
#endif
#endif

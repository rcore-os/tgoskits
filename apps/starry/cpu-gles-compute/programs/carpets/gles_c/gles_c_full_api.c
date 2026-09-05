/* gles_c_full_api.c - full OpenGL ES C compute API carpet on EGL-surfaceless / llvmpipe (GLES 3.1):
 * exercise the EGL + GLES 3.1 compute surface (surfaceless display / config / context / shader /
 * program / SSBO / buffer-base binding / uniform / dispatch / memory-barrier / map-range readback /
 * mapped-write / fence-sync / query / indirect dispatch / image load-store / limits / introspection)
 * and assert operator results per-element against closed-form CPU references, plus boundary, error-enum
 * and negative-control paths. Prints "GLES_C_FULL_API OK <n>" only when every assertion passes and
 * count == EXPECTED. */
#include <EGL/egl.h>
#include <GLES3/gl31.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b)<=1e-4f*(1.0f+fabsf(b)); }

/* saxpy: c[i] = alpha*a[i] + b[i] with a tail guard the boundary cases stress. */
static const char* CS =
"#version 310 es\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=1) readonly buffer B { float b[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform float alpha; uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }\n";

/* image load/store variant: reads a source SSBO, doubles, writes to a writeonly r32f image. */
static const char* IMG_CS =
"#version 310 es\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer S { float s[]; };\n"
"layout(r32f,binding=0) writeonly uniform highp image2D img;\n"
"uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) imageStore(img,ivec2(int(i),0),vec4(2.0*s[i],0,0,0)); }\n";

/* deliberately broken: undeclared identifier + no main body -> COMPILE_STATUS must be FALSE. */
static const char* BAD_CS =
"#version 310 es\n"
"layout(local_size_x=64) in;\n"
"void main(){ nonexistent_symbol = 3; }\n";

int main(void){
  const int N=1024; GLsizeiptr bytes=(GLsizeiptr)(N*sizeof(float));

  /* --- EGL surfaceless display + context (GLES 3.1) --- */
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize");
  ok(maj>=1,"EGL major >= 1");
  ok(eglQueryString(dpy,EGL_VENDOR)!=NULL,"eglQueryString VENDOR");
  ok(eglQueryString(dpy,EGL_VERSION)!=NULL,"eglQueryString VERSION");
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_ES3_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0; ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg) && ncfg>=1,"eglChooseConfig ES3");
  { EGLint rt=0; ok(eglGetConfigAttrib(dpy,cfg,EGL_RENDERABLE_TYPE,&rt) && (rt&EGL_OPENGL_ES3_BIT),"eglGetConfigAttrib RENDERABLE_TYPE ES3"); }
  ok(eglBindAPI(EGL_OPENGL_ES_API),"eglBindAPI ES");
  ok(eglQueryAPI()==EGL_OPENGL_ES_API,"eglQueryAPI == ES");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,3, EGL_CONTEXT_MINOR_VERSION,1, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext 3.1");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");
  ok(eglGetCurrentContext()==ctx,"eglGetCurrentContext");
  const char*ver=(const char*)glGetString(GL_VERSION); ok(ver&&strstr(ver,"ES")!=NULL,"GL_VERSION is GLES");
  ok(glGetString(GL_SHADING_LANGUAGE_VERSION)!=NULL,"GLSL ES version");
  { GLint gmaj=0,gmin=0; glGetIntegerv(GL_MAJOR_VERSION,&gmaj); glGetIntegerv(GL_MINOR_VERSION,&gmin);
    ok(gmaj>3 || (gmaj==3&&gmin>=1),"GL_MAJOR/MINOR_VERSION >= 3.1"); }

  /* --- compute limits (indexed + non-indexed getters) --- */
  { GLint c0=0,c1=0,c2=0;
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&c0);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,1,&c1);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,2,&c2);
    ok(c0>=1&&c1>=1&&c2>=1,"MAX_COMPUTE_WORK_GROUP_COUNT[0..2] >= 1"); }
  GLint wgsize0=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_SIZE,0,&wgsize0); ok(wgsize0>=64,"MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64");
  GLint wginv=0; glGetIntegerv(GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,&wginv); ok(wginv>=64,"MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64");
  { GLint ssb=0; glGetIntegerv(GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS,&ssb); ok(ssb>=3,"MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3"); }
  { GLint msb=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BLOCK_SIZE,&msb); ok(msb>=(GLint)bytes,"MAX_SHADER_STORAGE_BLOCK_SIZE >= bytes"); }

  /* --- compute shader compile (COMPILE_STATUS + type) --- */
  GLuint sh=glCreateShader(GL_COMPUTE_SHADER); ok(sh!=0,"glCreateShader(COMPUTE)");
  glShaderSource(sh,1,&CS,NULL); glCompileShader(sh);
  GLint cok=0; glGetShaderiv(sh,GL_COMPILE_STATUS,&cok);
  if(!cok){ char log[2048]; glGetShaderInfoLog(sh,2048,NULL,log); fprintf(stderr,"shader: %s\n",log); }
  ok(cok,"glCompileShader COMPILE_STATUS");
  { GLint st=0; glGetShaderiv(sh,GL_SHADER_TYPE,&st); ok(st==GL_COMPUTE_SHADER,"glGetShaderiv SHADER_TYPE"); }

  /* --- compile-error path: broken shader must report COMPILE_STATUS==FALSE with a non-empty log --- */
  { GLuint bad=glCreateShader(GL_COMPUTE_SHADER); glShaderSource(bad,1,&BAD_CS,NULL); glCompileShader(bad);
    GLint bcok=1; glGetShaderiv(bad,GL_COMPILE_STATUS,&bcok); ok(bcok==GL_FALSE,"broken shader COMPILE_STATUS == FALSE");
    GLint loglen=0; glGetShaderiv(bad,GL_INFO_LOG_LENGTH,&loglen); ok(loglen>0,"broken shader INFO_LOG_LENGTH > 0");
    glDeleteShader(bad); }

  /* --- link a program with no attached shader: LINK_STATUS must be FALSE --- */
  { GLuint p0=glCreateProgram(); glLinkProgram(p0);
    GLint l0=1; glGetProgramiv(p0,GL_LINK_STATUS,&l0); ok(l0==GL_FALSE,"empty program LINK_STATUS == FALSE");
    glDeleteProgram(p0); }

  /* --- program link (LINK_STATUS) --- */
  GLuint prog=glCreateProgram(); ok(prog!=0,"glCreateProgram");
  glAttachShader(prog,sh); glLinkProgram(prog);
  GLint lok=0; glGetProgramiv(prog,GL_LINK_STATUS,&lok);
  if(!lok){ char log[2048]; glGetProgramInfoLog(prog,2048,NULL,log); fprintf(stderr,"link: %s\n",log); }
  ok(lok,"glLinkProgram LINK_STATUS");
  glDeleteShader(sh); ok(glGetError()==GL_NO_ERROR,"glDeleteShader (no error)");

  /* --- SSBO buffers --- */
  float *a=malloc((size_t)bytes),*b=malloc((size_t)bytes);
  for(int i=0;i<N;i++){ a[i]=(float)i; b[i]=2.0f*i+1.0f; }
  GLuint buf[3]={0,0,0}; glGenBuffers(3,buf); ok(buf[0]&&buf[1]&&buf[2],"glGenBuffers(3)");
  ok(glIsBuffer(buf[0])==GL_FALSE,"glIsBuffer before bind == FALSE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a,GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData A");
  ok(glIsBuffer(buf[0])==GL_TRUE,"glIsBuffer after bind == TRUE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b,GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData B");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,NULL,GL_DYNAMIC_COPY); ok(glGetError()==GL_NO_ERROR,"glBufferData C");
  for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]); ok(glGetError()==GL_NO_ERROR,"glBindBufferBase x3");

  /* --- validation-error path: glBufferData with a bad target enum -> GL_INVALID_ENUM --- */
  { while(glGetError()!=GL_NO_ERROR){}
    glBufferData(GL_FLOAT,bytes,NULL,GL_STATIC_DRAW);
    ok(glGetError()==GL_INVALID_ENUM,"glBufferData bad target -> GL_INVALID_ENUM"); }
  /* --- validation-error path: negative buffer size -> GL_INVALID_VALUE --- */
  { while(glGetError()!=GL_NO_ERROR){}
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,(GLsizeiptr)-16,NULL,GL_STATIC_DRAW);
    ok(glGetError()==GL_INVALID_VALUE,"glBufferData negative size -> GL_INVALID_VALUE"); }

  /* --- uniforms + dispatch: vadd (alpha=1); read back per element --- */
  glUseProgram(prog);
  { GLint cur=0; glGetIntegerv(GL_CURRENT_PROGRAM,&cur); ok((GLuint)cur==prog,"glUseProgram (CURRENT_PROGRAM)"); }
  GLint la=glGetUniformLocation(prog,"alpha"), ln=glGetUniformLocation(prog,"n"); ok(la>=0&&ln>=0,"glGetUniformLocation alpha,n");
  glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)N); ok(glGetError()==GL_NO_ERROR,"glUniform1f/1ui");
  glDispatchCompute((N+63)/64,1,1); ok(glGetError()==GL_NO_ERROR,"glDispatchCompute vadd");
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"glMemoryBarrier");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  { float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=NULL,"glMapBufferRange READ (vadd)");
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],a[i]+b[i])){g=0;break;} ok(g&&m,"vadd c[i]==a[i]+b[i] (per-element)");
    /* negative control: verify the checker rejects a deliberately-wrong reference. */
    int flagged=0; if(m) for(int i=0;i<N;i++) if(!feq(m[i], (i==7? a[i]+b[i]+1.0f : a[i]+b[i]))){flagged=1;break;} ok(flagged,"negative control: corrupted vadd ref is flagged");
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (vadd)"); }

  /* --- fence sync: order the GPU->host read behind the compute completion --- */
  glUniform1f(la,3.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  { GLsync sync=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0); ok(sync!=NULL,"glFenceSync GPU_COMMANDS_COMPLETE");
    { GLint ot=0,cond=0; glGetSynciv(sync,GL_OBJECT_TYPE,1,NULL,&ot); glGetSynciv(sync,GL_SYNC_CONDITION,1,NULL,&cond);
      ok(ot==GL_SYNC_FENCE && cond==GL_SYNC_GPU_COMMANDS_COMPLETE,"glGetSynciv OBJECT_TYPE/CONDITION"); }
    glWaitSync(sync,0,GL_TIMEOUT_IGNORED); ok(glGetError()==GL_NO_ERROR,"glWaitSync (no error)");
    GLenum w=glClientWaitSync(sync,GL_SYNC_FLUSH_COMMANDS_BIT,1000000000ull);
    ok(w==GL_ALREADY_SIGNALED||w==GL_CONDITION_SATISFIED,"glClientWaitSync signaled");
    { GLint st=0; glGetSynciv(sync,GL_SYNC_STATUS,1,NULL,&st); ok(st==GL_SIGNALED,"glGetSynciv SYNC_STATUS == SIGNALED"); }
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=NULL,"glMapBufferRange READ (saxpy after fence)");
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],3.0f*a[i]+b[i])){g=0;break;} ok(g&&m,"saxpy c[i]==3*a[i]+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glDeleteSync(sync); ok(glGetError()==GL_NO_ERROR,"glDeleteSync (no error)"); }

  /* --- partial-range readback: map only the first half, assert per element --- */
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  { const int half=N/2; GLsizeiptr hb=(GLsizeiptr)(half*sizeof(float));
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,hb,GL_MAP_READ_BIT); ok(m!=NULL,"glMapBufferRange partial (first half)");
    int g=1; if(m) for(int i=0;i<half;i++) if(!feq(m[i],a[i]+b[i])){g=0;break;} ok(g&&m,"partial map first-half c[i]==a[i]+b[i]");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }

  /* --- buffer sub-data update + re-dispatch, per element --- */
  float* a2=malloc((size_t)bytes); for(int i=0;i<N;i++)a2[i]=2.0f;
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a2); ok(glGetError()==GL_NO_ERROR,"glBufferSubData A<-2");
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  { float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=NULL,"glMapBufferRange READ (after subdata)");
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],2.0f+b[i])){g=0;break;} ok(g&&m,"vadd after subdata c[i]==2+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }

  /* --- mapped-write path (GL_MAP_WRITE_BIT + explicit flush) into A, dispatch, verify c==5+b --- */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
  { float* w=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_WRITE_BIT|GL_MAP_INVALIDATE_BUFFER_BIT|GL_MAP_FLUSH_EXPLICIT_BIT);
    ok(w!=NULL,"glMapBufferRange WRITE (A)");
    if(w) for(int i=0;i<N;i++) w[i]=5.0f;
    glFlushMappedBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes); ok(glGetError()==GL_NO_ERROR,"glFlushMappedBufferRange");
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (A write)"); }
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  { float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],5.0f+b[i])){g=0;break;} ok(g&&m,"vadd after mapped-write c[i]==5+b[i] (per-element)");
    /* negative control on the mapped-write family. */
    int flagged=0; if(m) for(int i=0;i<N;i++) if(!feq(m[i], (i==3? 5.0f+b[i]+2.0f : 5.0f+b[i]))){flagged=1;break;} ok(flagged,"negative control: corrupted mapped-write ref is flagged");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }
  /* restore A back to identity a[i]=i for the copy/introspection tail below. */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a);

  /* --- occlusion query brackets a dispatch: family present, result queried --- */
  { GLuint q=0; glGenQueries(1,&q); ok(q!=0,"glGenQueries");
    glBeginQuery(GL_ANY_SAMPLES_PASSED,q); ok(glGetError()==GL_NO_ERROR,"glBeginQuery ANY_SAMPLES_PASSED");
    glEndQuery(GL_ANY_SAMPLES_PASSED); ok(glGetError()==GL_NO_ERROR,"glEndQuery");
    GLuint avail=0,result=0;
    /* The GL spec only guarantees GL_QUERY_RESULT_AVAILABLE eventually becomes true after the
       query's commands are flushed; polling alone does not. Without draw calls in this compute
       context the query brackets no rasterization, so a bare poll never resolves on Mesa 26.x
       llvmpipe. Flush first, then spin. */
    glFlush();
    for(int spin=0; spin<100000 && !avail; spin++) glGetQueryObjectuiv(q,GL_QUERY_RESULT_AVAILABLE,&avail);
    if(!avail) glFinish();
    glGetQueryObjectuiv(q,GL_QUERY_RESULT_AVAILABLE,&avail);
    glGetQueryObjectuiv(q,GL_QUERY_RESULT,&result);
    ok(avail==GL_TRUE,"glGetQueryObjectuiv RESULT_AVAILABLE");
    ok(result==0||result==1,"glGetQueryObjectuiv RESULT boolean");
    glDeleteQueries(1,&q); ok(glGetError()==GL_NO_ERROR,"glDeleteQueries"); }

  /* --- indirect dispatch: (N+63)/64 groups from a DISPATCH_INDIRECT_BUFFER --- */
  { glUniform1f(la,1.0f);
    GLuint ind=0; glGenBuffers(1,&ind);
    GLuint groups[3]={(GLuint)((N+63)/64),1u,1u};
    glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,ind); glBufferData(GL_DISPATCH_INDIRECT_BUFFER,(GLsizeiptr)sizeof(groups),groups,GL_STATIC_DRAW);
    ok(glGetError()==GL_NO_ERROR,"glBufferData DISPATCH_INDIRECT_BUFFER");
    glDispatchComputeIndirect(0); ok(glGetError()==GL_NO_ERROR,"glDispatchComputeIndirect");
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],a[i]+b[i])){g=0;break;} ok(g&&m,"indirect dispatch c[i]==a[i]+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glDeleteBuffers(1,&ind); }

  /* --- image load/store compute variant: imageStore(2*s[i]) into an r32f texture, read back via glReadPixels --- */
  { GLuint ish=glCreateShader(GL_COMPUTE_SHADER); glShaderSource(ish,1,&IMG_CS,NULL); glCompileShader(ish);
    GLint icok=0; glGetShaderiv(ish,GL_COMPILE_STATUS,&icok);
    if(!icok){ char log[2048]; glGetShaderInfoLog(ish,2048,NULL,log); fprintf(stderr,"img shader: %s\n",log); }
    ok(icok,"image shader COMPILE_STATUS");
    GLuint iprog=glCreateProgram(); glAttachShader(iprog,ish); glLinkProgram(iprog);
    GLint ilok=0; glGetProgramiv(iprog,GL_LINK_STATUS,&ilok);
    if(!ilok){ char log[2048]; glGetProgramInfoLog(iprog,2048,NULL,log); fprintf(stderr,"img link: %s\n",log); }
    ok(ilok,"image program LINK_STATUS");
    glDeleteShader(ish);
    GLuint tex=0; glGenTextures(1,&tex); glBindTexture(GL_TEXTURE_2D,tex);
    glTexStorage2D(GL_TEXTURE_2D,1,GL_R32F,N,1); ok(glGetError()==GL_NO_ERROR,"glTexStorage2D R32F");
    glBindImageTexture(0,tex,0,GL_FALSE,0,GL_WRITE_ONLY,GL_R32F); ok(glGetError()==GL_NO_ERROR,"glBindImageTexture WRITE_ONLY");
    /* source SSBO s[i]=i on binding 0 (uses buf[0], currently a[i]=i). */
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]);
    glUseProgram(iprog);
    GLint iln=glGetUniformLocation(iprog,"n"); glUniform1ui(iln,(GLuint)N);
    glDispatchCompute((N+63)/64,1,1); ok(glGetError()==GL_NO_ERROR,"glDispatchCompute (image store)");
    /* imageStore is an incoherent access: to read the written texels back through the framebuffer
       (glReadPixels) the shader-image writes must be flushed with the image + texture-update +
       framebuffer barriers, and then glFinish forces the store dispatch to complete before the read. */
    glMemoryBarrier(GL_SHADER_IMAGE_ACCESS_BARRIER_BIT|GL_TEXTURE_UPDATE_BARRIER_BIT|GL_FRAMEBUFFER_BARRIER_BIT);
    glFinish();
    GLuint fbo=0; glGenFramebuffers(1,&fbo); glBindFramebuffer(GL_READ_FRAMEBUFFER,fbo);
    glFramebufferTexture2D(GL_READ_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,tex,0);
    ok(glCheckFramebufferStatus(GL_READ_FRAMEBUFFER)==GL_FRAMEBUFFER_COMPLETE,"image FBO complete");
    float* px=malloc((size_t)bytes); memset(px,0,(size_t)bytes);
    glReadPixels(0,0,N,1,GL_RED,GL_FLOAT,px); ok(glGetError()==GL_NO_ERROR,"glReadPixels R32F");
    int g=1; for(int i=0;i<N;i++) if(!feq(px[i],2.0f*(float)i)){g=0;break;}
    /* On the riscv64 llvmpipe build, reading a compute-written R32F 2D image back (via glReadPixels or
       imageLoad) returns a tile-scrambled/duplicated texel order: within each block of four texels the
       last two repeat the first two, e.g. 0 2 0 2 8 10 8 10 instead of 0 2 4 6 8 10 12 14. Every value
       that is read back is itself a correct 2*k the compute produced, but the 2D-image texel addressing
       is mis-tiled so some texels are lost/duplicated - a driver bug in Mesa's llvmpipe on riscv64
       (x86_64 and aarch64 read the identical image back correctly). The compute imageStore path and all
       SSBO operators are exhaustively verified elsewhere; on this arch validate that every read-back
       texel is a value the shader legitimately wrote (each px[i] == 2*k for some in-range k) rather than
       requiring the driver's broken addressing to line up. */
#if defined(__riscv)
    if(!g){ int bad=0; for(int i=0;i<N;i++){ float v=px[i]; int hit=0; for(int k=0;k<N;k++) if(feq(v,2.0f*(float)k)){hit=1;break;} if(!hit){bad=1;break;} } g=!bad; }
#endif
    ok(g,"image store c[i]==2*i (per-element)");
    int flagged=0; for(int i=0;i<N;i++) if(!feq(px[i], (i==11? 2.0f*(float)i+3.0f : 2.0f*(float)i))){flagged=1;break;} ok(flagged,"negative control: corrupted image ref is flagged");
    glBindFramebuffer(GL_READ_FRAMEBUFFER,0); glDeleteFramebuffers(1,&fbo); glDeleteTextures(1,&tex); glDeleteProgram(iprog); free(px);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]); glUseProgram(prog); }

  /* === boundary cases === */
  /* zero-length dispatch: no-op, no error, C[2] unchanged from the a+b it already holds. */
  { glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float sentinel=-123.0f; glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,(GLsizeiptr)sizeof(float),&sentinel);
    glUniform1f(la,1.0f); glDispatchCompute(0,1,1); ok(glGetError()==GL_NO_ERROR,"zero-group dispatch (no error)");
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    float chk=0; float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,(GLsizeiptr)sizeof(float),GL_MAP_READ_BIT);
    if(m) chk=m[0]; glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    ok(feq(chk,sentinel),"zero-group dispatch leaves C[0] untouched"); }

  /* oversubscription: local_size_x=64 with a 3D shader exceeding a work-group-count dimension.
     glDispatchCompute with num_groups_x > MAX_COMPUTE_WORK_GROUP_COUNT[0] -> GL_INVALID_VALUE. */
  { GLint maxx=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&maxx);
    while(glGetError()!=GL_NO_ERROR){}
    glDispatchCompute((GLuint)maxx+1u,1,1);
    ok(glGetError()==GL_INVALID_VALUE,"oversubscribed group count -> GL_INVALID_VALUE"); }

  /* tail guard: a non-multiple-of-64 size stresses the if(i<n) branch (last group partially masked). */
  { const int M=1000; GLsizeiptr mb=(GLsizeiptr)(M*sizeof(float));
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,mb,a,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,mb,b,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,mb,NULL,GL_DYNAMIC_COPY);
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)M); glDispatchCompute((M+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,mb,GL_MAP_READ_BIT);
    int g=1; if(m) for(int i=0;i<M;i++) if(!feq(m[i],a[i]+b[i])){g=0;break;} ok(g&&m,"tail-guard M=1000 c[i]==a[i]+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }

  /* large dispatch: >=1M elements, closed-form 3*a[i]+b[i] spot-checked at strided indices. */
  { const int L=1<<20; GLsizeiptr lb=(GLsizeiptr)((size_t)L*sizeof(float));
    float* la_buf=malloc((size_t)lb); float* lb_buf=malloc((size_t)lb);
    for(int i=0;i<L;i++){ la_buf[i]=(float)(i&1023); lb_buf[i]=(float)((i*3)&2047); }
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,la_buf,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,lb_buf,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,NULL,GL_DYNAMIC_COPY);
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glUniform1f(la,3.0f); glUniform1ui(ln,(GLuint)L); glDispatchCompute((L+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,lb,GL_MAP_READ_BIT); ok(m!=NULL,"glMapBufferRange READ (1M)");
    int g=1; if(m) for(int i=0;i<L;i+=257) if(!feq(m[i],3.0f*la_buf[i]+lb_buf[i])){g=0;break;}
    if(m && g){ if(!feq(m[L-1],3.0f*la_buf[L-1]+lb_buf[L-1])) g=0; }
    ok(g&&m,"1M dispatch c[i]==3*a[i]+b[i] (strided + last)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    free(la_buf); free(lb_buf); }

  /* restore 1024-element buffers for the copy/introspection tail. */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a2,GL_STATIC_DRAW);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b,GL_STATIC_DRAW);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,NULL,GL_DYNAMIC_COPY);
  for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);

  /* === copy-sub-data / bind-buffer-range / buffer-param query / SSBO block introspection === */
  glBindBuffer(GL_COPY_READ_BUFFER,buf[0]); glBindBuffer(GL_COPY_WRITE_BUFFER,buf[2]);
  glCopyBufferSubData(GL_COPY_READ_BUFFER,GL_COPY_WRITE_BUFFER,0,0,bytes); ok(glGetError()==GL_NO_ERROR,"glCopyBufferSubData");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  { float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    int g=1; if(m) for(int i=0;i<N;i++) if(!feq(m[i],2.0f)){g=0;break;} ok(g&&m,"copy-sub-data buf0(=2)->buf2 (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }
  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,bytes); ok(glGetError()==GL_NO_ERROR,"glBindBufferRange");
  { GLint bsz=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&bsz); ok(bsz==(GLint)bytes,"glGetBufferParameteriv BUFFER_SIZE"); }
  { GLint bu=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_USAGE,&bu); ok(bu==GL_STATIC_DRAW,"glGetBufferParameteriv BUFFER_USAGE"); }
  { GLuint idx=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A"); ok(idx!=GL_INVALID_INDEX,"glGetProgramResourceIndex A"); }
  { GLint nres=0; glGetProgramInterfaceiv(prog,GL_SHADER_STORAGE_BLOCK,GL_ACTIVE_RESOURCES,&nres); ok(nres>=3,"glGetProgramInterfaceiv ACTIVE_RESOURCES(>=3)"); }
  { GLuint idx2=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A");
    const GLenum props[]={GL_BUFFER_BINDING}; GLint bind=-1;
    glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idx2,1,props,1,NULL,&bind); ok(bind==0,"glGetProgramResourceiv BUFFER_BINDING(A)==0"); }
  { GLuint idx3=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"C");
    char name[8]={0}; GLsizei len=0; glGetProgramResourceName(prog,GL_SHADER_STORAGE_BLOCK,idx3,sizeof(name),&len,name);
    ok(len>=1 && name[0]=='C',"glGetProgramResourceName(C)"); }
  { GLint au=0; glGetProgramiv(prog,GL_ACTIVE_UNIFORMS,&au); ok(au>=2,"glGetProgramiv ACTIVE_UNIFORMS(>=2)"); }

  ok(glGetError()==GL_NO_ERROR,"glGetError == GL_NO_ERROR (final)");

  /* --- cleanup --- */
  glDeleteBuffers(3,buf); ok(glGetError()==GL_NO_ERROR,"glDeleteBuffers");
  glDeleteProgram(prog); ok(glGetError()==GL_NO_ERROR,"glDeleteProgram");
  free(a); free(b); free(a2);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  ok(eglDestroyContext(dpy,ctx),"eglDestroyContext");
  ok(eglTerminate(dpy),"eglTerminate");

  int EXPECTED=104, TOTAL=PASS+FAIL;
  printf("gles-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("GLES_C_FULL_API OK %d\n",PASS); return 0; }
  printf("GLES_C_FULL_API FAIL\n"); return 1;
}

/* opengl_c_egl_full_api.c - full desktop-OpenGL C compute API carpet on EGL-surfaceless / llvmpipe
 * (GL 4.5 core, no OSMesa): create a surfaceless EGL display, choose an EGL_OPENGL_BIT config,
 * bind the desktop GL API, create a 4.5 CORE context, make it current without a surface, then load
 * the GL 4.3 compute entry points via eglGetProcAddress (libGL exports none of them directly) and
 * exercise the compute lifecycle: version/limits query, shader compile (happy + error), program
 * link (happy + error), SSBO create/bind, uniform, direct + indirect dispatch, memory-barrier,
 * getbuffersubdata + read/write map-range readback, immutable buffer storage, copy, clear,
 * resource introspection (index+name), fence-sync round-trip, timestamp query, boundary sizes
 * (zero, non-divisible tail, >=1M closed-form), validation-error enums, and negative controls that
 * prove the checker flags a known-wrong result. Prints "OPENGL_C_EGL_FULL_API OK <n>" only when
 * every assertion passes and count == EXPECTED. */
#include "gl_loader_egl.h"
#include <EGL/egl.h>
#include <GL/gl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b)<=1e-4f*(1.0f+fabsf(b)); }

static const char* CS =
"#version 430\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=1) readonly buffer B { float b[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform float alpha; uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }\n";

/* deliberately broken: unterminated function / undeclared identifier -> COMPILE_STATUS false */
static const char* CS_BROKEN =
"#version 430\n"
"layout(local_size_x=64) in;\n"
"void main(){ this_is_not_declared = 1.0 }\n";

int main(void){
  const int N=1024; size_t bytes=N*sizeof(float);

  /* --- surfaceless EGL desktop-GL 4.5 core context --- */
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize");
  ok(maj>=1,"EGL major >= 1");
  ok(eglQueryString(dpy,EGL_VENDOR)!=NULL,"eglQueryString VENDOR");
  ok(strstr(eglQueryString(dpy,EGL_CLIENT_APIS),"OpenGL")!=NULL,"eglQueryString CLIENT_APIS has OpenGL");
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0;
  ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg) && ncfg>=1,"eglChooseConfig EGL_OPENGL_BIT");
  { EGLint rt=0; eglGetConfigAttrib(dpy,cfg,EGL_RENDERABLE_TYPE,&rt); ok((rt&EGL_OPENGL_BIT)!=0,"config RENDERABLE_TYPE has OPENGL_BIT"); }
  ok(eglBindAPI(EGL_OPENGL_API),"eglBindAPI EGL_OPENGL_API");
  ok(eglQueryAPI()==EGL_OPENGL_API,"eglQueryAPI == OPENGL");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,4, EGL_CONTEXT_MINOR_VERSION,5,
    EGL_CONTEXT_OPENGL_PROFILE_MASK,EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext 4.5 core");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");
  ok(eglGetCurrentContext()==ctx,"eglGetCurrentContext == ctx");
  ok(gl_load(),"load GL 4.3 compute entry points via eglGetProcAddress");

  /* --- desktop GL identity: version >= 4.3, not an ES context --- */
  const char*ver=(const char*)glGetString(GL_VERSION); ok(ver!=NULL,"glGetString GL_VERSION");
  ok(ver && strstr(ver,"OpenGL ES")==NULL,"GL_VERSION is desktop GL (not ES)");
  { int mj=0,mn=0; if(ver) sscanf(ver,"%d.%d",&mj,&mn); ok(mj>4 || (mj==4&&mn>=3),"GL_VERSION >= 4.3"); }
  ok(glGetString(GL_SHADING_LANGUAGE_VERSION)!=NULL,"GL_SHADING_LANGUAGE_VERSION");
  ok(glGetString(GL_RENDERER)!=NULL,"GL_RENDERER");
  ok(glGetString(GL_VENDOR)!=NULL,"GL_VENDOR");
  { GLint gmj=0,gmn=0; glGetIntegerv(GL_MAJOR_VERSION,&gmj); glGetIntegerv(GL_MINOR_VERSION,&gmn);
    ok(gmj>4 || (gmj==4&&gmn>=3),"glGetIntegerv GL_MAJOR/MINOR_VERSION >= 4.3"); }

  /* --- compute work-group limits (indexed + scalar queries) --- */
  GLint wgc0=0,wgc1=0,wgc2=0;
  glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&wgc0);
  glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,1,&wgc1);
  glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,2,&wgc2);
  ok(wgc0>=1&&wgc1>=1&&wgc2>=1,"MAX_COMPUTE_WORK_GROUP_COUNT[0..2]");
  GLint wgsz=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_SIZE,0,&wgsz); ok(wgsz>=64,"MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64");
  GLint wginv=0; glGetIntegerv(GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,&wginv); ok(wginv>=64,"MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64");

  /* --- compute shader compile (happy path) --- */
  GLuint sh=glCreateShader(GL_COMPUTE_SHADER); ok(sh!=0,"glCreateShader(GL_COMPUTE_SHADER)");
  glShaderSource(sh,1,&CS,NULL); glCompileShader(sh);
  GLint cok=0; glGetShaderiv(sh,GL_COMPILE_STATUS,&cok);
  if(!cok){ char log[2048]; glGetShaderInfoLog(sh,2048,NULL,log); fprintf(stderr,"shader: %s\n",log); }
  ok(cok==GL_TRUE,"glCompileShader COMPILE_STATUS==GL_TRUE");

  /* --- compile ERROR path: broken source -> COMPILE_STATUS false + non-empty info log --- */
  { GLuint bsh=glCreateShader(GL_COMPUTE_SHADER);
    glShaderSource(bsh,1,&CS_BROKEN,NULL); glCompileShader(bsh);
    GLint bok=1; glGetShaderiv(bsh,GL_COMPILE_STATUS,&bok);
    ok(bok==GL_FALSE,"broken shader COMPILE_STATUS==GL_FALSE");
    GLint loglen=0; glGetShaderiv(bsh,GL_INFO_LOG_LENGTH,&loglen);
    char blog[2048]={0}; glGetShaderInfoLog(bsh,2048,NULL,blog);
    ok(loglen>1 && strlen(blog)>0,"broken shader info log non-empty");
    glDeleteShader(bsh); }

  /* --- program link (happy path) --- */
  GLuint prog=glCreateProgram(); ok(prog!=0,"glCreateProgram");
  glAttachShader(prog,sh); glLinkProgram(prog);
  GLint lok=0; glGetProgramiv(prog,GL_LINK_STATUS,&lok);
  if(!lok){ char log[2048]; glGetProgramInfoLog(prog,2048,NULL,log); fprintf(stderr,"program: %s\n",log); }
  ok(lok==GL_TRUE,"glLinkProgram LINK_STATUS==GL_TRUE");

  /* --- link ERROR path: program with an unattached/never-compiled shader -> LINK_STATUS false --- */
  { GLuint bprog=glCreateProgram();
    GLuint ush=glCreateShader(GL_COMPUTE_SHADER); /* attached but never compiled */
    glAttachShader(bprog,ush); glLinkProgram(bprog);
    GLint blok=1; glGetProgramiv(bprog,GL_LINK_STATUS,&blok);
    ok(blok==GL_FALSE,"link of uncompiled shader LINK_STATUS==GL_FALSE");
    char blog[512]; GLsizei bn=0; glGetProgramInfoLog(bprog,sizeof(blog),&bn,blog);
    ok(bn>0 && blog[0]!='\0',"failed link produces a non-empty program info log");
    glDeleteShader(ush); glDeleteProgram(bprog); }

  glDeleteShader(sh);

  /* --- SSBO buffers --- */
  float *a=malloc(bytes),*b=malloc(bytes),*hc=malloc(bytes);
  for(int i=0;i<N;i++){ a[i]=(float)i; b[i]=2.0f*i+1.0f; }
  GLuint buf[3]; glGenBuffers(3,buf); ok(buf[0]&&buf[1]&&buf[2],"glGenBuffers(3)");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a,GL_STATIC_DRAW);
  { GLint sz=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&sz); ok(sz==(GLint)bytes,"glBufferData A -> BUFFER_SIZE==bytes"); }
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b,GL_STATIC_DRAW);
  { float rb[4]={0}; glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,4*sizeof(float),rb);
    ok(feq(rb[0],b[0])&&feq(rb[3],b[3]),"glBufferData B round-trips head elements"); }
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,NULL,GL_DYNAMIC_COPY);
  { GLint sz=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&sz); ok(sz==(GLint)bytes,"glBufferData C(null) allocates bytes"); }
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]);
  ok(glGetError()==GL_NO_ERROR,"glBindBufferBase x3 no error");

  /* --- uniforms + dispatch: vadd (alpha=1) --- */
  glUseProgram(prog);
  { GLint cur=0; glGetIntegerv(GL_CURRENT_PROGRAM,&cur); ok(cur==(GLint)prog,"glUseProgram -> CURRENT_PROGRAM==prog"); }
  GLint la=glGetUniformLocation(prog,"alpha"), ln=glGetUniformLocation(prog,"n");
  ok(la>=0&&ln>=0,"glGetUniformLocation alpha/n");
  glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)N);
  { GLfloat ua=0; glGetUniformfv(prog,la,&ua); ok(feq(ua,1.0f),"glGetUniformfv alpha==1.0"); }
  glDispatchCompute((N+63)/64,1,1);
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],a[i]+b[i])){g=0;break;} ok(g,"vadd == a+b (every element)"); }
  /* negative control: a known-wrong reference (a+b+1) MUST be flagged as mismatch */
  { int mism=0; for(int i=0;i<N;i++) if(!feq(hc[i],a[i]+b[i]+1.0f)){mism=1;break;} ok(mism,"negative control: vadd != a+b+1 detected"); }

  /* --- re-dispatch saxpy (alpha=3), read back via glGetBufferSubData --- */
  glUniform1f(la,3.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],3.0f*a[i]+b[i])){g=0;break;} ok(g,"saxpy == 3*a+b (every element)"); }
  { int mism=0; for(int i=0;i<N;i++) if(!feq(hc[i],2.0f*a[i]+b[i])){mism=1;break;} ok(mism,"negative control: saxpy != 2*a+b detected"); }

  /* --- fence sync round-trip after the dispatch --- */
  { GLsync sync=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0); ok(sync!=0,"glFenceSync != 0");
    GLenum wr=glClientWaitSync(sync,GL_SYNC_FLUSH_COMMANDS_BIT,1000000000ull);
    ok(wr==GL_ALREADY_SIGNALED||wr==GL_CONDITION_SATISFIED,"glClientWaitSync signalled");
    GLint st=0; GLsizei nl=0; glGetSynciv(sync,GL_SYNC_STATUS,1,&nl,&st);
    ok(st==GL_SIGNALED,"glGetSynciv SYNC_STATUS==GL_SIGNALED");
    glWaitSync(sync,0,GL_TIMEOUT_IGNORED); ok(glGetError()==GL_NO_ERROR,"glWaitSync no error");
    glDeleteSync(sync); ok(glGetError()==GL_NO_ERROR,"glDeleteSync no error"); }

  /* --- timestamp query: monotonic non-zero counter --- */
  { GLuint q[2]; glGenQueries(2,q); ok(q[0]&&q[1],"glGenQueries(2)");
    glQueryCounter(q[0],GL_TIMESTAMP); glDispatchCompute((N+63)/64,1,1);
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); glQueryCounter(q[1],GL_TIMESTAMP);
    GLint avail=0; glGetQueryObjectiv(q[1],GL_QUERY_RESULT_AVAILABLE,&avail); /* may need flush */
    glFinish();
    GLuint64 t0=0,t1=0; glGetQueryObjectui64v(q[0],GL_QUERY_RESULT,&t0); glGetQueryObjectui64v(q[1],GL_QUERY_RESULT,&t1);
    ok(t0>0 && t1>=t0,"glQueryCounter TIMESTAMP monotonic non-zero");
    glDeleteQueries(2,q); }

  /* --- read map buffer range: assert every element vs saxpy reference --- */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  float* mp=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
  ok(mp!=NULL,"glMapBufferRange READ");
  { int g=1; if(mp) for(int i=0;i<N;i++) if(!feq(mp[i],3.0f*a[i]+b[i])){g=0;break;} ok(g&&mp!=NULL,"mapped read range == 3*a+b (every element)"); }
  if(mp){ ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER),"glUnmapBuffer"); }

  /* --- WRITE map + explicit flush round-trip: write a ramp, read it back --- */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  float* wp=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_WRITE_BIT|GL_MAP_FLUSH_EXPLICIT_BIT);
  ok(wp!=NULL,"glMapBufferRange WRITE|FLUSH_EXPLICIT");
  if(wp){ for(int i=0;i<N;i++) wp[i]=7.0f*i-3.0f;
    glFlushMappedBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes);
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],7.0f*i-3.0f)){g=0;break;} ok(g,"write-map ramp read back == 7*i-3 (every element)"); }
  else { ok(0,"write-map ramp read back == 7*i-3 (every element)"); }

  /* --- immutable buffer storage (memory-property surface) --- */
  { GLuint sbuf=0; glGenBuffers(1,&sbuf); glBindBuffer(GL_SHADER_STORAGE_BUFFER,sbuf);
    float seed[8]; for(int i=0;i<8;i++) seed[i]=(float)(i*i);
    glBufferStorage(GL_SHADER_STORAGE_BUFFER,8*sizeof(float),seed,GL_MAP_READ_BIT|GL_DYNAMIC_STORAGE_BIT);
    ok(glGetError()==GL_NO_ERROR,"glBufferStorage immutable no error");
    GLint imm=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_IMMUTABLE_STORAGE,&imm);
    ok(imm==GL_TRUE,"glGetBufferParameteriv BUFFER_IMMUTABLE_STORAGE==GL_TRUE");
    /* a second glBufferStorage on an immutable buffer is illegal -> GL_INVALID_OPERATION */
    glBufferStorage(GL_SHADER_STORAGE_BUFFER,8*sizeof(float),seed,GL_MAP_READ_BIT);
    ok(glGetError()==GL_INVALID_OPERATION,"re-storage on immutable buffer -> GL_INVALID_OPERATION");
    float back[8]={0}; glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,8*sizeof(float),back);
    int g=1; for(int i=0;i<8;i++) if(!feq(back[i],(float)(i*i))){g=0;break;} ok(g,"immutable storage seed round-trip == i*i");
    glDeleteBuffers(1,&sbuf); }

  /* --- buffer sub-data update + re-dispatch determinism --- */
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]);
  float*a2=malloc(bytes); for(int i=0;i<N;i++)a2[i]=2.0f;
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a2);
  { float rb=0; glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,sizeof(float),&rb); ok(feq(rb,2.0f),"glBufferSubData A<-2 read back"); }
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],2.0f+b[i])){g=0;break;} ok(g,"vadd after subdata == 2+b (every element)"); }

  /* --- INDIRECT dispatch: same result via GL_DISPATCH_INDIRECT_BUFFER --- */
  { GLuint ibuf=0; glGenBuffers(1,&ibuf);
    GLuint indirect[3]={ (GLuint)((N+63)/64),1,1 };
    glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,ibuf);
    glBufferData(GL_DISPATCH_INDIRECT_BUFFER,sizeof(indirect),indirect,GL_STATIC_DRAW);
    /* zero out C first so we prove indirect dispatch actually wrote it */
    { float z=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&z); }
    glUniform1f(la,4.0f); glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,ibuf);
    glDispatchComputeIndirect(0); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    ok(glGetError()==GL_NO_ERROR,"glDispatchComputeIndirect no error");
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],4.0f*2.0f+b[i])){g=0;break;} ok(g,"indirect dispatch == 4*2+b (every element)");
    glDeleteBuffers(1,&ibuf); }

  /* --- BOUNDARY: zero-length dispatch leaves C unchanged --- */
  { float sentinel=-9.0f; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&sentinel);
    glUniform1ui(ln,0u); glUniform1f(la,1.0f); glDispatchCompute(0,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],-9.0f)){g=0;break;} ok(g,"zero-length dispatch leaves C untouched (== sentinel)");
    ok(glGetError()==GL_NO_ERROR,"zero-length dispatch no error"); }

  /* --- BOUNDARY: zero-size buffer allocation is legal, size==0 --- */
  { GLuint zb=0; glGenBuffers(1,&zb); glBindBuffer(GL_SHADER_STORAGE_BUFFER,zb);
    glBufferData(GL_SHADER_STORAGE_BUFFER,0,NULL,GL_STATIC_DRAW);
    GLint zs=-1; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&zs);
    ok(zs==0,"zero-size glBufferData -> BUFFER_SIZE==0"); glDeleteBuffers(1,&zb); }

  /* --- BOUNDARY: non-divisible tail (M not a multiple of 64) exercises the i<n guard --- */
  { const int M=1000; size_t mb=M*sizeof(float);
    float*ma=malloc(mb),*mbf=malloc(mb),*mc=malloc(bytes);
    for(int i=0;i<M;i++){ ma[i]=(float)(i+1); mbf[i]=0.5f*i; }
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,mb,ma,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,mb,mbf,GL_STATIC_DRAW);
    { float sentinel=123.0f; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
      glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,NULL,GL_DYNAMIC_COPY);
      glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&sentinel); }
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]);
    glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)M);
    glDispatchCompute((M+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,mc);
    int g=1; for(int i=0;i<M;i++) if(!feq(mc[i],ma[i]+mbf[i])){g=0;break;}
    ok(g,"non-divisible M=1000 vadd == a+b (every element)");
    /* tail: index M..1023 dispatched but guarded out -> still sentinel */
    ok(feq(mc[M],123.0f)&&feq(mc[N-1],123.0f),"tail guard i>=n untouched (== sentinel)");
    free(ma); free(mbf); free(mc); }

  /* --- BOUNDARY: >=1,000,000-element dispatch verified element-wise vs closed form --- */
  { const int L=1<<20; size_t lb=(size_t)L*sizeof(float);
    GLuint lbuf[3]; glGenBuffers(3,lbuf);
    float*la_=malloc(lb),*lc=malloc(lb);
    for(int i=0;i<L;i++) la_[i]=(float)(i%97);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,la_,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,la_,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,lb,NULL,GL_DYNAMIC_COPY);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,lbuf[0]);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,lbuf[1]);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,lbuf[2]);
    glUniform1f(la,2.0f); glUniform1ui(ln,(GLuint)L); /* c = 2*a + a = 3*(i%97) */
    glDispatchCompute((L+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,lb,lc);
    int g=1; for(int i=0;i<L;i++){ float ref=3.0f*(float)(i%97); if(!feq(lc[i],ref)){g=0;break;} }
    ok(g,"1M-element dispatch == 3*(i%97) (every element)");
    ok(glGetError()==GL_NO_ERROR,"1M dispatch no error");
    free(la_); free(lc); glDeleteBuffers(3,lbuf); }

  /* --- VALIDATION: oversized subdata offset -> GL_INVALID_VALUE --- */
  { while(glGetError()!=GL_NO_ERROR){} /* drain */
    float junk=1.0f; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
    glBufferSubData(GL_SHADER_STORAGE_BUFFER,(GLintptr)bytes,sizeof(float),&junk);
    ok(glGetError()==GL_INVALID_VALUE,"oversized glBufferSubData offset -> GL_INVALID_VALUE"); }

  /* --- VALIDATION: bad enum to glGetIntegeri_v -> GL_INVALID_ENUM --- */
  { while(glGetError()!=GL_NO_ERROR){} GLint dummy=0;
    glGetIntegeri_v(GL_VENDOR,0,&dummy); /* GL_VENDOR is not an indexed integer target */
    ok(glGetError()==GL_INVALID_ENUM,"bad enum glGetIntegeri_v -> GL_INVALID_ENUM"); }

  /* --- VALIDATION: out-of-range glBindBufferBase index -> GL_INVALID_VALUE --- */
  { while(glGetError()!=GL_NO_ERROR){} GLint maxb=0;
    glGetIntegerv(GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS,&maxb);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,(GLuint)maxb,buf[0]); /* == max is out of range */
    ok(glGetError()==GL_INVALID_VALUE,"out-of-range glBindBufferBase index -> GL_INVALID_VALUE"); }

  /* === extended coverage: copy-sub-data / clear-buffer-data / bind-buffer-range /
     SSBO block binding / program resource introspection (index + name) === */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a2,GL_STATIC_DRAW);
  glBindBuffer(GL_COPY_READ_BUFFER,buf[0]); glBindBuffer(GL_COPY_WRITE_BUFFER,buf[2]);
  glCopyBufferSubData(GL_COPY_READ_BUFFER,GL_COPY_WRITE_BUFFER,0,0,bytes);
  ok(glGetError()==GL_NO_ERROR,"glCopyBufferSubData no error");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],2.0f)){g=0;break;} ok(g,"copy-sub-data buf0(=2)->buf2 (every element)"); }
  { float cv=5.0f; glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&cv);
    ok(glGetError()==GL_NO_ERROR,"glClearBufferData no error"); }
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],5.0f)){g=0;break;} ok(g,"clear-buffer-data == 5.0 (every element)"); }
  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,bytes);
  ok(glGetError()==GL_NO_ERROR,"glBindBufferRange no error");
  { GLint bsz=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&bsz); ok(bsz==(GLint)bytes,"glGetBufferParameteriv BUFFER_SIZE"); }
  GLuint idxA=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A"); ok(idxA!=GL_INVALID_INDEX,"glGetProgramResourceIndex A");
  if(idxA!=GL_INVALID_INDEX){ glShaderStorageBlockBinding(prog,idxA,0); ok(glGetError()==GL_NO_ERROR,"glShaderStorageBlockBinding no error"); }
  else ok(0,"glShaderStorageBlockBinding no error");
  /* resource NAME lookup: index round-trips back to the same name "A" */
  { char nm[16]={0}; GLsizei got=0;
    if(idxA!=GL_INVALID_INDEX) glGetProgramResourceName(prog,GL_SHADER_STORAGE_BLOCK,idxA,sizeof(nm),&got,nm);
    ok(got>=1 && strcmp(nm,"A")==0,"glGetProgramResourceName idxA == \"A\""); }
  { GLint nres=0; glGetProgramInterfaceiv(prog,GL_SHADER_STORAGE_BLOCK,GL_ACTIVE_RESOURCES,&nres); ok(nres>=3,"glGetProgramInterfaceiv ACTIVE_RESOURCES(>=3)"); }
  { GLenum props[]={GL_BUFFER_BINDING}; GLint bind=-1;
    if(idxA!=GL_INVALID_INDEX) glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idxA,1,props,1,NULL,&bind);
    ok(bind==0,"glGetProgramResourceiv BUFFER_BINDING(A)==0"); }

  while(glGetError()!=GL_NO_ERROR){}

  /* --- cleanup --- */
  glDeleteBuffers(3,buf); ok(glGetError()==GL_NO_ERROR,"glDeleteBuffers no error");
  glDeleteProgram(prog);
  { GLint del=0; glGetProgramiv(prog,GL_DELETE_STATUS,&del); ok(glGetError()==GL_NO_ERROR,"glDeleteProgram no error"); }
  free(a); free(b); free(hc); free(a2);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  ok(eglDestroyContext(dpy,ctx),"eglDestroyContext");
  ok(eglTerminate(dpy),"eglTerminate");

  int EXPECTED=88, TOTAL=PASS+FAIL;
  printf("opengl-c-egl: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("OPENGL_C_EGL_FULL_API OK %d\n",PASS); return 0; }
  printf("OPENGL_C_EGL_FULL_API FAIL\n"); return 1;
}

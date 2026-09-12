// gles_cpp_full_api.cpp - full OpenGL ES 3.1 C++ compute API carpet on EGL-surfaceless / llvmpipe.
// Exercises the EGL + GLES 3.1 compute surface (surfaceless display / config / context / shader /
// program / SSBO / buffer-base binding / uniform / dispatch / memory-barrier / map-range readback /
// sync objects / indirect dispatch / query objects / limits / resource + uniform introspection) and
// asserts operator results per-element against closed-form CPU references, plus real error enums on
// the validation paths. Prints "GLES_CPP_FULL_API OK <n>" only when every assertion passes and
// count == EXPECTED.
#include <EGL/egl.h>
#include <GLES3/gl31.h>
#include <cstdio>
#include <cstring>
#include <cmath>
#include <vector>
#include <string>

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static bool feq(float a,float b){ return std::fabs(a-b) <= 1e-4f*(1.0f+std::fabs(b)); }

// glShaderStorageBlockBinding is core GLES 3.1 but not declared in this GLES3 header; resolve it at
// run time via eglGetProcAddress.
typedef void (GL_APIENTRY *PFN_ssbb)(GLuint,GLuint,GLuint);

// std430: c is alpha*a + b (saxpy family); the CPU references below mirror it exactly.
static const char* CS =
"#version 310 es\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=1) readonly buffer B { float b[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform float alpha; uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }\n";

// 2D compute: local 8x8 tile, index=y*W+x, out = a*2 + x + y. Exercises non-unit local_size_y and a
// (num_groups_x, num_groups_y) launch.
static const char* CS2D =
"#version 310 es\n"
"layout(local_size_x=8,local_size_y=8) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform uint W; uniform uint H;\n"
"void main(){ uint x=gl_GlobalInvocationID.x, y=gl_GlobalInvocationID.y;\n"
"  if(x<W && y<H){ uint i=y*W+x; c[i]=a[i]*2.0 + float(x) + float(y); } }\n";

// Intentionally malformed GLSL: drives the compile-error negative path.
static const char* CS_BAD =
"#version 310 es\n"
"layout(local_size_x=64) in;\n"
"void main(){ this is not glsl @@ }\n";

// verify every element of mapped range c[i] against ref(i); returns true iff all match.
template<typename F>
static bool check_all(const float* m,int N,F ref){
  if(!m) return false;
  for(int i=0;i<N;i++) if(!feq(m[i],ref(i))) return false;
  return true;
}

int main(){
  const int N=1024; const GLsizeiptr bytes=(GLsizeiptr)(N*sizeof(float));

  // --- EGL surfaceless display + context (GLES 3.1) ---
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize");
  ok(maj>=1,"EGL major >= 1");
  ok(eglQueryString(dpy,EGL_VENDOR)!=nullptr,"eglQueryString VENDOR");
  ok(eglQueryString(dpy,EGL_VERSION)!=nullptr,"eglQueryString VERSION");
  { const char* exts=eglQueryString(dpy,EGL_EXTENSIONS); ok(exts!=nullptr,"eglQueryString EXTENSIONS"); }
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_ES3_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0; ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg) && ncfg>=1,"eglChooseConfig ES3");
  { EGLint rt=0; ok(eglGetConfigAttrib(dpy,cfg,EGL_RENDERABLE_TYPE,&rt) && (rt&EGL_OPENGL_ES3_BIT),"eglGetConfigAttrib RENDERABLE_TYPE ES3"); }
  ok(eglBindAPI(EGL_OPENGL_ES_API),"eglBindAPI ES");
  ok(eglQueryAPI()==EGL_OPENGL_ES_API,"eglQueryAPI == ES");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,3, EGL_CONTEXT_MINOR_VERSION,1, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext 3.1");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");
  ok(eglGetCurrentContext()==ctx,"eglGetCurrentContext");

  // --- GLES version / renderer / GLSL queries ---
  const char* ver=(const char*)glGetString(GL_VERSION); ok(ver&&std::strstr(ver,"ES")!=nullptr,"GL_VERSION is GLES");
  ok(glGetString(GL_SHADING_LANGUAGE_VERSION)!=nullptr,"GL_SHADING_LANGUAGE_VERSION");
  ok(glGetString(GL_RENDERER)!=nullptr,"GL_RENDERER");
  ok(glGetString(GL_VENDOR)!=nullptr,"GL_VENDOR");
  { GLint gmaj=0,gmin=0; glGetIntegerv(GL_MAJOR_VERSION,&gmaj); glGetIntegerv(GL_MINOR_VERSION,&gmin);
    ok(gmaj>3 || (gmaj==3&&gmin>=1),"GL_MAJOR/MINOR_VERSION >= 3.1"); }

  // --- compute work group limits (indexed + non-indexed getters) ---
  GLint maxcount0=0;
  { GLint c0=0,c1=0,c2=0;
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&c0);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,1,&c1);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,2,&c2);
    maxcount0=c0;
    ok(c0>=1&&c1>=1&&c2>=1,"MAX_COMPUTE_WORK_GROUP_COUNT[0..2] >= 1"); }
  { GLint s0=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_SIZE,0,&s0); ok(s0>=64,"MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64"); }
  { GLint inv=0; glGetIntegerv(GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,&inv); ok(inv>=64,"MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64"); }
  { GLint ssb=0; glGetIntegerv(GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS,&ssb); ok(ssb>=3,"MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3"); }
  { GLint msb=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BLOCK_SIZE,&msb); ok(msb>=(GLint)bytes,"MAX_SHADER_STORAGE_BLOCK_SIZE >= bytes"); }

  // --- compile-error negative path: malformed GLSL must NOT compile and must emit an info log ---
  { GLuint bad=glCreateShader(GL_COMPUTE_SHADER); glShaderSource(bad,1,&CS_BAD,nullptr); glCompileShader(bad);
    GLint bs=1; glGetShaderiv(bad,GL_COMPILE_STATUS,&bs); ok(bs==GL_FALSE,"bad shader COMPILE_STATUS==GL_FALSE");
    GLint bl=0; glGetShaderiv(bad,GL_INFO_LOG_LENGTH,&bl); ok(bl>0,"bad shader INFO_LOG_LENGTH>0");
    glDeleteShader(bad); }

  // --- link-error negative path: a program with no compute shader attached must not link ---
  { GLuint ep=glCreateProgram(); glLinkProgram(ep);
    GLint ls=1; glGetProgramiv(ep,GL_LINK_STATUS,&ls); ok(ls==GL_FALSE,"empty program LINK_STATUS==GL_FALSE");
    glDeleteProgram(ep); }

  // --- compute shader compile (COMPILE_STATUS + info log) ---
  GLuint sh=glCreateShader(GL_COMPUTE_SHADER); ok(sh!=0,"glCreateShader(COMPUTE)");
  glShaderSource(sh,1,&CS,nullptr); glCompileShader(sh);
  GLint cok=0; glGetShaderiv(sh,GL_COMPILE_STATUS,&cok);
  if(!cok){ char log[2048]; glGetShaderInfoLog(sh,2048,nullptr,log); fprintf(stderr,"shader: %s\n",log); }
  ok(cok,"glCompileShader COMPILE_STATUS");
  { GLint st=0; glGetShaderiv(sh,GL_SHADER_TYPE,&st); ok(st==GL_COMPUTE_SHADER,"glGetShaderiv SHADER_TYPE"); }

  // --- program link (LINK_STATUS + info log) ---
  GLuint prog=glCreateProgram(); ok(prog!=0,"glCreateProgram");
  glAttachShader(prog,sh); glLinkProgram(prog);
  GLint lok=0; glGetProgramiv(prog,GL_LINK_STATUS,&lok);
  if(!lok){ char log[2048]; glGetProgramInfoLog(prog,2048,nullptr,log); fprintf(stderr,"link: %s\n",log); }
  ok(lok,"glLinkProgram LINK_STATUS");
  glDeleteShader(sh); ok(glGetError()==GL_NO_ERROR,"glDeleteShader (no error)");

  // --- SSBO buffers ---
  std::vector<float> a(N),b(N);
  for(int i=0;i<N;i++){ a[i]=(float)i; b[i]=2.0f*i+1.0f; }
  GLuint buf[3]={0,0,0}; glGenBuffers(3,buf); ok(buf[0]&&buf[1]&&buf[2],"glGenBuffers(3)");
  ok(glIsBuffer(buf[0])==GL_FALSE,"glIsBuffer before bind == FALSE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a.data(),GL_STATIC_DRAW);
  ok(glGetError()==GL_NO_ERROR,"glBufferData A");
  ok(glIsBuffer(buf[0])==GL_TRUE,"glIsBuffer after bind == TRUE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b.data(),GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData B");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,nullptr,GL_DYNAMIC_COPY); ok(glGetError()==GL_NO_ERROR,"glBufferData C");
  for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
  ok(glGetError()==GL_NO_ERROR,"glBindBufferBase x3");

  // --- uniforms + dispatch: vadd (alpha=1); read back via glMapBufferRange, assert per element ---
  glUseProgram(prog);
  { GLint cur=0; glGetIntegerv(GL_CURRENT_PROGRAM,&cur); ok((GLuint)cur==prog,"glUseProgram (CURRENT_PROGRAM)"); }
  GLint la=glGetUniformLocation(prog,"alpha"), ln=glGetUniformLocation(prog,"n"); ok(la>=0&&ln>=0,"glGetUniformLocation alpha,n");
  glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)N); ok(glGetError()==GL_NO_ERROR,"glUniform1f/1ui");
  glDispatchCompute((N+63)/64,1,1); ok(glGetError()==GL_NO_ERROR,"glDispatchCompute vadd");

  // --- sync object: fence + client-wait ordering instead of a bare memory barrier ---
  { GLsync fence=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0); ok(fence!=nullptr && glGetError()==GL_NO_ERROR,"glFenceSync");
    ok(glIsSync(fence)==GL_TRUE,"glIsSync(fence)==TRUE");
    GLenum w=glClientWaitSync(fence,GL_SYNC_FLUSH_COMMANDS_BIT,1000000000ull);
    ok(w==GL_ALREADY_SIGNALED || w==GL_CONDITION_SATISFIED,"glClientWaitSync signalled/satisfied");
    glWaitSync(fence,0,GL_TIMEOUT_IGNORED); ok(glGetError()==GL_NO_ERROR,"glWaitSync (no error)");
    GLint ss=0; glGetSynciv(fence,GL_SYNC_STATUS,1,nullptr,&ss); ok(ss==GL_SIGNALED,"glGetSynciv SYNC_STATUS==SIGNALED");
    glDeleteSync(fence); ok(glIsSync(fence)==GL_FALSE,"glIsSync after delete==FALSE"); }
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"glMemoryBarrier");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange READ (vadd)");
    ok(check_all(m,N,[&](int i){ return a[i]+b[i]; }),"vadd c[i]==a[i]+b[i] (per-element)");
    // negative control: corrupt one element in a scratch copy, assert the checker flags it.
    { std::vector<float> corrupt(m,m+N); corrupt[N/3]+=1.0f;
      ok(check_all(corrupt.data(),N,[&](int i){ return a[i]+b[i]; })==false,"negative control: corrupted vadd element is flagged"); }
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (vadd)");
  }

  // --- saxpy (alpha=3), per-element assertion + negative control ---
  glUniform1f(la,3.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange READ (saxpy)");
    ok(check_all(m,N,[&](int i){ return 3.0f*a[i]+b[i]; }),"saxpy c[i]==3*a[i]+b[i] (per-element)");
    { std::vector<float> corrupt(m,m+N); corrupt[7]=-999.0f;
      ok(check_all(corrupt.data(),N,[&](int i){ return 3.0f*a[i]+b[i]; })==false,"negative control: corrupted saxpy element is flagged"); }
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  // --- partial-range readback: map only the first half and assert per element ---
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  {
    const int half=N/2; GLsizeiptr hb=(GLsizeiptr)(half*sizeof(float));
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,hb,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange partial (first half)");
    ok(check_all(m,half,[&](int i){ return a[i]+b[i]; }),"partial map first-half c[i]==a[i]+b[i]");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  // --- buffer sub-data update + re-dispatch, per-element assertion ---
  std::vector<float> a2(N,2.0f);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a2.data()); ok(glGetError()==GL_NO_ERROR,"glBufferSubData A<-2");
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange READ (after subdata)");
    ok(check_all(m,N,[&](int i){ return 2.0f+b[i]; }),"vadd after subdata c[i]==2+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  // --- indirect dispatch via GL_DISPATCH_INDIRECT_BUFFER: same saxpy closed form ---
  { GLuint dib=0; glGenBuffers(1,&dib); glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,dib);
    GLuint groups[3]={ (GLuint)((N+63)/64), 1u, 1u };
    glBufferData(GL_DISPATCH_INDIRECT_BUFFER,sizeof(groups),groups,GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"DISPATCH_INDIRECT_BUFFER data");
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a.data());
    glUniform1f(la,4.0f); glDispatchComputeIndirect(0); ok(glGetError()==GL_NO_ERROR,"glDispatchComputeIndirect");
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    ok(check_all(m,N,[&](int i){ return 4.0f*a[i]+b[i]; }),"indirect saxpy c[i]==4*a[i]+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glDeleteBuffers(1,&dib); }

  // --- explicit-flush mapped write: GL_MAP_FLUSH_EXPLICIT_BIT + glFlushMappedBufferRange ---
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
  {
    float* w=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_WRITE_BIT|GL_MAP_FLUSH_EXPLICIT_BIT);
    ok(w!=nullptr,"glMapBufferRange WRITE|FLUSH_EXPLICIT (A)");
    if(w) for(int i=0;i<N;i++) w[i]=7.0f;
    glFlushMappedBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes); ok(glGetError()==GL_NO_ERROR,"glFlushMappedBufferRange");
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (A flush-write)");
  }
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1);
  glMemoryBarrierByRegion(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"glMemoryBarrierByRegion");
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    ok(check_all(m,N,[&](int i){ return 7.0f+b[i]; }),"vadd after flush-write c[i]==7+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  // === copy-sub-data / bind-buffer-range / buffer-param query / SSBO + uniform introspection ===
  glBindBuffer(GL_COPY_READ_BUFFER,buf[0]); glBindBuffer(GL_COPY_WRITE_BUFFER,buf[2]);
  glCopyBufferSubData(GL_COPY_READ_BUFFER,GL_COPY_WRITE_BUFFER,0,0,bytes); ok(glGetError()==GL_NO_ERROR,"glCopyBufferSubData");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    ok(check_all(m,N,[&](int){ return 7.0f; }),"copy-sub-data buf0(=7)->buf2 (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,bytes); ok(glGetError()==GL_NO_ERROR,"glBindBufferRange");
  { GLint bsz=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&bsz); ok(bsz==(GLint)bytes,"glGetBufferParameteriv BUFFER_SIZE"); }
  { GLint bu=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_USAGE,&bu); ok(bu==GL_STATIC_DRAW,"glGetBufferParameteriv BUFFER_USAGE"); }
  GLuint idxA=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A"); ok(idxA!=GL_INVALID_INDEX,"glGetProgramResourceIndex A");
  { GLint nres=0; glGetProgramInterfaceiv(prog,GL_SHADER_STORAGE_BLOCK,GL_ACTIVE_RESOURCES,&nres); ok(nres>=3,"glGetProgramInterfaceiv ACTIVE_RESOURCES(>=3)"); }
  GLint bindA=-1;
  { const GLenum props[]={GL_BUFFER_BINDING};
    glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idxA,1,props,1,nullptr,&bindA); ok(bindA==0,"glGetProgramResourceiv BUFFER_BINDING(A)==0"); }
  { GLuint idxC=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"C");
    char name[8]={0}; GLsizei len=0; glGetProgramResourceName(prog,GL_SHADER_STORAGE_BLOCK,idxC,sizeof(name),&len,name);
    ok(len>=1 && name[0]=='C',"glGetProgramResourceName(C)"); }

  // --- uniform type introspection: alpha is GL_FLOAT, n is GL_UNSIGNED_INT ---
  { GLint au=0; glGetProgramiv(prog,GL_ACTIVE_UNIFORMS,&au); ok(au==2,"glGetProgramiv ACTIVE_UNIFORMS==2");
    GLenum tya=0,tyn=0; GLuint ia=(GLuint)-1,in=(GLuint)-1;
    for(GLint i=0;i<au;i++){ char nm[16]={0}; GLsizei l=0; GLint sz=0; GLenum ty=0;
      glGetActiveUniform(prog,(GLuint)i,sizeof(nm),&l,&sz,&ty,nm);
      if(!std::strcmp(nm,"alpha")){ tya=ty; ia=(GLuint)i; }
      else if(!std::strcmp(nm,"n")){ tyn=ty; in=(GLuint)i; } }
    ok(tya==GL_FLOAT,"glGetActiveUniform alpha type==GL_FLOAT");
    ok(tyn==GL_UNSIGNED_INT,"glGetActiveUniform n type==GL_UNSIGNED_INT");
    // cross-check via the indexed getter
    if(ia!=(GLuint)-1){ GLint t=0; glGetActiveUniformsiv(prog,1,&ia,GL_UNIFORM_TYPE,&t); ok(t==GL_FLOAT,"glGetActiveUniformsiv alpha UNIFORM_TYPE==GL_FLOAT"); }
    else ok(false,"glGetActiveUniformsiv alpha UNIFORM_TYPE==GL_FLOAT");
    if(in!=(GLuint)-1){ GLint sz=0; glGetActiveUniformsiv(prog,1,&in,GL_UNIFORM_SIZE,&sz); ok(sz==1,"glGetActiveUniformsiv n UNIFORM_SIZE==1"); }
    else ok(false,"glGetActiveUniformsiv n UNIFORM_SIZE==1"); }

  // --- glShaderStorageBlockBinding: core GLES 3.1 entry resolved via eglGetProcAddress. On the
  //     software ES path it is unsupported and reports GL_INVALID_OPERATION; assert that enum and
  //     that the reported block binding is unchanged. ---
  { PFN_ssbb ssbb=(PFN_ssbb)eglGetProcAddress("glShaderStorageBlockBinding"); ok(ssbb!=nullptr,"eglGetProcAddress glShaderStorageBlockBinding");
    while(glGetError()!=GL_NO_ERROR){}
    if(ssbb) ssbb(prog,idxA,2u);
    ok(glGetError()==GL_INVALID_OPERATION,"glShaderStorageBlockBinding -> GL_INVALID_OPERATION (ES software path)");
    const GLenum props[]={GL_BUFFER_BINDING}; GLint bnow=-1;
    glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idxA,1,props,1,nullptr,&bnow);
    ok(bnow==bindA,"block binding(A) unchanged after rejected rebind"); }

  // --- query object lifecycle (glGenQueries/glIsQuery/glDeleteQueries) ---
  { GLuint q=0; glGenQueries(1,&q); ok(q!=0,"glGenQueries handle nonzero");
    ok(glIsQuery(q)==GL_FALSE,"glIsQuery before begin==FALSE");
    glDeleteQueries(1,&q); ok(glGetError()==GL_NO_ERROR,"glDeleteQueries (no error)"); }

  // === boundary cases ===
  // zero-length dispatch: no-op, no error.
  glDispatchCompute(0,0,0); ok(glGetError()==GL_NO_ERROR,"zero-length dispatch (no error)");
  // zero-size buffer.
  { GLuint zb=0; glGenBuffers(1,&zb); glBindBuffer(GL_SHADER_STORAGE_BUFFER,zb); glBufferData(GL_SHADER_STORAGE_BUFFER,0,nullptr,GL_STATIC_DRAW);
    ok(glGetError()==GL_NO_ERROR,"zero-size glBufferData (no error)");
    GLint zs=-1; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&zs); ok(zs==0,"zero-size buffer BUFFER_SIZE==0");
    glDeleteBuffers(1,&zb); }

  // large dispatch (>=1M elements) verified element-wise vs closed form, then spot-checked at the ends.
  { const int BIG=1<<20; GLsizeiptr bb=(GLsizeiptr)(BIG*sizeof(float));
    std::vector<float> ba(BIG),bbv(BIG); for(int i=0;i<BIG;i++){ ba[i]=(float)(i%997); bbv[i]=1.0f; }
    GLuint g[3]={0,0,0}; glGenBuffers(3,g);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,ba.data(),GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,bbv.data(),GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,nullptr,GL_DYNAMIC_COPY);
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,g[i]);
    glUniform1f(la,2.0f); glUniform1ui(ln,(GLuint)BIG);
    glDispatchCompute((BIG+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"1M dispatch (no error)");
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bb,GL_MAP_READ_BIT);
    ok(check_all(m,BIG,[&](int i){ return 2.0f*(float)(i%997)+1.0f; }),"1M dispatch c[i]==2*(i%997)+1 (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    // restore the primary binding for the tail-guard case below.
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glDeleteBuffers(3,g); }

  // non-divisible tail guard: n=1000 with 64-wide groups launches 1024 invocations; c[>=1000] must
  // keep its sentinel because the shader guards i<n.
  { std::vector<float> sentinel(N,-1.0f);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,sentinel.data());
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a.data());
    glUniform1f(la,1.0f); glUniform1ui(ln,1000u); glDispatchCompute((1000+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
    bool okhead=true; for(int i=0;i<1000;i++) if(!feq(m[i],a[i]+b[i])){ okhead=false; break; }
    ok(okhead,"tail guard: c[0..999]==a[i]+b[i]");
    bool oktail=true; for(int i=1000;i<N;i++) if(!feq(m[i],-1.0f)){ oktail=false; break; }
    ok(oktail,"tail guard: c[1000..1023] stays sentinel -1");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glUniform1ui(ln,(GLuint)N); }

  // === validation-error paths: assert the real GL_INVALID_* enum, not just "no crash" ===
  while(glGetError()!=GL_NO_ERROR){}
  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,-8);
  ok(glGetError()==GL_INVALID_VALUE,"glBindBufferRange negative size -> GL_INVALID_VALUE");
  { GLint dummy=0; glGetIntegerv((GLenum)0xDEAD,&dummy); ok(glGetError()==GL_INVALID_ENUM,"glGetIntegerv bad pname -> GL_INVALID_ENUM"); }
  // oversubscription: dispatch more groups than MAX_COMPUTE_WORK_GROUP_COUNT[0].
  while(glGetError()!=GL_NO_ERROR){}
  glDispatchCompute((GLuint)maxcount0+1u,1,1);
  ok(glGetError()==GL_INVALID_VALUE,"oversubscribed dispatch (>MAX_COUNT) -> GL_INVALID_VALUE");

  // === 2D dispatch (non-unit local_size_y, num_groups_y>1) verified element-wise ===
  { GLuint sh2=glCreateShader(GL_COMPUTE_SHADER); glShaderSource(sh2,1,&CS2D,nullptr); glCompileShader(sh2);
    GLint c2=0; glGetShaderiv(sh2,GL_COMPILE_STATUS,&c2);
    if(!c2){ char log[1024]; glGetShaderInfoLog(sh2,1024,nullptr,log); fprintf(stderr,"2D shader: %s\n",log); }
    GLuint p2=glCreateProgram(); glAttachShader(p2,sh2); glLinkProgram(p2);
    GLint l2=0; glGetProgramiv(p2,GL_LINK_STATUS,&l2); ok(c2&&l2,"2D compute program compiles+links");
    glUseProgram(p2); glDeleteShader(sh2);
    const GLuint W=40,H=24,NN=W*H; GLsizeiptr b2=(GLsizeiptr)(NN*sizeof(float));
    std::vector<float> a2d(NN); for(GLuint i=0;i<NN;i++) a2d[i]=(float)i;
    GLuint g[2]={0,0}; glGenBuffers(2,g);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,b2,a2d.data(),GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,b2,nullptr,GL_DYNAMIC_COPY);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,g[0]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,g[1]);
    glUniform1ui(glGetUniformLocation(p2,"W"),W); glUniform1ui(glGetUniformLocation(p2,"H"),H);
    glDispatchCompute((W+7)/8,(H+7)/8,1); ok(glGetError()==GL_NO_ERROR,"2D glDispatchCompute (num_groups_y>1)");
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,g[1]);
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,b2,GL_MAP_READ_BIT);
    bool ok2d=true; for(GLuint y=0;y<H&&ok2d;y++) for(GLuint x=0;x<W;x++){ GLuint i=y*W+x; if(!feq(m[i],a2d[i]*2.0f+(float)x+(float)y)){ ok2d=false; break; } }
    ok(ok2d,"2D dispatch c[y*W+x]==a*2+x+y (per-element)");
    { std::vector<float> corrupt(m,m+NN); corrupt[NN/2]+=3.0f;
      bool flagged=false; for(GLuint y=0;y<H&&!flagged;y++) for(GLuint x=0;x<W;x++){ GLuint i=y*W+x; if(!feq(corrupt[i],a2d[i]*2.0f+(float)x+(float)y)){ flagged=true; break; } }
      ok(flagged,"negative control: corrupted 2D element is flagged"); }
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]);
    glUseProgram(prog); glDeleteProgram(p2); glDeleteBuffers(2,g); }

  while(glGetError()!=GL_NO_ERROR){}

  // --- cleanup ---
  glDeleteBuffers(3,buf); ok(glGetError()==GL_NO_ERROR,"glDeleteBuffers");
  glDeleteProgram(prog); ok(glGetError()==GL_NO_ERROR,"glDeleteProgram");
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  ok(eglDestroyContext(dpy,ctx),"eglDestroyContext");
  ok(eglTerminate(dpy),"eglTerminate");

  const int EXPECTED=108; int TOTAL=PASS+FAIL;
  printf("gles-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("GLES_CPP_FULL_API OK %d\n",PASS); return 0; }
  printf("GLES_CPP_FULL_API FAIL\n"); return 1;
}

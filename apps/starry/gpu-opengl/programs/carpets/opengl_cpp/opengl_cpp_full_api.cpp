// opengl_cpp_full_api.cpp - full desktop OpenGL C++ compute API carpet on OSMesa / llvmpipe
// (GL 4.5 core, GL 4.3 compute shaders): exercise the surfaceless OSMesa + GL 4.3 compute surface
// (context-attribs create / make-current / proc-address loading / shader / program / SSBO /
// buffer-base binding / uniform / dispatch / memory-barrier / map-range + get-sub-data readback /
// copy-sub-data / clear-buffer[-sub]-data / limits / program-resource introspection) plus the
// GL 4.3+ compute surface a real program exercises: fence sync (glFenceSync/glClientWaitSync/
// glGetSynciv), timestamp query (glGenQueries/glQueryCounter/glGetQueryObjectui64v), indirect
// dispatch (glDispatchComputeIndirect), immutable storage (glBufferStorage) + DSA map
// (glMapNamedBufferRange), classic glMapBuffer + glFlushMappedBufferRange, DSA program-uniform
// setters, active-uniform reflection (glGetActiveUniform[siv]/glGetProgramResourceLocation) and a
// synchronous debug callback. Asserts operator results per-element against closed-form CPU
// references, exercises boundary sizes (zero-length dispatch, a 1,000,003-element non-divisible
// dispatch verified element-wise), asserts the real returned enum on intentionally-invalid calls
// (GL_INVALID_VALUE / GL_INVALID_OPERATION / GL_INVALID_ENUM) and carries negative controls that
// prove the checker rejects a wrong value. Prints "OPENGL_CPP_FULL_API OK <n>" only when every
// assertion passes and count == EXPECTED. Distinct from the opengl_c_egl EGL variant: this is the
// OSMesa (off-screen, CPU-rendered) variant.
#include <GL/osmesa.h>
#include <GL/glcorearb.h>
#include <GL/gl.h>
#include <cstdio>
#include <cstring>
#include <cmath>
#include <vector>
#include <string>

// glEnable/glIsEnabled are core GL 1.0 and linked directly from libOSMesa, but the pared-down
// glcorearb.h in use here does not declare prototypes; declare them with their GL signatures.
extern "C" void APIENTRY glEnable(GLenum cap);
extern "C" GLboolean APIENTRY glIsEnabled(GLenum cap);

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static bool feq(float a,float b){ return std::fabs(a-b) <= 1e-4f*(1.0f+std::fabs(b)); }

// verify every element of a mapped/read-back range m[i] against ref(i); true iff all match.
template<typename F>
static bool check_all(const float* m,int N,F ref){
  if(!m) return false;
  for(int i=0;i<N;i++) if(!feq(m[i],ref(i))) return false;
  return true;
}

// GL 4.3 compute entry points loaded via OSMesaGetProcAddress (no GLEW). Core GL 1.x symbols
// (glGetString/glGetIntegerv/glGetError/glIsBuffer) are resolved by linking libOSMesa directly.
#define GLPROCS(X) \
  X(PFNGLGETPOINTERVPROC,glGetPointerv) \
  X(PFNGLCREATESHADERPROC,glCreateShader) X(PFNGLSHADERSOURCEPROC,glShaderSource) \
  X(PFNGLCOMPILESHADERPROC,glCompileShader) X(PFNGLGETSHADERIVPROC,glGetShaderiv) \
  X(PFNGLGETSHADERINFOLOGPROC,glGetShaderInfoLog) X(PFNGLCREATEPROGRAMPROC,glCreateProgram) \
  X(PFNGLATTACHSHADERPROC,glAttachShader) X(PFNGLLINKPROGRAMPROC,glLinkProgram) \
  X(PFNGLGETPROGRAMIVPROC,glGetProgramiv) X(PFNGLGETPROGRAMINFOLOGPROC,glGetProgramInfoLog) \
  X(PFNGLUSEPROGRAMPROC,glUseProgram) X(PFNGLDELETESHADERPROC,glDeleteShader) \
  X(PFNGLDELETEPROGRAMPROC,glDeleteProgram) X(PFNGLGENBUFFERSPROC,glGenBuffers) \
  X(PFNGLBINDBUFFERPROC,glBindBuffer) X(PFNGLISBUFFERPROC,glIsBuffer) \
  X(PFNGLBUFFERDATAPROC,glBufferData) \
  X(PFNGLBUFFERSUBDATAPROC,glBufferSubData) X(PFNGLBINDBUFFERBASEPROC,glBindBufferBase) \
  X(PFNGLMAPBUFFERRANGEPROC,glMapBufferRange) X(PFNGLUNMAPBUFFERPROC,glUnmapBuffer) \
  X(PFNGLGETBUFFERSUBDATAPROC,glGetBufferSubData) X(PFNGLDELETEBUFFERSPROC,glDeleteBuffers) \
  X(PFNGLDISPATCHCOMPUTEPROC,glDispatchCompute) X(PFNGLMEMORYBARRIERPROC,glMemoryBarrier) \
  X(PFNGLGETUNIFORMLOCATIONPROC,glGetUniformLocation) X(PFNGLUNIFORM1FPROC,glUniform1f) \
  X(PFNGLUNIFORM1UIPROC,glUniform1ui) X(PFNGLGETINTEGERI_VPROC,glGetIntegeri_v) \
  X(PFNGLBINDBUFFERRANGEPROC,glBindBufferRange) X(PFNGLCOPYBUFFERSUBDATAPROC,glCopyBufferSubData) \
  X(PFNGLCLEARBUFFERDATAPROC,glClearBufferData) X(PFNGLGETBUFFERPARAMETERIVPROC,glGetBufferParameteriv) \
  X(PFNGLSHADERSTORAGEBLOCKBINDINGPROC,glShaderStorageBlockBinding) \
  X(PFNGLGETPROGRAMRESOURCEINDEXPROC,glGetProgramResourceIndex) \
  X(PFNGLGETPROGRAMINTERFACEIVPROC,glGetProgramInterfaceiv) \
  X(PFNGLGETPROGRAMRESOURCEIVPROC,glGetProgramResourceiv) \
  X(PFNGLGETPROGRAMRESOURCENAMEPROC,glGetProgramResourceName) \
  X(PFNGLFENCESYNCPROC,glFenceSync) X(PFNGLCLIENTWAITSYNCPROC,glClientWaitSync) \
  X(PFNGLWAITSYNCPROC,glWaitSync) X(PFNGLDELETESYNCPROC,glDeleteSync) \
  X(PFNGLGETSYNCIVPROC,glGetSynciv) \
  X(PFNGLGENQUERIESPROC,glGenQueries) X(PFNGLDELETEQUERIESPROC,glDeleteQueries) \
  X(PFNGLQUERYCOUNTERPROC,glQueryCounter) X(PFNGLGETQUERYOBJECTUI64VPROC,glGetQueryObjectui64v) \
  X(PFNGLGETQUERYOBJECTIVPROC,glGetQueryObjectiv) \
  X(PFNGLDISPATCHCOMPUTEINDIRECTPROC,glDispatchComputeIndirect) \
  X(PFNGLBUFFERSTORAGEPROC,glBufferStorage) X(PFNGLMAPNAMEDBUFFERRANGEPROC,glMapNamedBufferRange) \
  X(PFNGLUNMAPNAMEDBUFFERPROC,glUnmapNamedBuffer) \
  X(PFNGLMAPBUFFERPROC,glMapBuffer) X(PFNGLFLUSHMAPPEDBUFFERRANGEPROC,glFlushMappedBufferRange) \
  X(PFNGLPROGRAMUNIFORM1FPROC,glProgramUniform1f) X(PFNGLPROGRAMUNIFORM1UIPROC,glProgramUniform1ui) \
  X(PFNGLGETACTIVEUNIFORMPROC,glGetActiveUniform) X(PFNGLGETACTIVEUNIFORMSIVPROC,glGetActiveUniformsiv) \
  X(PFNGLGETPROGRAMRESOURCELOCATIONPROC,glGetProgramResourceLocation) \
  X(PFNGLCLEARBUFFERSUBDATAPROC,glClearBufferSubData) \
  X(PFNGLGETBUFFERPOINTERVPROC,glGetBufferPointerv) \
  X(PFNGLDEBUGMESSAGECONTROLPROC,glDebugMessageControl) \
  X(PFNGLDEBUGMESSAGEINSERTPROC,glDebugMessageInsert) \
  X(PFNGLDEBUGMESSAGECALLBACKPROC,glDebugMessageCallback)
#define DECL(t,n) static t n;
GLPROCS(DECL)
static int g_dbg_msgs=0;
static GLuint g_dbg_last_id=0;
static char g_dbg_last[128]={0};
static void APIENTRY dbg_cb(GLenum,GLenum,GLuint id,GLenum,GLsizei len,const GLchar* msg,const void*){
  g_dbg_msgs++; g_dbg_last_id=id;
  if(msg){ int n=(len>0&&len<(GLsizei)sizeof(g_dbg_last))?(int)len:(int)sizeof(g_dbg_last)-1;
    std::strncpy(g_dbg_last,msg,n); g_dbg_last[n]='\0'; }
}
static bool gl_load(){ bool okall=true;
#define LOAD(t,n) n=(t)OSMesaGetProcAddress(#n); if(!n) okall=false;
  GLPROCS(LOAD)
  return okall; }

// std430: c[i] = alpha*a[i] + b[i]; vadd is the alpha==1 case. CPU references below mirror it.
static const char* CS =
"#version 430\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=1) readonly buffer B { float b[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform float alpha; uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }\n";

// deliberately malformed: references an undeclared identifier -> must fail COMPILE_STATUS.
static const char* CS_BAD =
"#version 430\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) writeonly buffer C { float c[]; };\n"
"void main(){ c[gl_GlobalInvocationID.x] = undeclared_symbol_xyz + 1.0; }\n";

int main(){
  const int N=1024; const GLsizeiptr bytes=(GLsizeiptr)(N*sizeof(float));

  // --- surfaceless OSMesa context (GL 4.5 core) ---
  int attribs[]={ OSMESA_FORMAT,OSMESA_RGBA, OSMESA_PROFILE,OSMESA_CORE_PROFILE,
    OSMESA_CONTEXT_MAJOR_VERSION,4, OSMESA_CONTEXT_MINOR_VERSION,5, 0 };
  OSMesaContext ctx=OSMesaCreateContextAttribs(attribs,nullptr);
  ok(ctx!=nullptr,"OSMesaCreateContextAttribs 4.5 core");
  static unsigned char fb[16*16*4];
  ok(OSMesaMakeCurrent(ctx,fb,GL_UNSIGNED_BYTE,16,16),"OSMesaMakeCurrent");
  { OSMesaContext cur=OSMesaGetCurrentContext(); ok(cur==ctx,"OSMesaGetCurrentContext"); }
  ok(gl_load(),"load GL 4.3 compute entry points (OSMesaGetProcAddress)");

  // --- version / renderer / GLSL queries: assert >= 4.3 and NOT ES ---
  const char* ver=(const char*)glGetString(GL_VERSION);
  ok(ver!=nullptr,"glGetString GL_VERSION");
  ok(ver&&std::strstr(ver,"ES")==nullptr,"GL_VERSION is desktop GL (not ES)");
  { GLint gmaj=0,gmin=0; glGetIntegerv(GL_MAJOR_VERSION,&gmaj); glGetIntegerv(GL_MINOR_VERSION,&gmin);
    ok(gmaj>4 || (gmaj==4&&gmin>=3),"GL_MAJOR/MINOR_VERSION >= 4.3"); }
  ok(glGetString(GL_SHADING_LANGUAGE_VERSION)!=nullptr,"GL_SHADING_LANGUAGE_VERSION");
  ok(glGetString(GL_RENDERER)!=nullptr,"GL_RENDERER");
  ok(glGetString(GL_VENDOR)!=nullptr,"GL_VENDOR");

  // --- debug/validation channel: register callback (synchronous), assert it installs cleanly ---
  glDebugMessageCallback(dbg_cb,nullptr);
  ok(glGetError()==GL_NO_ERROR,"glDebugMessageCallback install");
  { void* cb=nullptr; glGetPointerv(GL_DEBUG_CALLBACK_FUNCTION,&cb);
    ok(cb==(void*)dbg_cb,"GL_DEBUG_CALLBACK_FUNCTION == installed callback"); }

  // --- debug channel end-to-end: enable synchronous debug output, configure the filter with
  //     glDebugMessageControl (allow all application/other messages), inject a message with
  //     glDebugMessageInsert and assert the installed callback synchronously received exactly it ---
  glEnable(GL_DEBUG_OUTPUT); glEnable(GL_DEBUG_OUTPUT_SYNCHRONOUS);
  ok(glIsEnabled(GL_DEBUG_OUTPUT_SYNCHRONOUS)==GL_TRUE,"GL_DEBUG_OUTPUT_SYNCHRONOUS enabled");
  glDebugMessageControl(GL_DEBUG_SOURCE_APPLICATION,GL_DEBUG_TYPE_OTHER,GL_DONT_CARE,0,nullptr,GL_TRUE);
  ok(glGetError()==GL_NO_ERROR,"glDebugMessageControl (enable APPLICATION/OTHER filter)");
  { const int before=g_dbg_msgs;
    const GLuint MID=0xABCD; const char* MSG="carpet-debug-insert-probe";
    glDebugMessageInsert(GL_DEBUG_SOURCE_APPLICATION,GL_DEBUG_TYPE_OTHER,MID,
        GL_DEBUG_SEVERITY_NOTIFICATION,-1,MSG);
    ok(glGetError()==GL_NO_ERROR,"glDebugMessageInsert (no error)");
    // synchronous debug output => callback must have fired exactly once, delivering our id+text
    ok(g_dbg_msgs==before+1,"glDebugMessageInsert drove callback (g_dbg_msgs grew by 1)");
    ok(g_dbg_last_id==MID && std::strcmp(g_dbg_last,MSG)==0,
        "inserted message delivered to callback verbatim (id+text match)"); }

  // --- OSMesa off-screen surface state: OSMesaGetIntegerv must report exactly the width/height/
  //     format we passed to OSMesaMakeCurrent (16x16 RGBA), a closed-form check of the context ---
  { GLint ow=-1,oh=-1,ofmt=-1;
    OSMesaGetIntegerv(OSMESA_WIDTH,&ow);   ok(ow==16,"OSMesaGetIntegerv OSMESA_WIDTH==16");
    OSMesaGetIntegerv(OSMESA_HEIGHT,&oh);  ok(oh==16,"OSMesaGetIntegerv OSMESA_HEIGHT==16");
    OSMesaGetIntegerv(OSMESA_FORMAT,&ofmt);ok(ofmt==OSMESA_RGBA,"OSMesaGetIntegerv OSMESA_FORMAT==OSMESA_RGBA"); }

  // --- compute work group limits (indexed + non-indexed getters) ---
  { GLint c0=0,c1=0,c2=0;
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&c0);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,1,&c1);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,2,&c2);
    ok(c0>=1&&c1>=1&&c2>=1,"MAX_COMPUTE_WORK_GROUP_COUNT[0..2] >= 1"); }
  { GLint s0=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_SIZE,0,&s0); ok(s0>=64,"MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64"); }
  { GLint inv=0; glGetIntegerv(GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,&inv); ok(inv>=64,"MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64"); }
  { GLint ssb=0; glGetIntegerv(GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS,&ssb); ok(ssb>=3,"MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3"); }
  { GLint msb=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BLOCK_SIZE,&msb); ok(msb>=(GLint)bytes,"MAX_SHADER_STORAGE_BLOCK_SIZE >= bytes"); }

  // --- compute shader compile (COMPILE_STATUS + info log + SHADER_TYPE) ---
  GLuint sh=glCreateShader(GL_COMPUTE_SHADER); ok(sh!=0,"glCreateShader(COMPUTE)");
  glShaderSource(sh,1,&CS,nullptr); glCompileShader(sh);
  GLint cok=0; glGetShaderiv(sh,GL_COMPILE_STATUS,&cok);
  if(!cok){ char log[2048]; glGetShaderInfoLog(sh,2048,nullptr,log); fprintf(stderr,"shader: %s\n",log); }
  ok(cok,"glCompileShader COMPILE_STATUS");
  { GLint st=0; glGetShaderiv(sh,GL_SHADER_TYPE,&st); ok(st==GL_COMPUTE_SHADER,"glGetShaderiv SHADER_TYPE"); }

  // --- NEGATIVE: malformed shader must fail to compile with a non-empty info log ---
  { GLuint bad=glCreateShader(GL_COMPUTE_SHADER); glShaderSource(bad,1,&CS_BAD,nullptr); glCompileShader(bad);
    GLint bcok=1; glGetShaderiv(bad,GL_COMPILE_STATUS,&bcok);
    ok(bcok==GL_FALSE,"malformed shader COMPILE_STATUS==GL_FALSE");
    GLint loglen=0; glGetShaderiv(bad,GL_INFO_LOG_LENGTH,&loglen);
    char blog[2048]={0}; GLsizei got=0; glGetShaderInfoLog(bad,sizeof(blog),&got,blog);
    ok(got>0 && blog[0]!='\0',"malformed shader info log non-empty");
    glDeleteShader(bad); }

  // --- program link (LINK_STATUS + info log) ---
  GLuint prog=glCreateProgram(); ok(prog!=0,"glCreateProgram");
  glAttachShader(prog,sh); glLinkProgram(prog);
  GLint lok=0; glGetProgramiv(prog,GL_LINK_STATUS,&lok);
  if(!lok){ char log[2048]; glGetProgramInfoLog(prog,2048,nullptr,log); fprintf(stderr,"link: %s\n",log); }
  ok(lok,"glLinkProgram LINK_STATUS");
  glDeleteShader(sh); ok(glGetError()==GL_NO_ERROR,"glDeleteShader (no error)");

  // --- SSBO buffers ---
  std::vector<float> a(N),b(N),hc(N);
  for(int i=0;i<N;i++){ a[i]=(float)i; b[i]=2.0f*i+1.0f; }
  GLuint buf[3]={0,0,0}; glGenBuffers(3,buf); ok(buf[0]&&buf[1]&&buf[2],"glGenBuffers(3)");
  ok(glIsBuffer(buf[0])==GL_FALSE,"glIsBuffer before bind == FALSE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a.data(),GL_STATIC_DRAW);
  ok(glGetError()==GL_NO_ERROR,"glBufferData A");
  ok(glIsBuffer(buf[0])==GL_TRUE,"glIsBuffer after bind == TRUE");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b.data(),GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData B");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,nullptr,GL_DYNAMIC_COPY); ok(glGetError()==GL_NO_ERROR,"glBufferData C(null)");
  for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
  ok(glGetError()==GL_NO_ERROR,"glBindBufferBase x3");

  // --- uniforms + dispatch: vadd (alpha=1); read back via glGetBufferSubData, per element ---
  glUseProgram(prog);
  { GLint cur=0; glGetIntegerv(GL_CURRENT_PROGRAM,&cur); ok((GLuint)cur==prog,"glUseProgram (CURRENT_PROGRAM)"); }
  GLint la=glGetUniformLocation(prog,"alpha"), ln=glGetUniformLocation(prog,"n"); ok(la>=0&&ln>=0,"glGetUniformLocation alpha,n");
  glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)N); ok(glGetError()==GL_NO_ERROR,"glUniform1f/1ui");
  glDispatchCompute((N+63)/64,1,1); ok(glGetError()==GL_NO_ERROR,"glDispatchCompute vadd");
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"glMemoryBarrier");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int i){ return a[i]+b[i]; }),"vadd c[i]==a[i]+b[i] (getBufferSubData, per-element)");

  // --- saxpy (alpha=3): read back via glGetBufferSubData, per element ---
  glUniform1f(la,3.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int i){ return 3.0f*a[i]+b[i]; }),"saxpy c[i]==3*a[i]+b[i] (getBufferSubData, per-element)");

  // --- saxpy readback via glMapBufferRange, per element ---
  {
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange READ (saxpy)");
    ok(check_all(m,N,[&](int i){ return 3.0f*a[i]+b[i]; }),"saxpy c[i]==3*a[i]+b[i] (mapped, per-element)");
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (saxpy)");
  }

  // --- partial-range map: first half only, per element ---
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  {
    const int half=N/2; GLsizeiptr hb=(GLsizeiptr)(half*sizeof(float));
    float* m=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,hb,GL_MAP_READ_BIT); ok(m!=nullptr,"glMapBufferRange partial (first half)");
    ok(check_all(m,half,[&](int i){ return a[i]+b[i]; }),"partial map first-half c[i]==a[i]+b[i] (per-element)");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
  }

  // --- buffer sub-data update + re-dispatch, per element ---
  std::vector<float> a2(N,2.0f);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,a2.data()); ok(glGetError()==GL_NO_ERROR,"glBufferSubData A<-2");
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int i){ return 2.0f+b[i]; }),"vadd after subdata c[i]==2+b[i] (per-element)");

  // --- mapped write (GL_MAP_WRITE_BIT) into A, dispatch, verify c==map_val+b per element ---
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
  {
    float* w=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_WRITE_BIT|GL_MAP_INVALIDATE_BUFFER_BIT);
    ok(w!=nullptr,"glMapBufferRange WRITE (A)");
    if(w) for(int i=0;i<N;i++) w[i]=5.0f;
    ok(glUnmapBuffer(GL_SHADER_STORAGE_BUFFER)==GL_TRUE,"glUnmapBuffer (A write)");
  }
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int i){ return 5.0f+b[i]; }),"vadd after mapped-write c[i]==5+b[i] (per-element)");

  // === extended coverage (glcorearb.h compute surface): copy-sub-data / clear-buffer-data /
  //     bind-buffer-range / buffer-param query / SSBO block introspection ===
  glBindBuffer(GL_COPY_READ_BUFFER,buf[0]); glBindBuffer(GL_COPY_WRITE_BUFFER,buf[2]);
  glCopyBufferSubData(GL_COPY_READ_BUFFER,GL_COPY_WRITE_BUFFER,0,0,bytes); ok(glGetError()==GL_NO_ERROR,"glCopyBufferSubData");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int){ return 5.0f; }),"copy-sub-data buf0(=5 from mapped-write)->buf2 (per-element)");

  { float cv=7.25f; glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&cv); ok(glGetError()==GL_NO_ERROR,"glClearBufferData"); }
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int){ return 7.25f; }),"clear-buffer-data == 7.25 (per-element)");

  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,bytes); ok(glGetError()==GL_NO_ERROR,"glBindBufferRange");
  { GLint bsz=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&bsz); ok(bsz==(GLint)bytes,"glGetBufferParameteriv BUFFER_SIZE"); }
  { GLint bu=0; glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_USAGE,&bu); ok(bu==GL_STATIC_DRAW,"glGetBufferParameteriv BUFFER_USAGE"); }

  // --- program-resource introspection (SSBO blocks) ---
  { GLuint idx=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A"); ok(idx!=GL_INVALID_INDEX,"glGetProgramResourceIndex A");
    if(idx!=GL_INVALID_INDEX){ glShaderStorageBlockBinding(prog,idx,0); ok(glGetError()==GL_NO_ERROR,"glShaderStorageBlockBinding"); } }
  { GLint nres=0; glGetProgramInterfaceiv(prog,GL_SHADER_STORAGE_BLOCK,GL_ACTIVE_RESOURCES,&nres); ok(nres>=3,"glGetProgramInterfaceiv ACTIVE_RESOURCES(>=3)"); }
  { GLuint idx2=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A");
    const GLenum props[]={GL_BUFFER_BINDING}; GLint bind=-1;
    glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idx2,1,props,1,nullptr,&bind); ok(bind==0,"glGetProgramResourceiv BUFFER_BINDING(A)==0"); }
  { GLuint idx3=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"C");
    char name[8]={0}; GLsizei len=0; glGetProgramResourceName(prog,GL_SHADER_STORAGE_BLOCK,idx3,sizeof(name),&len,name);
    ok(len>=1 && name[0]=='C',"glGetProgramResourceName(C)"); }
  { GLint au=0; glGetProgramiv(prog,GL_ACTIVE_UNIFORMS,&au); ok(au>=2,"glGetProgramiv ACTIVE_UNIFORMS(>=2)"); }

  // === active-uniform reflection: type + name of each declared uniform ===
  { GLint au=0; glGetProgramiv(prog,GL_ACTIVE_UNIFORMS,&au);
    bool sawAlpha=false,sawN=false; GLenum tAlpha=0,tN=0;
    for(GLint i=0;i<au;i++){ char nm[16]={0}; GLsizei ln=0; GLint sz=0; GLenum ty=0;
      glGetActiveUniform(prog,(GLuint)i,sizeof(nm),&ln,&sz,&ty,nm);
      if(!std::strcmp(nm,"alpha")){ sawAlpha=true; tAlpha=ty; }
      if(!std::strcmp(nm,"n")){ sawN=true; tN=ty; } }
    ok(sawAlpha && tAlpha==GL_FLOAT,"glGetActiveUniform alpha type==GL_FLOAT");
    ok(sawN && tN==GL_UNSIGNED_INT,"glGetActiveUniform n type==GL_UNSIGNED_INT"); }
  { // glGetActiveUniformsiv: query TYPE of the resource whose location is alpha
    GLuint uidx=(GLuint)glGetProgramResourceIndex(prog,GL_UNIFORM,"alpha");
    ok(uidx!=GL_INVALID_INDEX,"glGetProgramResourceIndex(UNIFORM,alpha)");
    GLint ty=0; glGetActiveUniformsiv(prog,1,&uidx,GL_UNIFORM_TYPE,&ty);
    ok(ty==GL_FLOAT,"glGetActiveUniformsiv alpha UNIFORM_TYPE==GL_FLOAT"); }
  { GLint loc=glGetProgramResourceLocation(prog,GL_UNIFORM,"alpha");
    ok(loc==la,"glGetProgramResourceLocation(alpha)==glGetUniformLocation(alpha)"); }

  // === DSA program-uniform setters: set alpha=4 without glUseProgram binding, dispatch, verify ===
  glProgramUniform1f(prog,la,4.0f); glProgramUniform1ui(prog,ln,(GLuint)N);
  ok(glGetError()==GL_NO_ERROR,"glProgramUniform1f/1ui");
  { // A currently holds 5.0 (mapped-write above); c = 4*5 + b
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(check_all(hc.data(),N,[&](int i){ return 4.0f*5.0f+b[i]; }),"glProgramUniform saxpy c[i]==4*5+b[i] (per-element)"); }

  // === GPU->CPU completion via a fence sync (instead of only glMemoryBarrier) ===
  glProgramUniform1f(prog,la,1.0f);
  glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  { GLsync fence=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0);
    ok(fence!=nullptr,"glFenceSync returns a sync object");
    GLenum ws=glClientWaitSync(fence,GL_SYNC_FLUSH_COMMANDS_BIT,1000000000ull);
    ok(ws==GL_ALREADY_SIGNALED||ws==GL_CONDITION_SATISFIED,"glClientWaitSync completes (already|condition satisfied)");
    { GLint stype=0; glGetSynciv(fence,GL_SYNC_STATUS,1,nullptr,&stype);
      ok(stype==GL_SIGNALED,"glGetSynciv SYNC_STATUS==SIGNALED after wait"); }
    glWaitSync(fence,0,GL_TIMEOUT_IGNORED); ok(glGetError()==GL_NO_ERROR,"glWaitSync (server-side, no error)");
    glDeleteSync(fence); ok(glGetError()==GL_NO_ERROR,"glDeleteSync"); }
  // fence-gated readback of the alpha=1 result
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
  ok(check_all(hc.data(),N,[&](int i){ return 5.0f+b[i]; }),"fence-gated readback c[i]==5+b[i] (per-element)");

  // === timestamp query around a dispatch ===
  { GLuint q[2]={0,0}; glGenQueries(2,q); ok(q[0]&&q[1],"glGenQueries(2)");
    glQueryCounter(q[0],GL_TIMESTAMP);
    glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glQueryCounter(q[1],GL_TIMESTAMP);
    GLint avail=0; glGetQueryObjectiv(q[1],GL_QUERY_RESULT_AVAILABLE,&avail);
    GLuint64 t0=0,t1=0; glGetQueryObjectui64v(q[0],GL_QUERY_RESULT,&t0); glGetQueryObjectui64v(q[1],GL_QUERY_RESULT,&t1);
    ok(t1>=t0,"glQueryCounter TIMESTAMP end>=start");
    glDeleteQueries(2,q); ok(glGetError()==GL_NO_ERROR,"glDeleteQueries"); }

  // === indirect dispatch via a GL_DISPATCH_INDIRECT_BUFFER ===
  { GLuint ind=0; glGenBuffers(1,&ind);
    GLuint groups[3]={(GLuint)((N+63)/64),1,1};
    glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,ind);
    glBufferData(GL_DISPATCH_INDIRECT_BUFFER,sizeof(groups),groups,GL_STATIC_DRAW);
    ok(glGetError()==GL_NO_ERROR,"indirect-buffer glBufferData");
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glProgramUniform1f(prog,la,2.0f);
    glDispatchComputeIndirect(0);
    glMemoryBarrier((GLbitfield)(GL_SHADER_STORAGE_BARRIER_BIT|GL_COMMAND_BARRIER_BIT));
    ok(glGetError()==GL_NO_ERROR,"glDispatchComputeIndirect (no error)");
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(check_all(hc.data(),N,[&](int i){ return 2.0f*5.0f+b[i]; }),"indirect dispatch c[i]==2*5+b[i] (per-element)");
    glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,0); glDeleteBuffers(1,&ind); }

  // === immutable storage (glBufferStorage) + DSA glMapNamedBufferRange readback ===
  { GLuint sb=0; glGenBuffers(1,&sb); glBindBuffer(GL_SHADER_STORAGE_BUFFER,sb);
    std::vector<float> src(N); for(int i=0;i<N;i++) src[i]=(float)(i*i%37);
    glBufferStorage(GL_SHADER_STORAGE_BUFFER,bytes,src.data(),GL_MAP_READ_BIT);
    ok(glGetError()==GL_NO_ERROR,"glBufferStorage (immutable, MAP_READ)");
    float* m=(float*)glMapNamedBufferRange(sb,0,bytes,GL_MAP_READ_BIT);
    ok(m!=nullptr,"glMapNamedBufferRange READ");
    ok(check_all(m,N,[&](int i){ return (float)(i*i%37); }),"immutable-storage readback c[i]==i*i%37 (per-element)");
    ok(glUnmapNamedBuffer(sb)==GL_TRUE,"glUnmapNamedBuffer");
    // NEGATIVE: re-specifying an immutable store must raise GL_INVALID_OPERATION
    while(glGetError()!=GL_NO_ERROR){}
    glBufferStorage(GL_SHADER_STORAGE_BUFFER,bytes,src.data(),GL_MAP_READ_BIT);
    ok(glGetError()==GL_INVALID_OPERATION,"re-glBufferStorage on immutable -> GL_INVALID_OPERATION");
    glDeleteBuffers(1,&sb); }

  // === classic glMapBuffer + glFlushMappedBufferRange write path ===
  { glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
    float* w=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,
        (GLbitfield)(GL_MAP_WRITE_BIT|GL_MAP_FLUSH_EXPLICIT_BIT));
    ok(w!=nullptr,"glMapBufferRange WRITE|FLUSH_EXPLICIT");
    if(w) for(int i=0;i<N;i++) w[i]=9.0f;
    glFlushMappedBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes);
    ok(glGetError()==GL_NO_ERROR,"glFlushMappedBufferRange");
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glProgramUniform1f(prog,la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(check_all(hc.data(),N,[&](int i){ return 9.0f+b[i]; }),"map-flush-explicit write c[i]==9+b[i] (per-element)"); }
  { // read the just-written A back via classic glMapBuffer(READ_ONLY)
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
    float* m=(float*)glMapBuffer(GL_SHADER_STORAGE_BUFFER,GL_READ_ONLY);
    ok(m!=nullptr,"glMapBuffer READ_ONLY");
    ok(m && check_all(m,N,[&](int){ return 9.0f; }),"glMapBuffer readback A==9 (per-element)");
    // glGetBufferPointerv: BUFFER_MAP_POINTER must equal the pointer glMapBuffer just returned
    { void* qp=nullptr; glGetBufferPointerv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_MAP_POINTER,&qp);
      ok(qp==(void*)m,"glGetBufferPointerv BUFFER_MAP_POINTER == mapped range"); }
    glUnmapBuffer(GL_SHADER_STORAGE_BUFFER);
    // NEGATIVE control: once unmapped, BUFFER_MAP_POINTER must read back NULL
    { void* qp=(void*)0x1; glGetBufferPointerv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_MAP_POINTER,&qp);
      ok(qp==nullptr,"glGetBufferPointerv BUFFER_MAP_POINTER==NULL after unmap"); } }

  // === glClearBufferSubData: clear only the first half, verify the boundary ===
  { glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float cv=3.5f; GLsizeiptr half=(GLsizeiptr)((N/2)*sizeof(float));
    glClearBufferSubData(GL_SHADER_STORAGE_BUFFER,GL_R32F,0,half,GL_RED,GL_FLOAT,&cv);
    ok(glGetError()==GL_NO_ERROR,"glClearBufferSubData (first half)");
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(check_all(hc.data(),N/2,[&](int){ return 3.5f; }),"clear-sub-data first half==3.5 (per-element)");
    ok(check_all(hc.data()+N/2,N/2,[&](int i){ return 9.0f+b[i+N/2]; }),"clear-sub-data second half untouched (per-element)"); }

  // === BOUNDARY: zero-length dispatch must run cleanly and write nothing new ===
  { float sentinel=-11.0f; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&sentinel);
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,buf[i]);
    glProgramUniform1f(prog,la,1.0f); glProgramUniform1ui(prog,ln,0u); // n=0: shader writes nothing
    glDispatchCompute(0,1,1); ok(glGetError()==GL_NO_ERROR,"zero-length glDispatchCompute (no error)");
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(check_all(hc.data(),N,[&](int){ return -11.0f; }),"zero-length dispatch wrote nothing (sentinel intact)");
    glProgramUniform1ui(prog,ln,(GLuint)N); }

  // === BOUNDARY: >=1,000,000-element dispatch with a non-divisible tail, verified element-wise ===
  { const int BIGN=1000003; const GLsizeiptr bbytes=(GLsizeiptr)(BIGN*sizeof(float));
    GLuint bg[3]={0,0,0}; glGenBuffers(3,bg);
    std::vector<float> ba(BIGN),bb(BIGN),bhc(BIGN);
    for(int i=0;i<BIGN;i++){ ba[i]=(float)(i&1023); bb[i]=(float)((i*3)&2047); }
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,bg[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bbytes,ba.data(),GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,bg[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bbytes,bb.data(),GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,bg[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bbytes,nullptr,GL_DYNAMIC_COPY);
    ok(glGetError()==GL_NO_ERROR,"1M-element SSBO glBufferData");
    for(int i=0;i<3;i++) glBindBufferBase(GL_SHADER_STORAGE_BUFFER,i,bg[i]);
    glProgramUniform1f(prog,la,2.0f); glProgramUniform1ui(prog,ln,(GLuint)BIGN);
    GLuint grp=(GLuint)((BIGN+63)/64); // non-divisible: 1000003%64 != 0, exercises the i<n tail guard
    glDispatchCompute(grp,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,bg[2]);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bbytes,bhc.data());
    bool allok=true; for(int i=0;i<BIGN;i++){ float want=2.0f*ba[i]+bb[i]; if(!feq(bhc[i],want)){ allok=false; break; } }
    ok(allok,"1M non-divisible dispatch c[i]==2*a[i]+b[i] element-wise");
    // tail element index BIGN-1 explicitly (the guard boundary)
    ok(feq(bhc[BIGN-1],2.0f*ba[BIGN-1]+bb[BIGN-1]),"1M tail element c[N-1] correct");
    glDeleteBuffers(3,bg);
    glProgramUniform1ui(prog,ln,(GLuint)N); }

  // === NEGATIVE (validation): assert real glGetError enums, not just "no crash" ===
  while(glGetError()!=GL_NO_ERROR){}
  { GLint maxidx=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&maxidx);
    glDispatchCompute((GLuint)maxidx+1u,1,1); // exceeds MAX_COMPUTE_WORK_GROUP_COUNT[0]
    ok(glGetError()==GL_INVALID_VALUE,"dispatch beyond MAX_WORK_GROUP_COUNT -> GL_INVALID_VALUE"); }
  { GLint maxb=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS,&maxb);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,(GLuint)maxb+1u,buf[0]); // out-of-range binding index
    ok(glGetError()==GL_INVALID_VALUE,"glBindBufferBase index>=MAX -> GL_INVALID_VALUE"); }
  { glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
    // conflicting map flags: READ together with INVALIDATE_BUFFER is illegal
    void* p=glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,
        (GLbitfield)(GL_MAP_READ_BIT|GL_MAP_INVALIDATE_BUFFER_BIT));
    ok(p==nullptr && glGetError()==GL_INVALID_OPERATION,"conflicting map flags -> NULL + GL_INVALID_OPERATION"); }
  { glGetString((GLenum)0xDEAD); // invalid name enum
    ok(glGetError()==GL_INVALID_ENUM,"glGetString(bad enum) -> GL_INVALID_ENUM"); }
  while(glGetError()!=GL_NO_ERROR){}

  // === NEGATIVE CONTROLS: prove the checker rejects a wrong value (one per operator family) ===
  { std::vector<float> good(4,10.0f); good[2]=999.0f; // corrupt one element
    ok(!check_all(good.data(),4,[&](int){ return 10.0f; }),"negative control: corrupted constant flagged"); }
  { // vadd family: last-known correct c==5+b in hc? re-fetch and corrupt
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float cv=1.0f; glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&cv);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc.data());
    ok(!check_all(hc.data(),N,[&](int i){ return (float)i+1.0f; }),"negative control: vadd wrong-ref flagged"); }
  { float w[3]={2.0f,4.0f,6.0f}; ok(!feq(w[1],w[1]+0.5f),"negative control: feq rejects 0.5 delta"); }

  glProgramUniform1f(prog,la,1.0f); glProgramUniform1ui(prog,ln,(GLuint)N);

  ok(glGetError()==GL_NO_ERROR,"glGetError == GL_NO_ERROR (final)");

  // --- cleanup ---
  glDeleteBuffers(3,buf); ok(glGetError()==GL_NO_ERROR,"glDeleteBuffers");
  glDeleteProgram(prog); ok(glGetError()==GL_NO_ERROR,"glDeleteProgram");
  OSMesaDestroyContext(ctx);

  const int EXPECTED=119; int TOTAL=PASS+FAIL;
  printf("opengl-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("OPENGL_CPP_FULL_API OK %d\n",PASS); return 0; }
  printf("OPENGL_CPP_FULL_API FAIL\n"); return 1;
}

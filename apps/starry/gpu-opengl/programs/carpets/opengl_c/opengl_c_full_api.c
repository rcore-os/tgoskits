/* opengl_c_full_api.c - OpenGL C compute API carpet on OSMesa/llvmpipe (GL 4.5 core): exercises the
 * GL compute-shader API surface (surfaceless context / shader-compile+link with error paths /
 * SSBO / buffer-base binding / uniforms / elementwise + shared-memory reduction dispatch /
 * memory-barrier / fence-sync / indirect dispatch / persistent write mapping / multi-dim dispatch /
 * program-resource reflection / limits query / injected GL_INVALID_* validation paths) and asserts
 * every operator result against a closed-form reference, with negative controls proving the checker
 * rejects wrong data. Prints "OPENGL_C_FULL_API OK <n>" only when all assertions pass and
 * count == EXPECTED. */
#include "gl_loader.h"
#include <GL/gl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b)<=1e-4f*(1.0f+fabsf(b)); }

/* saxpy: c = alpha*a + b, with an i<n tail guard */
static const char* CS =
"#version 430\n"
"layout(local_size_x=64) in;\n"
"layout(std430,binding=0) readonly buffer A { float a[]; };\n"
"layout(std430,binding=1) readonly buffer B { float b[]; };\n"
"layout(std430,binding=2) writeonly buffer C { float c[]; };\n"
"uniform float alpha; uniform uint n;\n"
"void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }\n";

/* per-workgroup shared-memory tree reduction: out[wg] = sum(in[wg*256 .. wg*256+255]) */
static const char* RED =
"#version 430\n"
"layout(local_size_x=256) in;\n"
"layout(std430,binding=0) readonly buffer In { float src[]; };\n"
"layout(std430,binding=1) writeonly buffer Out { float dst[]; };\n"
"shared float s[256];\n"
"void main(){\n"
"  uint lid=gl_LocalInvocationID.x; uint gid=gl_GlobalInvocationID.x;\n"
"  s[lid]=src[gid]; barrier();\n"
"  for(uint stride=128u; stride>0u; stride>>=1u){ if(lid<stride) s[lid]+=s[lid+stride]; barrier(); }\n"
"  if(lid==0u) dst[gl_WorkGroupID.x]=s[0];\n"
"}\n";

/* 2D indexer: c[y*W+x] = float(x + y*W), proves gl_GlobalInvocationID.xy across y-workgroups */
static const char* GRID =
"#version 430\n"
"layout(local_size_x=8, local_size_y=8) in;\n"
"layout(std430,binding=0) writeonly buffer C { float c[]; };\n"
"uniform uint W;\n"
"void main(){ uint x=gl_GlobalInvocationID.x, y=gl_GlobalInvocationID.y; c[y*W+x]=float(x+y*W); }\n";

static GLuint compile(const char* src, GLint* status){
  GLuint sh=glCreateShader(GL_COMPUTE_SHADER);
  glShaderSource(sh,1,&src,NULL); glCompileShader(sh);
  glGetShaderiv(sh,GL_COMPILE_STATUS,status);
  return sh;
}
static GLuint link_prog(GLuint sh,GLint* status){
  GLuint p=glCreateProgram(); glAttachShader(p,sh); glLinkProgram(p);
  glGetProgramiv(p,GL_LINK_STATUS,status); return p;
}

int main(void){
  const int N=1024; size_t bytes=N*sizeof(float);

  /* --- surfaceless context (GL 4.5 core) --- */
  int attribs[]={ OSMESA_FORMAT,OSMESA_RGBA, OSMESA_PROFILE,OSMESA_CORE_PROFILE,
    OSMESA_CONTEXT_MAJOR_VERSION,4, OSMESA_CONTEXT_MINOR_VERSION,5, 0 };
  OSMesaContext ctx=OSMesaCreateContextAttribs(attribs,NULL); ok(ctx!=NULL,"OSMesaCreateContextAttribs 4.5 core");
  static unsigned char fb[16*16*4];
  ok(OSMesaMakeCurrent(ctx,fb,GL_UNSIGNED_BYTE,16,16),"OSMesaMakeCurrent");
  ok(OSMesaGetCurrentContext()==ctx,"OSMesaGetCurrentContext == ctx");
  { GLint w=0,h=0,fmt=0; OSMesaGetIntegerv(OSMESA_WIDTH,&w); OSMesaGetIntegerv(OSMESA_HEIGHT,&h); OSMesaGetIntegerv(OSMESA_FORMAT,&fmt);
    ok(w==16&&h==16&&fmt==OSMESA_RGBA,"OSMesaGetIntegerv WIDTH/HEIGHT/FORMAT match MakeCurrent"); }
  ok(gl_load(),"load GL 4.3 compute entry points");

  /* --- version / renderer introspection: assert software backend actually in use --- */
  const char*ver=(const char*)glGetString(GL_VERSION); ok(ver&&strstr(ver,"4.5")!=NULL,"GL_VERSION 4.5");
  { GLint gmaj=0,gmin=0; glGetIntegerv(GL_MAJOR_VERSION,&gmaj); glGetIntegerv(GL_MINOR_VERSION,&gmin);
    ok(gmaj==4&&gmin==5,"GL_MAJOR/MINOR_VERSION == 4.5"); }
  ok(glGetString(GL_SHADING_LANGUAGE_VERSION)!=NULL,"GL_SHADING_LANGUAGE_VERSION");
  { const char*rnd=(const char*)glGetString(GL_RENDERER);
    ok(rnd&&(strstr(rnd,"llvmpipe")||strstr(rnd,"softpipe")||strstr(rnd,"SWR")),"GL_RENDERER is software rasterizer"); }
  ok(glGetString(GL_VENDOR)!=NULL,"GL_VENDOR");

  /* --- compute limits query (indexed + non-indexed) --- */
  { GLint c0=0,c1=0,c2=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&c0);
    glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,1,&c1); glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,2,&c2);
    ok(c0>=65535&&c1>=65535&&c2>=65535,"MAX_COMPUTE_WORK_GROUP_COUNT[0..2] >= 65535"); }
  GLint wgs=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_SIZE,0,&wgs); ok(wgs>=256,"MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 256");
  GLint maxInv=0; glGetIntegerv(GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,&maxInv); ok(maxInv>=256,"MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 256");
  { GLint shm=0; glGetIntegerv(GL_MAX_COMPUTE_SHARED_MEMORY_SIZE,&shm); ok(shm>=(GLint)(256*sizeof(float)),"MAX_COMPUTE_SHARED_MEMORY_SIZE >= reduction shared bytes"); }
  { GLint msb=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BLOCK_SIZE,&msb); ok(msb>=(GLint)bytes,"MAX_SHADER_STORAGE_BLOCK_SIZE >= bytes"); }

  /* --- shader compile (happy) + SHADER_TYPE reflection --- */
  GLint cok=0; GLuint sh=compile(CS,&cok);
  if(!cok){ char log[1024]; glGetShaderInfoLog(sh,1024,NULL,log); fprintf(stderr,"shader: %s\n",log); }
  ok(cok==GL_TRUE,"glCompileShader COMPILE_STATUS true");
  { GLint st=0; glGetShaderiv(sh,GL_SHADER_TYPE,&st); ok(st==GL_COMPUTE_SHADER,"glGetShaderiv SHADER_TYPE == COMPUTE"); }

  /* --- compile-ERROR path: deliberately broken shader, assert COMPILE_STATUS==GL_FALSE + non-empty log --- */
  { const char* bad="#version 430\nlayout(local_size_x=1) in;\nvoid main(){ this_is_not_glsl @#$; }\n";
    GLint bcok=99; GLuint bsh=compile(bad,&bcok);
    ok(bcok==GL_FALSE,"broken shader COMPILE_STATUS == GL_FALSE");
    GLint loglen=0; glGetShaderiv(bsh,GL_INFO_LOG_LENGTH,&loglen);
    char blog[2048]={0}; GLsizei got=0; glGetShaderInfoLog(bsh,2048,&got,blog);
    ok(got>0&&blog[0]!='\0',"broken shader glGetShaderInfoLog non-empty");
    glDeleteShader(bsh); }

  /* --- program link (happy) --- */
  GLint lok=0; GLuint prog=link_prog(sh,&lok); ok(lok==GL_TRUE,"glLinkProgram LINK_STATUS true");
  glDeleteShader(sh); ok(glGetError()==GL_NO_ERROR,"glDeleteShader (no error)");

  /* --- link-ERROR path: program whose only attached shader failed to compile -> LINK_STATUS==GL_FALSE --- */
  { const char* bad2="#version 430\nlayout(local_size_x=1) in;\nvoid nope( { }\n";
    GLint c2=99; GLuint sh2=compile(bad2,&c2); ok(c2==GL_FALSE,"link-path shader fails to compile (precondition)");
    GLint l2=99; GLuint p2=link_prog(sh2,&l2);
    ok(l2==GL_FALSE,"program with uncompiled shader LINK_STATUS == GL_FALSE");
    GLint pl=0; glGetProgramiv(p2,GL_INFO_LOG_LENGTH,&pl); char plog[2048]={0}; glGetProgramInfoLog(p2,2048,NULL,plog);
    ok(pl>0&&plog[0]!='\0',"link failure glGetProgramInfoLog non-empty");
    glDeleteShader(sh2); glDeleteProgram(p2); }

  /* --- SSBO buffers --- */
  float *a=malloc(bytes),*b=malloc(bytes),*hc=malloc(bytes);
  for(int i=0;i<N;i++){ a[i]=(float)i; b[i]=2.0f*i+1.0f; }
  GLuint buf[3]; glGenBuffers(3,buf); ok(buf[0]&&buf[1]&&buf[2],"glGenBuffers(3)");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,a,GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData A");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,b,GL_STATIC_DRAW); ok(glGetError()==GL_NO_ERROR,"glBufferData B");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bytes,NULL,GL_DYNAMIC_COPY); ok(glGetError()==GL_NO_ERROR,"glBufferData C(null)");
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]);
  glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]); ok(glGetError()==GL_NO_ERROR,"glBindBufferBase x3 (no error)");

  /* --- uniforms + dispatch: vadd (alpha=1) --- */
  glUseProgram(prog); ok(glGetError()==GL_NO_ERROR,"glUseProgram (no error)");
  GLint la=glGetUniformLocation(prog,"alpha"), ln=glGetUniformLocation(prog,"n");
  ok(la>=0&&ln>=0,"glGetUniformLocation alpha/n");
  glUniform1f(la,1.0f); glUniform1ui(ln,(GLuint)N); ok(glGetError()==GL_NO_ERROR,"glUniform1f/1ui (no error)");
  glDispatchCompute((N+63)/64,1,1);
  glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT); ok(glGetError()==GL_NO_ERROR,"dispatch+barrier vadd (no error)");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],a[i]+b[i])){g=0;break;} ok(g,"vadd == a+b"); }
  /* negative control: checker must reject a wrong reference (a*a+b != a+b for i>1) */
  { int flagged=0; for(int i=0;i<N;i++) if(!feq(hc[i],a[i]*a[i]+b[i])){flagged=1;break;} ok(flagged,"NEG vadd: checker flags a*a+b != a+b"); }

  /* --- re-dispatch saxpy (alpha=3) via fence-sync instead of barrier-only --- */
  glUniform1f(la,3.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  GLsync sy=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0); ok(sy!=NULL,"glFenceSync returns sync object");
  GLenum wr=glClientWaitSync(sy,GL_SYNC_FLUSH_COMMANDS_BIT,1000000000ull);
  ok(wr==GL_ALREADY_SIGNALED||wr==GL_CONDITION_SATISFIED,"glClientWaitSync signalled (not TIMEOUT/WAIT_FAILED)");
  glDeleteSync(sy); ok(glGetError()==GL_NO_ERROR,"glDeleteSync (no error)");
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],3.0f*a[i]+b[i])){g=0;break;} ok(g,"saxpy == 3*a+b (fence-synced)"); }
  { int flagged=0; for(int i=0;i<N;i++) if(!feq(hc[i],3.0f*a[i]+b[i]+1.0f)){flagged=1;break;} ok(flagged,"NEG saxpy: checker flags off-by-one ref"); }

  /* --- map READ range: verify mapped values match saxpy result --- */
  float* mp=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_READ_BIT);
  ok(mp!=NULL,"glMapBufferRange READ");
  if(mp){ ok(feq(mp[0],b[0]) && feq(mp[10],3.0f*a[10]+b[10]),"mapped READ values match saxpy"); glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }

  /* --- map WRITE + explicit flush: host->device via mapping, then re-dispatch and verify --- */
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]);
  float* wp=(float*)glMapBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes,GL_MAP_WRITE_BIT|GL_MAP_FLUSH_EXPLICIT_BIT);
  ok(wp!=NULL,"glMapBufferRange WRITE|FLUSH_EXPLICIT");
  if(wp){ for(int i=0;i<N;i++) wp[i]=10.0f; glFlushMappedBufferRange(GL_SHADER_STORAGE_BUFFER,0,bytes);
    ok(glGetError()==GL_NO_ERROR,"glFlushMappedBufferRange (no error)"); glUnmapBuffer(GL_SHADER_STORAGE_BUFFER); }
  glUniform1f(la,1.0f); glDispatchCompute((N+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],10.0f+b[i])){g=0;break;} ok(g,"vadd after mapped-write A=10 == 10+b"); }

  /* --- non-multiple-of-64 dispatch exercising the i<n tail guard --- */
  { const int M=1000; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]);
    float sentinel[N]; for(int i=0;i<N;i++) sentinel[i]=-7.0f; glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,sentinel);
    glUniform1ui(ln,(GLuint)M); glUniform1f(la,1.0f);
    glDispatchCompute((M+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<M;i++) if(!feq(hc[i],10.0f+b[i])){g=0;break;}
    for(int i=M;i<N;i++) if(!feq(hc[i],-7.0f)){g=0;break;}
    ok(g,"tail guard: i<1000 computed, i>=1000 untouched (-7)"); glUniform1ui(ln,(GLuint)N); }

  /* --- indirect dispatch: dispatch args sourced from a buffer --- */
  { GLuint ind; glGenBuffers(1,&ind);
    GLuint dargs[3]={(GLuint)((N+63)/64),1u,1u};
    glBindBuffer(GL_DISPATCH_INDIRECT_BUFFER,ind); glBufferData(GL_DISPATCH_INDIRECT_BUFFER,sizeof dargs,dargs,GL_STATIC_DRAW);
    glUniform1f(la,4.0f); glDispatchComputeIndirect(0);
    glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT|GL_COMMAND_BARRIER_BIT);
    ok(glGetError()==GL_NO_ERROR,"glDispatchComputeIndirect (no error)");
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],4.0f*10.0f+b[i])){g=0;break;} ok(g,"indirect saxpy == 4*10+b");
    glDeleteBuffers(1,&ind); }

  /* === shared/local-memory reduction: out[wg] = sum(in block of 256) === */
  { const int RN=4096, RG=RN/256; size_t rb=RN*sizeof(float), ob=RG*sizeof(float);
    float* rin=malloc(rb); double total=0; for(int i=0;i<RN;i++){ rin[i]=(float)(i%13); total+=rin[i]; }
    GLint rc=0; GLuint rsh=compile(RED,&rc);
    if(!rc){ char log[1024]; glGetShaderInfoLog(rsh,1024,NULL,log); fprintf(stderr,"red: %s\n",log); }
    ok(rc==GL_TRUE,"reduction shader compiles");
    GLint rl=0; GLuint rprog=link_prog(rsh,&rl); ok(rl==GL_TRUE,"reduction program links"); glDeleteShader(rsh);
    GLuint rbuf[2]; glGenBuffers(2,rbuf);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,rbuf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,rb,rin,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,rbuf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,ob,NULL,GL_DYNAMIC_COPY);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,rbuf[0]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,rbuf[1]);
    glUseProgram(rprog); glDispatchCompute(RG,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    float* partial=malloc(ob); glBindBuffer(GL_SHADER_STORAGE_BUFFER,rbuf[1]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,ob,partial);
    double got=0; for(int g=0;g<RG;g++) got+=partial[g];
    ok(fabs(got-total)<=1e-3*(1.0+total),"shared-memory reduction sum == closed-form total");
    /* per-block closed form: block g sums (256 contiguous i%13 values) */
    { int g0ok=1; for(int g=0;g<RG;g++){ double bs=0; for(int k=0;k<256;k++) bs+=rin[g*256+k]; if(!feq(partial[g],(float)bs)){g0ok=0;break;} } ok(g0ok,"reduction per-block partial == block sum"); }
    { int flagged=0; for(int g=0;g<RG;g++) if(!feq(partial[g],(float)0.0f)){flagged=1;break;} ok(flagged,"NEG reduction: checker flags zero-ref"); }
    glDeleteBuffers(2,rbuf); glDeleteProgram(rprog); free(rin); free(partial); glUseProgram(prog);
  }

  /* === multi-dimensional (y-workgroup) dispatch: 2D global-invocation indexing === */
  { const int W=32,H=32,GN=W*H; size_t gb=GN*sizeof(float);
    GLint gc=0; GLuint gsh=compile(GRID,&gc);
    if(!gc){ char log[1024]; glGetShaderInfoLog(gsh,1024,NULL,log); fprintf(stderr,"grid: %s\n",log); }
    ok(gc==GL_TRUE,"2D grid shader compiles");
    GLint gl2=0; GLuint gprog=link_prog(gsh,&gl2); ok(gl2==GL_TRUE,"2D grid program links"); glDeleteShader(gsh);
    GLuint gbuf; glGenBuffers(1,&gbuf); glBindBuffer(GL_SHADER_STORAGE_BUFFER,gbuf); glBufferData(GL_SHADER_STORAGE_BUFFER,gb,NULL,GL_DYNAMIC_COPY);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,gbuf);
    glUseProgram(gprog); GLint lw=glGetUniformLocation(gprog,"W"); glUniform1ui(lw,(GLuint)W);
    glDispatchCompute(W/8,H/8,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    float* gh=malloc(gb); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,gb,gh);
    int g=1; for(int y=0;y<H;y++) for(int x=0;x<W;x++) if(!feq(gh[y*W+x],(float)(x+y*W))){g=0;} ok(g,"2D dispatch c[y*W+x] == x+y*W");
    { int flagged=0; for(int i=0;i<GN;i++) if(!feq(gh[i],(float)(i+1))){flagged=1;break;} ok(flagged,"NEG 2D: checker flags shifted ref");}
    glDeleteBuffers(1,&gbuf); glDeleteProgram(gprog); free(gh); glUseProgram(prog);
  }

  /* === >=1M-element dispatch verified element-wise vs closed form === */
  { const int BN=1<<20; size_t bb=(size_t)BN*sizeof(float);
    float* big=malloc(bb); for(int i=0;i<BN;i++) big[i]=(float)(i&255);
    GLuint lbuf[3]; glGenBuffers(3,lbuf);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[0]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,big,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[1]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,big,GL_STATIC_DRAW);
    glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[2]); glBufferData(GL_SHADER_STORAGE_BUFFER,bb,NULL,GL_DYNAMIC_COPY);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,lbuf[0]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,lbuf[1]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,lbuf[2]);
    glUniform1f(la,2.0f); glUniform1ui(ln,(GLuint)BN); glDispatchCompute((BN+63)/64,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glFinish();
    float* lo=malloc(bb); glBindBuffer(GL_SHADER_STORAGE_BUFFER,lbuf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bb,lo);
    int g=1; for(int i=0;i<BN;i++){ float e=2.0f*(float)(i&255)+(float)(i&255); if(!feq(lo[i],e)){g=0;break;} } ok(g,"1M-element saxpy element-wise == 3*(i&255)");
    glUniform1ui(ln,(GLuint)N); glDeleteBuffers(3,lbuf); free(big); free(lo);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,0,buf[0]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,1,buf[1]); glBindBufferBase(GL_SHADER_STORAGE_BUFFER,2,buf[2]);
  }

  /* === zero-length dispatch: no invocations run, destination unchanged === */
  { glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); float mark[N]; for(int i=0;i<N;i++) mark[i]=99.0f;
    glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,mark);
    glUniform1ui(ln,0u); glDispatchCompute(0,1,1); glMemoryBarrier(GL_SHADER_STORAGE_BARRIER_BIT);
    glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
    int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],99.0f)){g=0;break;} ok(g,"zero-length dispatch leaves buffer unchanged");
    glUniform1ui(ln,(GLuint)N); }

  /* === injected validation error paths: assert exact GL_INVALID_* enum === */
  while(glGetError()!=GL_NO_ERROR){}
  { GLint maxbind=0; glGetIntegerv(GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS,&maxbind);
    glBindBufferBase(GL_SHADER_STORAGE_BUFFER,(GLuint)maxbind+16u,buf[0]);
    ok(glGetError()==GL_INVALID_VALUE,"glBindBufferBase out-of-range index -> GL_INVALID_VALUE"); }
  { glBufferData(GL_SHADER_STORAGE_BUFFER,-16,NULL,GL_STATIC_DRAW);
    ok(glGetError()==GL_INVALID_VALUE,"glBufferData negative size -> GL_INVALID_VALUE"); }
  { glGetIntegerv((GLenum)0xDEAD,NULL);
    ok(glGetError()==GL_INVALID_ENUM,"glGetIntegerv bogus pname -> GL_INVALID_ENUM"); }
  { glUseProgram(0); glDispatchCompute(1,1,1);
    ok(glGetError()==GL_INVALID_OPERATION,"glDispatchCompute with no program -> GL_INVALID_OPERATION"); glUseProgram(prog); }
  { GLint maxwgc=0; glGetIntegeri_v(GL_MAX_COMPUTE_WORK_GROUP_COUNT,0,&maxwgc);
    glDispatchCompute((GLuint)maxwgc+1u,1,1);
    ok(glGetError()==GL_INVALID_VALUE,"oversubscribed dispatch > MAX_WORK_GROUP_COUNT -> GL_INVALID_VALUE"); }
  while(glGetError()!=GL_NO_ERROR){}

  /* === buffer manipulation: copy-sub-data / clear-buffer-data / bind-range / param query === */
  { float twos[N]; for(int i=0;i<N;i++) twos[i]=2.0f; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,twos); }
  glBindBuffer(GL_COPY_READ_BUFFER,buf[0]); glBindBuffer(GL_COPY_WRITE_BUFFER,buf[2]);
  glCopyBufferSubData(GL_COPY_READ_BUFFER,GL_COPY_WRITE_BUFFER,0,0,bytes); ok(glGetError()==GL_NO_ERROR,"glCopyBufferSubData (no error)");
  glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[2]); glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],2.0f)){g=0;break;} ok(g,"copy-sub-data buf0(=2)->buf2"); }
  { float cv=5.0f; glClearBufferData(GL_SHADER_STORAGE_BUFFER,GL_R32F,GL_RED,GL_FLOAT,&cv); ok(glGetError()==GL_NO_ERROR,"glClearBufferData (no error)"); }
  glGetBufferSubData(GL_SHADER_STORAGE_BUFFER,0,bytes,hc);
  { int g=1; for(int i=0;i<N;i++) if(!feq(hc[i],5.0f)){g=0;break;} ok(g,"clear-buffer-data == 5.0"); }
  glBindBufferRange(GL_SHADER_STORAGE_BUFFER,0,buf[0],0,bytes); ok(glGetError()==GL_NO_ERROR,"glBindBufferRange (no error)");
  { GLint bsz=0; glBindBuffer(GL_SHADER_STORAGE_BUFFER,buf[0]); glGetBufferParameteriv(GL_SHADER_STORAGE_BUFFER,GL_BUFFER_SIZE,&bsz); ok(bsz==(GLint)bytes,"glGetBufferParameteriv BUFFER_SIZE"); }

  /* === program resource reflection: index + per-resource properties (glGetProgramResourceiv) === */
  { GLuint idx=glGetProgramResourceIndex(prog,GL_SHADER_STORAGE_BLOCK,"A"); ok(idx!=GL_INVALID_INDEX,"glGetProgramResourceIndex A");
    if(idx!=GL_INVALID_INDEX){
      glShaderStorageBlockBinding(prog,idx,0); ok(glGetError()==GL_NO_ERROR,"glShaderStorageBlockBinding (no error)");
      GLenum props[2]={GL_BUFFER_BINDING,GL_NUM_ACTIVE_VARIABLES}; GLint vals[2]={-1,-1};
      glGetProgramResourceiv(prog,GL_SHADER_STORAGE_BLOCK,idx,2,props,2,NULL,vals);
      ok(vals[0]==0,"glGetProgramResourceiv BUFFER_BINDING(A) == 0");
      ok(vals[1]==1,"glGetProgramResourceiv NUM_ACTIVE_VARIABLES(A) == 1"); } }
  { GLint nres=0; glGetProgramInterfaceiv(prog,GL_SHADER_STORAGE_BLOCK,GL_ACTIVE_RESOURCES,&nres); ok(nres==3,"glGetProgramInterfaceiv ACTIVE_RESOURCES == 3"); }

  ok(glGetError()==GL_NO_ERROR,"glGetError == GL_NO_ERROR at end of clean path");

  /* --- cleanup --- */
  glDeleteBuffers(3,buf); ok(glGetError()==GL_NO_ERROR,"glDeleteBuffers (no error)");
  glDeleteProgram(prog); ok(glGetError()==GL_NO_ERROR,"glDeleteProgram (no error)");
  free(a); free(b); free(hc);
  OSMesaDestroyContext(ctx);

  int EXPECTED=78, TOTAL=PASS+FAIL;
  printf("opengl-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("OPENGL_C_FULL_API OK %d\n",PASS); return 0; }
  printf("OPENGL_C_FULL_API FAIL\n"); return 1;
}

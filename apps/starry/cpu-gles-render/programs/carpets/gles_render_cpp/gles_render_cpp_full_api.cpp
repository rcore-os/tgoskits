// gles_render_cpp_full_api.cpp - OpenGL ES 3.1 RENDER carpet on EGL-surfaceless / llvmpipe (no window,
// no eglGetProcAddress loader: GLES entry points are exported directly by libGLESv2). Creates a
// surfaceless EGL ES3 context, renders into an off-screen FBO (RGBA8 color texture + depth
// renderbuffer) and reads pixels back with glReadPixels, checking every pixel against a closed-form
// reference for: clear-color, a solid quad through a compiled+linked program, an axis-aligned linear
// gradient (a triangle-strip quad interpolates per-triangle, so only an axis-aligned gradient matches
// a full-quad closed form), a procedural checkerboard from gl_FragCoord, viewport restriction,
// scissor clears, the depth test (LESS occlusion), alpha blending (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over
// all channels incl alpha), a 1x1 FBO, a sub-rectangle readback. Exhaustive per-API coverage:
// primitive topologies (indexed GL_TRIANGLES / GL_TRIANGLE_FAN / GL_LINES / GL_POINTS), a blend
// factor+equation matrix (ONE/ZERO, ONE/ONE, ZERO/ONE, DST_COLOR, GLES3-core GL_MAX and
// GL_FUNC_REVERSE_SUBTRACT), the full depth-func matrix (8 comparisons at window depth 0.75), face
// culling + winding (FRONT_AND_BACK / BACK CCW vs CW), 2x2 texture upload+NEAREST sampling, and state
// queries (glGetIntegerv GL_VIEWPORT / glIsEnabled / glGetBooleanv GL_DEPTH_WRITEMASK / glGetError),
// closing with a negative control. Prints "GLES_RENDER_CPP_FULL_API OK <n>" only when every assertion
// passes and count == EXPECTED. Software rasterizer (llvmpipe), single-threaded, deterministic - no GPU.
#include <EGL/egl.h>
#include <GLES3/gl31.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }

static const int W=64, H=64;
static unsigned char buf[W*H*4];
static void readback(){ glReadPixels(0,0,W,H,GL_RGBA,GL_UNSIGNED_BYTE,buf); }
static unsigned char px(int x,int y,int c){ return buf[(y*W+x)*4+c]; }
static bool peq(int x,int y,int r,int g,int b,int a,int tol){
  return abs((int)px(x,y,0)-r)<=tol && abs((int)px(x,y,1)-g)<=tol &&
         abs((int)px(x,y,2)-b)<=tol && abs((int)px(x,y,3)-a)<=tol;
}
static bool all_eq(int r,int g,int b,int a,int tol){
  for(int y=0;y<H;y++) for(int x=0;x<W;x++) if(!peq(x,y,r,g,b,a,tol)) return false;
  return true;
}
static GLuint mkprog(const char* vs,const char* fs,int* okflag){
  GLuint v=glCreateShader(GL_VERTEX_SHADER); glShaderSource(v,1,&vs,nullptr); glCompileShader(v);
  GLint cs=0; glGetShaderiv(v,GL_COMPILE_STATUS,&cs); if(!cs)*okflag=0;
  GLuint f=glCreateShader(GL_FRAGMENT_SHADER); glShaderSource(f,1,&fs,nullptr); glCompileShader(f);
  glGetShaderiv(f,GL_COMPILE_STATUS,&cs); if(!cs)*okflag=0;
  GLuint p=glCreateProgram(); glAttachShader(p,v); glAttachShader(p,f); glLinkProgram(p);
  GLint ls=0; glGetProgramiv(p,GL_LINK_STATUS,&ls); if(!ls)*okflag=0;
  glDeleteShader(v); glDeleteShader(f); return p;
}

static const char* VS_POS =
"#version 310 es\nlayout(location=0) in vec2 p;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); }\n";
static const char* VS_POS3 =
"#version 310 es\nlayout(location=0) in vec3 p;\nvoid main(){ gl_Position=vec4(p,1.0); }\n";
static const char* VS_COL =
"#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec4 c;\nout vec4 vc;\n"
"void main(){ gl_Position=vec4(p,0.0,1.0); vc=c; }\n";
static const char* FS_UNI =
"#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n";
static const char* FS_VCOL =
"#version 310 es\nprecision highp float;\nin vec4 vc;\nlayout(location=0) out vec4 o;\nvoid main(){ o=vc; }\n";
static const char* FS_CHECK =
"#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nvoid main(){ ivec2 c=ivec2(gl_FragCoord.xy); "
"bool e=(((c.x>>3)+(c.y>>3))&1)==0; o=e?vec4(1.0):vec4(0.0,0.0,0.0,1.0); }\n";

int main(){
  // --- surfaceless EGL OpenGL ES 3.1 context ---
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize"); ok(maj>=1,"EGL major>=1");
  ok(strstr(eglQueryString(dpy,EGL_CLIENT_APIS),"OpenGL_ES")!=nullptr,"CLIENT_APIS has OpenGL_ES");
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_ES3_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0;
  ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg)&&ncfg>=1,"eglChooseConfig ES3");
  ok(eglBindAPI(EGL_OPENGL_ES_API),"eglBindAPI ES"); ok(eglQueryAPI()==EGL_OPENGL_ES_API,"queryAPI ES");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,3, EGL_CONTEXT_MINOR_VERSION,1, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext ES 3.1");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");
  { const char* ver=(const char*)glGetString(GL_VERSION); ok(ver&&strstr(ver,"ES")!=nullptr,"GL_VERSION is GLES"); }
  ok(glGetString(GL_RENDERER)!=nullptr,"glGetString RENDERER");

  // --- off-screen FBO: RGBA8 color texture + depth renderbuffer ---
  GLuint tex=0; glGenTextures(1,&tex); glBindTexture(GL_TEXTURE_2D,tex);
  glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,W,H,0,GL_RGBA,GL_UNSIGNED_BYTE,nullptr);
  glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_NEAREST);
  glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_NEAREST);
  GLuint rb=0; glGenRenderbuffers(1,&rb); glBindRenderbuffer(GL_RENDERBUFFER,rb);
  glRenderbufferStorage(GL_RENDERBUFFER,GL_DEPTH_COMPONENT24,W,H);
  GLuint fbo=0; glGenFramebuffers(1,&fbo); glBindFramebuffer(GL_FRAMEBUFFER,fbo);
  glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,tex,0);
  glFramebufferRenderbuffer(GL_FRAMEBUFFER,GL_DEPTH_ATTACHMENT,GL_RENDERBUFFER,rb);
  ok(glCheckFramebufferStatus(GL_FRAMEBUFFER)==GL_FRAMEBUFFER_COMPLETE,"FBO complete");
  glViewport(0,0,W,H);
  ok(glGetError()==GL_NO_ERROR,"no GL error after FBO setup");

  const float quad[]={ -1,-1, 1,-1, -1,1, 1,1 };
  GLuint vao=0,vbo=0; glGenVertexArrays(1,&vao); glBindVertexArray(vao);
  glGenBuffers(1,&vbo); glBindBuffer(GL_ARRAY_BUFFER,vbo);
  glBufferData(GL_ARRAY_BUFFER,sizeof(quad),quad,GL_STATIC_DRAW);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);

  // --- Test 1: clear color ---
  glClearColor(0.0f,0.25f,0.5f,1.0f); glClear(GL_COLOR_BUFFER_BIT); readback();
  ok(all_eq(0,64,128,255,2),"clear (0,0.25,0.5,1) all pixels (0,64,128,255)");
  ok(peq(0,0,0,64,128,255,2),"clear pixel (0,0)");
  ok(peq(W-1,H-1,0,64,128,255,2),"clear pixel (63,63)");

  // --- Test 2: solid quad ---
  int pok=1; GLuint pu=mkprog(VS_POS,FS_UNI,&pok); ok(pok==1,"solid program compiles+links");
  glUseProgram(pu); GLint ul=glGetUniformLocation(pu,"u"); ok(ul>=0,"uniform u location");
  glUniform4f(ul,1.0f,0.0f,0.0f,1.0f);
  glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(255,0,0,255,1),"solid red quad fills every pixel");
  ok(glGetError()==GL_NO_ERROR,"no GL error after solid draw");

  // --- Test 3: axis-aligned linear gradient (horizontal red->blue) ---
  const float gcol[]={ 1,0,0,1, 0,0,1,1, 1,0,0,1, 0,0,1,1 };
  GLuint cbo=0; glGenBuffers(1,&cbo); glBindBuffer(GL_ARRAY_BUFFER,cbo);
  glBufferData(GL_ARRAY_BUFFER,sizeof(gcol),gcol,GL_STATIC_DRAW);
  glVertexAttribPointer(1,4,GL_FLOAT,GL_FALSE,4*sizeof(float),(void*)0); glEnableVertexAttribArray(1);
  int gok=1; GLuint pg=mkprog(VS_COL,FS_VCOL,&gok); ok(gok==1,"gradient program compiles+links");
  glUseProgram(pg); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  { int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){ float u=(x+0.5f)/W;
      int r=(int)lroundf((1.f-u)*255.f), b=(int)lroundf(u*255.f);
      if(!peq(x,y,r,0,b,255,4)) bad++; }
    ok(bad==0,"gradient matches horizontal-linear closed-form for all pixels");
    ok(peq(0,0,255,0,0,255,8),"gradient left edge ~ red");
    ok(peq(W-1,H-1,0,0,255,255,8),"gradient right edge ~ blue");
    ok(peq(W/2,H/2,128,0,128,255,4),"gradient center ~ (128,0,128)"); }
  glDisableVertexAttribArray(1); glBindBuffer(GL_ARRAY_BUFFER,vbo);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);

  // --- Test 4: checkerboard from gl_FragCoord ---
  int cok=1; GLuint pc=mkprog(VS_POS,FS_CHECK,&cok); ok(cok==1,"checker program compiles+links");
  glUseProgram(pc); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  { int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){ bool e=((((x)>>3)+((y)>>3))&1)==0; int w=e?255:0;
      if(!peq(x,y,w,w,w,255,1)) bad++; }
    ok(bad==0,"checkerboard matches (x/8+y/8) parity for all pixels");
    ok(peq(0,0,255,255,255,255,1),"checker cell (0,0) white");
    ok(peq(8,0,0,0,0,255,1),"checker cell (8,0) black"); }

  // --- Test 5: viewport restriction ---
  glUseProgram(pu); glUniform4f(ul,0.0f,1.0f,0.0f,1.0f);
  glClearColor(1,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glViewport(0,0,W/2,H/2); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish();
  glViewport(0,0,W,H); readback();
  ok(peq(5,5,0,255,0,255,1),"viewport: inside (5,5) green");
  ok(peq(W-5,H-5,255,0,0,255,1),"viewport: outside (59,59) red");
  ok(peq(W/2+2,H/2+2,255,0,0,255,1),"viewport: just outside quadrant red");

  // --- Test 6: scissor-box clear ---
  glClearColor(0,0,1,1); glClear(GL_COLOR_BUFFER_BIT);
  glEnable(GL_SCISSOR_TEST); glScissor(16,16,32,32);
  glClearColor(0,1,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glDisable(GL_SCISSOR_TEST); readback();
  ok(peq(32,32,0,255,0,255,1),"scissor: inside box green");
  ok(peq(2,2,0,0,255,255,1),"scissor: outside box blue");
  ok(peq(50,50,0,0,255,255,1),"scissor: past box blue");

  // --- Test 7: depth test (GL_LESS occlusion) ---
  glEnable(GL_DEPTH_TEST); glDepthFunc(GL_LESS);
  glClearColor(0,0,0,1); glClearDepthf(1.0f); glClear(GL_COLOR_BUFFER_BIT|GL_DEPTH_BUFFER_BIT);
  int dok=1; GLuint pd=mkprog(VS_POS3,FS_UNI,&dok); ok(dok==1,"depth program compiles+links");
  glUseProgram(pd); GLint ud=glGetUniformLocation(pd,"u");
  glBindBuffer(GL_ARRAY_BUFFER,vbo);
  const float farq[]={ -1,-1,0.5f, 1,-1,0.5f, -1,1,0.5f, 1,1,0.5f };
  glBufferData(GL_ARRAY_BUFFER,sizeof(farq),farq,GL_DYNAMIC_DRAW);
  glVertexAttribPointer(0,3,GL_FLOAT,GL_FALSE,3*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
  glUniform4f(ud,1,0,0,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4);
  const float nearq[]={ 0,-1,-0.5f, 1,-1,-0.5f, 0,1,-0.5f, 1,1,-0.5f };
  glBufferData(GL_ARRAY_BUFFER,sizeof(nearq),nearq,GL_DYNAMIC_DRAW);
  glVertexAttribPointer(0,3,GL_FLOAT,GL_FALSE,3*sizeof(float),(void*)0);
  glUniform4f(ud,0,1,0,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(peq(W-4,H/2,0,255,0,255,1),"depth: near green wins on right half");
  ok(peq(4,H/2,255,0,0,255,1),"depth: far red on left half");
  glDisable(GL_DEPTH_TEST);
  glBufferData(GL_ARRAY_BUFFER,sizeof(quad),quad,GL_STATIC_DRAW);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0);

  // --- Test 8: alpha blending ---
  glUseProgram(pu);
  glClearColor(1,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glEnable(GL_BLEND); glBlendFunc(GL_SRC_ALPHA,GL_ONE_MINUS_SRC_ALPHA);
  glUniform4f(ul,0.0f,0.0f,1.0f,0.5f);
  glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); glDisable(GL_BLEND); readback();
  ok(all_eq(128,0,128,191,3),"alpha blend 0.5*blue over red -> rgb(128,0,128) a191");

  // --- Test 9: sub-rectangle readback ---
  glUseProgram(pu); glUniform4f(ul,0.2f,0.4f,0.6f,1.0f);
  glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish();
  { unsigned char sub[4*4*4]; glReadPixels(10,10,4,4,GL_RGBA,GL_UNSIGNED_BYTE,sub);
    bool s=true; for(int i=0;i<16;i++){ if(abs((int)sub[i*4+0]-51)>2||abs((int)sub[i*4+1]-102)>2||
      abs((int)sub[i*4+2]-153)>2||abs((int)sub[i*4+3]-255)>2) s=false; }
    ok(s,"sub-rect (10,10,4x4) == (51,102,153,255)"); }

  // --- Test 10: 1x1 FBO ---
  { GLuint t1=0,f1=0; glGenTextures(1,&t1); glBindTexture(GL_TEXTURE_2D,t1);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,1,1,0,GL_RGBA,GL_UNSIGNED_BYTE,nullptr);
    glGenFramebuffers(1,&f1); glBindFramebuffer(GL_FRAMEBUFFER,f1);
    glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,t1,0);
    ok(glCheckFramebufferStatus(GL_FRAMEBUFFER)==GL_FRAMEBUFFER_COMPLETE,"1x1 FBO complete");
    glViewport(0,0,1,1); glClearColor(0.5f,0.5f,0.5f,1.0f); glClear(GL_COLOR_BUFFER_BIT);
    unsigned char one[4]; glReadPixels(0,0,1,1,GL_RGBA,GL_UNSIGNED_BYTE,one);
    ok(abs((int)one[0]-128)<=2&&abs((int)one[1]-128)<=2&&abs((int)one[2]-128)<=2,"1x1 pixel (128,128,128)");
    glDeleteFramebuffers(1,&f1); glDeleteTextures(1,&t1);
    glBindFramebuffer(GL_FRAMEBUFFER,fbo); glViewport(0,0,W,H); }

  // ============================ exhaustive per-API render coverage (GLES 3.1) ============================
  glUseProgram(pu); glBindVertexArray(vao); glBindBuffer(GL_ARRAY_BUFFER,vbo);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
  glDisable(GL_DEPTH_TEST); glDisable(GL_SCISSOR_TEST); glDisable(GL_CULL_FACE); glDisable(GL_BLEND);

  // --- Test 11: primitive topologies (indexed triangles, fan, lines, points) ---
  glUniform4f(ul,1,0,0,1);
  { GLuint ibo=0; glGenBuffers(1,&ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER,ibo);
    const unsigned short idx[6]={0,1,2, 2,1,3}; glBufferData(GL_ELEMENT_ARRAY_BUFFER,sizeof(idx),idx,GL_STATIC_DRAW);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glDrawElements(GL_TRIANGLES,6,GL_UNSIGNED_SHORT,0); glFinish(); readback();
    ok(all_eq(255,0,0,255,1),"indexed GL_TRIANGLES fills quad");
    glDeleteBuffers(1,&ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER,0); glBindBuffer(GL_ARRAY_BUFFER,vbo);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); }
  { const float fan[]={ 0,0, -1,-1, 1,-1, 1,1, -1,1, -1,-1 };
    GLuint fb=0; glGenBuffers(1,&fb); glBindBuffer(GL_ARRAY_BUFFER,fb); glBufferData(GL_ARRAY_BUFFER,sizeof(fan),fan,GL_STATIC_DRAW);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0);
    glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_FAN,0,6); glFinish(); readback();
    ok(all_eq(255,0,0,255,1),"GL_TRIANGLE_FAN fills quad");
    glDeleteBuffers(1,&fb); glBindBuffer(GL_ARRAY_BUFFER,vbo); glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); }
  { const float ln[]={ -1,0, 1,0 };
    GLuint lb=0; glGenBuffers(1,&lb); glBindBuffer(GL_ARRAY_BUFFER,lb); glBufferData(GL_ARRAY_BUFFER,sizeof(ln),ln,GL_STATIC_DRAW);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_LINES,0,2); glFinish(); readback();
    int mid=0; for(int x=0;x<W;x++) if(peq(x,H/2,255,0,0,255,2)||peq(x,H/2-1,255,0,0,255,2)) mid++;
    ok(mid>=W-2,"GL_LINES draws the middle row"); ok(peq(0,H-1,0,0,0,255,2),"GL_LINES leaves top row clear");
    glDeleteBuffers(1,&lb); glBindBuffer(GL_ARRAY_BUFFER,vbo); glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); }
  { int spok=1; GLuint ps=mkprog("#version 310 es\nlayout(location=0) in vec2 p;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); gl_PointSize=1.0; }\n",FS_UNI,&spok);
    ok(spok==1,"point program compiles+links");
    const float pt[]={ 0,0 }; GLuint pb=0; glGenBuffers(1,&pb); glBindBuffer(GL_ARRAY_BUFFER,pb); glBufferData(GL_ARRAY_BUFFER,sizeof(pt),pt,GL_STATIC_DRAW);
    glUseProgram(ps); GLint psu=glGetUniformLocation(ps,"u"); glUniform4f(psu,1,0,0,1);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_POINTS,0,1); glFinish(); readback();
    bool hit=false; for(int y=H/2-2;y<=H/2+2;y++) for(int x=W/2-2;x<=W/2+2;x++) if(peq(x,y,255,0,0,255,2)) hit=true;
    ok(hit,"GL_POINTS draws a pixel at the center");
    glDeleteProgram(ps); glDeleteBuffers(1,&pb); glUseProgram(pu); glBindBuffer(GL_ARRAY_BUFFER,vbo);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); }

  // --- Test 12: blend factor + equation matrix (closed-form) ---
  glEnable(GL_BLEND);
  glBlendEquation(GL_FUNC_ADD); glBlendFunc(GL_ONE,GL_ZERO);
  glClearColor(0.5f,0.5f,0.5f,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0,0,1,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(0,0,255,255,2),"blend ONE/ZERO: src replaces dst");
  glBlendFunc(GL_ONE,GL_ONE);
  glClearColor(0.5f,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0,0,0.5f,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(128,0,128,255,2),"blend ONE/ONE FUNC_ADD: src+dst = (128,0,128)");
  glBlendFunc(GL_ZERO,GL_ONE);
  glClearColor(0.2f,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0,1,0,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(51,0,0,255,2),"blend ZERO/ONE: dst kept (51,0,0)");
  glBlendFunc(GL_DST_COLOR,GL_ZERO);
  glClearColor(0.5f,0.5f,0.5f,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0,0,1,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(0,0,128,255,2),"blend DST_COLOR/ZERO: src*dst modulate (0,0,128)");
  glBlendEquation(GL_MAX); glBlendFunc(GL_ONE,GL_ONE);
  glClearColor(0.2f,0.6f,0.2f,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0.6f,0.2f,0.6f,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(153,153,153,255,2),"blend equation GL_MAX: per-channel max");
  glBlendEquation(GL_FUNC_REVERSE_SUBTRACT); glBlendFunc(GL_ONE,GL_ONE);
  glClearColor(1,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glUniform4f(ul,0.25f,0,0,1); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(191,0,0,0,3),"blend equation REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0");
  glBlendEquation(GL_FUNC_ADD); glDisable(GL_BLEND);

  // --- Test 13: depth-func matrix (NDC z=0.5 -> window depth 0.75; clear depth 0.75) ---
  glEnable(GL_DEPTH_TEST); glDepthMask(GL_TRUE); glBindBuffer(GL_ARRAY_BUFFER,vbo);
  { const float dq[]={ -1,-1,0.5f, 1,-1,0.5f, -1,1,0.5f, 1,1,0.5f };
    glBufferData(GL_ARRAY_BUFFER,sizeof(dq),dq,GL_DYNAMIC_DRAW);
    glVertexAttribPointer(0,3,GL_FLOAT,GL_FALSE,3*sizeof(float),(void*)0);
    glUseProgram(pd); GLint udd=glGetUniformLocation(pd,"u");
    struct { GLenum f; bool draws; const char* n; } dt[] = {
      {GL_ALWAYS,true,"ALWAYS"},{GL_NEVER,false,"NEVER"},{GL_LESS,false,"LESS"},{GL_LEQUAL,true,"LEQUAL"},
      {GL_EQUAL,true,"EQUAL"},{GL_GREATER,false,"GREATER"},{GL_GEQUAL,true,"GEQUAL"},{GL_NOTEQUAL,false,"NOTEQUAL"} };
    for(auto&d:dt){ glDepthFunc(d.f); glClearColor(0,0,0,1); glClearDepthf(0.75f);
      glClear(GL_COLOR_BUFFER_BIT|GL_DEPTH_BUFFER_BIT); glUniform4f(udd,0,1,0,1);
      glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
      ok(peq(W/2,H/2,0,255,0,255,2)==d.draws,d.n); } }
  glDisable(GL_DEPTH_TEST); glDepthFunc(GL_LESS);
  glBindBuffer(GL_ARRAY_BUFFER,vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(quad),quad,GL_STATIC_DRAW);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0);

  // --- Test 14: face culling + winding ---
  glUseProgram(pu); glUniform4f(ul,1,0,0,1);
  glDisable(GL_CULL_FACE); glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(255,0,0,255,1),"cull disabled: quad drawn");
  glEnable(GL_CULL_FACE); glCullFace(GL_FRONT_AND_BACK); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(all_eq(0,0,0,255,1),"cull FRONT_AND_BACK: nothing drawn");
  { glCullFace(GL_BACK); glFrontFace(GL_CCW); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback(); bool ccw=peq(W/2,H/2,255,0,0,255,2);
    glFrontFace(GL_CW); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback(); bool cw=peq(W/2,H/2,255,0,0,255,2);
    ok(ccw!=cw,"cull BACK: CCW vs CW winding flips visibility"); }
  glDisable(GL_CULL_FACE); glFrontFace(GL_CCW);

  // --- Test 15: texture upload + sampling (2x2 texels, NEAREST) ---
  { int tok=1; GLuint pt=mkprog("#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 t;\nout vec2 uv;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); uv=t; }\n",
      "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n",&tok);
    ok(tok==1,"texture program compiles+links");
    unsigned char tx[16]={255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,255,255};
    GLuint smp=0; glGenTextures(1,&smp); glActiveTexture(GL_TEXTURE0); glBindTexture(GL_TEXTURE_2D,smp);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,2,2,0,GL_RGBA,GL_UNSIGNED_BYTE,tx);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_NEAREST); glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_NEAREST);
    const float tq[]={ -1,-1,0,0, 1,-1,1,0, -1,1,0,1, 1,1,1,1 };
    GLuint tb=0; glGenBuffers(1,&tb); glBindBuffer(GL_ARRAY_BUFFER,tb); glBufferData(GL_ARRAY_BUFFER,sizeof(tq),tq,GL_STATIC_DRAW);
    glUseProgram(pt); glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,4*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
    glVertexAttribPointer(1,2,GL_FLOAT,GL_FALSE,4*sizeof(float),(void*)(2*sizeof(float))); glEnableVertexAttribArray(1);
    glUniform1i(glGetUniformLocation(pt,"s"),0);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
    ok(peq(W/4,H/4,255,0,0,255,2),"texture NEAREST bottom-left red");
    ok(peq(3*W/4,H/4,0,255,0,255,2),"texture NEAREST bottom-right green");
    ok(peq(W/4,3*H/4,0,0,255,255,2),"texture NEAREST top-left blue");
    ok(peq(3*W/4,3*H/4,255,255,255,255,2),"texture NEAREST top-right white");
    glDeleteProgram(pt); glDeleteTextures(1,&smp); glDeleteBuffers(1,&tb); glDisableVertexAttribArray(1);
    glUseProgram(pu); glBindBuffer(GL_ARRAY_BUFFER,vbo); glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0); }

  // --- Test 16: state queries (glGet*, glIsEnabled) ---
  { GLint vp[4]={0}; glGetIntegerv(GL_VIEWPORT,vp);
    ok(vp[0]==0&&vp[1]==0&&vp[2]==(GLint)W&&vp[3]==(GLint)H,"glGetIntegerv GL_VIEWPORT == {0,0,W,H}");
    glEnable(GL_BLEND); ok(glIsEnabled(GL_BLEND)==GL_TRUE,"glIsEnabled(GL_BLEND) true after enable");
    glDisable(GL_BLEND); ok(glIsEnabled(GL_BLEND)==GL_FALSE,"glIsEnabled(GL_BLEND) false after disable");
    GLboolean dm=GL_FALSE; glGetBooleanv(GL_DEPTH_WRITEMASK,&dm); ok(dm==GL_TRUE,"glGetBooleanv GL_DEPTH_WRITEMASK true (default)");
    ok(glGetError()==GL_NO_ERROR,"glGetError == GL_NO_ERROR after full render suite"); }

  // --- Negative control ---
  glUseProgram(pu); glUniform4f(ul,1,0,0,1); glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
  ok(!all_eq(0,255,0,255,2),"negative control: red buffer is NOT green");
  ok(!peq(0,0,0,0,0,255,2),"negative control: red pixel is NOT black");

  glDeleteProgram(pu); glDeleteProgram(pg); glDeleteProgram(pc); glDeleteProgram(pd);
  glDeleteBuffers(1,&vbo); glDeleteBuffers(1,&cbo); glDeleteVertexArrays(1,&vao);
  glDeleteRenderbuffers(1,&rb); glDeleteTextures(1,&tex); glDeleteFramebuffers(1,&fbo);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  eglDestroyContext(dpy,ctx); eglTerminate(dpy);

  int EXPECTED=77, TOTAL=PASS+FAIL;
  printf("gles-render-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("GLES_RENDER_CPP_FULL_API OK %d\n",PASS); return 0; }
  return 1;
}

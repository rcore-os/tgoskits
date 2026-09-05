// scene_2dui.cpp - 2D UI compositing RENDER-scene carpet on EGL-surfaceless / GLES 3.1 / llvmpipe.
// Same context bring-up + off-screen FBO + assertion harness as gles_render_cpp_full_api.cpp: a
// surfaceless EGL ES3.1 context, an off-screen RGBA8 color texture + DEPTH24 renderbuffer FBO, and
// glReadPixels closed-form per-pixel asserts. Orthographic pixel-space projection (gl_FragCoord.xy is
// the pixel center). Every scene primitive has an INDEPENDENT closed-form software reference computed
// in C++ (not derived from the GL output) and asserted per pixel: filled axis-aligned rectangles, an
// analytic rounded-rect (inside/corner-arc/outside coverage), a nine-patch-style scaled border frame
// (fixed corners, stretched edges), an 8x8 bitmap-font glyph blit (assert every lit/unlit texel), a
// scissor-clipped fill, and MULTI-LAYER Porter-Duff over compositing of 3 stacked semi-transparent
// layers Co = Cs + Cd*(1-As) accumulated in C++ and matched channel-by-channel incl alpha. Closes with
// a negative control. Prints "SCENE_2DUI OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
// Software rasterizer (llvmpipe), single-threaded, deterministic.
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
static GLuint mkprog(const char* vs,const char* fs,int* okflag){
  GLuint v=glCreateShader(GL_VERTEX_SHADER); glShaderSource(v,1,&vs,nullptr); glCompileShader(v);
  GLint cs=0; glGetShaderiv(v,GL_COMPILE_STATUS,&cs); if(!cs)*okflag=0;
  GLuint f=glCreateShader(GL_FRAGMENT_SHADER); glShaderSource(f,1,&fs,nullptr); glCompileShader(f);
  glGetShaderiv(f,GL_COMPILE_STATUS,&cs); if(!cs)*okflag=0;
  GLuint p=glCreateProgram(); glAttachShader(p,v); glAttachShader(p,f); glLinkProgram(p);
  GLint ls=0; glGetProgramiv(p,GL_LINK_STATUS,&ls); if(!ls)*okflag=0;
  glDeleteShader(v); glDeleteShader(f); return p;
}
static int clampi(int v,int lo,int hi){ return v<lo?lo:(v>hi?hi:v); }
static int q8(float f){ int v=(int)lroundf(f*255.f); return clampi(v,0,255); }

// Vertex shader: input pixel-space rect corners in [0,W]x[0,H], map to NDC. y stays bottom-up
// (matches gl_FragCoord and glReadPixels which are both bottom-origin), so no flip.
static const char* VS_PIX =
"#version 310 es\nlayout(location=0) in vec2 p;\nuniform vec2 vp;\n"
"void main(){ vec2 n = (p/vp)*2.0 - 1.0; gl_Position=vec4(n,0.0,1.0); }\n";
static const char* FS_UNI =
"#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n";

// Draw a filled pixel-space rectangle [x0,x1)x[y0,y1) with a solid color via two triangles.
static GLint g_vp_loc=-1, g_u_loc=-1; static GLuint g_prog=0, g_vbo=0;
static void fill_rect(float x0,float y0,float x1,float y1,float r,float g,float b,float a){
  float v[12]={ x0,y0, x1,y0, x0,y1,  x0,y1, x1,y0, x1,y1 };
  glBindBuffer(GL_ARRAY_BUFFER,g_vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(v),v,GL_DYNAMIC_DRAW);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
  glUniform4f(g_u_loc,r,g,b,a); glDrawArrays(GL_TRIANGLES,0,6);
}

int main(){
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize");
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_ES3_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0; ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg)&&ncfg>=1,"eglChooseConfig ES3");
  ok(eglBindAPI(EGL_OPENGL_ES_API),"eglBindAPI ES");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,3, EGL_CONTEXT_MINOR_VERSION,1, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext ES 3.1");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");

  GLuint tex=0; glGenTextures(1,&tex); glBindTexture(GL_TEXTURE_2D,tex);
  glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,W,H,0,GL_RGBA,GL_UNSIGNED_BYTE,nullptr);
  glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_NEAREST); glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_NEAREST);
  GLuint rb=0; glGenRenderbuffers(1,&rb); glBindRenderbuffer(GL_RENDERBUFFER,rb);
  glRenderbufferStorage(GL_RENDERBUFFER,GL_DEPTH_COMPONENT24,W,H);
  GLuint fbo=0; glGenFramebuffers(1,&fbo); glBindFramebuffer(GL_FRAMEBUFFER,fbo);
  glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,tex,0);
  glFramebufferRenderbuffer(GL_FRAMEBUFFER,GL_DEPTH_ATTACHMENT,GL_RENDERBUFFER,rb);
  ok(glCheckFramebufferStatus(GL_FRAMEBUFFER)==GL_FRAMEBUFFER_COMPLETE,"FBO complete");
  glViewport(0,0,W,H);

  GLuint vao=0; glGenVertexArrays(1,&vao); glBindVertexArray(vao);
  glGenBuffers(1,&g_vbo);
  int pok=1; g_prog=mkprog(VS_PIX,FS_UNI,&pok); ok(pok==1,"pixel-fill program compiles+links");
  glUseProgram(g_prog); g_vp_loc=glGetUniformLocation(g_prog,"vp"); g_u_loc=glGetUniformLocation(g_prog,"u");
  ok(g_vp_loc>=0&&g_u_loc>=0,"uniform locations");
  glUniform2f(g_vp_loc,(float)W,(float)H);
  ok(glGetError()==GL_NO_ERROR,"no GL error after setup");

  // ---- Scene A: filled rectangles ----
  // clear to opaque dark, draw a red rect [8,16)x[8,24) and a green rect [40,48)x[32,52).
  glClearColor(0.0f,0.0f,0.0f,1.0f); glClear(GL_COLOR_BUFFER_BIT);
  fill_rect(8,8, 16,24, 1,0,0,1);
  fill_rect(40,32, 48,52, 0,1,0,1);
  glFinish(); readback();
  { int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
      int er,eg,eb;
      if(x>=8&&x<16&&y>=8&&y<24){ er=255;eg=0;eb=0; }
      else if(x>=40&&x<48&&y>=32&&y<52){ er=0;eg=255;eb=0; }
      else { er=0;eg=0;eb=0; }
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"filled rectangles: every pixel matches closed-form rect coverage");
    ok(peq(10,10,255,0,0,255,1),"rect A interior red"); ok(peq(44,40,0,255,0,255,1),"rect B interior green");
    ok(peq(30,30,0,0,0,255,1),"gap between rects is background"); }

  // ---- Scene B: analytic rounded-rect ----
  // Rounded-rect: box [12,52)x[12,52), corner radius r=8. Coverage = inside the box AND (not in a
  // corner square, OR within radius of that corner's center). Drawn by CPU-tessellating covered
  // spans into fill_rect calls is overkill; instead render with a fragment shader doing the same
  // analytic test, then assert against the identical C++ closed form.
  { const char* FS_RR =
      "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\n"
      "uniform vec4 box;\nuniform float rad;\nuniform vec4 col;\n"
      "void main(){ vec2 p=gl_FragCoord.xy; float x0=box.x,y0=box.y,x1=box.z,y1=box.w;\n"
      "  bool inside = p.x>=x0&&p.x<x1&&p.y>=y0&&p.y<y1;\n"
      "  if(!inside){ discard; }\n"
      "  vec2 c = p; bool corner=false; vec2 cc=vec2(0.0);\n"
      "  if(p.x<x0+rad&&p.y<y0+rad){corner=true;cc=vec2(x0+rad,y0+rad);}\n"
      "  else if(p.x>=x1-rad&&p.y<y0+rad){corner=true;cc=vec2(x1-rad,y0+rad);}\n"
      "  else if(p.x<x0+rad&&p.y>=y1-rad){corner=true;cc=vec2(x0+rad,y1-rad);}\n"
      "  else if(p.x>=x1-rad&&p.y>=y1-rad){corner=true;cc=vec2(x1-rad,y1-rad);}\n"
      "  if(corner && distance(c,cc)>rad){ discard; }\n"
      "  o=col; }\n";
    int rok=1; GLuint prr=mkprog(VS_PIX,FS_RR,&rok); ok(rok==1,"rounded-rect program compiles+links");
    glUseProgram(prr); glUniform2f(glGetUniformLocation(prr,"vp"),(float)W,(float)H);
    glUniform4f(glGetUniformLocation(prr,"box"),12,12,52,52);
    glUniform1f(glGetUniformLocation(prr,"rad"),8.0f);
    glUniform4f(glGetUniformLocation(prr,"col"),1,1,0,1);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
    // full-screen quad in pixel space
    float fq[12]={0,0, (float)W,0, 0,(float)H, 0,(float)H, (float)W,0, (float)W,(float)H};
    glBindBuffer(GL_ARRAY_BUFFER,g_vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(fq),fq,GL_DYNAMIC_DRAW);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0);
    glDrawArrays(GL_TRIANGLES,0,6); glFinish(); readback();
    auto covered=[&](int x,int y)->bool{
      float cx=x+0.5f, cy=y+0.5f; float x0=12,y0=12,x1=52,y1=52,r=8;
      if(!(cx>=x0&&cx<x1&&cy>=y0&&cy<y1)) return false;
      float ccx,ccy; bool corner=false;
      if(cx<x0+r&&cy<y0+r){corner=true;ccx=x0+r;ccy=y0+r;}
      else if(cx>=x1-r&&cy<y0+r){corner=true;ccx=x1-r;ccy=y0+r;}
      else if(cx<x0+r&&cy>=y1-r){corner=true;ccx=x0+r;ccy=y1-r;}
      else if(cx>=x1-r&&cy>=y1-r){corner=true;ccx=x1-r;ccy=y1-r;}
      if(corner){ float dx=cx-ccx,dy=cy-ccy; if(sqrtf(dx*dx+dy*dy)>r) return false; }
      return true; };
    int bad=0, lit=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
      bool cov=covered(x,y); if(cov)lit++;
      int er=cov?255:0, eg=cov?255:0, eb=0;
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"rounded-rect: every pixel matches analytic corner-arc coverage");
    ok(lit>0,"rounded-rect: some pixels covered");
    ok(peq(32,32,255,255,0,255,1),"rounded-rect center lit");
    ok(peq(12,12,0,0,0,255,1),"rounded-rect clipped corner (12,12) is background"); // corner cut
    ok(peq(32,13,255,255,0,255,1),"rounded-rect straight top edge lit");
    glDeleteProgram(prr); glUseProgram(g_prog); }

  // ---- Scene C: nine-patch-style scaled border frame ----
  // A frame: fixed 4px border corners, stretched edges, hollow center. Assert border vs interior.
  // Border region = box [4,60)x[4,60) minus interior [10,54)x[10,54). Border color blue, interior clear.
  glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  // draw the outer box blue then punch the interior with background (dark) - order matters, opaque.
  fill_rect(4,4, 60,60, 0,0,1,1);
  fill_rect(10,10, 54,54, 0.1f,0.1f,0.1f,1.0f);
  glFinish(); readback();
  { int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
      bool inbox = x>=4&&x<60&&y>=4&&y<60;
      bool ininner = x>=10&&x<54&&y>=10&&y<54;
      int er,eg,eb;
      if(ininner){ er=q8(0.1f);eg=q8(0.1f);eb=q8(0.1f); }
      else if(inbox){ er=0;eg=0;eb=255; }
      else { er=0;eg=0;eb=0; }
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"nine-patch border frame: closed-form border-vs-interior coverage");
    ok(peq(5,32,0,0,255,255,1),"nine-patch left border blue");
    ok(peq(32,5,0,0,255,255,1),"nine-patch top border blue");
    ok(peq(32,32,q8(0.1f),q8(0.1f),q8(0.1f),255,1),"nine-patch hollow interior"); }

  // ---- Scene D: 8x8 bitmap-font glyph blit ----
  // Hardcoded 8x8 glyph for 'H'. Uploaded as an 8x8 R8 texture (as RGBA where lit=white). Blitted
  // NEAREST to pixel rect [20,28)x[20,28) (1:1, no scaling). Assert every one of the 64 texels.
  static const unsigned char GLYPH_H[8] = {
    0x00, // ........
    0x42, // .x....x.
    0x42, // .x....x.
    0x7E, // .xxxxxx.
    0x42, // .x....x.
    0x42, // .x....x.
    0x42, // .x....x.
    0x00  // ........
  };
  { // bit b (0..7) of a row is column, MSB = leftmost column (x=0).
    unsigned char rgba[8*8*4];
    for(int r=0;r<8;r++) for(int c=0;c<8;c++){
      bool lit = (GLYPH_H[r]>>(7-c))&1; unsigned char v=lit?255:0;
      // texture row 0 = glyph top; we place glyph top at higher y (bottom-origin blit flips), so
      // store texel (c, r) and account for flip in the reference.
      int idx=(r*8+c)*4; rgba[idx]=v; rgba[idx+1]=v; rgba[idx+2]=v; rgba[idx+3]=255;
    }
    GLuint gtex=0; glGenTextures(1,&gtex); glActiveTexture(GL_TEXTURE0); glBindTexture(GL_TEXTURE_2D,gtex);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,8,8,0,GL_RGBA,GL_UNSIGNED_BYTE,rgba);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_NEAREST); glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_S,GL_CLAMP_TO_EDGE); glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_T,GL_CLAMP_TO_EDGE);
    int tok=1; GLuint ptex=mkprog(
      "#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 t;\nout vec2 uv;\nuniform vec2 vp;\n"
      "void main(){ vec2 n=(p/vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); uv=t; }\n",
      "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n",&tok);
    ok(tok==1,"glyph program compiles+links");
    glUseProgram(ptex); glUniform2f(glGetUniformLocation(ptex,"vp"),(float)W,(float)H);
    glUniform1i(glGetUniformLocation(ptex,"s"),0);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
    // pixel rect [20,28)x[20,28), uv 0..1 spanning the 8x8 glyph, v=0 at y=20 (bottom).
    float gq[24]={ 20,20,0,0,  28,20,1,0,  20,28,0,1,   20,28,0,1,  28,20,1,0,  28,28,1,1 };
    glBindBuffer(GL_ARRAY_BUFFER,g_vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(gq),gq,GL_DYNAMIC_DRAW);
    glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,4*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
    glVertexAttribPointer(1,2,GL_FLOAT,GL_FALSE,4*sizeof(float),(void*)(2*sizeof(float))); glEnableVertexAttribArray(1);
    glDrawArrays(GL_TRIANGLES,0,6); glFinish(); readback();
    int bad=0;
    for(int dy=0;dy<8;dy++) for(int dx=0;dx<8;dx++){
      int sx=20+dx, sy=20+dy;
      // screen pixel sy maps to v=(dy+0.5)/8 -> texel row tr = dy (bottom-origin). Glyph stored with
      // row 0 at top, so screen-bottom (dy=0) samples texture row tr=dy which is glyph-row (7-dy)
      // visually, but we only need the exact texel the sampler fetches: NEAREST v=(dy+0.5)/8 -> row dy.
      int trow=dy, tcol=dx;
      bool lit=(GLYPH_H[trow]>>(7-tcol))&1; int v=lit?255:0;
      if(!peq(sx,sy,v,v,v,255,1)) bad++; }
    ok(bad==0,"glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap");
    // spot: glyph row 3 (0x7E) is the crossbar - fully lit cols 1..6. In screen space that's y=23.
    ok(peq(21,23,255,255,255,255,1),"glyph crossbar lit (col1,row3)");
    ok(peq(23,20,0,0,0,255,1),"glyph row0 blank");
    ok(peq(24,21,0,0,0,255,1),"glyph row1 middle blank (0x42)");
    glDeleteProgram(ptex); glDeleteTextures(1,&gtex); glDisableVertexAttribArray(1); glUseProgram(g_prog); }

  // ---- Scene E: scissor-clipped fill ----
  glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
  glEnable(GL_SCISSOR_TEST); glScissor(16,16,20,20);
  fill_rect(0,0, (float)W,(float)H, 1,0,1,1); // magenta full-screen, clipped to scissor
  glDisable(GL_SCISSOR_TEST); glFinish(); readback();
  { int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
      bool in = x>=16&&x<36&&y>=16&&y<36;
      int er=in?255:0, eg=0, eb=in?255:0;
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"scissor-clipped fill: magenta only within [16,36)^2");
    ok(peq(20,20,255,0,255,255,1),"scissor inside magenta"); ok(peq(40,40,0,0,0,255,1),"scissor outside background"); }

  // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
  // Background opaque, then 3 stacked semi-transparent layers via SRC_ALPHA/ONE_MINUS_SRC_ALPHA.
  // Reference: start Cd=bg, As from each layer; Co = Cs*As + Cd*(1-As) (premul-equivalent with GL's
  // SRC_ALPHA,ONE_MINUS_SRC_ALPHA since Cs here is straight color). Assert the fully-overlapped pixel.
  {
    float bg[4]={0.10f,0.10f,0.10f,1.0f};
    // layer defs: color rgb + alpha, each covering a rect; we test the pixel where all overlap.
    struct L{ float r,g,b,a; float x0,y0,x1,y1; };
    L layers[3]={
      {1.0f,0.0f,0.0f,0.50f,  8,8, 56,56},   // red 50%
      {0.0f,1.0f,0.0f,0.25f, 12,12, 52,52},  // green 25%
      {0.0f,0.0f,1.0f,0.75f, 16,16, 48,48},  // blue 75%
    };
    glClearColor(bg[0],bg[1],bg[2],bg[3]); glClear(GL_COLOR_BUFFER_BIT);
    glEnable(GL_BLEND); glBlendFunc(GL_SRC_ALPHA,GL_ONE_MINUS_SRC_ALPHA); glBlendEquation(GL_FUNC_ADD);
    for(int i=0;i<3;i++){ L&l=layers[i]; fill_rect(l.x0,l.y0,l.x1,l.y1,l.r,l.g,l.b,l.a); }
    glDisable(GL_BLEND); glFinish(); readback();
    // Closed-form for a pixel inside all three rects. GL blends color AND alpha with the same
    // factors: Co = Cs*As + Cd*(1-As); Ao = As*As + Ad*(1-As) with SRC_ALPHA on alpha too.
    auto composite=[&](int tx,int ty, float outc[4]){
      float c[4]={bg[0],bg[1],bg[2],bg[3]};
      for(int i=0;i<3;i++){ L&l=layers[i];
        float cx=tx+0.5f, cy=ty+0.5f;
        if(cx>=l.x0&&cx<l.x1&&cy>=l.y0&&cy<l.y1){
          float as=l.a; float src[4]={l.r,l.g,l.b,l.a};
          for(int k=0;k<4;k++) c[k]=src[k]*as + c[k]*(1.0f-as);
        }
      }
      for(int k=0;k<4;k++) outc[k]=c[k];
    };
    int bad=0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++){
      float e[4]; composite(x,y,e);
      if(!peq(x,y,q8(e[0]),q8(e[1]),q8(e[2]),q8(e[3]),2)) bad++; }
    ok(bad==0,"multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)");
    // hand-check the center (all 3 overlap): iterate the over operator.
    { float c[4]={bg[0],bg[1],bg[2],bg[3]};
      float L0[4]={1,0,0,0.5f},L1[4]={0,1,0,0.25f},L2[4]={0,0,1,0.75f};
      float*ls[3]={L0,L1,L2};
      for(int i=0;i<3;i++){ float as=ls[i][3]; for(int k=0;k<4;k++) c[k]=ls[i][k]*as + c[k]*(1.f-as); }
      ok(peq(32,32,q8(c[0]),q8(c[1]),q8(c[2]),q8(c[3]),2),"multi-layer over center pixel matches hand-iterated over"); }
    // a pixel covered only by layer0 (e.g. (10,32) is in red rect but outside green/blue)
    { float as=0.5f; float er=1.0f*as+bg[0]*(1-as), eg=0*as+bg[1]*(1-as), eb=0*as+bg[2]*(1-as), ea=as*as+bg[3]*(1-as);
      ok(peq(10,32,q8(er),q8(eg),q8(eb),q8(ea),2),"multi-layer over: single-layer region matches one over"); }
  }

  // ---- Negative control ----
  glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT); fill_rect(8,8,16,24,1,0,0,1); glFinish(); readback();
  ok(!peq(10,10,0,255,0,255,4),"negative control: red rect pixel is NOT green");
  ok(!peq(30,30,255,0,0,255,4),"negative control: background is NOT red");

  glDeleteBuffers(1,&g_vbo); glDeleteVertexArrays(1,&vao); glDeleteProgram(g_prog);
  glDeleteRenderbuffers(1,&rb); glDeleteTextures(1,&tex); glDeleteFramebuffers(1,&fbo);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  eglDestroyContext(dpy,ctx); eglTerminate(dpy);

  int EXPECTED=37, TOTAL=PASS+FAIL;
  printf("scene-2dui: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_2DUI OK %d\n",PASS); return 0; }
  return 1;
}

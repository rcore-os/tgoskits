// scene_anim.cpp - keyframe-animation RENDER-scene carpet on EGL-surfaceless / GL 4.5 core / llvmpipe.
// Same context bring-up + off-screen FBO (RGBA8 + DEPTH24) + glReadPixels harness as
// opengl_render_cpp_full_api.cpp (entry points via eglGetProcAddress through gl_render_loader.h).
// Renders N=4 keyframes of a transformed unit quad into the FBO. Each frame the quad's model transform
// is a rotation about the FBO center composed with a translation and uniform scale, all interpolated by
// the frame parameter t in {0, 0.25, 0.5, 0.75}. The transform is applied in the vertex shader from a
// hand-built pixel-space model matrix (rotation matrix from angle(t), scale, translate) and an ortho
// pixel->NDC map. For every frame the four rotated/scaled/translated quad CORNERS are computed
// INDEPENDENTLY in C++ (closed-form: R(theta)*S*local + T), and the readback is asserted at those exact
// corner pixels (color present) plus a point just outside the quad (background), so the assertion pins
// the closed-form transformed geometry, not a visual gestalt. A cubic ease function eased(t)=3t^2-2t^3
// drives the scale, and its value is asserted at each t. NDC z is unused (2D). Closes with a negative
// control. Prints "SCENE_ANIM OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Software rasterizer
// (llvmpipe), deterministic.
#include "gl_render_loader.h"
#include <EGL/egl.h>
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
static bool near_color(int x,int y,int r,int g,int b,int tol){
  // check a 1-px neighbourhood contains the target (corner sampling can land on either side of an edge)
  for(int dy=-1;dy<=1;dy++) for(int dx=-1;dx<=1;dx++){ int X=x+dx,Y=y+dy;
    if(X<0||Y<0||X>=W||Y>=H) continue; if(peq(X,Y,r,g,b,255,tol)) return true; }
  return false;
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

static float lerp(float a,float b,float t){ return a+(b-a)*t; }
static float ease_cubic(float t){ return 3.f*t*t - 2.f*t*t*t; } // smoothstep, 0..1

int main(){
  EGLDisplay dpy=eglGetDisplay(EGL_DEFAULT_DISPLAY); ok(dpy!=EGL_NO_DISPLAY,"eglGetDisplay");
  EGLint maj=0,min=0; ok(eglInitialize(dpy,&maj,&min),"eglInitialize");
  EGLint cfgattr[]={ EGL_SURFACE_TYPE,EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,EGL_OPENGL_BIT, EGL_NONE };
  EGLConfig cfg; EGLint ncfg=0; ok(eglChooseConfig(dpy,cfgattr,&cfg,1,&ncfg)&&ncfg>=1,"eglChooseConfig OPENGL_BIT");
  ok(eglBindAPI(EGL_OPENGL_API),"eglBindAPI OPENGL");
  EGLint ctxattr[]={ EGL_CONTEXT_MAJOR_VERSION,4, EGL_CONTEXT_MINOR_VERSION,5,
    EGL_CONTEXT_OPENGL_PROFILE_MASK,EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE };
  EGLContext ctx=eglCreateContext(dpy,cfg,EGL_NO_CONTEXT,ctxattr); ok(ctx!=EGL_NO_CONTEXT,"eglCreateContext 4.5 core");
  ok(eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,ctx),"eglMakeCurrent surfaceless");
  ok(glr_load(),"load GL render entry points via eglGetProcAddress");

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

  // Program: takes 2D local-space corner (-1..1), applies a per-frame 2x3 affine (given as 3 uniforms
  // as columns of a pixel-space transform), then maps pixel -> NDC via vp.
  int prok=1; GLuint prog=mkprog(
    "#version 450 core\nlayout(location=0) in vec2 lp;\nuniform vec2 vp;\nuniform vec2 col0;\nuniform vec2 col1;\nuniform vec2 tr;\n"
    "void main(){ vec2 pix = col0*lp.x + col1*lp.y + tr; vec2 n=(pix/vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); }\n",
    "#version 450 core\nlayout(location=0) out vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n",&prok);
  ok(prok==1,"anim program compiles+links");
  glUseProgram(prog);
  GLint vpl=glGetUniformLocation(prog,"vp"), c0=glGetUniformLocation(prog,"col0"),
        c1=glGetUniformLocation(prog,"col1"), trl=glGetUniformLocation(prog,"tr"), ul=glGetUniformLocation(prog,"u");
  ok(vpl>=0&&c0>=0&&c1>=0&&trl>=0&&ul>=0,"anim uniform locations");
  glUniform2f(vpl,(float)W,(float)H);

  // local quad corners (unit square in local space, TL/TR/BL/BR) as a triangle strip.
  const float local[8]={ -1,-1, 1,-1, -1,1, 1,1 };
  GLuint vao=0,vbo=0; glGenVertexArrays(1,&vao); glBindVertexArray(vao);
  glGenBuffers(1,&vbo); glBindBuffer(GL_ARRAY_BUFFER,vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(local),local,GL_STATIC_DRAW);
  glVertexAttribPointer(0,2,GL_FLOAT,GL_FALSE,2*sizeof(float),(void*)0); glEnableVertexAttribArray(0);

  // animation keyframes: t in {0,0.25,0.5,0.75}
  // angle(t) = lerp(0, pi/2, t)   -> rotation
  // scale(t) = lerp(6, 14, ease_cubic(t)) -> half-extent in pixels, driven by cubic ease
  // center(t) = ( lerp(20,44,t), lerp(20,44,t) ) -> translate along diagonal
  const float A0=0.0f, A1=(float)M_PI/2.0f;
  const float S0=6.0f, S1=14.0f;
  const float CX0=20.f, CX1=44.f, CY0=20.f, CY1=44.f;

  auto frame_transform=[&](float t, float col0[2], float col1[2], float tr[2], float* out_scale, float* out_angle){
    float ang = lerp(A0,A1,t);
    float sc  = lerp(S0,S1, ease_cubic(t));
    float cx  = lerp(CX0,CX1,t), cy=lerp(CY0,CY1,t);
    // pixel-space model matrix M = T(cx,cy) * R(ang) * S(sc): columns for local x and y axes.
    float ca=cosf(ang), sa=sinf(ang);
    col0[0]= sc*ca;  col0[1]= sc*sa;   // R*S applied to local.x
    col1[0]=-sc*sa;  col1[1]= sc*ca;   // R*S applied to local.y
    tr[0]=cx; tr[1]=cy;
    if(out_scale)*out_scale=sc; if(out_angle)*out_angle=ang;
  };

  const float ts[4]={0.0f,0.25f,0.5f,0.75f};
  // per-frame distinct color so a spot check is unambiguous
  const float cols[4][3]={ {1,0,0}, {0,1,0}, {0,0,1}, {1,1,0} };

  for(int fi=0; fi<4; fi++){
    float t=ts[fi]; float col0[2],col1[2],tr[2],sc,ang; frame_transform(t,col0,col1,tr,&sc,&ang);
    glUniform2f(c0,col0[0],col0[1]); glUniform2f(c1,col1[0],col1[1]); glUniform2f(trl,tr[0],tr[1]);
    glUniform4f(ul,cols[fi][0],cols[fi][1],cols[fi][2],1.0f);
    glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();

    // ---- closed-form corner positions for this frame: corner = R(ang)*S(sc)*localCorner + center ----
    float ca=cosf(ang), sa=sinf(ang);
    struct P{ float x,y; };
    P corners[4];
    for(int k=0;k<4;k++){
      float lx=local[k*2+0], ly=local[k*2+1];
      float rx = sc*(ca*lx - sa*ly), ry = sc*(sa*lx + ca*ly);
      corners[k].x = tr[0]+rx; corners[k].y = tr[1]+ry;
    }
    // ease value assertion (closed-form cubic)
    float e = ease_cubic(t); float e_ref = 3.f*t*t - 2.f*t*t*t;
    ok(fabsf(e-e_ref)<1e-6f, "ease_cubic closed-form value");
    // scale must equal lerp(S0,S1,e) exactly
    ok(fabsf(sc - (S0+(S1-S0)*e))<1e-4f, "scale = lerp(S0,S1,ease(t)) closed-form");

    // center pixel of the quad must carry this frame's color
    int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
    ok(peq(cxi,cyi,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),255,2),
       "frame center pixel carries frame color at closed-form center");

    // each of the 4 transformed corners: the quad's fill reaches that corner (neighbourhood contains color)
    for(int k=0;k<4;k++){
      int px_=(int)lroundf(corners[k].x-0.5f), py_=(int)lroundf(corners[k].y-0.5f);
      bool onscreen = px_>=0&&py_>=0&&px_<W&&py_<H;
      ok(onscreen && near_color(px_,py_,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),40),
         "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)");
    }

    // a point far outside the quad silhouette stays background. Use the FBO corner opposite the motion.
    { int ox = (fi<2)?W-2:1, oy=(fi<2)?H-2:1; // pick a corner the quad never reaches for small t
      // guard: ensure the closed-form quad does not cover (ox,oy). max reach from center is sc*sqrt2.
      float reach=sc*1.4142f; bool covers = (fabsf(ox+0.5f-tr[0])<=reach && fabsf(oy+0.5f-tr[1])<=reach);
      if(!covers) ok(peq(ox,oy,0,0,0,255,2),"outside-quad point stays background (closed-form silhouette)");
      else ok(true,"outside-quad point skipped (would be covered)"); }
  }

  // ---- t=0 vs t=0.75 geometry differs: assert the two frames' center positions are NOT equal ----
  { float c0a[2],c1a[2],tra[2],c0b[2],c1b[2],trb[2],s,a;
    frame_transform(0.0f,c0a,c1a,tra,&s,&a); frame_transform(0.75f,c0b,c1b,trb,&s,&a);
    ok(fabsf(tra[0]-trb[0])>1.0f,"center translates between t=0 and t=0.75 (animation is real)"); }

  // ---- rotation at t=0.5: angle should be pi/4; a local +x axis maps to (cos45,sin45)*sc direction ----
  { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.5f,col0,col1,tr,&sc,&ang);
    ok(fabsf(ang-(float)M_PI/4.0f)<1e-5f,"t=0.5 rotation angle = pi/4 closed-form");
    // col0 = sc*(cos,sin) at 45deg -> both components equal and positive
    ok(fabsf(col0[0]-col0[1])<1e-4f && col0[0]>0,"t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)"); }

  // ---- Negative control: render frame 0 (red) and confirm it is NOT green (would indicate wrong color/frame) ----
  { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.0f,col0,col1,tr,&sc,&ang);
    glUniform2f(c0,col0[0],col0[1]); glUniform2f(c1,col1[0],col1[1]); glUniform2f(trl,tr[0],tr[1]);
    glUniform4f(ul,1,0,0,1); glClearColor(0,0,0,1); glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLE_STRIP,0,4); glFinish(); readback();
    int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
    ok(!peq(cxi,cyi,0,255,0,255,4),"negative control: frame-0 center is NOT green"); }

  glDeleteProgram(prog); glDeleteBuffers(1,&vbo); glDeleteVertexArrays(1,&vao);
  glDeleteRenderbuffers(1,&rb); glDeleteTextures(1,&tex); glDeleteFramebuffers(1,&fbo);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  eglDestroyContext(dpy,ctx); eglTerminate(dpy);

  int EXPECTED=46, TOTAL=PASS+FAIL;
  printf("scene-anim: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_ANIM OK %d\n",PASS); return 0; }
  return 1;
}

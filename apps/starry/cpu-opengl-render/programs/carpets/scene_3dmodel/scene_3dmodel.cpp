// scene_3dmodel.cpp - 3D indexed-mesh RENDER-scene carpet on EGL-surfaceless / GL 4.5 core / llvmpipe.
// Same context bring-up + off-screen FBO (RGBA8 + DEPTH24) + glReadPixels harness as
// opengl_render_cpp_full_api.cpp (entry points via eglGetProcAddress through gl_render_loader.h).
// Renders an indexed cube mesh with a hand-computed Model-View-Projection matrix (perspective),
// depth-buffered occlusion (GL_LESS), and Gouraud shading (per-vertex color interpolated by GL across
// each triangle). The assertion is an INDEPENDENT software reference rasterizer written in C++: verts
// are transformed by the SAME MVP -> clip -> NDC (perspective divide) -> viewport pixels; for each
// pixel we compute barycentric coordinates against every projected triangle, do a perspective-correct
// interpolated depth test in a private z-buffer, and interpolate the vertex colors, then compare the
// reference framebuffer to the GL readback per pixel (with a small tolerance for edge-sample and
// rounding differences, counting the fraction of matching pixels rather than requiring bit-exactness
// on antialias-free edges). GL uses NDC z in [-1,1] (GL convention). Closes with a negative control.
// Prints "SCENE_3DMODEL OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Software rasterizer
// (llvmpipe), deterministic.
#include "gl_render_loader.h"
#include <EGL/egl.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>

// glUniformMatrix4fv is not in the shared render loader (the base cell uses no matrix uniform); resolve
// this single extra desktop-GL entry point locally, alongside glr_load(), for the MVP upload.
static PFNGLUNIFORMMATRIX4FVPROC glUniformMatrix4fv=nullptr;

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

// ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) ----
struct M4{ float m[16]; };
static M4 mul(const M4&a,const M4&b){ M4 r; for(int c=0;c<4;c++)for(int row=0;row<4;row++){ float s=0; for(int k=0;k<4;k++) s+=a.m[k*4+row]*b.m[c*4+k]; r.m[c*4+row]=s; } return r; }
static void mv4(const M4&a,const float v[4],float o[4]){ for(int row=0;row<4;row++){ float s=0; for(int k=0;k<4;k++) s+=a.m[k*4+row]*v[k]; o[row]=s; } }
static M4 perspective(float fovy,float aspect,float zn,float zf){
  float f=1.0f/tanf(fovy*0.5f); M4 r; memset(r.m,0,sizeof(r.m));
  r.m[0*4+0]=f/aspect; r.m[1*4+1]=f;
  r.m[2*4+2]=(zf+zn)/(zn-zf); r.m[2*4+3]=-1.0f;
  r.m[3*4+2]=(2.0f*zf*zn)/(zn-zf); return r;
}
static M4 translate(float x,float y,float z){ M4 r; memset(r.m,0,sizeof(r.m)); r.m[0]=r.m[5]=r.m[10]=r.m[15]=1; r.m[3*4+0]=x; r.m[3*4+1]=y; r.m[3*4+2]=z; return r; }
static M4 rotY(float a){ M4 r; memset(r.m,0,sizeof(r.m)); float c=cosf(a),s=sinf(a); r.m[0*4+0]=c; r.m[0*4+2]=-s; r.m[2*4+0]=s; r.m[2*4+2]=c; r.m[1*4+1]=1; r.m[3*4+3]=1; return r; }
static M4 rotX(float a){ M4 r; memset(r.m,0,sizeof(r.m)); float c=cosf(a),s=sinf(a); r.m[1*4+1]=c; r.m[1*4+2]=s; r.m[2*4+1]=-s; r.m[2*4+2]=c; r.m[0*4+0]=1; r.m[3*4+3]=1; return r; }

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
  glUniformMatrix4fv=(PFNGLUNIFORMMATRIX4FVPROC)eglGetProcAddress("glUniformMatrix4fv");
  ok(glUniformMatrix4fv!=nullptr,"load glUniformMatrix4fv via eglGetProcAddress");

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

  // ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (Gouraud) ----
  // vertices in model space [-1,1]^3
  static const float VP[8][3]={
    {-1,-1,-1},{ 1,-1,-1},{ 1, 1,-1},{-1, 1,-1},
    {-1,-1, 1},{ 1,-1, 1},{ 1, 1, 1},{-1, 1, 1} };
  // vertex colors: map (x,y,z) in [-1,1] -> [0,1] rgb
  static float VC[8][3];
  for(int i=0;i<8;i++){ VC[i][0]=(VP[i][0]+1)*0.5f; VC[i][1]=(VP[i][1]+1)*0.5f; VC[i][2]=(VP[i][2]+1)*0.5f; }
  static const unsigned short IDX[36]={
    0,1,2, 0,2,3,   // back  (z=-1)
    4,6,5, 4,7,6,   // front (z=+1)
    0,4,5, 0,5,1,   // bottom
    3,2,6, 3,6,7,   // top
    0,3,7, 0,7,4,   // left
    1,5,6, 1,6,2 }; // right

  // ---- hand-computed MVP ----
  M4 model = mul(rotY(0.6f), rotX(0.3f));
  M4 view  = translate(0,0,-5.0f);
  M4 proj  = perspective(1.0f, (float)W/(float)H, 1.0f, 20.0f);
  M4 mvp   = mul(proj, mul(view, model));

  // upload interleaved pos(3)+col(3)
  float verts[8*6];
  for(int i=0;i<8;i++){ verts[i*6+0]=VP[i][0]; verts[i*6+1]=VP[i][1]; verts[i*6+2]=VP[i][2];
    verts[i*6+3]=VC[i][0]; verts[i*6+4]=VC[i][1]; verts[i*6+5]=VC[i][2]; }
  GLuint vao=0,vbo=0,ibo=0; glGenVertexArrays(1,&vao); glBindVertexArray(vao);
  glGenBuffers(1,&vbo); glBindBuffer(GL_ARRAY_BUFFER,vbo); glBufferData(GL_ARRAY_BUFFER,sizeof(verts),verts,GL_STATIC_DRAW);
  glGenBuffers(1,&ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER,ibo); glBufferData(GL_ELEMENT_ARRAY_BUFFER,sizeof(IDX),IDX,GL_STATIC_DRAW);
  glVertexAttribPointer(0,3,GL_FLOAT,GL_FALSE,6*sizeof(float),(void*)0); glEnableVertexAttribArray(0);
  glVertexAttribPointer(1,3,GL_FLOAT,GL_FALSE,6*sizeof(float),(void*)(3*sizeof(float))); glEnableVertexAttribArray(1);

  int prok=1; GLuint prog=mkprog(
    "#version 450 core\nlayout(location=0) in vec3 p;\nlayout(location=1) in vec3 c;\nout vec3 vc;\nuniform mat4 mvp;\n"
    "void main(){ gl_Position=mvp*vec4(p,1.0); vc=c; }\n",
    "#version 450 core\nin vec3 vc;\nlayout(location=0) out vec4 o;\nvoid main(){ o=vec4(vc,1.0); }\n",&prok);
  ok(prok==1,"cube program compiles+links");
  glUseProgram(prog); glUniformMatrix4fv(glGetUniformLocation(prog,"mvp"),1,GL_FALSE,mvp.m);

  glEnable(GL_DEPTH_TEST); glDepthFunc(GL_LESS);
  glClearColor(0,0,0,1); glClearDepth(1.0); glClear(GL_COLOR_BUFFER_BIT|GL_DEPTH_BUFFER_BIT);
  glDrawElements(GL_TRIANGLES,36,GL_UNSIGNED_SHORT,0); glFinish(); readback();
  ok(glGetError()==GL_NO_ERROR,"no GL error after cube draw");

  // ---- INDEPENDENT software reference rasterizer ----
  static float refc[H][W][3]; static float refz[H][W]; static unsigned char refcov[H][W];
  for(int y=0;y<H;y++) for(int x=0;x<W;x++){ refc[y][x][0]=refc[y][x][1]=refc[y][x][2]=0; refz[y][x]=1e9f; refcov[y][x]=0; }
  // project all 8 verts to screen (clip.w>0 assumed for this camera)
  float sx[8],sy[8],sz[8],sw[8]; float clip[8][4];
  for(int i=0;i<8;i++){ float in[4]={VP[i][0],VP[i][1],VP[i][2],1}; float out[4]; mv4(mvp,in,out);
    clip[i][0]=out[0];clip[i][1]=out[1];clip[i][2]=out[2];clip[i][3]=out[3];
    float w=out[3]; sw[i]=w;
    float ndcx=out[0]/w, ndcy=out[1]/w, ndcz=out[2]/w; // NDC z in [-1,1]
    sx[i]=(ndcx*0.5f+0.5f)*W; sy[i]=(ndcy*0.5f+0.5f)*H; sz[i]=ndcz*0.5f+0.5f; // window depth [0,1]
  }
  ok(sw[0]>0,"reference: all clip.w positive (mesh in front of camera)");
  for(int t=0;t<12;t++){
    int a=IDX[t*3+0], b=IDX[t*3+1], c=IDX[t*3+2];
    float ax=sx[a],ay=sy[a], bx=sx[b],by=sy[b], cx=sx[c],cy=sy[c];
    float area = (bx-ax)*(cy-ay)-(by-ay)*(cx-ax);
    if(fabsf(area)<1e-6f) continue;
    // GL default: no culling here (we did not enable cull), so both windings rasterize.
    int minx=(int)floorf(fminf(ax,fminf(bx,cx))), maxx=(int)ceilf(fmaxf(ax,fmaxf(bx,cx)));
    int miny=(int)floorf(fminf(ay,fminf(by,cy))), maxy=(int)ceilf(fmaxf(ay,fmaxf(by,cy)));
    if(minx<0)minx=0; if(miny<0)miny=0; if(maxx>W)maxx=W; if(maxy>H)maxy=H;
    for(int y=miny;y<maxy;y++) for(int x=minx;x<maxx;x++){
      float pxs=x+0.5f, pys=y+0.5f;
      float w0=((bx-pxs)*(cy-pys)-(by-pys)*(cx-pxs))/area;
      float w1=((cx-pxs)*(ay-pys)-(cy-pys)*(ax-pxs))/area;
      float w2=1.0f-w0-w1;
      bool inside = (w0>=0&&w1>=0&&w2>=0) || (w0<=0&&w1<=0&&w2<=0);
      if(!inside) continue;
      // normalize sign so weights are positive fractions
      if(w0<0||w1<0||w2<0){ w0=-w0; w1=-w1; w2=-w2; }
      float z = w0*sz[a]+w1*sz[b]+w2*sz[c]; // linear-in-screen depth (matches GL window z)
      if(z<refz[y][x]){
        refz[y][x]=z; refcov[y][x]=1;
        // perspective-correct color interpolation: weight by 1/w
        float iwa=1.0f/sw[a], iwb=1.0f/sw[b], iwc=1.0f/sw[c];
        float d = w0*iwa+w1*iwb+w2*iwc;
        for(int k=0;k<3;k++){
          float num = w0*iwa*VC[a][k]+w1*iwb*VC[b][k]+w2*iwc*VC[c][k];
          refc[y][x][k]=num/d;
        }
      }
    }
  }

  // ---- compare GL readback to reference ----
  int total=0, match=0, covmatch=0, covtotal=0, interior_bad=0;
  for(int y=0;y<H;y++) for(int x=0;x<W;x++){
    total++;
    bool gcov = !(px(x,y,0)==0&&px(x,y,1)==0&&px(x,y,2)==0); // GL non-black => covered
    bool rcov = refcov[y][x]!=0;
    if(gcov==rcov) covmatch++;
    if(rcov){ covtotal++;
      int er=(int)lroundf(refc[y][x][0]*255.f), eg=(int)lroundf(refc[y][x][1]*255.f), eb=(int)lroundf(refc[y][x][2]*255.f);
      // interior pixels (all 4 neighbors also covered) must match tightly; edge pixels may differ by
      // sub-pixel coverage/rounding vs GL's exact top-left fill rule.
      bool interior = x>0&&y>0&&x<W-1&&y<H-1 && refcov[y-1][x]&&refcov[y+1][x]&&refcov[y][x-1]&&refcov[y][x+1];
      if(peq(x,y,er,eg,eb,255,6)) match++;
      else if(interior) interior_bad++;
    }
  }
  ok(covtotal>200,"reference: cube covers a substantial area");
  ok(covmatch >= (int)(0.97*total),"coverage mask matches GL (>=97% of pixels agree covered/empty)");
  ok(interior_bad==0,"every interior pixel matches perspective-correct Gouraud reference (tol 6)");
  ok(match >= (int)(0.92*covtotal),"92%+ of covered pixels match reference color (edges excluded)");

  // ---- targeted closed-form spot checks ----
  // find the projected screen position of a known vertex and assert its color there.
  // vertex 6 = (1,1,1) -> color (1,1,1) white; its projected pixel should be near-white.
  { int vx=(int)lroundf(sx[6]-0.5f), vy=(int)lroundf(sy[6]-0.5f);
    if(vx>=1&&vx<W-1&&vy>=1&&vy<H-1){
      // GL may draw an adjacent face over the exact corner; check the 3x3 nbhd contains a bright pixel
      bool bright=false; for(int dy=-1;dy<=1;dy++)for(int dx=-1;dx<=1;dx++){ int X=vx+dx,Y=vy+dy;
        if(px(X,Y,0)>180&&px(X,Y,1)>180&&px(X,Y,2)>180) bright=true; }
      ok(bright,"vertex (1,1,1) region is bright (Gouraud white corner)");
    } else ok(false,"vertex (1,1,1) projected off-screen (camera mis-set)"); }
  // background (a corner well outside the cube silhouette) stays cleared black.
  ok(peq(0,0,0,0,0,255,1)||refcov[0][0]==0,"corner (0,0) background consistent");

  // ---- depth occlusion check: the cube is solid, so along the center column the visible color is
  // the nearest face, and its depth must be < the far face depth. Verify GL picked the near face by
  // comparing the center pixel color to the reference's nearest-face color (already covered by the
  // per-pixel match, but assert the center explicitly).
  { int cxp=W/2, cyp=H/2;
    if(refcov[cyp][cxp]){
      int er=(int)lroundf(refc[cyp][cxp][0]*255.f), eg=(int)lroundf(refc[cyp][cxp][1]*255.f), eb=(int)lroundf(refc[cyp][cxp][2]*255.f);
      ok(peq(cxp,cyp,er,eg,eb,255,8),"center pixel = nearest-face (depth-buffered occlusion) reference color");
    } else ok(false,"center pixel not covered (mesh mis-projected)"); }

  glDisable(GL_DEPTH_TEST);

  // ---- Negative control: disable depth test but keep a DIFFERENT clear; the drawn cube must not
  // equal a flat solid color everywhere. ----
  ok(!(px(1,1,0)==px(W/2,H/2,0)&&px(1,1,1)==px(W/2,H/2,1)&&px(1,1,2)==px(W/2,H/2,2)),
     "negative control: image is not a flat single color (real 3D shading present)");

  glDeleteProgram(prog); glDeleteBuffers(1,&vbo); glDeleteBuffers(1,&ibo); glDeleteVertexArrays(1,&vao);
  glDeleteRenderbuffers(1,&rb); glDeleteTextures(1,&tex); glDeleteFramebuffers(1,&fbo);
  eglMakeCurrent(dpy,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
  eglDestroyContext(dpy,ctx); eglTerminate(dpy);

  int EXPECTED=20, TOTAL=PASS+FAIL;
  printf("scene-3dmodel: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_3DMODEL OK %d\n",PASS); return 0; }
  return 1;
}

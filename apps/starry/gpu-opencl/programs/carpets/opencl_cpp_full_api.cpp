// opencl_cpp_full_api.cpp - full OpenCL C++ (opencl.hpp / cl2.hpp) API carpet: exercise the
// cl:: object surface (Platform / Device / Context / CommandQueue / Buffer / Program / Kernel /
// KernelFunctor / Event / profiling / host-mapping / rect transfers / images / separate
// compile+link / user events + callbacks / out-of-order queue + dependency chains / sub-devices)
// and assert operator results against closed-form references, real queried properties, and real
// error enums. Prints "OPENCL_CPP_FULL_API OK <n>" only when every assertion passes and count
// == EXPECTED.
#define CL_HPP_TARGET_OPENCL_VERSION 300
#define CL_HPP_MINIMUM_OPENCL_VERSION 120
#define CL_HPP_ENABLE_EXCEPTIONS
#include <CL/opencl.hpp>
#include <cstdio>
#include <vector>
#include <cmath>
#include <string>
#include <atomic>
#include <thread>
#include <chrono>

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static bool feq(float a,float b){ return std::fabs(a-b) <= 1e-4f*(1.0f+std::fabs(b)); }

static std::atomic<int> g_cb_count{0};
static void CL_CALLBACK on_complete(cl_event,cl_int status,void* ud){
  if(status==CL_COMPLETE) static_cast<std::atomic<int>*>(ud)->fetch_add(1);
}

static const char* SRC =
"__kernel void vadd(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]+b[i];}\n"
"__kernel void saxpy(float alpha,__global const float*x,__global float*y){int i=get_global_id(0);y[i]=alpha*x[i]+y[i];}\n"
"__kernel void reduce_sum(__global const float*a,__global float*o,__local float*s){\n"
" int l=get_local_id(0),g=get_global_id(0),ls=get_local_size(0);s[l]=a[g];barrier(CLK_LOCAL_MEM_FENCE);\n"
" for(int d=ls/2;d>0;d>>=1){if(l<d)s[l]+=s[l+d];barrier(CLK_LOCAL_MEM_FENCE);}if(l==0)o[get_group_id(0)]=s[0];}\n"
"__kernel void square_guarded(__global const float*a,__global float*b,int n){int i=get_global_id(0);if(i<n)b[i]=a[i]*a[i];}\n"
"__kernel void add_scalar(__global float*a,float v){int i=get_global_id(0);a[i]+=v;}\n";

int main(){
  try{
    // --- platform + device APIs ---
    std::vector<cl::Platform> plats; cl::Platform::get(&plats);
    ok(plats.size()>=1,"cl::Platform::get");
    cl::Platform plat=plats[0];
    ok(!plat.getInfo<CL_PLATFORM_NAME>().empty(),"platform NAME");
    ok(plat.getInfo<CL_PLATFORM_VERSION>().rfind("OpenCL ",0)==0,"platform VERSION starts with 'OpenCL '");
    std::vector<cl::Device> devs; plat.getDevices(CL_DEVICE_TYPE_ALL,&devs);
    ok(devs.size()>=1,"platform.getDevices");
    cl::Device dev=devs[0];
    size_t maxwg=dev.getInfo<CL_DEVICE_MAX_WORK_GROUP_SIZE>();
    ok(maxwg>=1,"device MAX_WORK_GROUP_SIZE");
    ok(dev.getInfo<CL_DEVICE_GLOBAL_MEM_SIZE>()>0,"device GLOBAL_MEM_SIZE");
    ok(dev.getInfo<CL_DEVICE_MAX_WORK_ITEM_DIMENSIONS>()>=3,"device MAX_WORK_ITEM_DIMENSIONS>=3");
    ok(dev.getInfo<CL_DEVICE_TYPE>()!=0,"device TYPE non-zero");

    // --- context + queue APIs ---
    cl::Context ctx(dev);
    ok(ctx.getInfo<CL_CONTEXT_NUM_DEVICES>()==1u,"context NUM_DEVICES");
    ok(ctx.getInfo<CL_CONTEXT_DEVICES>()[0]()==dev(),"context DEVICES[0]==dev");
    cl::CommandQueue q(ctx,dev,CL_QUEUE_PROFILING_ENABLE);
    ok(q.getInfo<CL_QUEUE_CONTEXT>()()==ctx(),"queue CONTEXT==ctx");
    ok((q.getInfo<CL_QUEUE_PROPERTIES>()&CL_QUEUE_PROFILING_ENABLE)!=0,"queue PROPERTIES has PROFILING");

    // --- buffer APIs ---
    const int N=1024; size_t bytes=N*sizeof(float);
    std::vector<float> a(N),b(N),hc(N);
    for(int i=0;i<N;i++){a[i]=(float)i;b[i]=2.0f*i+1.0f;}
    cl::Buffer A(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,a.data());
    ok(A.getInfo<CL_MEM_SIZE>()==bytes,"Buffer A size");
    cl::Buffer B(ctx,CL_MEM_READ_ONLY,bytes);
    cl::Buffer C(ctx,CL_MEM_WRITE_ONLY,bytes);
    // enqueueWriteBuffer: verify the write landed by reading it back
    q.enqueueWriteBuffer(B,CL_TRUE,0,bytes,b.data());
    { std::vector<float> rb(N); q.enqueueReadBuffer(B,CL_TRUE,0,bytes,rb.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(rb[i],b[i])){g=false;break;} ok(g,"enqueueWriteBuffer B round-trips"); }

    // --- program + kernel APIs ---
    cl::Program prog(ctx,SRC);
    prog.build({dev},"-cl-std=CL1.2 -cl-kernel-arg-info");
    ok(prog.getBuildInfo<CL_PROGRAM_BUILD_STATUS>(dev)==CL_BUILD_SUCCESS,"program BUILD_STATUS==SUCCESS");
    cl::Kernel kadd(prog,"vadd"); ok(kadd.getInfo<CL_KERNEL_NUM_ARGS>()==3u,"kernel NUM_ARGS==3");
    ok(kadd.getInfo<CL_KERNEL_FUNCTION_NAME>()=="vadd","kernel FUNCTION_NAME==vadd");
    ok(kadd.getWorkGroupInfo<CL_KERNEL_WORK_GROUP_SIZE>(dev)>=1,"kernel WORK_GROUP_SIZE>=1");
    // kernel-arg reflection: assert address-space qualifier of arg 0 is __global
    ok(kadd.getArgInfo<CL_KERNEL_ARG_ADDRESS_QUALIFIER>(0)==CL_KERNEL_ARG_ADDRESS_GLOBAL,"kernel ARG0 ADDRESS_GLOBAL");

    // --- NDRange + event/profiling + vadd correctness ---
    kadd.setArg(0,A); kadd.setArg(1,B); kadd.setArg(2,C);
    cl::Event ev;
    q.enqueueNDRangeKernel(kadd,cl::NullRange,cl::NDRange(N),cl::NDRange(64),nullptr,&ev);
    ev.wait();
    q.enqueueReadBuffer(C,CL_TRUE,0,bytes,hc.data());
    { bool g=true; for(int i=0;i<N;i++) if(!feq(hc[i],a[i]+b[i])){g=false;break;} ok(g,"vadd == a+b"); }
    ok(ev.getInfo<CL_EVENT_COMMAND_EXECUTION_STATUS>()==CL_COMPLETE,"event STATUS==COMPLETE");
    ok(ev.getProfilingInfo<CL_PROFILING_COMMAND_END>()>=ev.getProfilingInfo<CL_PROFILING_COMMAND_START>(),"event profiling END>=START");

    // --- KernelFunctor (typed) saxpy correctness ---
    cl::Buffer Y(ctx,CL_MEM_READ_WRITE|CL_MEM_COPY_HOST_PTR,bytes,b.data());
    auto saxpy=cl::KernelFunctor<float,cl::Buffer,cl::Buffer>(prog,"saxpy");
    saxpy(cl::EnqueueArgs(q,cl::NDRange(N),cl::NDRange(64)),3.0f,A,Y).wait();
    q.enqueueReadBuffer(Y,CL_TRUE,0,bytes,hc.data());
    { bool g=true; for(int i=0;i<N;i++) if(!feq(hc[i],3.0f*a[i]+b[i])){g=false;break;} ok(g,"KernelFunctor saxpy == alpha*x+y"); }

    // --- local memory + barrier reduction ---
    int ng=N/64; cl::Buffer R(ctx,CL_MEM_WRITE_ONLY,ng*sizeof(float));
    cl::Kernel kred(prog,"reduce_sum");
    kred.setArg(0,A); kred.setArg(1,R); kred.setArg(2,cl::Local(64*sizeof(float)));
    q.enqueueNDRangeKernel(kred,cl::NullRange,cl::NDRange(N),cl::NDRange(64)); q.finish();
    std::vector<float> hr(ng); q.enqueueReadBuffer(R,CL_TRUE,0,ng*sizeof(float),hr.data());
    { double t=0; for(int i=0;i<ng;i++) t+=hr[i]; double ref=0; for(int i=0;i<N;i++) ref+=a[i]; ok(std::fabs(t-ref)<1.0,"reduce_sum local+barrier == sum(a)"); }

    // --- copy + fill APIs ---
    cl::Buffer D(ctx,CL_MEM_READ_WRITE,bytes);
    q.enqueueCopyBuffer(A,D,0,0,bytes); q.enqueueReadBuffer(D,CL_TRUE,0,bytes,hc.data());
    { bool g=true; for(int i=0;i<N;i++) if(!feq(hc[i],a[i])){g=false;break;} ok(g,"enqueueCopyBuffer bytes"); }
    float fv=7.5f; q.enqueueFillBuffer(D,fv,0,bytes); q.finish();
    q.enqueueReadBuffer(D,CL_TRUE,0,bytes,hc.data());
    { bool g=true; for(int i=0;i<N;i++) if(!feq(hc[i],7.5f)){g=false;break;} ok(g,"enqueueFillBuffer == 7.5"); }

    // --- host-mapping family: enqueueMapBuffer -> write through mapped pointer -> unmap -> verify ---
    cl::Buffer M(ctx,CL_MEM_READ_WRITE,bytes);
    { float* mp=static_cast<float*>(q.enqueueMapBuffer(M,CL_TRUE,CL_MAP_WRITE,0,bytes));
      ok(mp!=nullptr,"enqueueMapBuffer returns pointer");
      for(int i=0;i<N;i++) mp[i]=(float)i*0.25f;
      q.enqueueUnmapMemObject(M,mp); q.finish(); }
    q.enqueueReadBuffer(M,CL_TRUE,0,bytes,hc.data());
    { bool g=true; for(int i=0;i<N;i++) if(!feq(hc[i],(float)i*0.25f)){g=false;break;} ok(g,"mapped-write visible after unmap"); }
    { float* rp=static_cast<float*>(q.enqueueMapBuffer(M,CL_TRUE,CL_MAP_READ,0,bytes));
      bool g=(rp!=nullptr); for(int i=0;g&&i<N;i++) if(!feq(rp[i],(float)i*0.25f)){g=false;break;}
      if(rp) q.enqueueUnmapMemObject(M,rp); q.finish(); ok(g,"enqueueMapBuffer READ sees mapped data"); }

    // --- strided/rect transfers: write then read a rect region and verify the round-trip ---
    { cl::Buffer RB(ctx,CL_MEM_READ_WRITE,bytes);
      cl::array<cl::size_type,3> bo{0,0,0}, ho{0,0,0}, rgn{bytes,1,1};
      q.enqueueWriteBufferRect(RB,CL_TRUE,bo,ho,rgn,0,0,0,0,a.data());
      std::vector<float> rr(N,-1);
      q.enqueueReadBufferRect(RB,CL_TRUE,bo,ho,rgn,0,0,0,0,rr.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(rr[i],a[i])){g=false;break;} ok(g,"write/read BufferRect round-trip"); }

    // --- introspection / sub-buffer / mem+kernel+context info ---
    { std::string kn=prog.getInfo<CL_PROGRAM_KERNEL_NAMES>(); ok(kn.find("vadd")!=std::string::npos && kn.find("saxpy")!=std::string::npos,"program KERNEL_NAMES contains vadd+saxpy"); }
    { cl_buffer_region reg{ (size_t)(N/2)*sizeof(float),(size_t)(N/2)*sizeof(float) };
      cl::Buffer subb=A.createSubBuffer(CL_MEM_READ_ONLY,CL_BUFFER_CREATE_TYPE_REGION,&reg);
      std::vector<float> hs(N/2); q.enqueueReadBuffer(subb,CL_TRUE,0,(size_t)(N/2)*sizeof(float),hs.data());
      ok(feq(hs[0],a[N/2]),"sub-buffer maps A's second half (value check)"); }
    ok((A.getInfo<CL_MEM_FLAGS>()&CL_MEM_READ_ONLY)!=0,"Buffer MEM_FLAGS has READ_ONLY");

    // --- separate compilation: clCompileProgram + clLinkProgram across two sources ---
    { const char* hdr="float triple(float x);\nfloat triple(float x){return 3.0f*x;}\n";
      const char* usr="float triple(float x);\n__kernel void trip(__global const float*a,__global float*b){int i=get_global_id(0);b[i]=triple(a[i]);}\n";
      cl::Program ph(ctx,std::string(hdr)); ph.compile();
      cl::Program pu(ctx,std::string(usr)); pu.compile();
      cl::Program linked=cl::linkProgram(std::vector<cl::Program>{ph,pu});
      cl::Kernel kt(linked,"trip");
      cl::Buffer TA(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,a.data());
      cl::Buffer TB(ctx,CL_MEM_WRITE_ONLY,bytes);
      kt.setArg(0,TA); kt.setArg(1,TB);
      q.enqueueNDRangeKernel(kt,cl::NullRange,cl::NDRange(N)); q.finish();
      std::vector<float> to(N); q.enqueueReadBuffer(TB,CL_TRUE,0,bytes,to.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(to[i],3.0f*a[i])){g=false;break;} ok(g,"compile+link cross-source: trip==3*a"); }

    // --- BUILD-ERROR negative path: intentionally-broken kernel, catch BuildError, inspect LOG+STATUS ---
    { cl::Program bad(ctx,std::string("__kernel void broken(__global float*a){ this is not valid }\n"));
      bool threw=false; cl_int berr=0; size_t loglen=0; cl_build_status bst=CL_BUILD_SUCCESS;
      try{ bad.build({dev}); }
      catch(cl::BuildError& be){ threw=true; berr=be.err();
        auto logs=be.getBuildLog(); if(!logs.empty()) loglen=logs[0].second.size();
        bst=bad.getBuildInfo<CL_PROGRAM_BUILD_STATUS>(dev); }
      ok(threw && berr==CL_BUILD_PROGRAM_FAILURE,"BuildError err==CL_BUILD_PROGRAM_FAILURE");
      ok(loglen>0,"CL_PROGRAM_BUILD_LOG non-empty on failure");
      ok(bst==CL_BUILD_ERROR,"BUILD_STATUS==CL_BUILD_ERROR"); }

    // --- VALIDATION-ERROR paths: assert the exact returned CL_INVALID_* enum ---
    { bool z=false; cl_int ze=0;
      try{ cl::Buffer Zbad(ctx,CL_MEM_READ_WRITE,0); }
      catch(cl::Error& e){ z=true; ze=e.err(); }
      ok(z && ze==CL_INVALID_BUFFER_SIZE,"zero-size Buffer -> CL_INVALID_BUFFER_SIZE"); }
    { bool w=false; cl_int we=0;
      cl::Kernel ks(prog,"add_scalar"); cl::Buffer KB(ctx,CL_MEM_READ_WRITE,64*sizeof(float));
      ks.setArg(0,KB); ks.setArg(1,1.0f);
      try{ q.enqueueNDRangeKernel(ks,cl::NullRange,cl::NDRange(maxwg*2),cl::NDRange(maxwg*2)); q.finish(); }
      catch(cl::Error& e){ w=true; we=e.err(); }
      ok(w && we==CL_INVALID_WORK_GROUP_SIZE,"local>max -> CL_INVALID_WORK_GROUP_SIZE"); }
    { bool ai=false; cl_int aie=0;
      try{ kadd.setArg(9,A); }
      catch(cl::Error& e){ ai=true; aie=e.err(); }
      ok(ai && aie==CL_INVALID_ARG_INDEX,"out-of-range setArg -> CL_INVALID_ARG_INDEX"); }

    // --- sub-device partitioning: assert the documented outcome (error enum when unsupported) ---
    { std::vector<cl::Device> subs; cl_device_partition_property pp[]={CL_DEVICE_PARTITION_EQUALLY,1,0};
      cl_int rc=CL_SUCCESS; bool threw=false; cl_int te=0;
      try{ rc=dev.createSubDevices(pp,&subs); }
      catch(cl::Error& e){ threw=true; te=e.err(); }
      bool documented = (rc==CL_SUCCESS && subs.size()>=1) ||
                        (threw && (te==CL_DEVICE_PARTITION_FAILED||te==CL_INVALID_VALUE||te==CL_INVALID_DEVICE));
      ok(documented,"createSubDevices returns success-with-subdevices or a documented error enum"); }

    // --- image family: Image2D CL_RGBA/CL_FLOAT write+read round-trip with pixel value check ---
    if(dev.getInfo<CL_DEVICE_IMAGE_SUPPORT>()){
      cl::ImageFormat fmt(CL_RGBA,CL_FLOAT);
      const int W=8,H=8; std::vector<float> px(W*H*4);
      for(int i=0;i<W*H*4;i++) px[i]=(float)i*0.5f;
      cl::Image2D img(ctx,CL_MEM_READ_WRITE,fmt,W,H);
      ok(img.getImageInfo<CL_IMAGE_WIDTH>()==(size_t)W,"Image2D WIDTH==8");
      ok(img.getImageInfo<CL_IMAGE_HEIGHT>()==(size_t)H,"Image2D HEIGHT==8");
      cl::array<cl::size_type,3> o0{0,0,0}, rgn{(cl::size_type)W,(cl::size_type)H,1};
      q.enqueueWriteImage(img,CL_TRUE,o0,rgn,0,0,px.data());
      std::vector<float> rd(W*H*4,-1);
      q.enqueueReadImage(img,CL_TRUE,o0,rgn,0,0,rd.data());
      bool g=true; for(int i=0;i<W*H*4;i++) if(!feq(rd[i],px[i])){g=false;break;} ok(g,"Image2D write/read round-trip pixels");
    } else { ok(false,"device reports no image support"); ok(false,"image height unavailable"); ok(false,"image roundtrip unavailable"); }

    // --- user event + completion callback + real dependency chain A->B->C on out-of-order queue ---
    cl::CommandQueue ooq(ctx,dev,CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE);
    ok((ooq.getInfo<CL_QUEUE_PROPERTIES>()&CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE)!=0,"out-of-order queue PROPERTIES");
    { const int Mn=256; size_t mb=Mn*sizeof(float); std::vector<float> z(Mn,1.0f);
      cl::Buffer Z(ctx,CL_MEM_READ_WRITE|CL_MEM_COPY_HOST_PTR,mb,z.data());
      cl::Kernel add(prog,"add_scalar"); add.setArg(0,Z);
      cl::UserEvent gate(ctx);
      cl::Event e1,e2,e3;
      // first step waits on a host-driven user event; nothing may run until we release it
      std::vector<cl::Event> w0{gate};
      add.setArg(1,2.0f); ooq.enqueueNDRangeKernel(add,cl::NullRange,cl::NDRange(Mn),cl::NullRange,&w0,&e1);
      std::vector<cl::Event> w1{e1}; add.setArg(1,3.0f); ooq.enqueueNDRangeKernel(add,cl::NullRange,cl::NDRange(Mn),cl::NullRange,&w1,&e2);
      std::vector<cl::Event> w2{e2}; add.setArg(1,5.0f); ooq.enqueueNDRangeKernel(add,cl::NullRange,cl::NDRange(Mn),cl::NullRange,&w2,&e3);
      e3.setCallback(CL_COMPLETE,on_complete,&g_cb_count);
      // gate still pending -> chain must not have completed
      ok(e1.getInfo<CL_EVENT_COMMAND_EXECUTION_STATUS>()>CL_COMPLETE,"user-event gate holds chain (A not complete)");
      gate.setStatus(CL_COMPLETE);
      e3.wait();
      std::vector<float> zo(Mn); ooq.enqueueReadBuffer(Z,CL_TRUE,0,mb,zo.data());
      bool g=true; for(int i=0;i<Mn;i++) if(!feq(zo[i],1.0f+2.0f+3.0f+5.0f)){g=false;break;} ok(g,"A->B->C dependency chain == 1+2+3+5");
      for(int t=0;t<200 && g_cb_count.load()==0;t++) std::this_thread::sleep_for(std::chrono::milliseconds(2));
      ok(g_cb_count.load()>=1,"event completion callback fired"); }

    // --- BOUNDARY: >=1,000,000-element dispatch with non-divisible tail guard, element-wise vs closed form ---
    { const int BN=1000003; size_t bb=(size_t)BN*sizeof(float);
      std::vector<float> ba(BN); for(int i=0;i<BN;i++) ba[i]=(float)(i%997)*0.5f;
      cl::Buffer BA(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bb,ba.data());
      cl::Buffer BB(ctx,CL_MEM_WRITE_ONLY,bb);
      cl::Kernel sq(prog,"square_guarded"); sq.setArg(0,BA); sq.setArg(1,BB); sq.setArg(2,BN);
      size_t lws=64, gws=((BN+lws-1)/lws)*lws; // non-divisible -> tail work-items masked by guard
      q.enqueueNDRangeKernel(sq,cl::NullRange,cl::NDRange(gws),cl::NDRange(lws)); q.finish();
      std::vector<float> bo(BN); q.enqueueReadBuffer(BB,CL_TRUE,0,bb,bo.data());
      bool g=true; for(int i=0;i<BN;i++){ float e=ba[i]*ba[i]; if(std::fabs(bo[i]-e)>1e-3f*(1.0f+std::fabs(e))){g=false;break;} }
      ok(g,"1000003-element square == a*a (tail guard exact)");
      ok(feq(bo[BN-1],ba[BN-1]*ba[BN-1]),"last (tail) element computed correctly"); }

    // --- BOUNDARY: zero-length NDRange is a no-op that leaves the buffer untouched ---
    { cl::Buffer ZB(ctx,CL_MEM_READ_WRITE,bytes); q.enqueueFillBuffer(ZB,42.0f,0,bytes); q.finish();
      cl::Kernel add0(prog,"add_scalar"); add0.setArg(0,ZB); add0.setArg(1,100.0f);
      q.enqueueNDRangeKernel(add0,cl::NullRange,cl::NDRange(0)); q.finish();
      std::vector<float> zc(N); q.enqueueReadBuffer(ZB,CL_TRUE,0,bytes,zc.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(zc[i],42.0f)){g=false;break;} ok(g,"zero-length dispatch is a no-op (buffer==42)"); }

    // --- marker/barrier with wait list: sequence a fill behind a marker and verify effect ---
    { cl::Buffer P(ctx,CL_MEM_READ_WRITE,bytes);
      cl::Event mev; q.enqueueMarkerWithWaitList(nullptr,&mev);
      std::vector<cl::Event> wl{mev};
      q.enqueueFillBuffer(P,3.25f,0,bytes,&wl); q.finish();
      std::vector<float> pc(N); q.enqueueReadBuffer(P,CL_TRUE,0,bytes,pc.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(pc[i],3.25f)){g=false;break;} ok(g,"marker-gated fill applied"); }
    q.enqueueBarrierWithWaitList();
    { std::vector<cl::Memory> migs{A}; q.enqueueMigrateMemObjects(migs,CL_MIGRATE_MEM_OBJECT_HOST);
      q.finish(); std::vector<float> mc(N); q.enqueueReadBuffer(A,CL_TRUE,0,bytes,mc.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(mc[i],a[i])){g=false;break;} ok(g,"migrate preserves A's contents"); }

    // --- sync APIs: flush+finish drain the queue; a pending fill is visible afterwards ---
    { cl::Buffer S(ctx,CL_MEM_READ_WRITE,bytes); q.enqueueFillBuffer(S,9.0f,0,bytes);
      q.flush(); q.finish();
      std::vector<float> sc(N); q.enqueueReadBuffer(S,CL_TRUE,0,bytes,sc.data());
      bool g=true; for(int i=0;i<N;i++) if(!feq(sc[i],9.0f)){g=false;break;} ok(g,"flush+finish drains pending fill"); }
  } catch(cl::Error& e){ fprintf(stderr,"cl::Error %s (%d)\n",e.what(),e.err()); ok(false,"unexpected cl::Error thrown"); }

  int EXPECTED=54, TOTAL=PASS+FAIL;
  printf("opencl-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("OPENCL_CPP_FULL_API OK %d\n",PASS); return 0; }
  printf("OPENCL_CPP_FULL_API FAIL\n"); return 1;
}

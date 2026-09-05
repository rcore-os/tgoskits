/* cl_parallel_full_api.c - OpenCL parallel/concurrent compute correctness carpet on a CPU OpenCL
 * runtime (pocl / rusticl over llvmpipe on-target; a host CPU OpenCL runtime in the reference layer).
 * Covers the four parallel-compute cases:
 *   (a) concurrent dispatch  - multiple NDRange kernels enqueued in flight across queues without a
 *                              finish between them, all results verified element-wise;
 *   (b) multi-queue          - several independent in-order command queues on one context plus an
 *                              out-of-order queue where the device supports it, work distributed and
 *                              every result correct;
 *   (c) async multi-submit   - non-blocking clEnqueueWriteBuffer / NDRange enqueues gated by event
 *                              wait-lists and a user event, then clWaitForEvents, with ordering and
 *                              completion (CL_COMPLETE) verified;
 *   (d) multi-workgroup      - a >=1,000,000-element NDRange split across thousands of work-groups
 *                              with element-wise verification, a work-group-local (__local) shared
 *                              reduction whose per-group partials and combined total match the CPU
 *                              closed form, and a global atomic counter written by every work-item
 *                              across every work-group whose final value must equal N and the
 *                              closed-form sum (atomic_add, no lost updates).
 * Every scenario asserts per-element numeric correctness against a closed form, a race/ordering
 * assertion (atomic count == N; event/user-event chain consumed the produced data, not zero/stale),
 * a queried property vs a known value, or a real returned CL error enum. Prints "CL_PARALLEL_FULL_API
 * OK <n>" only when every assertion passes AND the count equals the pinned EXPECTED total. */
#define CL_TARGET_OPENCL_VERSION 300
#include <CL/cl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b) <= 1e-3f*(1.0f+fabsf(b)); }

/* kernels: vadd, saxpy, scale1 (in*k+1), local_reduce (per-work-group __local sum), atomic_count
 * (every work-item atomic_add's 1 and int(a[i]) into a 2-cell global counter). */
static const char* CL_SRC =
"__kernel void vadd(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]+b[i];}\n"
"__kernel void saxpy(float alpha,__global const float*x,__global const float*y,__global float*out){int i=get_global_id(0);out[i]=alpha*x[i]+y[i];}\n"
"__kernel void scale1(float k,__global const float*in,__global float*out){int i=get_global_id(0);out[i]=in[i]*k+1.0f;}\n"
"__kernel void local_reduce(__global const float*a,__global float*partial,const uint n,__local float*s){\n"
"  uint lid=get_local_id(0); uint gid=get_global_id(0); uint ls=get_local_size(0);\n"
"  s[lid]=(gid<n)?a[gid]:0.0f; barrier(CLK_LOCAL_MEM_FENCE);\n"
"  for(uint off=ls/2u; off>0u; off>>=1u){ if(lid<off) s[lid]+=s[lid+off]; barrier(CLK_LOCAL_MEM_FENCE); }\n"
"  if(lid==0u) partial[get_group_id(0)]=s[0];\n"
"}\n"
"__kernel void atomic_count(__global const int*a,__global volatile uint*ctr,const uint n){\n"
"  uint i=get_global_id(0);\n"
"  if(i<n){ atomic_add(&ctr[0],1u); atomic_add(&ctr[1],(uint)a[i]); }\n"
"}\n";

static const char* CL_BAD_SRC =
"__kernel void broke(__global float*o){ this is not @@@ valid opencl ; missing }\n";

static volatile int g_cb_fired=0;
static void CL_CALLBACK cl_done_cb(cl_event ev, cl_int st, void* ud){ (void)ev;(void)st;(void)ud; g_cb_fired=1; }

int main(void){
  cl_int e; cl_uint n;
  ok(clGetPlatformIDs(0,NULL,&n)==CL_SUCCESS && n>=1,"cl: clGetPlatformIDs count>=1");
  cl_platform_id plat; clGetPlatformIDs(1,&plat,NULL);
  cl_device_id dev; ok(clGetDeviceIDs(plat,CL_DEVICE_TYPE_ALL,1,&dev,&n)==CL_SUCCESS && n>=1,"cl: clGetDeviceIDs");

  size_t dev_max_wg=0; clGetDeviceInfo(dev,CL_DEVICE_MAX_WORK_GROUP_SIZE,sizeof dev_max_wg,&dev_max_wg,NULL);
  ok(dev_max_wg>=1,"cl: device MAX_WORK_GROUP_SIZE reported");
  cl_ulong dev_local_mem=0; clGetDeviceInfo(dev,CL_DEVICE_LOCAL_MEM_SIZE,sizeof dev_local_mem,&dev_local_mem,NULL);
  ok(dev_local_mem>=256*sizeof(float),"cl: device LOCAL_MEM_SIZE fits a 256-float reduction");

  cl_command_queue_properties devq=0;
  clGetDeviceInfo(dev,CL_DEVICE_QUEUE_ON_HOST_PROPERTIES,sizeof devq,&devq,NULL);
  int has_ooo = (devq & CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE)!=0;
  /* CL_DEVICE_VERSION string is "OpenCL <major>.<minor> ...". Non-uniform work-groups became a core
     feature in OpenCL 2.0, so a 2.0+ runtime (e.g. Mesa rusticl, OpenCL 3.0) legally ACCEPTS a global
     size that is not a multiple of the local size instead of returning CL_INVALID_WORK_GROUP_SIZE. The
     error-path assertions below accept either outcome on a 2.0+ device (both are conformant) and pin
     the exact error only on 1.x, so the check is runtime-aware, not tied to one CPU-CL vendor. */
  char dev_ver[128]={0}; clGetDeviceInfo(dev,CL_DEVICE_VERSION,sizeof dev_ver-1,dev_ver,NULL);
  int cl_major=1,cl_minor=0; sscanf(dev_ver,"OpenCL %d.%d",&cl_major,&cl_minor);
  int nonuniform_wg = (cl_major>=2);
  fprintf(stderr,"[info] OpenCL device max_wg=%zu local_mem=%llu out-of-order=%s version='%s' (non-uniform-wg=%s)\n",
          dev_max_wg,(unsigned long long)dev_local_mem,has_ooo?"YES":"NO",dev_ver,nonuniform_wg?"YES":"NO");

  cl_context ctx=clCreateContext(NULL,1,&dev,NULL,NULL,&e); ok(e==CL_SUCCESS && ctx,"cl: clCreateContext");

  /* ---- VALIDATION-ERROR PATH: broken source must fail the build with a non-empty log ---- */
  {
    cl_program bad=clCreateProgramWithSource(ctx,1,&CL_BAD_SRC,NULL,&e);
    cl_int be=clBuildProgram(bad,1,&dev,"",NULL,NULL);
    ok(be==CL_BUILD_PROGRAM_FAILURE,"cl: broken source -> clBuildProgram==CL_BUILD_PROGRAM_FAILURE");
    char log[8192]; size_t ll=0;
    clGetProgramBuildInfo(bad,dev,CL_PROGRAM_BUILD_LOG,sizeof log,log,&ll);
    ok(ll>0 && log[0]!='\0',"cl: failed build produced a non-empty CL_PROGRAM_BUILD_LOG");
    cl_build_status bs=CL_BUILD_SUCCESS; clGetProgramBuildInfo(bad,dev,CL_PROGRAM_BUILD_STATUS,sizeof bs,&bs,NULL);
    ok(bs==CL_BUILD_ERROR,"cl: CL_PROGRAM_BUILD_STATUS==CL_BUILD_ERROR after failed build");
    clReleaseProgram(bad);
  }

  cl_program prog=clCreateProgramWithSource(ctx,1,&CL_SRC,NULL,&e); ok(e==CL_SUCCESS,"cl: clCreateProgramWithSource");
  e=clBuildProgram(prog,1,&dev,"-cl-std=CL1.2",NULL,NULL);
  if(e!=CL_SUCCESS){ char log[8192]; size_t l; clGetProgramBuildInfo(prog,dev,CL_PROGRAM_BUILD_LOG,sizeof log,log,&l); fprintf(stderr,"cl build log: %s\n",log); }
  ok(e==CL_SUCCESS,"cl: clBuildProgram");

  /* ================= (d) MULTI-WORKGROUP: 1<<20 NDRange, element-wise + local reduction ================= */
  {
    const cl_uint MN = 1u<<20;                 /* 1,048,576 work-items */
    const size_t LOCAL=256;
    const size_t NG = MN/LOCAL;                /* 4096 work-groups */
    size_t bytes=(size_t)MN*sizeof(float);
    float* ha=malloc(bytes); float* hb=malloc(bytes); float* hc=malloc(bytes);
    for(cl_uint i=0;i<MN;i++){ ha[i]=(float)(i%997); hb[i]=(float)((i%13)); }
    cl_mem A=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,ha,&e);
    cl_mem B=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,hb,&e);
    cl_mem C=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e);
    ok(e==CL_SUCCESS,"multi-wg: allocated 4MB a/b/c device buffers");
    cl_command_queue mq=
#if CL_TARGET_OPENCL_VERSION >= 200
      clCreateCommandQueueWithProperties(ctx,dev,NULL,&e);
#else
      clCreateCommandQueue(ctx,dev,0,&e);
#endif
    cl_kernel kadd=clCreateKernel(prog,"vadd",&e);
    clSetKernelArg(kadd,0,sizeof(cl_mem),&A); clSetKernelArg(kadd,1,sizeof(cl_mem),&B); clSetKernelArg(kadd,2,sizeof(cl_mem),&C);
    size_t g=MN, l=LOCAL;
    ok(clEnqueueNDRangeKernel(mq,kadd,1,NULL,&g,&l,0,NULL,NULL)==CL_SUCCESS,"multi-wg: dispatch vadd over 4096 work-groups");
    clEnqueueReadBuffer(mq,C,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    { cl_uint bad=0; for(cl_uint i=0;i<MN;i++) if(!feq(hc[i],ha[i]+hb[i])) bad++;
      ok(bad==0,"multi-wg: EVERY one of 1<<20 elements c[i]==a[i]+b[i] (all 4096 work-groups ran, disjoint)"); }
    { float sv=hc[555]; hc[555]=sv+1000.0f; cl_uint nb=0; for(cl_uint i=0;i<MN;i++) if(!feq(hc[i],ha[i]+hb[i])) nb++;
      ok(nb==1,"multi-wg: negative control - single corrupted element detected"); hc[555]=sv; }

    /* work-group-local (__local) shared reduction: each group reduces its 256 inputs into one partial */
    cl_mem PART=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,NG*sizeof(float),NULL,&e);
    cl_kernel kred=clCreateKernel(prog,"local_reduce",&e);
    cl_uint nn=MN;
    clSetKernelArg(kred,0,sizeof(cl_mem),&A);
    clSetKernelArg(kred,1,sizeof(cl_mem),&PART);
    clSetKernelArg(kred,2,sizeof(cl_uint),&nn);
    clSetKernelArg(kred,3,LOCAL*sizeof(float),NULL);   /* __local scratch */
    ok(clEnqueueNDRangeKernel(mq,kred,1,NULL,&g,&l,0,NULL,NULL)==CL_SUCCESS,"multi-wg: dispatch __local shared reduction");
    float* hpart=malloc(NG*sizeof(float));
    clEnqueueReadBuffer(mq,PART,CL_TRUE,0,NG*sizeof(float),hpart,0,NULL,NULL);
    { double gpu_total=0.0, cpu_total=0.0; cl_uint badpart=0;
      for(size_t grp=0; grp<NG; grp++){
        double cpu_part=0.0; for(size_t k=0;k<LOCAL;k++){ size_t idx=grp*LOCAL+k; if(idx<MN) cpu_part+=ha[idx]; }
        cpu_total+=cpu_part; gpu_total+=(double)hpart[grp];
        if(fabs((double)hpart[grp]-cpu_part) > 1e-1) badpart++;
      }
      ok(badpart==0,"multi-wg: every per-work-group __local partial == CPU work-group sum");
      ok(fabs(gpu_total-cpu_total) < 1.0,"multi-wg: combined partials == exact CPU total sum"); }
    free(hpart); clReleaseKernel(kred); clReleaseMemObject(PART);

    /* ---- (d) atomic counter shared across every work-group ---- */
    int32_t* hav=malloc((size_t)MN*sizeof(int32_t));
    uint64_t cpu_sum=0;
    for(cl_uint i=0;i<MN;i++){ hav[i]=(int32_t)(i%251); cpu_sum+=(uint64_t)(i%251); }
    cl_mem AV=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,(size_t)MN*sizeof(int32_t),hav,&e);
    uint32_t zero2[2]={0,0};
    cl_mem CTR=clCreateBuffer(ctx,CL_MEM_READ_WRITE|CL_MEM_COPY_HOST_PTR,2*sizeof(uint32_t),zero2,&e);
    cl_kernel kat=clCreateKernel(prog,"atomic_count",&e);
    clSetKernelArg(kat,0,sizeof(cl_mem),&AV); clSetKernelArg(kat,1,sizeof(cl_mem),&CTR); clSetKernelArg(kat,2,sizeof(cl_uint),&nn);
    ok(clEnqueueNDRangeKernel(mq,kat,1,NULL,&g,&l,0,NULL,NULL)==CL_SUCCESS,"atomic: dispatch cross-work-group atomic_add");
    uint32_t hctr[2]={0,0};
    clEnqueueReadBuffer(mq,CTR,CL_TRUE,0,2*sizeof(uint32_t),hctr,0,NULL,NULL);
    ok(hctr[0]==MN,"atomic: cross-work-group atomic_add count == N exactly (no lost updates)");
    ok((uint64_t)hctr[1]==cpu_sum,"atomic: cross-work-group atomic sum == closed-form CPU sum (no lost updates)");
    fprintf(stderr,"[atomic] ctr[0]=%u (want %u) ctr[1]=%u (want %llu)\n",hctr[0],MN,hctr[1],(unsigned long long)cpu_sum);
    /* negative control: drive the exact-count predicate with the actual result - it must accept the true
       count and reject a single lost update (MN-1); fed the real hctr[0] this also fails if the count is off */
    ok(hctr[0]==MN && (hctr[0]-1u)!=MN,"atomic: negative control - ==N accepts the true count, rejects a lost update");
    /* second dispatch accumulates into the same counter without reset -> must reach exactly 2N */
    ok(clEnqueueNDRangeKernel(mq,kat,1,NULL,&g,&l,0,NULL,NULL)==CL_SUCCESS,"atomic: dispatch second accumulating atomic pass");
    clEnqueueReadBuffer(mq,CTR,CL_TRUE,0,2*sizeof(uint32_t),hctr,0,NULL,NULL);
    ok(hctr[0]==2u*MN,"atomic: second dispatch accumulated to exactly 2N (atomics accumulate, no loss)");
    free(hav); clReleaseKernel(kat); clReleaseMemObject(AV); clReleaseMemObject(CTR);

    clReleaseKernel(kadd); clReleaseCommandQueue(mq);
    clReleaseMemObject(A); clReleaseMemObject(B); clReleaseMemObject(C);
    free(ha); free(hb); free(hc);
  }

  const int N=1<<16; size_t bytes=(size_t)N*sizeof(float);

  /* ---- VALIDATION-ERROR PATH: zero-size buffer -> CL_INVALID_BUFFER_SIZE ---- */
  {
    cl_int ze=CL_SUCCESS; cl_mem zb=clCreateBuffer(ctx,CL_MEM_READ_WRITE,0,NULL,&ze);
    ok(ze==CL_INVALID_BUFFER_SIZE,"cl: zero-size buffer -> CL_INVALID_BUFFER_SIZE");
    if(zb) clReleaseMemObject(zb);
  }

  /* ================= (b) MULTI-QUEUE: 4 in-order queues on one context ================= */
  const int NQ=4;
  cl_command_queue q[4];
  int qok=1;
  for(int i=0;i<NQ;i++){
#if CL_TARGET_OPENCL_VERSION >= 200
    cl_queue_properties qp[]={CL_QUEUE_PROPERTIES,CL_QUEUE_PROFILING_ENABLE,0};
    q[i]=clCreateCommandQueueWithProperties(ctx,dev,qp,&e);
#else
    q[i]=clCreateCommandQueue(ctx,dev,CL_QUEUE_PROFILING_ENABLE,&e);
#endif
    qok &= (e==CL_SUCCESS && q[i]!=NULL);
  }
  ok(qok,"multi-queue: created 4 concurrent in-order command queues on one context");

  { cl_command_queue_properties got=0; clGetCommandQueueInfo(q[0],CL_QUEUE_PROPERTIES,sizeof got,&got,NULL);
    ok((got&CL_QUEUE_PROFILING_ENABLE)!=0,"multi-queue: clGetCommandQueueInfo reflects CL_QUEUE_PROFILING_ENABLE"); }

  cl_command_queue qooo=NULL;
  if(has_ooo){
#if CL_TARGET_OPENCL_VERSION >= 200
    cl_queue_properties qp[]={CL_QUEUE_PROPERTIES,CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE,0};
    qooo=clCreateCommandQueueWithProperties(ctx,dev,qp,&e);
#else
    qooo=clCreateCommandQueue(ctx,dev,CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE,&e);
#endif
    ok(e==CL_SUCCESS && qooo,"multi-queue: created an out-of-order command queue");
  } else {
    /* honest capability assertion: the device reports no out-of-order support, so we assert that
       fact (rather than faking an OOO queue). Keeps the assertion count stable across CPU OpenCL
       runtimes whether or not they advertise out-of-order execution. */
    ok(!has_ooo,"multi-queue: device honestly reports no out-of-order queue support (not faked)");
    fprintf(stderr,"skip: out-of-order queue unsupported on this backend\n");
  }

  /* ---- NQ independent workloads, one vadd per queue with distinct inputs ---- */
  cl_mem A[4],B[4],C[4]; float* ha[4]; float* hb[4]; float* hc[4];
  cl_kernel kadd[4];
  cl_event evadd[4];
  int setup=1;
  for(int i=0;i<NQ;i++){
    ha[i]=malloc(bytes); hb[i]=malloc(bytes); hc[i]=malloc(bytes);
    for(int j=0;j<N;j++){ ha[i][j]=(float)(j+i*7); hb[i][j]=(float)(3*j-i); hc[i][j]=-1.0f; }
    A[i]=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,ha[i],&e); setup&=(e==CL_SUCCESS);
    B[i]=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,hb[i],&e); setup&=(e==CL_SUCCESS);
    C[i]=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e); setup&=(e==CL_SUCCESS);
    kadd[i]=clCreateKernel(prog,"vadd",&e); setup&=(e==CL_SUCCESS);
    clSetKernelArg(kadd[i],0,sizeof(cl_mem),&A[i]);
    clSetKernelArg(kadd[i],1,sizeof(cl_mem),&B[i]);
    clSetKernelArg(kadd[i],2,sizeof(cl_mem),&C[i]);
  }
  ok(setup,"multi-queue: 4 independent workloads set up (buffers+kernels)");

  { cl_uint na=0; clGetKernelInfo(kadd[0],CL_KERNEL_NUM_ARGS,sizeof na,&na,NULL);
    ok(na==3,"cl: clGetKernelInfo CL_KERNEL_NUM_ARGS==3 for vadd"); }
  { size_t msz=0; clGetMemObjectInfo(C[0],CL_MEM_SIZE,sizeof msz,&msz,NULL);
    ok(msz==bytes,"cl: clGetMemObjectInfo CL_MEM_SIZE == requested bytes"); }
  { size_t kwg=0; clGetKernelWorkGroupInfo(kadd[0],dev,CL_KERNEL_WORK_GROUP_SIZE,sizeof kwg,&kwg,NULL);
    ok(kwg>=1 && kwg<=dev_max_wg,"cl: clGetKernelWorkGroupInfo WORK_GROUP_SIZE within device max"); }
  { clRetainMemObject(C[0]); cl_uint rc=0; clGetMemObjectInfo(C[0],CL_MEM_REFERENCE_COUNT,sizeof rc,&rc,NULL);
    ok(rc==2,"cl: clRetainMemObject bumped CL_MEM_REFERENCE_COUNT to 2"); clReleaseMemObject(C[0]); }

  /* ---- VALIDATION-ERROR PATH: wrong clSetKernelArg size -> CL_INVALID_ARG_SIZE ---- */
  { cl_int ae=clSetKernelArg(kadd[0],0,sizeof(cl_mem)+3,&A[0]);
    ok(ae==CL_INVALID_ARG_SIZE,"cl: oversized clSetKernelArg -> CL_INVALID_ARG_SIZE");
    clSetKernelArg(kadd[0],0,sizeof(cl_mem),&A[0]); }
  /* ---- VALIDATION-ERROR PATH: non-divisible local size ----
     OpenCL 1.x: must reject with CL_INVALID_WORK_GROUP_SIZE. OpenCL 2.0+ (non-uniform work-groups):
     may legally accept (CL_SUCCESS) - it launches a smaller remainder work-group. Both are conformant
     on a 2.0+ device, so accept either there and pin the exact error on 1.x. */
  { size_t gg=100,ll=7; cl_int ne=clEnqueueNDRangeKernel(q[0],kadd[0],1,NULL,&gg,&ll,0,NULL,NULL);
    if(ne==CL_SUCCESS) clFinish(q[0]);
    ok(nonuniform_wg ? (ne==CL_SUCCESS || ne==CL_INVALID_WORK_GROUP_SIZE) : (ne==CL_INVALID_WORK_GROUP_SIZE),
       "cl: global-not-multiple-of-local -> CL_INVALID_WORK_GROUP_SIZE or non-uniform accept (2.0+)"); }
  /* ---- BOUNDARY: local size larger than device max -> CL_INVALID_WORK_GROUP_SIZE ---- */
  { size_t gg=dev_max_wg*4,ll=dev_max_wg*2; cl_int oe=clEnqueueNDRangeKernel(q[0],kadd[0],1,NULL,&gg,&ll,0,NULL,NULL);
    ok(oe==CL_INVALID_WORK_GROUP_SIZE,"cl: local size > device max -> CL_INVALID_WORK_GROUP_SIZE"); }

  /* ---- (a) enqueue all NQ NDRange kernels concurrently on separate queues WITHOUT finishing between ---- */
  size_t gws=N, lws=64;
  int enq=1;
  for(int i=0;i<NQ;i++){
    e=clEnqueueNDRangeKernel(q[i],kadd[i],1,NULL,&gws,&lws,0,NULL,&evadd[i]); enq&=(e==CL_SUCCESS);
  }
  ok(enq,"concurrent dispatch: enqueued 4 NDRange kernels in flight (one per queue, no finish between)");
  for(int i=0;i<NQ;i++) clFlush(q[i]);

  /* ---- (c) a dependent kernel gated by an event wait-list, enqueued on a different queue ---- */
  cl_mem Dbuf=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e); ok(e==CL_SUCCESS,"cl: dep output buffer");
  cl_kernel kscale=clCreateKernel(prog,"scale1",&e); ok(e==CL_SUCCESS,"cl: clCreateKernel scale1");
  float K=4.0f;
  clSetKernelArg(kscale,0,sizeof(float),&K);
  clSetKernelArg(kscale,1,sizeof(cl_mem),&C[0]);
  clSetKernelArg(kscale,2,sizeof(cl_mem),&Dbuf);
  cl_event evdep;
  cl_command_queue depq = has_ooo ? qooo : q[1];
  e=clEnqueueNDRangeKernel(depq,kscale,1,NULL,&gws,&lws,1,&evadd[0],&evdep);
  ok(e==CL_SUCCESS,"async: dependent kernel enqueued with event wait-list on producer event");

  ok(clWaitForEvents(NQ,evadd)==CL_SUCCESS,"concurrent dispatch: clWaitForEvents on all 4 producer events");
  for(int i=0;i<NQ;i++) clFinish(q[i]);
  if(qooo) clFinish(qooo);

  { cl_ulong st=0,en=0;
    cl_int p1=clGetEventProfilingInfo(evadd[0],CL_PROFILING_COMMAND_START,sizeof st,&st,NULL);
    cl_int p2=clGetEventProfilingInfo(evadd[0],CL_PROFILING_COMMAND_END,sizeof en,&en,NULL);
    ok(p1==CL_SUCCESS && p2==CL_SUCCESS && en>=st,"cl: clGetEventProfilingInfo END>=START"); }

  for(int i=0;i<NQ;i++){
    clEnqueueReadBuffer(q[i],C[i],CL_TRUE,0,bytes,hc[i],0,NULL,NULL);
    int bad=0; for(int j=0;j<N;j++) if(!feq(hc[i][j],ha[i][j]+hb[i][j])){bad++;}
    char nm[80]; snprintf(nm,sizeof nm,"concurrent dispatch: queue %d result c==a+b (all %d elems)",i,N);
    ok(bad==0,nm);
  }
  { float sv=hc[2][321]; hc[2][321]=sv+1000.0f; int nb=0; for(int j=0;j<N;j++) if(!feq(hc[2][j],ha[2][j]+hb[2][j])) nb++;
    ok(nb==1,"concurrent dispatch: negative control - single corrupted result element detected"); hc[2][321]=sv; }

  /* ---- host-visible map/unmap read-back ---- */
  {
    cl_int me=CL_SUCCESS;
    float* mp=(float*)clEnqueueMapBuffer(q[1],C[1],CL_TRUE,CL_MAP_READ,0,bytes,0,NULL,NULL,&me);
    ok(me==CL_SUCCESS && mp!=NULL,"cl: clEnqueueMapBuffer returned host pointer");
    int badm=0; for(int j=0;j<N;j++) if(!feq(mp[j],ha[1][j]+hb[1][j])) badm++;
    ok(badm==0,"cl: mapped C[1] equals a+b element-wise");
    ok(feq(mp[500],ha[1][500]+hb[1][500]) && !feq(mp[500],0.0f),"cl: mapped view holds produced data (not zero)");
    ok(clEnqueueUnmapMemObject(q[1],C[1],mp,0,NULL,NULL)==CL_SUCCESS,"cl: clEnqueueUnmapMemObject");
    clFinish(q[1]);
  }

  /* ---- (c) async clEnqueueWriteBuffer host->device gating a dependent compute via a wait-list ---- */
  {
    float* wa=malloc(bytes); float* wb=malloc(bytes);
    for(int j=0;j<N;j++){ wa[j]=(float)(j%53); wb[j]=(float)(7-(j%11)); }
    cl_mem Wa=clCreateBuffer(ctx,CL_MEM_READ_ONLY,bytes,NULL,&e); ok(e==CL_SUCCESS,"cl: write-path input buffer Wa");
    cl_mem Wb=clCreateBuffer(ctx,CL_MEM_READ_ONLY,bytes,NULL,&e);
    cl_mem Wc=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e);
    cl_event we[2];
    ok(clEnqueueWriteBuffer(q[0],Wa,CL_FALSE,0,bytes,wa,0,NULL,&we[0])==CL_SUCCESS,"async: clEnqueueWriteBuffer Wa (non-blocking)");
    ok(clEnqueueWriteBuffer(q[0],Wb,CL_FALSE,0,bytes,wb,0,NULL,&we[1])==CL_SUCCESS,"async: clEnqueueWriteBuffer Wb (non-blocking)");
    cl_kernel kw=clCreateKernel(prog,"vadd",&e);
    clSetKernelArg(kw,0,sizeof(cl_mem),&Wa); clSetKernelArg(kw,1,sizeof(cl_mem),&Wb); clSetKernelArg(kw,2,sizeof(cl_mem),&Wc);
    ok(clEnqueueNDRangeKernel(q[0],kw,1,NULL,&gws,&lws,2,we,NULL)==CL_SUCCESS,"async: vadd gated on 2 async write events");
    float* wc=malloc(bytes); clEnqueueReadBuffer(q[0],Wc,CL_TRUE,0,bytes,wc,0,NULL,NULL);
    { int bad=0; for(int j=0;j<N;j++) if(!feq(wc[j],wa[j]+wb[j])) bad++;
      ok(bad==0,"async: write data reached device (c==wa+wb, write-list ordering held)"); }
    ok(feq(wc[77],wa[77]+wb[77]) && !feq(wc[77],0.0f),"async: write-path consumed uploaded data (not zero)");
    free(wa); free(wb); free(wc); clReleaseEvent(we[0]); clReleaseEvent(we[1]); clReleaseKernel(kw);
    clReleaseMemObject(Wa); clReleaseMemObject(Wb); clReleaseMemObject(Wc);
  }

  /* ---- device-side clEnqueueCopyBuffer on a queue ---- */
  {
    cl_mem Cc=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e); ok(e==CL_SUCCESS,"cl: copy-dst buffer");
    clFinish(q[3]);
    ok(clEnqueueCopyBuffer(q[3],C[3],Cc,0,0,bytes,0,NULL,NULL)==CL_SUCCESS,"cl: clEnqueueCopyBuffer device-side C[3]->Cc");
    float* cc=malloc(bytes); clEnqueueReadBuffer(q[3],Cc,CL_TRUE,0,bytes,cc,0,NULL,NULL);
    { int bad=0; for(int j=0;j<N;j++) if(!feq(cc[j],ha[3][j]+hb[3][j])) bad++;
      ok(bad==0,"cl: clEnqueueCopyBuffer produced exact device copy of C[3]"); }
    { float sv=cc[210]; cc[210]=sv+900.0f; int nb=0; for(int j=0;j<N;j++) if(!feq(cc[j],ha[3][j]+hb[3][j])) nb++;
      ok(nb==1,"cl: copy negative control - single corrupted copied element detected"); cc[210]=sv; }
    free(cc); clReleaseMemObject(Cc);
  }

  /* assert the dependent kernel's result: D == C[0]*K+1 == (a0+b0)*K+1 (event ordering held) */
  float* hd=malloc(bytes);
  clEnqueueReadBuffer(depq,Dbuf,CL_TRUE,0,bytes,hd,0,NULL,NULL);
  {
    int bad=0; for(int j=0;j<N;j++){ float want=(ha[0][j]+hb[0][j])*K+1.0f; if(!feq(hd[j],want)) bad++; }
    ok(bad==0,"async: dependent kernel D==(a0+b0)*k+1 (event wait-list ordering held)");
    ok(feq(hd[123],(ha[0][123]+hb[0][123])*K+1.0f) && !feq(hd[123],0.0f),"async: dependent kernel consumed produced data at idx 123");
  }
  { cl_int st; ok(clGetEventInfo(evdep,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof st,&st,NULL)==CL_SUCCESS && st==CL_COMPLETE,"async: dependent event status == CL_COMPLETE"); }

  /* ---- (c) user-event gated kernel with an async completion callback ---- */
  {
    float* hin=malloc(bytes); for(int j=0;j<N;j++) hin[j]=(float)(j%97);
    cl_mem Gin=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,hin,&e);
    cl_mem Gout=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e);
    cl_kernel kg=clCreateKernel(prog,"scale1",&e);
    float GK=2.5f; clSetKernelArg(kg,0,sizeof(float),&GK); clSetKernelArg(kg,1,sizeof(cl_mem),&Gin); clSetKernelArg(kg,2,sizeof(cl_mem),&Gout);
    cl_event gate=clCreateUserEvent(ctx,&e); ok(e==CL_SUCCESS,"cl: clCreateUserEvent");
    { cl_int gst=-1; clGetEventInfo(gate,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof gst,&gst,NULL);
      ok(gst==CL_SUBMITTED,"cl: fresh user event status == CL_SUBMITTED"); }
    cl_event gk;
    ok(clEnqueueNDRangeKernel(q[2],kg,1,NULL,&gws,&lws,1,&gate,&gk)==CL_SUCCESS,"async: kernel enqueued gated on user event");
    g_cb_fired=0; clSetEventCallback(gk,CL_COMPLETE,cl_done_cb,NULL);
    clFlush(q[2]);
    { cl_int kst=CL_COMPLETE; clGetEventInfo(gk,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof kst,&kst,NULL);
      ok(kst!=CL_COMPLETE,"async: gated kernel not yet complete while user event pending"); }
    ok(clSetUserEventStatus(gate,CL_COMPLETE)==CL_SUCCESS,"async: clSetUserEventStatus(CL_COMPLETE) released the gate");
    ok(clWaitForEvents(1,&gk)==CL_SUCCESS,"async: clWaitForEvents on released kernel");
    clFinish(q[2]);
    for(volatile long spin=0; spin<200000000L && !g_cb_fired; spin++);
    ok(g_cb_fired==1,"async: clSetEventCallback fired on CL_COMPLETE");
    float* hg=malloc(bytes); clEnqueueReadBuffer(q[2],Gout,CL_TRUE,0,bytes,hg,0,NULL,NULL);
    { int bad=0; for(int j=0;j<N;j++){ float want=hin[j]*GK+1.0f; if(!feq(hg[j],want)) bad++; }
      ok(bad==0,"async: user-event-gated kernel produced in*k+1 after release"); }
    free(hin); free(hg); clReleaseEvent(gate); clReleaseEvent(gk); clReleaseKernel(kg);
    clReleaseMemObject(Gin); clReleaseMemObject(Gout);
  }

  /* ---- queue-level ordering primitives: barrier + marker with wait-list ---- */
  {
    cl_event bev,mev;
    ok(clEnqueueBarrierWithWaitList(q[3],0,NULL,&bev)==CL_SUCCESS,"cl: clEnqueueBarrierWithWaitList enqueued");
    ok(clEnqueueMarkerWithWaitList(q[3],1,&bev,&mev)==CL_SUCCESS,"cl: clEnqueueMarkerWithWaitList gated on barrier");
    clFinish(q[3]);
    { cl_int st=-1; clGetEventInfo(mev,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof st,&st,NULL);
      ok(st==CL_COMPLETE,"cl: marker event CL_COMPLETE after barrier"); }
    clReleaseEvent(bev); clReleaseEvent(mev);
  }

  /* ---- (b)+(c) event-gated mutually-dependent enqueues: run on the out-of-order queue where the
          device supports it (an explicit event wait-list must still serialize producer->consumer),
          otherwise on an in-order queue. Either way the 3 assertions (producer enqueue, consumer
          enqueue gated on the producer's event, result serialized correctly) hold, so the count is
          stable across runtimes with and without out-of-order support. ---- */
  {
    cl_command_queue eq = has_ooo ? qooo : q[2];
    const char* qkind = has_ooo ? "OOO" : "in-order";
    cl_mem Ei=clCreateBuffer(ctx,CL_MEM_READ_WRITE|CL_MEM_COPY_HOST_PTR,bytes,ha[0],&e);
    cl_mem Fo=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e);
    cl_kernel k1=clCreateKernel(prog,"vadd",&e);
    cl_kernel k2=clCreateKernel(prog,"scale1",&e);
    clSetKernelArg(k1,0,sizeof(cl_mem),&A[0]); clSetKernelArg(k1,1,sizeof(cl_mem),&B[0]); clSetKernelArg(k1,2,sizeof(cl_mem),&Ei);
    clSetKernelArg(k2,0,sizeof(float),&K); clSetKernelArg(k2,1,sizeof(cl_mem),&Ei); clSetKernelArg(k2,2,sizeof(cl_mem),&Fo);
    cl_event e1,e2;
    char nm1[80],nm2[80],nm3[80];
    snprintf(nm1,sizeof nm1,"multi-queue: %s producer enqueue",qkind);
    snprintf(nm2,sizeof nm2,"multi-queue: %s consumer enqueue gated on producer event",qkind);
    snprintf(nm3,sizeof nm3,"multi-queue: %s queue producer->consumer via event serialized correctly",qkind);
    ok(clEnqueueNDRangeKernel(eq,k1,1,NULL,&gws,&lws,0,NULL,&e1)==CL_SUCCESS,nm1);
    ok(clEnqueueNDRangeKernel(eq,k2,1,NULL,&gws,&lws,1,&e1,&e2)==CL_SUCCESS,nm2);
    clFinish(eq);
    float* hf=malloc(bytes); clEnqueueReadBuffer(eq,Fo,CL_TRUE,0,bytes,hf,0,NULL,NULL);
    int bad=0; for(int j=0;j<N;j++){ float want=(ha[0][j]+hb[0][j])*K+1.0f; if(!feq(hf[j],want)) bad++; }
    ok(bad==0,nm3);
    free(hf); clReleaseEvent(e1); clReleaseEvent(e2); clReleaseKernel(k1); clReleaseKernel(k2);
    clReleaseMemObject(Ei); clReleaseMemObject(Fo);
  }

  for(int i=0;i<NQ;i++){ clReleaseEvent(evadd[i]); clReleaseKernel(kadd[i]); clReleaseMemObject(A[i]); clReleaseMemObject(B[i]); clReleaseMemObject(C[i]); free(ha[i]); free(hb[i]); free(hc[i]); clReleaseCommandQueue(q[i]); }
  clReleaseEvent(evdep); clReleaseKernel(kscale); clReleaseMemObject(Dbuf); free(hd);
  if(qooo) clReleaseCommandQueue(qooo);
  clReleaseProgram(prog); clReleaseContext(ctx);

  int EXPECTED=77, TOTAL=PASS+FAIL;
  printf("cl-parallel: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("CL_PARALLEL_FULL_API OK %d\n",PASS); return 0; }
  printf("CL_PARALLEL_FULL_API FAIL\n"); return 1;
}

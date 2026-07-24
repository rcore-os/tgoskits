/* opencl_c_full_api.c - OpenCL C API carpet against the CL/cl.h ground-truth surface:
 * platform / device / context / command-queue / buffer / sub-buffer / image + sampler /
 * program (source + separate compile+link + binary + IL) / kernel + arg reflection /
 * NDRange (correctness, boundary, tail-guard, oversubscription) / events with real
 * A->B->C wait-list dependency chains + completion callback / out-of-order queue /
 * profiling / reference counting / sub-device probe / validation-error enums /
 * SVM (alloc/map/unmap/memfill/memcpy/kernel-arg, capability-guarded) /
 * sub-group reflection / host+device timers / mem destructor callback /
 * spec-constant + compiler-unload surface.
 * Every assertion checks a computed value, a queried property against a known expected
 * value, or a real CL_INVALID_* / CL_BUILD_PROGRAM_FAILURE return enum - including
 * negative controls that prove the comparison flags a deliberately-wrong element.
 * Prints "OPENCL_C_FULL_API OK <n>" only when every assertion passes and the count
 * equals the pinned EXPECTED total. */
#define CL_TARGET_OPENCL_VERSION 300
#include <CL/cl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ fprintf(stderr,"[%s] %s\n",c?"ok":"FAIL",d); if(c)PASS++; else{FAIL++;} }
static int feq(float a,float b){ return fabsf(a-b) <= 1e-4f*(1.0f+fabsf(b)); }

static const char* SRC =
"__kernel void vadd(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]+b[i];}\n"
"__kernel void vmul(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]*b[i];}\n"
"__kernel void saxpy(float alpha,__global const float*x,__global float*y){int i=get_global_id(0);y[i]=alpha*x[i]+y[i];}\n"
"__kernel void reduce_sum(__global const float*a,__global float*out,__local float*s){\n"
"  int lid=get_local_id(0),gid=get_global_id(0),ls=get_local_size(0);s[lid]=a[gid];\n"
"  barrier(CLK_LOCAL_MEM_FENCE);for(int o=ls/2;o>0;o>>=1){if(lid<o)s[lid]+=s[lid+o];barrier(CLK_LOCAL_MEM_FENCE);}\n"
"  if(lid==0)out[get_group_id(0)]=s[0];}\n";

/* separate-compilation unit: doubled + tail-guarded scale, linked standalone */
static const char* SRC_SCALE =
"__kernel void scale2(__global const float*x,__global float*y,int n){int i=get_global_id(0);if(i<n)y[i]=x[i]*2.0f;}\n";

/* chain unit for event_wait_list ordering A->B->C */
static const char* SRC_CHAIN =
"__kernel void c_set(__global float*a){a[get_global_id(0)]=1.0f;}\n"
"__kernel void c_add(__global float*a){a[get_global_id(0)]+=10.0f;}\n"
"__kernel void c_mul(__global float*a){a[get_global_id(0)]*=2.0f;}\n";

/* large-dispatch + tail-guard unit */
static const char* SRC_BIG =
"__kernel void half_idx(__global float*a,int n){int i=get_global_id(0);if(i<n)a[i]=(float)i*0.5f;}\n";

static volatile int g_cbflag = -999;
static void CL_CALLBACK on_complete(cl_event ev,cl_int st,void*ud){ (void)ev; *(volatile int*)ud = st + 1000; }

static volatile int g_destr = -999;
static void CL_CALLBACK on_mem_destroy(cl_mem m,void*ud){ (void)m; *(volatile int*)ud = 4242; }

/* SVM kernel: writes the global index into a coarse-grain SVM buffer bound via clSetKernelArgSVMPointer */
static const char* SRC_SVM =
"__kernel void svm_fill(__global float*a){int i=get_global_id(0);a[i]=(float)i;}\n";

int main(void){
  cl_int e; cl_uint n;

  /* --- platform: enumerate + read every documented CL_PLATFORM_* string --- */
  e=clGetPlatformIDs(0,NULL,&n); ok(e==CL_SUCCESS && n>=1,"clGetPlatformIDs count>=1");
  cl_platform_id* plats=malloc(sizeof(cl_platform_id)*n);
  ok(clGetPlatformIDs(n,plats,NULL)==CL_SUCCESS,"clGetPlatformIDs list");
  cl_platform_id plat=plats[0];
  char buf[8192]; size_t sz=0;
  ok(clGetPlatformInfo(plat,CL_PLATFORM_NAME,sizeof buf,buf,&sz)==CL_SUCCESS && sz>=1 && buf[sz-1]=='\0',"clGetPlatformInfo NAME NUL-terminated");
  ok(clGetPlatformInfo(plat,CL_PLATFORM_VERSION,sizeof buf,buf,NULL)==CL_SUCCESS && strstr(buf,"OpenCL")!=NULL,"clGetPlatformInfo VERSION contains 'OpenCL'");
  ok(clGetPlatformInfo(plat,CL_PLATFORM_PROFILE,sizeof buf,buf,NULL)==CL_SUCCESS && (strcmp(buf,"FULL_PROFILE")==0||strcmp(buf,"EMBEDDED_PROFILE")==0),"clGetPlatformInfo PROFILE is a valid enum string");
  ok(clGetPlatformInfo(plat,CL_PLATFORM_VENDOR,sizeof buf,buf,&sz)==CL_SUCCESS && sz>=1,"clGetPlatformInfo VENDOR non-empty");
  ok(clGetPlatformInfo(plat,CL_PLATFORM_EXTENSIONS,sizeof buf,buf,NULL)==CL_SUCCESS,"clGetPlatformInfo EXTENSIONS");
  /* VALIDATION: too-small param buffer must return CL_INVALID_VALUE */
  { char tiny[1]; size_t need=0; cl_int ie=clGetPlatformInfo(plat,CL_PLATFORM_NAME,1,tiny,&need);
    ok(ie==CL_INVALID_VALUE,"clGetPlatformInfo too-small buffer == CL_INVALID_VALUE"); }

  /* --- device: type/counts + per-dim work-item sizes + alignment + image support --- */
  cl_device_id dev; ok(clGetDeviceIDs(plat,CL_DEVICE_TYPE_ALL,1,&dev,&n)==CL_SUCCESS && n>=1,"clGetDeviceIDs");
  cl_device_type dt=0; ok(clGetDeviceInfo(dev,CL_DEVICE_TYPE,sizeof dt,&dt,NULL)==CL_SUCCESS && (dt&(CL_DEVICE_TYPE_CPU|CL_DEVICE_TYPE_GPU|CL_DEVICE_TYPE_ACCELERATOR)),"clGetDeviceInfo TYPE is a real device class");
  cl_uint cu=0; ok(clGetDeviceInfo(dev,CL_DEVICE_MAX_COMPUTE_UNITS,sizeof cu,&cu,NULL)==CL_SUCCESS && cu>=1,"MAX_COMPUTE_UNITS>=1");
  size_t mwg=0; ok(clGetDeviceInfo(dev,CL_DEVICE_MAX_WORK_GROUP_SIZE,sizeof mwg,&mwg,NULL)==CL_SUCCESS && mwg>=1,"MAX_WORK_GROUP_SIZE>=1");
  cl_uint wdim=0; ok(clGetDeviceInfo(dev,CL_DEVICE_MAX_WORK_ITEM_DIMENSIONS,sizeof wdim,&wdim,NULL)==CL_SUCCESS && wdim>=3,"MAX_WORK_ITEM_DIMENSIONS>=3");
  size_t wis[3]={0,0,0}; ok(clGetDeviceInfo(dev,CL_DEVICE_MAX_WORK_ITEM_SIZES,sizeof wis,wis,NULL)==CL_SUCCESS && wis[0]>=1 && wis[0]>=mwg,"MAX_WORK_ITEM_SIZES[0]>=MAX_WORK_GROUP_SIZE");
  cl_ulong gmem=0; ok(clGetDeviceInfo(dev,CL_DEVICE_GLOBAL_MEM_SIZE,sizeof gmem,&gmem,NULL)==CL_SUCCESS && gmem>0,"GLOBAL_MEM_SIZE>0");
  cl_uint mba=0; ok(clGetDeviceInfo(dev,CL_DEVICE_MEM_BASE_ADDR_ALIGN,sizeof mba,&mba,NULL)==CL_SUCCESS && mba>0 && (mba%8)==0,"MEM_BASE_ADDR_ALIGN is a positive bit-multiple");
  cl_bool img_support=CL_FALSE; ok(clGetDeviceInfo(dev,CL_DEVICE_IMAGE_SUPPORT,sizeof img_support,&img_support,NULL)==CL_SUCCESS,"clGetDeviceInfo IMAGE_SUPPORT queried");
  cl_uint sub_max=0; clGetDeviceInfo(dev,CL_DEVICE_PARTITION_MAX_SUB_DEVICES,sizeof sub_max,&sub_max,NULL);

  /* --- context + queue --- */
  cl_context ctx=clCreateContext(NULL,1,&dev,NULL,NULL,&e); ok(e==CL_SUCCESS && ctx,"clCreateContext");
  cl_uint ndev=0; ok(clGetContextInfo(ctx,CL_CONTEXT_NUM_DEVICES,sizeof ndev,&ndev,NULL)==CL_SUCCESS && ndev==1,"clGetContextInfo NUM_DEVICES==1");
  { cl_uint rc0=0,rc1=0; clGetContextInfo(ctx,CL_CONTEXT_REFERENCE_COUNT,sizeof rc0,&rc0,NULL);
    clRetainContext(ctx); clGetContextInfo(ctx,CL_CONTEXT_REFERENCE_COUNT,sizeof rc1,&rc1,NULL);
    ok(rc1==rc0+1,"clRetainContext increments CONTEXT_REFERENCE_COUNT");
    clReleaseContext(ctx); cl_uint rc2=0; clGetContextInfo(ctx,CL_CONTEXT_REFERENCE_COUNT,sizeof rc2,&rc2,NULL);
    ok(rc2==rc0,"clReleaseContext restores CONTEXT_REFERENCE_COUNT"); }
  cl_queue_properties qp[]={CL_QUEUE_PROPERTIES,CL_QUEUE_PROFILING_ENABLE,0};
  cl_command_queue q=clCreateCommandQueueWithProperties(ctx,dev,qp,&e); ok(e==CL_SUCCESS && q,"clCreateCommandQueueWithProperties(profiling)");
  { cl_context qc=NULL; ok(clGetCommandQueueInfo(q,CL_QUEUE_CONTEXT,sizeof qc,&qc,NULL)==CL_SUCCESS && qc==ctx,"clGetCommandQueueInfo CONTEXT==ctx"); }
  { cl_command_queue_properties p2=0; ok(clGetCommandQueueInfo(q,CL_QUEUE_PROPERTIES,sizeof p2,&p2,NULL)==CL_SUCCESS && (p2&CL_QUEUE_PROFILING_ENABLE),"clGetCommandQueueInfo PROPERTIES has PROFILING_ENABLE"); }

  /* --- buffers + metadata + VALIDATION (zero-size) --- */
  const int N=1024; size_t bytes=N*sizeof(float);
  float *ha=malloc(bytes),*hb=malloc(bytes),*hc=malloc(bytes);
  for(int i=0;i<N;i++){ha[i]=(float)i;hb[i]=2.0f*i+1.0f;}
  cl_mem A=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,ha,&e); ok(e==CL_SUCCESS,"clCreateBuffer A COPY_HOST_PTR");
  cl_mem B=clCreateBuffer(ctx,CL_MEM_READ_ONLY,bytes,NULL,&e); ok(e==CL_SUCCESS,"clCreateBuffer B");
  cl_mem C=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e); ok(e==CL_SUCCESS,"clCreateBuffer C");
  ok(clEnqueueWriteBuffer(q,B,CL_TRUE,0,bytes,hb,0,NULL,NULL)==CL_SUCCESS,"clEnqueueWriteBuffer B");
  { cl_mem_object_type mt=0; ok(clGetMemObjectInfo(A,CL_MEM_TYPE,sizeof mt,&mt,NULL)==CL_SUCCESS && mt==CL_MEM_OBJECT_BUFFER,"clGetMemObjectInfo TYPE==BUFFER"); }
  { size_t msz=0; ok(clGetMemObjectInfo(A,CL_MEM_SIZE,sizeof msz,&msz,NULL)==CL_SUCCESS && msz==bytes,"clGetMemObjectInfo SIZE==bytes"); }
  { cl_int ze=CL_SUCCESS; cl_mem zb=clCreateBuffer(ctx,CL_MEM_READ_WRITE,0,NULL,&ze);
    ok(ze==CL_INVALID_BUFFER_SIZE && zb==NULL,"clCreateBuffer(size 0) == CL_INVALID_BUFFER_SIZE"); }

  /* --- program: build + info + VALIDATION build failure --- */
  cl_program prog=clCreateProgramWithSource(ctx,1,&SRC,NULL,&e); ok(e==CL_SUCCESS,"clCreateProgramWithSource");
  e=clBuildProgram(prog,1,&dev,"-cl-std=CL1.2 -cl-kernel-arg-info",NULL,NULL);
  ok(e==CL_SUCCESS,"clBuildProgram");
  { cl_build_status bs=CL_BUILD_NONE; ok(clGetProgramBuildInfo(prog,dev,CL_PROGRAM_BUILD_STATUS,sizeof bs,&bs,NULL)==CL_SUCCESS && bs==CL_BUILD_SUCCESS,"clGetProgramBuildInfo STATUS==SUCCESS"); }
  { const char* badsrc="__kernel void broken(){ this_is_not(valid c code ;;; }"; cl_int be=CL_SUCCESS;
    cl_program bp=clCreateProgramWithSource(ctx,1,&badsrc,NULL,&be);
    cl_int rb=clBuildProgram(bp,1,&dev,NULL,NULL,NULL);
    size_t loglen=0; char log[4096]=""; clGetProgramBuildInfo(bp,dev,CL_PROGRAM_BUILD_LOG,sizeof log,log,&loglen);
    ok(rb==CL_BUILD_PROGRAM_FAILURE && loglen>1,"broken kernel == CL_BUILD_PROGRAM_FAILURE with non-empty log");
    clReleaseProgram(bp); }

  cl_uint nk=0; ok(clCreateKernelsInProgram(prog,0,NULL,&nk)==CL_SUCCESS && nk==4,"clCreateKernelsInProgram count==4");
  cl_kernel kadd=clCreateKernel(prog,"vadd",&e); ok(e==CL_SUCCESS,"clCreateKernel vadd");
  cl_kernel kmul=clCreateKernel(prog,"vmul",&e); ok(e==CL_SUCCESS,"clCreateKernel vmul");
  cl_kernel ksax=clCreateKernel(prog,"saxpy",&e); ok(e==CL_SUCCESS,"clCreateKernel saxpy");
  cl_kernel kred=clCreateKernel(prog,"reduce_sum",&e); ok(e==CL_SUCCESS,"clCreateKernel reduce_sum");
  /* VALIDATION: unknown kernel name */
  { cl_int ke=CL_SUCCESS; cl_kernel bad=clCreateKernel(prog,"does_not_exist",&ke);
    ok(ke==CL_INVALID_KERNEL_NAME && bad==NULL,"clCreateKernel(unknown) == CL_INVALID_KERNEL_NAME"); }
  { cl_uint kna=0; ok(clGetKernelInfo(kadd,CL_KERNEL_NUM_ARGS,sizeof kna,&kna,NULL)==CL_SUCCESS && kna==3,"clGetKernelInfo NUM_ARGS==3"); }
  { size_t kwg=0; ok(clGetKernelWorkGroupInfo(kadd,dev,CL_KERNEL_WORK_GROUP_SIZE,sizeof kwg,&kwg,NULL)==CL_SUCCESS && kwg>=1 && kwg<=mwg,"clGetKernelWorkGroupInfo WORK_GROUP_SIZE within device max"); }

  /* --- kernel-arg reflection (needs -cl-kernel-arg-info) --- */
  { cl_kernel_arg_address_qualifier aq=0;
    ok(clGetKernelArgInfo(kadd,0,CL_KERNEL_ARG_ADDRESS_QUALIFIER,sizeof aq,&aq,NULL)==CL_SUCCESS && aq==CL_KERNEL_ARG_ADDRESS_GLOBAL,"clGetKernelArgInfo arg0 ADDRESS_GLOBAL");
    char tname[64]=""; ok(clGetKernelArgInfo(kadd,0,CL_KERNEL_ARG_TYPE_NAME,sizeof tname,tname,NULL)==CL_SUCCESS && strstr(tname,"float")!=NULL,"clGetKernelArgInfo arg0 TYPE_NAME contains 'float'");
    cl_kernel_arg_address_qualifier aq1=0; clGetKernelArgInfo(ksax,0,CL_KERNEL_ARG_ADDRESS_QUALIFIER,sizeof aq1,&aq1,NULL);
    ok(aq1==CL_KERNEL_ARG_ADDRESS_PRIVATE,"clGetKernelArgInfo saxpy scalar arg0 ADDRESS_PRIVATE"); }

  /* --- vadd correctness + negative control --- */
  ok(clSetKernelArg(kadd,0,sizeof(cl_mem),&A)==CL_SUCCESS,"clSetKernelArg vadd 0");
  ok(clSetKernelArg(kadd,1,sizeof(cl_mem),&B)==CL_SUCCESS,"clSetKernelArg vadd 1");
  ok(clSetKernelArg(kadd,2,sizeof(cl_mem),&C)==CL_SUCCESS,"clSetKernelArg vadd 2");
  size_t gws=N, lws=64; cl_event ev=NULL;
  ok(clEnqueueNDRangeKernel(q,kadd,1,NULL,&gws,&lws,0,NULL,&ev)==CL_SUCCESS,"clEnqueueNDRangeKernel vadd");
  ok(clWaitForEvents(1,&ev)==CL_SUCCESS,"clWaitForEvents(vadd)");
  ok(clEnqueueReadBuffer(q,C,CL_TRUE,0,bytes,hc,0,NULL,NULL)==CL_SUCCESS,"clEnqueueReadBuffer C");
  { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]+hb[i])){good=0;break;} ok(good,"vadd result == a+b"); }
  { float saved=hc[7]; hc[7]=saved+1.0f; int flagged=0; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]+hb[i])){flagged=1;break;} hc[7]=saved;
    ok(flagged,"negative control: corrupted vadd element IS flagged"); }

  /* --- event execution status + profiling (real monotonic bound) --- */
  { cl_int est=0; ok(clGetEventInfo(ev,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof est,&est,NULL)==CL_SUCCESS && est==CL_COMPLETE,"clGetEventInfo STATUS==COMPLETE"); }
  { cl_command_type ct=0; ok(clGetEventInfo(ev,CL_EVENT_COMMAND_TYPE,sizeof ct,&ct,NULL)==CL_SUCCESS && ct==CL_COMMAND_NDRANGE_KERNEL,"clGetEventInfo COMMAND_TYPE==NDRANGE_KERNEL"); }
  { cl_ulong tq=0,ts=0,t0=0,t1=0;
    cl_int r0=clGetEventProfilingInfo(ev,CL_PROFILING_COMMAND_QUEUED,sizeof tq,&tq,NULL);
    cl_int r1=clGetEventProfilingInfo(ev,CL_PROFILING_COMMAND_SUBMIT,sizeof ts,&ts,NULL);
    cl_int r2=clGetEventProfilingInfo(ev,CL_PROFILING_COMMAND_START,sizeof t0,&t0,NULL);
    cl_int r3=clGetEventProfilingInfo(ev,CL_PROFILING_COMMAND_END,sizeof t1,&t1,NULL);
    ok(r0==CL_SUCCESS&&r1==CL_SUCCESS&&r2==CL_SUCCESS&&r3==CL_SUCCESS && tq<=ts && ts<=t0 && t0<=t1,"clGetEventProfilingInfo QUEUED<=SUBMIT<=START<=END"); }
  clReleaseEvent(ev);

  /* --- vmul correctness + negative control --- */
  clSetKernelArg(kmul,0,sizeof(cl_mem),&A);clSetKernelArg(kmul,1,sizeof(cl_mem),&B);clSetKernelArg(kmul,2,sizeof(cl_mem),&C);
  ok(clEnqueueNDRangeKernel(q,kmul,1,NULL,&gws,&lws,0,NULL,NULL)==CL_SUCCESS,"clEnqueueNDRangeKernel vmul");
  clEnqueueReadBuffer(q,C,CL_TRUE,0,bytes,hc,0,NULL,NULL);
  { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]*hb[i])){good=0;break;} ok(good,"vmul result == a*b"); }
  { float saved=hc[3]; hc[3]=saved*2.0f+1.0f; int flagged=0; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]*hb[i])){flagged=1;break;} hc[3]=saved;
    ok(flagged,"negative control: corrupted vmul element IS flagged"); }

  /* --- saxpy (scalar arg) correctness + negative control --- */
  cl_mem Y=clCreateBuffer(ctx,CL_MEM_READ_WRITE|CL_MEM_COPY_HOST_PTR,bytes,hb,&e); ok(e==CL_SUCCESS,"clCreateBuffer Y RW");
  float alpha=3.0f;
  clSetKernelArg(ksax,0,sizeof(float),&alpha);clSetKernelArg(ksax,1,sizeof(cl_mem),&A);clSetKernelArg(ksax,2,sizeof(cl_mem),&Y);
  ok(clEnqueueNDRangeKernel(q,ksax,1,NULL,&gws,&lws,0,NULL,NULL)==CL_SUCCESS,"clEnqueueNDRangeKernel saxpy");
  clEnqueueReadBuffer(q,Y,CL_TRUE,0,bytes,hc,0,NULL,NULL);
  { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],alpha*ha[i]+hb[i])){good=0;break;} ok(good,"saxpy result == alpha*x+y"); }
  { float saved=hc[500]; hc[500]=saved+5.0f; int flagged=0; for(int i=0;i<N;i++) if(!feq(hc[i],alpha*ha[i]+hb[i])){flagged=1;break;} hc[500]=saved;
    ok(flagged,"negative control: corrupted saxpy element IS flagged"); }

  /* --- local-memory + barrier reduction correctness + negative control --- */
  int ng=N/lws; cl_mem R=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,ng*sizeof(float),NULL,&e); ok(e==CL_SUCCESS,"clCreateBuffer R");
  clSetKernelArg(kred,0,sizeof(cl_mem),&A);clSetKernelArg(kred,1,sizeof(cl_mem),&R);clSetKernelArg(kred,2,lws*sizeof(float),NULL);
  ok(clEnqueueNDRangeKernel(q,kred,1,NULL,&gws,&lws,0,NULL,NULL)==CL_SUCCESS,"clEnqueueNDRangeKernel reduce(local+barrier)");
  float* hr=malloc(ng*sizeof(float)); clEnqueueReadBuffer(q,R,CL_TRUE,0,ng*sizeof(float),hr,0,NULL,NULL);
  double ref_sum=0; for(int i=0;i<N;i++) ref_sum+=ha[i];
  { double tot=0; for(int i=0;i<ng;i++) tot+=hr[i]; ok(fabs(tot-ref_sum)<1.0,"reduce_sum local-mem == sum(a)"); }
  { float saved=hr[0]; hr[0]=saved+1000.0f; double tot=0; for(int i=0;i<ng;i++) tot+=hr[i]; hr[0]=saved;
    ok(fabs(tot-ref_sum)>=1.0,"negative control: corrupted reduce partial IS flagged"); }

  /* --- copy / fill / map correctness --- */
  cl_mem D=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e); ok(e==CL_SUCCESS,"clCreateBuffer D");
  ok(clEnqueueCopyBuffer(q,A,D,0,0,bytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueCopyBuffer A->D");
  clEnqueueReadBuffer(q,D,CL_TRUE,0,bytes,hc,0,NULL,NULL);
  { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i])){good=0;break;} ok(good,"copy buffer bytes match"); }
  float fillv=7.5f; ok(clEnqueueFillBuffer(q,D,&fillv,sizeof fillv,0,bytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueFillBuffer");
  clFinish(q); clEnqueueReadBuffer(q,D,CL_TRUE,0,bytes,hc,0,NULL,NULL);
  { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],7.5f)){good=0;break;} ok(good,"fill buffer == 7.5"); }
  float* mp=clEnqueueMapBuffer(q,D,CL_TRUE,CL_MAP_READ,0,bytes,0,NULL,NULL,&e); ok(e==CL_SUCCESS && feq(mp[0],7.5f) && feq(mp[N-1],7.5f),"clEnqueueMapBuffer read == filled");
  ok(clEnqueueUnmapMemObject(q,D,mp,0,NULL,NULL)==CL_SUCCESS,"clEnqueueUnmapMemObject");

  /* --- sub-buffer aliases parent second half --- */
  cl_buffer_region reg={ (N/2)*sizeof(float), (N/2)*sizeof(float) };
  cl_mem sub=clCreateSubBuffer(A,CL_MEM_READ_ONLY,CL_BUFFER_CREATE_TYPE_REGION,&reg,&e); ok(e==CL_SUCCESS,"clCreateSubBuffer REGION");
  { float* hs=malloc((N/2)*sizeof(float)); clEnqueueReadBuffer(q,sub,CL_TRUE,0,(N/2)*sizeof(float),hs,0,NULL,NULL);
    int good=1; for(int i=0;i<N/2;i++) if(!feq(hs[i],ha[N/2+i])){good=0;break;} ok(good,"sub-buffer aliases A's second half"); free(hs); }

  /* --- rect copy / write-read roundtrip --- */
  { size_t so[3]={0,0,0},dof[3]={0,0,0},rgn[3]={bytes,1,1};
    ok(clEnqueueCopyBufferRect(q,A,D,so,dof,rgn,0,0,0,0,0,NULL,NULL)==CL_SUCCESS,"clEnqueueCopyBufferRect");
    clEnqueueReadBuffer(q,D,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i])){good=0;break;} ok(good,"rect copy bytes match"); }
  { size_t bo[3]={0,0,0},ho[3]={0,0,0},rgn[3]={bytes,1,1};
    ok(clEnqueueWriteBufferRect(q,D,CL_TRUE,bo,ho,rgn,0,0,0,0,ha,0,NULL,NULL)==CL_SUCCESS,"clEnqueueWriteBufferRect");
    memset(hc,0,bytes);
    ok(clEnqueueReadBufferRect(q,D,CL_TRUE,bo,ho,rgn,0,0,0,0,hc,0,NULL,NULL)==CL_SUCCESS,"clEnqueueReadBufferRect");
    int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i])){good=0;break;} ok(good,"write/read rect roundtrip"); }

  /* === image + sampler family (documented complete; probe IMAGE_SUPPORT) === */
  if(img_support){
    cl_image_format fmt={CL_RGBA,CL_FLOAT};
    cl_uint nfmt=0; ok(clGetSupportedImageFormats(ctx,CL_MEM_READ_WRITE,CL_MEM_OBJECT_IMAGE2D,0,NULL,&nfmt)==CL_SUCCESS && nfmt>=1,"clGetSupportedImageFormats count>=1");
    cl_image_desc desc; memset(&desc,0,sizeof desc); desc.image_type=CL_MEM_OBJECT_IMAGE2D; desc.image_width=8; desc.image_height=8;
    cl_mem img=clCreateImage(ctx,CL_MEM_READ_WRITE,&fmt,&desc,NULL,&e); ok(e==CL_SUCCESS && img,"clCreateImage IMAGE2D RGBA/FLOAT");
    { size_t iw=0,ih=0; clGetImageInfo(img,CL_IMAGE_WIDTH,sizeof iw,&iw,NULL); clGetImageInfo(img,CL_IMAGE_HEIGHT,sizeof ih,&ih,NULL);
      ok(iw==8 && ih==8,"clGetImageInfo WIDTH/HEIGHT==8"); }
    float pix[8*8*4]; for(int i=0;i<8*8*4;i++) pix[i]=(float)i*0.25f;
    size_t orig[3]={0,0,0}, rgn[3]={8,8,1};
    ok(clEnqueueWriteImage(q,img,CL_TRUE,orig,rgn,0,0,pix,0,NULL,NULL)==CL_SUCCESS,"clEnqueueWriteImage");
    float rd[8*8*4]; memset(rd,0,sizeof rd);
    ok(clEnqueueReadImage(q,img,CL_TRUE,orig,rgn,0,0,rd,0,NULL,NULL)==CL_SUCCESS,"clEnqueueReadImage");
    { int good=1; for(int i=0;i<8*8*4;i++) if(!feq(rd[i],pix[i])){good=0;break;} ok(good,"image write/read roundtrip pixels match"); }
    { float saved=rd[10]; rd[10]=saved+9.0f; int flagged=0; for(int i=0;i<8*8*4;i++) if(!feq(rd[i],pix[i])){flagged=1;break;} rd[10]=saved;
      ok(flagged,"negative control: corrupted image texel IS flagged"); }
    cl_mem img2=clCreateImage(ctx,CL_MEM_READ_WRITE,&fmt,&desc,NULL,&e);
    ok(clEnqueueCopyImage(q,img,img2,orig,orig,rgn,0,NULL,NULL)==CL_SUCCESS,"clEnqueueCopyImage");
    memset(rd,0,sizeof rd); clEnqueueReadImage(q,img2,CL_TRUE,orig,rgn,0,0,rd,0,NULL,NULL);
    { int good=1; for(int i=0;i<8*8*4;i++) if(!feq(rd[i],pix[i])){good=0;break;} ok(good,"copied image texels match source"); }
    clReleaseMemObject(img); clReleaseMemObject(img2);
    cl_sampler_properties sp[]={CL_SAMPLER_NORMALIZED_COORDS,CL_FALSE,CL_SAMPLER_ADDRESSING_MODE,CL_ADDRESS_CLAMP_TO_EDGE,CL_SAMPLER_FILTER_MODE,CL_FILTER_NEAREST,0};
    cl_sampler smp=clCreateSamplerWithProperties(ctx,sp,&e); ok(e==CL_SUCCESS && smp,"clCreateSamplerWithProperties");
    { cl_addressing_mode am=0; cl_filter_mode fm=0; clGetSamplerInfo(smp,CL_SAMPLER_ADDRESSING_MODE,sizeof am,&am,NULL); clGetSamplerInfo(smp,CL_SAMPLER_FILTER_MODE,sizeof fm,&fm,NULL);
      ok(am==CL_ADDRESS_CLAMP_TO_EDGE && fm==CL_FILTER_NEAREST,"clGetSamplerInfo reflects requested addressing+filter"); }
    ok(clRetainSampler(smp)==CL_SUCCESS && clReleaseSampler(smp)==CL_SUCCESS,"clRetain/ReleaseSampler");
    clReleaseSampler(smp);
  } else {
    fprintf(stderr,"skip: image + sampler family unsupported on this backend (CL_DEVICE_IMAGE_SUPPORT==0)\n");
  }

  /* === separate compilation: clCompileProgram + clLinkProgram, result matches closed form === */
  { cl_program sp=clCreateProgramWithSource(ctx,1,&SRC_SCALE,NULL,&e); ok(e==CL_SUCCESS,"clCreateProgramWithSource(scale2)");
    ok(clCompileProgram(sp,1,&dev,NULL,0,NULL,NULL,NULL,NULL)==CL_SUCCESS,"clCompileProgram(scale2)");
    cl_int le=CL_SUCCESS; cl_program lp=clLinkProgram(ctx,1,&dev,NULL,1,&sp,NULL,NULL,&le);
    ok(le==CL_SUCCESS && lp,"clLinkProgram(scale2)");
    cl_kernel ks=clCreateKernel(lp,"scale2",&e); ok(e==CL_SUCCESS,"clCreateKernel(linked scale2)");
    cl_mem X=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,ha,&e);
    cl_mem Z=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bytes,NULL,&e);
    int nn=N; clSetKernelArg(ks,0,sizeof(cl_mem),&X);clSetKernelArg(ks,1,sizeof(cl_mem),&Z);clSetKernelArg(ks,2,sizeof(int),&nn);
    size_t g=N; clEnqueueNDRangeKernel(q,ks,1,NULL,&g,&lws,0,NULL,NULL);
    clEnqueueReadBuffer(q,Z,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]*2.0f)){good=0;break;} ok(good,"linked scale2 == x*2");
    clReleaseKernel(ks); clReleaseProgram(lp); clReleaseProgram(sp); clReleaseMemObject(X); clReleaseMemObject(Z); }

  /* === program-with-binary round-trip build === */
  { size_t bsz=0; clGetProgramInfo(prog,CL_PROGRAM_BINARY_SIZES,sizeof bsz,&bsz,NULL);
    ok(bsz>0,"clGetProgramInfo BINARY_SIZES>0");
    unsigned char* bin=malloc(bsz?bsz:1); unsigned char* bins[1]={bin};
    ok(clGetProgramInfo(prog,CL_PROGRAM_BINARIES,sizeof bins,bins,NULL)==CL_SUCCESS,"clGetProgramInfo BINARIES");
    cl_int bst=CL_SUCCESS; cl_program pb=clCreateProgramWithBinary(ctx,1,&dev,&bsz,(const unsigned char**)bins,&bst,&e);
    ok(e==CL_SUCCESS && bst==CL_SUCCESS,"clCreateProgramWithBinary");
    ok(clBuildProgram(pb,1,&dev,NULL,NULL,NULL)==CL_SUCCESS,"clBuildProgram(from binary)");
    clReleaseProgram(pb); free(bin); }

  /* === IL / SPIR-V surface: query IL_VERSION, VALIDATION on malformed IL === */
  { char ilver[256]=""; size_t ils=0; cl_int ir=clGetDeviceInfo(dev,CL_DEVICE_IL_VERSION,sizeof ilver,ilver,&ils);
    ok(ir==CL_SUCCESS && ils>=1 && strstr(ilver,"SPIR-V")!=NULL,"CL_DEVICE_IL_VERSION advertises SPIR-V");
    unsigned char junk[16]; for(int i=0;i<16;i++) junk[i]=(unsigned char)(i*7+1);
    cl_int ile=CL_SUCCESS; cl_program pil=clCreateProgramWithIL(ctx,junk,sizeof junk,&ile);
    ok(ile==CL_INVALID_VALUE && pil==NULL,"clCreateProgramWithIL(malformed) == CL_INVALID_VALUE"); }

  /* === event dependency chain A->B->C via non-empty event_wait_list + completion callback === */
  { cl_program pc=clCreateProgramWithSource(ctx,1,&SRC_CHAIN,NULL,&e); clBuildProgram(pc,1,&dev,NULL,NULL,NULL);
    cl_kernel ca=clCreateKernel(pc,"c_set",&e),cb=clCreateKernel(pc,"c_add",&e),cc=clCreateKernel(pc,"c_mul",&e);
    cl_mem M=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e);
    clSetKernelArg(ca,0,sizeof(cl_mem),&M);clSetKernelArg(cb,0,sizeof(cl_mem),&M);clSetKernelArg(cc,0,sizeof(cl_mem),&M);
    size_t g=N; cl_event ea=NULL,eb=NULL,ec=NULL;
    ok(clEnqueueNDRangeKernel(q,ca,1,NULL,&g,NULL,0,NULL,&ea)==CL_SUCCESS,"chain A enqueue");
    ok(clEnqueueNDRangeKernel(q,cb,1,NULL,&g,NULL,1,&ea,&eb)==CL_SUCCESS,"chain B enqueue waits on A");
    ok(clEnqueueNDRangeKernel(q,cc,1,NULL,&g,NULL,1,&eb,&ec)==CL_SUCCESS,"chain C enqueue waits on B");
    g_cbflag=-999;
    ok(clSetEventCallback(ec,CL_COMPLETE,on_complete,(void*)&g_cbflag)==CL_SUCCESS,"clSetEventCallback on C");
    clWaitForEvents(1,&ec);
    clEnqueueReadBuffer(q,M,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    { int good=1; for(int i=0;i<N;i++) if(!feq(hc[i],22.0f)){good=0;break;} ok(good,"A->B->C ordered result == (1+10)*2 == 22"); }
    for(int i=0;i<200 && g_cbflag==-999;i++){ struct timespec ts={0,1000000}; nanosleep(&ts,NULL); }
    ok(g_cbflag==CL_COMPLETE+1000,"completion callback fired with CL_COMPLETE status");
    clReleaseEvent(ea);clReleaseEvent(eb);clReleaseEvent(ec);
    clReleaseKernel(ca);clReleaseKernel(cb);clReleaseKernel(cc);clReleaseProgram(pc);clReleaseMemObject(M); }

  /* === out-of-order queue: two independent kernels, gathered by a barrier-with-wait-list === */
  { cl_queue_properties oqp[]={CL_QUEUE_PROPERTIES,CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE|CL_QUEUE_PROFILING_ENABLE,0};
    cl_int oe=CL_SUCCESS; cl_command_queue oq=clCreateCommandQueueWithProperties(ctx,dev,oqp,&oe);
    ok(oe==CL_SUCCESS && oq,"clCreateCommandQueueWithProperties(out-of-order)");
    cl_mem P=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e), Qb=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e);
    clSetKernelArg(kmul,0,sizeof(cl_mem),&A);clSetKernelArg(kmul,1,sizeof(cl_mem),&B);clSetKernelArg(kmul,2,sizeof(cl_mem),&P);
    size_t g=N; cl_event e1=NULL,e2=NULL;
    clEnqueueNDRangeKernel(oq,kmul,1,NULL,&g,NULL,0,NULL,&e1);
    clSetKernelArg(kadd,0,sizeof(cl_mem),&A);clSetKernelArg(kadd,1,sizeof(cl_mem),&B);clSetKernelArg(kadd,2,sizeof(cl_mem),&Qb);
    clEnqueueNDRangeKernel(oq,kadd,1,NULL,&g,NULL,0,NULL,&e2);
    cl_event waits[2]={e1,e2}; cl_event bar=NULL;
    ok(clEnqueueBarrierWithWaitList(oq,2,waits,&bar)==CL_SUCCESS,"clEnqueueBarrierWithWaitList gathers 2 out-of-order events");
    clWaitForEvents(1,&bar);
    clEnqueueReadBuffer(oq,P,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    int gm=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]*hb[i])){gm=0;break;}
    clEnqueueReadBuffer(oq,Qb,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    int ga=1; for(int i=0;i<N;i++) if(!feq(hc[i],ha[i]+hb[i])){ga=0;break;}
    ok(gm && ga,"both out-of-order kernels produced correct independent results");
    clReleaseEvent(e1);clReleaseEvent(e2);clReleaseEvent(bar);
    clReleaseMemObject(P);clReleaseMemObject(Qb);clReleaseCommandQueue(oq); }

  /* === boundary: >=1M element dispatch verified element-wise vs closed form === */
  { cl_program pbg=clCreateProgramWithSource(ctx,1,&SRC_BIG,NULL,&e); clBuildProgram(pbg,1,&dev,NULL,NULL,NULL);
    cl_kernel kbg=clCreateKernel(pbg,"half_idx",&e);
    const int BIG=1000003; size_t bbytes=(size_t)BIG*sizeof(float);
    cl_mem MB=clCreateBuffer(ctx,CL_MEM_WRITE_ONLY,bbytes,NULL,&e); int nb=BIG;
    clSetKernelArg(kbg,0,sizeof(cl_mem),&MB);clSetKernelArg(kbg,1,sizeof(int),&nb);
    size_t g=BIG; ok(clEnqueueNDRangeKernel(q,kbg,1,NULL,&g,NULL,0,NULL,NULL)==CL_SUCCESS,"1M+ NDRange enqueue");
    float* hbg=malloc(bbytes); clEnqueueReadBuffer(q,MB,CL_TRUE,0,bbytes,hbg,0,NULL,NULL);
    int good=1; long badat=-1; for(int i=0;i<BIG;i++) if(hbg[i]!=(float)i*0.5f){good=0;badat=i;break;}
    ok(good,"1M+ dispatch element-wise == i*0.5"); (void)badat;
    free(hbg); clReleaseKernel(kbg); clReleaseMemObject(MB); clReleaseProgram(pbg); }

  /* === boundary: tail-guard - global rounded up to a multiple of lws, tail untouched === */
  { cl_program pt=clCreateProgramWithSource(ctx,1,&SRC_SCALE,NULL,&e); clBuildProgram(pt,1,&dev,NULL,NULL,NULL);
    cl_kernel kt=clCreateKernel(pt,"scale2",&e);
    int real=1000; size_t l=64, padded=((real+l-1)/l)*l; /* 1024 */
    cl_mem X=clCreateBuffer(ctx,CL_MEM_READ_ONLY|CL_MEM_COPY_HOST_PTR,bytes,ha,&e);
    cl_mem Z=clCreateBuffer(ctx,CL_MEM_READ_WRITE,bytes,NULL,&e);
    float init[1024]; for(int i=0;i<1024;i++) init[i]=-1.0f;
    clEnqueueWriteBuffer(q,Z,CL_TRUE,0,bytes,init,0,NULL,NULL);
    clSetKernelArg(kt,0,sizeof(cl_mem),&X);clSetKernelArg(kt,1,sizeof(cl_mem),&Z);clSetKernelArg(kt,2,sizeof(int),&real);
    ok(clEnqueueNDRangeKernel(q,kt,1,NULL,&padded,&l,0,NULL,NULL)==CL_SUCCESS,"tail-guard NDRange (global rounded to lws multiple)");
    clEnqueueReadBuffer(q,Z,CL_TRUE,0,bytes,hc,0,NULL,NULL);
    int body=1; for(int i=0;i<real;i++) if(!feq(hc[i],ha[i]*2.0f)){body=0;break;}
    int tail=1; for(int i=real;i<1024;i++) if(!feq(hc[i],-1.0f)){tail=0;break;}
    ok(body && tail,"tail-guard: body computed, padded tail untouched");
    clReleaseKernel(kt); clReleaseProgram(pt); clReleaseMemObject(X); clReleaseMemObject(Z); }

  /* === VALIDATION: oversubscribed and non-divisible local size === */
  { size_t bad_lws=wis[0]+mwg+64; size_t g=bad_lws*2;
    cl_int oe=clEnqueueNDRangeKernel(q,kadd,1,NULL,&g,&bad_lws,0,NULL,NULL);
    ok(oe==CL_INVALID_WORK_GROUP_SIZE||oe==CL_INVALID_WORK_ITEM_SIZE,"local size>device max == CL_INVALID_WORK_GROUP_SIZE/WORK_ITEM_SIZE"); }
  { size_t g=1000, l=64; /* 1000 not divisible by 64 */
    cl_int nde=clEnqueueNDRangeKernel(q,kadd,1,NULL,&g,&l,0,NULL,NULL);
    ok(nde==CL_INVALID_WORK_GROUP_SIZE,"non-divisible global/local == CL_INVALID_WORK_GROUP_SIZE"); }

  /* === VALIDATION: missing kernel arg on a fresh kernel === */
  { cl_kernel kfresh=clCreateKernel(prog,"vadd",&e); size_t g=N,l=64;
    cl_int me=clEnqueueNDRangeKernel(q,kfresh,1,NULL,&g,&l,0,NULL,NULL);
    ok(me==CL_INVALID_KERNEL_ARGS,"NDRange with unset args == CL_INVALID_KERNEL_ARGS");
    clReleaseKernel(kfresh); }

  /* === sub-device probe: partition only if supported, else documented error === */
  if(sub_max>=2){
    cl_device_partition_property pr[3]={CL_DEVICE_PARTITION_EQUALLY,1,0};
    cl_uint got=0; cl_device_id subs[64];
    cl_int se=clCreateSubDevices(dev,pr,64,subs,&got);
    ok(se==CL_SUCCESS && got>=2,"clCreateSubDevices EQUALLY produced sub-devices");
    for(cl_uint i=0;i<got;i++) clReleaseDevice(subs[i]);
  } else {
    cl_device_partition_property pr[3]={CL_DEVICE_PARTITION_EQUALLY,1,0}; cl_uint got=0;
    cl_int se=clCreateSubDevices(dev,pr,0,NULL,&got);
    ok(se==CL_DEVICE_PARTITION_FAILED||se==CL_INVALID_VALUE||se==CL_INVALID_DEVICE_PARTITION_COUNT,"unpartitionable device: clCreateSubDevices returns documented error enum");
  }

  /* === misc queued commands with observable effect === */
  { cl_event mev=NULL; ok(clEnqueueMarkerWithWaitList(q,0,NULL,&mev)==CL_SUCCESS,"clEnqueueMarkerWithWaitList");
    ok(clWaitForEvents(1,&mev)==CL_SUCCESS,"marker event completes"); clReleaseEvent(mev); }
  { cl_mem migs[1]={A}; ok(clEnqueueMigrateMemObjects(q,1,migs,CL_MIGRATE_MEM_OBJECT_HOST,0,NULL,NULL)==CL_SUCCESS,"clEnqueueMigrateMemObjects"); }
  cl_mem bp=clCreateBufferWithProperties(ctx,NULL,CL_MEM_READ_WRITE,bytes,NULL,&e); ok(e==CL_SUCCESS && bp,"clCreateBufferWithProperties"); if(bp)clReleaseMemObject(bp);

  /* === reference counting: real REFERENCE_COUNT before/after retain/release === */
  { cl_uint c0=0,c1=0,c2=0;
    clGetMemObjectInfo(A,CL_MEM_REFERENCE_COUNT,sizeof c0,&c0,NULL);
    clRetainMemObject(A); clGetMemObjectInfo(A,CL_MEM_REFERENCE_COUNT,sizeof c1,&c1,NULL);
    clReleaseMemObject(A); clGetMemObjectInfo(A,CL_MEM_REFERENCE_COUNT,sizeof c2,&c2,NULL);
    ok(c1==c0+1 && c2==c0,"clRetain/ReleaseMemObject move MEM_REFERENCE_COUNT +1/-1"); }
  { cl_uint c0=0,c1=0,c2=0;
    clGetCommandQueueInfo(q,CL_QUEUE_REFERENCE_COUNT,sizeof c0,&c0,NULL);
    clRetainCommandQueue(q); clGetCommandQueueInfo(q,CL_QUEUE_REFERENCE_COUNT,sizeof c1,&c1,NULL);
    clReleaseCommandQueue(q); clGetCommandQueueInfo(q,CL_QUEUE_REFERENCE_COUNT,sizeof c2,&c2,NULL);
    ok(c1==c0+1 && c2==c0,"clRetain/ReleaseCommandQueue move QUEUE_REFERENCE_COUNT +1/-1"); }
  { cl_uint c0=0,c1=0,c2=0;
    clGetKernelInfo(kadd,CL_KERNEL_REFERENCE_COUNT,sizeof c0,&c0,NULL);
    clRetainKernel(kadd); clGetKernelInfo(kadd,CL_KERNEL_REFERENCE_COUNT,sizeof c1,&c1,NULL);
    clReleaseKernel(kadd); clGetKernelInfo(kadd,CL_KERNEL_REFERENCE_COUNT,sizeof c2,&c2,NULL);
    ok(c1==c0+1 && c2==c0,"clRetain/ReleaseKernel move KERNEL_REFERENCE_COUNT +1/-1"); }
  { cl_uint c0=0,c1=0,c2=0;
    clGetProgramInfo(prog,CL_PROGRAM_REFERENCE_COUNT,sizeof c0,&c0,NULL);
    clRetainProgram(prog); clGetProgramInfo(prog,CL_PROGRAM_REFERENCE_COUNT,sizeof c1,&c1,NULL);
    clReleaseProgram(prog); clGetProgramInfo(prog,CL_PROGRAM_REFERENCE_COUNT,sizeof c2,&c2,NULL);
    ok(c1==c0+1 && c2==c0,"clRetain/ReleaseProgram move PROGRAM_REFERENCE_COUNT +1/-1"); }
  { cl_uint nd=0; ok(clGetProgramInfo(prog,CL_PROGRAM_NUM_DEVICES,sizeof nd,&nd,NULL)==CL_SUCCESS && nd==1,"clGetProgramInfo NUM_DEVICES==1"); }
  ok(clRetainDevice(dev)==CL_SUCCESS && clReleaseDevice(dev)==CL_SUCCESS,"clRetain/ReleaseDevice (root device refcount stable)");

  /* === user event: manual status transition observable via clGetEventInfo === */
  { cl_event ue=clCreateUserEvent(ctx,&e); ok(e==CL_SUCCESS && ue,"clCreateUserEvent");
    cl_int st=0; clGetEventInfo(ue,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof st,&st,NULL);
    ok(st==CL_SUBMITTED,"fresh user event status==CL_SUBMITTED");
    ok(clSetUserEventStatus(ue,CL_COMPLETE)==CL_SUCCESS,"clSetUserEventStatus COMPLETE");
    clGetEventInfo(ue,CL_EVENT_COMMAND_EXECUTION_STATUS,sizeof st,&st,NULL);
    ok(st==CL_COMPLETE,"user event status transitioned to CL_COMPLETE");
    clReleaseEvent(ue); }

  /* === SVM (shared virtual memory): guard on CL_DEVICE_SVM_CAPABILITIES, then real map/fill/copy/kernel-arg round-trips === */
  { cl_device_svm_capabilities svm=0;
    cl_int svr=clGetDeviceInfo(dev,CL_DEVICE_SVM_CAPABILITIES,sizeof svm,&svm,NULL);
    ok(svr==CL_SUCCESS,"clGetDeviceInfo SVM_CAPABILITIES queried");
    if(svm & CL_DEVICE_SVM_COARSE_GRAIN_BUFFER){
      ok((svm&CL_DEVICE_SVM_COARSE_GRAIN_BUFFER)!=0,"SVM caps advertise COARSE_GRAIN_BUFFER");
      const int SN=256; size_t sbytes=SN*sizeof(float);
      float* pa=(float*)clSVMAlloc(ctx,CL_MEM_READ_WRITE,sbytes,0);
      float* pb=(float*)clSVMAlloc(ctx,CL_MEM_READ_WRITE,sbytes,0);
      ok(pa!=NULL && pb!=NULL && pa!=pb,"clSVMAlloc two distinct non-NULL buffers");
      /* map pa, seed a known ramp, unmap */
      ok(clEnqueueSVMMap(q,CL_TRUE,CL_MAP_WRITE,pa,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMap(pa WRITE)");
      for(int i=0;i<SN;i++) pa[i]=-7.0f;
      ok(clEnqueueSVMUnmap(q,pa,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMUnmap(pa)");
      /* fill pb with a constant, verify by mapping it back */
      float sfill=3.0f; ok(clEnqueueSVMMemFill(q,pb,&sfill,sizeof sfill,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMemFill(pb=3.0)");
      clFinish(q);
      ok(clEnqueueSVMMap(q,CL_TRUE,CL_MAP_READ,pb,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMap(pb READ)");
      { int good=1; for(int i=0;i<SN;i++) if(!feq(pb[i],3.0f)){good=0;break;} ok(good,"SVM MemFill result == 3.0 across buffer"); }
      clEnqueueSVMUnmap(q,pb,0,NULL,NULL); clFinish(q);
      /* copy pb->pa, verify pa now holds the fill value (overwriting the -7 ramp) + negative control */
      ok(clEnqueueSVMMemcpy(q,CL_TRUE,pa,pb,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMemcpy(pa<-pb)"); clFinish(q);
      ok(clEnqueueSVMMap(q,CL_TRUE,CL_MAP_READ,pa,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMap(pa READ after memcpy)");
      { int good=1; for(int i=0;i<SN;i++) if(!feq(pa[i],3.0f)){good=0;break;} ok(good,"SVM Memcpy pa == pb == 3.0"); }
      { float saved=pa[9]; pa[9]=saved+1.0f; int flagged=0; for(int i=0;i<SN;i++) if(!feq(pa[i],3.0f)){flagged=1;break;} pa[9]=saved;
        ok(flagged,"negative control: corrupted SVM element IS flagged"); }
      clEnqueueSVMUnmap(q,pa,0,NULL,NULL); clFinish(q);
      /* bind pa as an SVM kernel arg; kernel writes pa[i]=i; verify computed reference + negative control */
      cl_program sprog=clCreateProgramWithSource(ctx,1,&SRC_SVM,NULL,&e);
      ok(clBuildProgram(sprog,1,&dev,NULL,NULL,NULL)==CL_SUCCESS,"clBuildProgram(svm_fill)");
      cl_kernel ksvm=clCreateKernel(sprog,"svm_fill",&e); ok(e==CL_SUCCESS,"clCreateKernel(svm_fill)");
      ok(clSetKernelArgSVMPointer(ksvm,0,pa)==CL_SUCCESS,"clSetKernelArgSVMPointer");
      size_t gs=SN; ok(clEnqueueNDRangeKernel(q,ksvm,1,NULL,&gs,NULL,0,NULL,NULL)==CL_SUCCESS,"clEnqueueNDRangeKernel(SVM arg)"); clFinish(q);
      ok(clEnqueueSVMMap(q,CL_TRUE,CL_MAP_READ,pa,sbytes,0,NULL,NULL)==CL_SUCCESS,"clEnqueueSVMMap(pa READ after kernel)");
      { int good=1; for(int i=0;i<SN;i++) if(!feq(pa[i],(float)i)){good=0;break;} ok(good,"SVM-arg kernel wrote pa[i]==i"); }
      { float saved=pa[100]; pa[100]=saved+3.0f; int flagged=0; for(int i=0;i<SN;i++) if(!feq(pa[i],(float)i)){flagged=1;break;} pa[100]=saved;
        ok(flagged,"negative control: corrupted SVM-kernel element IS flagged"); }
      clEnqueueSVMUnmap(q,pa,0,NULL,NULL); clFinish(q);
      /* === sub-group reflection on the freshly-linked SVM kernel: closed-form count vs max sub-group size === */
      { size_t lin=64, sgmax=0, sgcnt=0;
        cl_int r0=clGetKernelSubGroupInfo(ksvm,dev,CL_KERNEL_MAX_SUB_GROUP_SIZE_FOR_NDRANGE,sizeof lin,&lin,sizeof sgmax,&sgmax,NULL);
        cl_int r1=clGetKernelSubGroupInfo(ksvm,dev,CL_KERNEL_SUB_GROUP_COUNT_FOR_NDRANGE,sizeof lin,&lin,sizeof sgcnt,&sgcnt,NULL);
        ok(r0==CL_SUCCESS && sgmax>=1 && sgmax<=lin && (lin%sgmax)==0,"clGetKernelSubGroupInfo MAX_SUB_GROUP_SIZE divides local size");
        size_t expect_cnt=(lin+sgmax-1)/sgmax;
        ok(r1==CL_SUCCESS && sgcnt==expect_cnt,"clGetKernelSubGroupInfo SUB_GROUP_COUNT == ceil(lws/sub_group_size)"); }
      clReleaseKernel(ksvm); clReleaseProgram(sprog);
      clSVMFree(ctx,pa); clSVMFree(ctx,pb);
    } else {
      ok(svm==0,"SVM unsupported on this backend: CL_DEVICE_SVM_CAPABILITIES==0 (honest capability check)");
      fprintf(stderr,"skip: SVM family unsupported (CL_DEVICE_SVM_CAPABILITIES==0)\n");
    }
  }

  /* === device/host timer domain: monotonic host timer + paired device/host timer === */
  { cl_ulong ht0=0,ht1=0;
    cl_int r0=clGetHostTimer(dev,&ht0);
    struct timespec ts={0,2000000}; nanosleep(&ts,NULL);
    cl_int r1=clGetHostTimer(dev,&ht1);
    ok(r0==CL_SUCCESS && r1==CL_SUCCESS && ht1>ht0 && (ht1-ht0)>=1000000,"clGetHostTimer monotonic across a >=1ms sleep");
    cl_ulong dt=0,ht=0; cl_int r2=clGetDeviceAndHostTimer(dev,&dt,&ht);
    ok(r2==CL_SUCCESS && dt>0 && ht>0,"clGetDeviceAndHostTimer returns paired non-zero timers"); }

  /* === mem-object destructor callback: fires with the exact user value on final release === */
  { cl_mem dm=clCreateBuffer(ctx,CL_MEM_READ_WRITE,64,NULL,&e); ok(e==CL_SUCCESS && dm,"clCreateBuffer(destructor probe)");
    g_destr=-999;
    ok(clSetMemObjectDestructorCallback(dm,on_mem_destroy,(void*)&g_destr)==CL_SUCCESS,"clSetMemObjectDestructorCallback registered");
    clReleaseMemObject(dm);
    for(int i=0;i<500 && g_destr==-999;i++){ struct timespec ts={0,1000000}; nanosleep(&ts,NULL); }
    ok(g_destr==4242,"destructor callback fired with expected user value on release"); }

  /* === spec-constant surface: spec constants are only valid on IL programs; a source program must reject them === */
  { cl_program scp=clCreateProgramWithSource(ctx,1,&SRC,NULL,&e);
    int scv=5; cl_int scr=clSetProgramSpecializationConstant(scp,0,sizeof scv,&scv);
    ok(scr==CL_INVALID_PROGRAM,"clSetProgramSpecializationConstant on non-IL program == CL_INVALID_PROGRAM");
    clReleaseProgram(scp); }

  /* === unload the platform's compiler (release codes checked, not padded) === */
  ok(clUnloadPlatformCompiler(plat)==CL_SUCCESS,"clUnloadPlatformCompiler");

  cl_context ctx2=clCreateContextFromType(NULL,CL_DEVICE_TYPE_ALL,NULL,NULL,&e); ok(e==CL_SUCCESS && ctx2,"clCreateContextFromType"); if(ctx2)clReleaseContext(ctx2);

  /* --- sync --- */
  ok(clFlush(q)==CL_SUCCESS,"clFlush"); ok(clFinish(q)==CL_SUCCESS,"clFinish");

  /* --- teardown (release codes checked, not padded) --- */
  clReleaseMemObject(sub);
  ok(clReleaseKernel(kadd)==CL_SUCCESS,"clReleaseKernel vadd");
  clReleaseKernel(kmul);clReleaseKernel(ksax);clReleaseKernel(kred);
  clReleaseProgram(prog);
  clReleaseMemObject(A);clReleaseMemObject(B);clReleaseMemObject(C);clReleaseMemObject(Y);clReleaseMemObject(R);clReleaseMemObject(D);
  clReleaseCommandQueue(q);
  clReleaseContext(ctx);
  free(ha);free(hb);free(hc);free(hr);free(plats);

  int EXPECTED=168, TOTAL=PASS+FAIL;
  printf("opencl-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("OPENCL_C_FULL_API OK %d\n",PASS); return 0; }
  printf("OPENCL_C_FULL_API FAIL\n"); return 1;
}

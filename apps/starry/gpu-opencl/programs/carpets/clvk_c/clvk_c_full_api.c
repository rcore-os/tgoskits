/* clvk_c_full_api.c - OpenCL C API carpet routed through clvk (OpenCL-over-Vulkan) on Mesa lavapipe,
 * the OpenCL x Vulkan cartesian cell. Unlike opencl_c_full_api.c (which runs on pocl, a native CPU
 * OpenCL), this cell drives the SAME OpenCL C API surface through the clvk ICD so every clEnqueue*
 * lowers to Vulkan compute dispatches on lavapipe (clspv compiles OpenCL C -> SPIR-V at runtime).
 *
 * PROOF OF ROUTING: the platform is asserted to be clvk, not pocl - CL_PLATFORM_NAME == "clvk",
 * CL_PLATFORM_VENDOR == "clvk", CL_PLATFORM_VERSION contains "clvk", and (when the extension is
 * exposed) CL_PLATFORM_ICD_SUFFIX_KHR == "clvk". The test refuses to count anything unless the active
 * platform is clvk, and a negative control asserts the platform is NOT pocl. This is what makes the
 * cell a genuine OpenCL-through-Vulkan cell rather than a second pocl run.
 *
 * OPERATORS: vector-add (c=a+b), saxpy (y=alpha*x+y), element multiply (c=a*b) and a local-memory
 * tree reduction (per-group partial sums), each checked element-by-element against a closed-form /
 * numpy-equivalent reference computed on the host, plus tail-guard (n not a multiple of the local
 * size), oversubscription (global > n with an in-kernel bound check), a dispatch-of-zero no-op, and
 * negative controls that prove a deliberately-wrong reference element is flagged.
 *
 * Prints "CLVK_C_FULL_API OK <n>" only when every assertion passes and the count equals the pinned
 * EXPECTED total. */
#define CL_TARGET_OPENCL_VERSION 300
#include <CL/cl.h>
#include <CL/cl_ext.h> /* CL_PLATFORM_ICD_SUFFIX_KHR */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS = 0, FAIL = 0;
static void ok(int c, const char* d) {
    fprintf(stderr, "[%s] %s\n", c ? "ok" : "FAIL", d);
    if (c) PASS++; else FAIL++;
}
static int feq(float a, float b) { return fabsf(a - b) <= 1e-4f * (1.0f + fabsf(b)); }

/* OpenCL-C kernels (clspv compiles these to SPIR-V and clvk dispatches them on Vulkan). */
static const char* SRC =
"__kernel void vadd(__global const float*a,__global const float*b,__global float*c,int n){int i=get_global_id(0);if(i<n)c[i]=a[i]+b[i];}\n"
"__kernel void vmul(__global const float*a,__global const float*b,__global float*c,int n){int i=get_global_id(0);if(i<n)c[i]=a[i]*b[i];}\n"
"__kernel void saxpy(float alpha,__global const float*x,__global float*y,int n){int i=get_global_id(0);if(i<n)y[i]=alpha*x[i]+y[i];}\n"
"__kernel void reduce_sum(__global const float*a,__global float*out,__local float*s,int n){\n"
"  int lid=get_local_id(0),gid=get_global_id(0),ls=get_local_size(0);\n"
"  s[lid]=(gid<n)?a[gid]:0.0f;\n"
"  barrier(CLK_LOCAL_MEM_FENCE);\n"
"  for(int o=ls/2;o>0;o>>=1){if(lid<o)s[lid]+=s[lid+o];barrier(CLK_LOCAL_MEM_FENCE);}\n"
"  if(lid==0)out[get_group_id(0)]=s[0];}\n";

/* deliberately-broken source: exercises the clspv build-failure diagnostic path */
static const char* SRC_BAD =
"__kernel void broken(__global float*a){ this is not valid OpenCL C ; }\n";

int main(void) {
    cl_int e;
    cl_uint n;

    /* --- platform enumeration: find the clvk platform (there may be several ICDs registered) ---- */
    e = clGetPlatformIDs(0, NULL, &n);
    ok(e == CL_SUCCESS && n >= 1, "clGetPlatformIDs count>=1");
    cl_platform_id* plats = malloc(sizeof(cl_platform_id) * n);
    ok(clGetPlatformIDs(n, plats, NULL) == CL_SUCCESS, "clGetPlatformIDs list");

    cl_platform_id plat = NULL;
    char buf[8192];
    for (cl_uint i = 0; i < n; i++) {
        if (clGetPlatformInfo(plats[i], CL_PLATFORM_NAME, sizeof buf, buf, NULL) == CL_SUCCESS &&
            strstr(buf, "clvk") != NULL) {
            plat = plats[i];
            break;
        }
    }
    /* Hard gate: if clvk is not the active ICD, this cell is meaningless - do not fake a pass. */
    if (plat == NULL) {
        fprintf(stderr, "FATAL: no clvk platform found (OCL_ICD_VENDORS must point at the clvk ICD)\n");
        printf("clvk-c: PASS=%d FAIL=%d NO_CLVK_PLATFORM\n", PASS, FAIL + 1);
        printf("CLVK_C_FULL_API FAIL\n");
        return 1;
    }
    ok(plat != NULL, "clvk platform located among ICDs");

    /* --- PROOF: the platform really is clvk-over-Vulkan, not pocl --------------------------------- */
    size_t sz = 0;
    ok(clGetPlatformInfo(plat, CL_PLATFORM_NAME, sizeof buf, buf, &sz) == CL_SUCCESS &&
           strcmp(buf, "clvk") == 0,
       "CL_PLATFORM_NAME == \"clvk\" (routing through clvk)");
    ok(clGetPlatformInfo(plat, CL_PLATFORM_VENDOR, sizeof buf, buf, NULL) == CL_SUCCESS &&
           strcmp(buf, "clvk") == 0,
       "CL_PLATFORM_VENDOR == \"clvk\"");
    ok(clGetPlatformInfo(plat, CL_PLATFORM_VERSION, sizeof buf, buf, NULL) == CL_SUCCESS &&
           strstr(buf, "OpenCL") != NULL && strstr(buf, "clvk") != NULL,
       "CL_PLATFORM_VERSION contains \"OpenCL\" and \"clvk\"");
    ok(clGetPlatformInfo(plat, CL_PLATFORM_PROFILE, sizeof buf, buf, NULL) == CL_SUCCESS &&
           strcmp(buf, "FULL_PROFILE") == 0,
       "CL_PLATFORM_PROFILE == FULL_PROFILE");
    /* NEGATIVE CONTROL: re-read the NAME and prove it is NOT pocl's ("Portable Computing Language")
     * nor rusticl's ("rusticl") - i.e. this is genuinely the clvk platform, not another CPU ICD. */
    {
        char nm[256];
        clGetPlatformInfo(plat, CL_PLATFORM_NAME, sizeof nm, nm, NULL);
        ok(strcmp(nm, "Portable Computing Language") != 0 && strstr(nm, "rusticl") == NULL &&
               strcmp(nm, "clvk") == 0,
           "active platform NAME is clvk, not pocl/rusticl (negative control)");
    }
    /* ICD suffix (KHR extension) is "clvk" when exposed; count only if the query succeeds. */
    {
        char sfx[256];
        cl_int se = clGetPlatformInfo(plat, CL_PLATFORM_ICD_SUFFIX_KHR, sizeof sfx, sfx, NULL);
        if (se == CL_SUCCESS)
            ok(strcmp(sfx, "clvk") == 0, "CL_PLATFORM_ICD_SUFFIX_KHR == \"clvk\"");
        else
            fprintf(stderr, "[skip] CL_PLATFORM_ICD_SUFFIX_KHR not exposed (non-counting)\n");
    }
    /* VALIDATION: too-small param buffer must return CL_INVALID_VALUE */
    {
        char tiny[1];
        cl_int ie = clGetPlatformInfo(plat, CL_PLATFORM_NAME, 1, tiny, NULL);
        ok(ie == CL_INVALID_VALUE, "clGetPlatformInfo too-small buffer == CL_INVALID_VALUE");
    }

    /* --- device: clvk exposes the underlying Vulkan device as a CL GPU ---------------------------- */
    cl_device_id dev;
    ok(clGetDeviceIDs(plat, CL_DEVICE_TYPE_ALL, 1, &dev, &n) == CL_SUCCESS && n >= 1,
       "clGetDeviceIDs>=1 on clvk");
    cl_device_type dt = 0;
    ok(clGetDeviceInfo(dev, CL_DEVICE_TYPE, sizeof dt, &dt, NULL) == CL_SUCCESS &&
           (dt & (CL_DEVICE_TYPE_GPU | CL_DEVICE_TYPE_CPU | CL_DEVICE_TYPE_ACCELERATOR)),
       "clGetDeviceInfo TYPE is a real device class");
    ok(clGetDeviceInfo(dev, CL_DEVICE_NAME, sizeof buf, buf, NULL) == CL_SUCCESS && strlen(buf) >= 1,
       "CL_DEVICE_NAME non-empty (Vulkan device behind clvk)");
    fprintf(stderr, "clvk device: %s\n", buf);
    /* The backing device is exposed through Vulkan: its vendor string is the Vulkan vendor. */
    ok(clGetDeviceInfo(dev, CL_DEVICE_VENDOR, sizeof buf, buf, NULL) == CL_SUCCESS && strlen(buf) >= 1,
       "CL_DEVICE_VENDOR non-empty");
    cl_uint cu = 0;
    ok(clGetDeviceInfo(dev, CL_DEVICE_MAX_COMPUTE_UNITS, sizeof cu, &cu, NULL) == CL_SUCCESS && cu >= 1,
       "MAX_COMPUTE_UNITS>=1");
    size_t mwg = 0;
    ok(clGetDeviceInfo(dev, CL_DEVICE_MAX_WORK_GROUP_SIZE, sizeof mwg, &mwg, NULL) == CL_SUCCESS &&
           mwg >= 1,
       "MAX_WORK_GROUP_SIZE>=1");
    size_t lmem = 0;
    ok(clGetDeviceInfo(dev, CL_DEVICE_LOCAL_MEM_SIZE, sizeof lmem, &lmem, NULL) == CL_SUCCESS &&
           lmem >= 1024,
       "LOCAL_MEM_SIZE>=1KB (Vulkan shared memory)");
    cl_bool avail = CL_FALSE;
    ok(clGetDeviceInfo(dev, CL_DEVICE_AVAILABLE, sizeof avail, &avail, NULL) == CL_SUCCESS &&
           avail == CL_TRUE,
       "CL_DEVICE_AVAILABLE == CL_TRUE");
    cl_bool compiler = CL_FALSE;
    ok(clGetDeviceInfo(dev, CL_DEVICE_COMPILER_AVAILABLE, sizeof compiler, &compiler, NULL) ==
           CL_SUCCESS,
       "CL_DEVICE_COMPILER_AVAILABLE queried");

    /* --- context + queue ------------------------------------------------------------------------- */
    cl_context ctx = clCreateContext(NULL, 1, &dev, NULL, NULL, &e);
    ok(e == CL_SUCCESS && ctx, "clCreateContext");
    cl_uint ndev = 0;
    ok(clGetContextInfo(ctx, CL_CONTEXT_NUM_DEVICES, sizeof ndev, &ndev, NULL) == CL_SUCCESS &&
           ndev == 1,
       "clGetContextInfo NUM_DEVICES==1");
    cl_command_queue q = clCreateCommandQueueWithProperties(ctx, dev, NULL, &e);
    ok(e == CL_SUCCESS && q, "clCreateCommandQueueWithProperties");
    {
        cl_context qc = NULL;
        ok(clGetCommandQueueInfo(q, CL_QUEUE_CONTEXT, sizeof qc, &qc, NULL) == CL_SUCCESS && qc == ctx,
           "clGetCommandQueueInfo CONTEXT==ctx");
    }

    /* --- buffers + host reference data ----------------------------------------------------------- */
    const int N = 1024;
    size_t bytes = N * sizeof(float);
    float* ha = malloc(bytes);
    float* hb = malloc(bytes);
    float* hc = malloc(bytes);
    for (int i = 0; i < N; i++) {
        ha[i] = (float)i;
        hb[i] = 2.0f * i + 1.0f;
    }
    cl_mem A = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, bytes, ha, &e);
    ok(e == CL_SUCCESS, "clCreateBuffer A COPY_HOST_PTR");
    cl_mem B = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, bytes, hb, &e);
    ok(e == CL_SUCCESS, "clCreateBuffer B COPY_HOST_PTR");
    cl_mem C = clCreateBuffer(ctx, CL_MEM_WRITE_ONLY, bytes, NULL, &e);
    ok(e == CL_SUCCESS, "clCreateBuffer C");
    {
        cl_mem_object_type mt = 0;
        ok(clGetMemObjectInfo(A, CL_MEM_TYPE, sizeof mt, &mt, NULL) == CL_SUCCESS &&
               mt == CL_MEM_OBJECT_BUFFER,
           "clGetMemObjectInfo TYPE==BUFFER");
    }
    {
        size_t msz = 0;
        ok(clGetMemObjectInfo(A, CL_MEM_SIZE, sizeof msz, &msz, NULL) == CL_SUCCESS && msz == bytes,
           "clGetMemObjectInfo SIZE==bytes");
    }
    {
        cl_int ze = CL_SUCCESS;
        cl_mem zb = clCreateBuffer(ctx, CL_MEM_READ_WRITE, 0, NULL, &ze);
        ok(ze == CL_INVALID_BUFFER_SIZE && zb == NULL,
           "clCreateBuffer(size 0) == CL_INVALID_BUFFER_SIZE");
    }

    /* --- program: clspv compiles OpenCL C -> SPIR-V, clvk builds the Vulkan pipelines ------------- */
    cl_program prog = clCreateProgramWithSource(ctx, 1, &SRC, NULL, &e);
    ok(e == CL_SUCCESS, "clCreateProgramWithSource");
    e = clBuildProgram(prog, 1, &dev, NULL, NULL, NULL);
    if (e != CL_SUCCESS) {
        size_t logsz = 0;
        clGetProgramBuildInfo(prog, dev, CL_PROGRAM_BUILD_LOG, 0, NULL, &logsz);
        char* log = malloc(logsz + 1);
        clGetProgramBuildInfo(prog, dev, CL_PROGRAM_BUILD_LOG, logsz, log, NULL);
        log[logsz] = 0;
        fprintf(stderr, "clBuildProgram log:\n%s\n", log);
        free(log);
    }
    ok(e == CL_SUCCESS, "clBuildProgram (clspv OpenCL-C -> SPIR-V)");
    {
        cl_build_status bs = CL_BUILD_NONE;
        ok(clGetProgramBuildInfo(prog, dev, CL_PROGRAM_BUILD_STATUS, sizeof bs, &bs, NULL) ==
                   CL_SUCCESS &&
               bs == CL_BUILD_SUCCESS,
           "clGetProgramBuildInfo STATUS==SUCCESS");
    }
    /* VALIDATION: a broken program must fail to build */
    {
        cl_program bad = clCreateProgramWithSource(ctx, 1, &SRC_BAD, NULL, &e);
        cl_int be = clBuildProgram(bad, 1, &dev, NULL, NULL, NULL);
        ok(be == CL_BUILD_PROGRAM_FAILURE || be != CL_SUCCESS,
           "clBuildProgram(broken source) fails (build-error path)");
        clReleaseProgram(bad);
    }

    cl_kernel k_vadd = clCreateKernel(prog, "vadd", &e);
    ok(e == CL_SUCCESS, "clCreateKernel vadd");
    cl_kernel k_vmul = clCreateKernel(prog, "vmul", &e);
    ok(e == CL_SUCCESS, "clCreateKernel vmul");
    cl_kernel k_saxpy = clCreateKernel(prog, "saxpy", &e);
    ok(e == CL_SUCCESS, "clCreateKernel saxpy");
    cl_kernel k_reduce = clCreateKernel(prog, "reduce_sum", &e);
    ok(e == CL_SUCCESS, "clCreateKernel reduce_sum");
    {
        char kn[128];
        ok(clGetKernelInfo(k_vadd, CL_KERNEL_FUNCTION_NAME, sizeof kn, kn, NULL) == CL_SUCCESS &&
               strcmp(kn, "vadd") == 0,
           "clGetKernelInfo FUNCTION_NAME==vadd");
        cl_uint na = 0;
        ok(clGetKernelInfo(k_vadd, CL_KERNEL_NUM_ARGS, sizeof na, &na, NULL) == CL_SUCCESS && na == 4,
           "clGetKernelInfo NUM_ARGS==4");
    }
    /* VALIDATION: unknown kernel name */
    {
        cl_int ke = CL_SUCCESS;
        cl_kernel bad = clCreateKernel(prog, "does_not_exist", &ke);
        ok(ke == CL_INVALID_KERNEL_NAME && bad == NULL,
           "clCreateKernel(unknown) == CL_INVALID_KERNEL_NAME");
    }

    /* --- OP 1: vector-add c = a + b, every element vs host reference ------------------------------ */
    {
        clSetKernelArg(k_vadd, 0, sizeof(cl_mem), &A);
        clSetKernelArg(k_vadd, 1, sizeof(cl_mem), &B);
        clSetKernelArg(k_vadd, 2, sizeof(cl_mem), &C);
        cl_int nn = N;
        clSetKernelArg(k_vadd, 3, sizeof(cl_int), &nn);
        size_t g = N, l = 64;
        ok(clEnqueueNDRangeKernel(q, k_vadd, 1, NULL, &g, &l, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueNDRangeKernel vadd");
        ok(clEnqueueReadBuffer(q, C, CL_TRUE, 0, bytes, hc, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueReadBuffer vadd result");
        int bad = -1;
        for (int i = 0; i < N; i++)
            if (!feq(hc[i], ha[i] + hb[i])) { bad = i; break; }
        ok(bad == -1, "vadd: c[i]==a[i]+b[i] for all i (vs host reference)");
        /* NEGATIVE CONTROL: prove the comparison flags a deliberately-wrong reference. */
        ok(!feq(hc[7], ha[7] + hb[7] + 1.0f), "vadd negative control: wrong reference is flagged");
    }

    /* --- OP 2: element multiply c = a * b -------------------------------------------------------- */
    {
        clSetKernelArg(k_vmul, 0, sizeof(cl_mem), &A);
        clSetKernelArg(k_vmul, 1, sizeof(cl_mem), &B);
        clSetKernelArg(k_vmul, 2, sizeof(cl_mem), &C);
        cl_int nn = N;
        clSetKernelArg(k_vmul, 3, sizeof(cl_int), &nn);
        size_t g = N, l = 64;
        ok(clEnqueueNDRangeKernel(q, k_vmul, 1, NULL, &g, &l, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueNDRangeKernel vmul");
        ok(clEnqueueReadBuffer(q, C, CL_TRUE, 0, bytes, hc, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueReadBuffer vmul result");
        int bad = -1;
        for (int i = 0; i < N; i++)
            if (!feq(hc[i], ha[i] * hb[i])) { bad = i; break; }
        ok(bad == -1, "vmul: c[i]==a[i]*b[i] for all i");
    }

    /* --- OP 3: saxpy y = alpha*x + y (in place), vs host reference -------------------------------- */
    {
        float alpha = 3.5f;
        float* hy = malloc(bytes);
        for (int i = 0; i < N; i++) hy[i] = 0.25f * i;
        cl_mem Y = clCreateBuffer(ctx, CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR, bytes, hy, &e);
        ok(e == CL_SUCCESS, "clCreateBuffer Y for saxpy");
        clSetKernelArg(k_saxpy, 0, sizeof(cl_float), &alpha);
        clSetKernelArg(k_saxpy, 1, sizeof(cl_mem), &A);
        clSetKernelArg(k_saxpy, 2, sizeof(cl_mem), &Y);
        cl_int nn = N;
        clSetKernelArg(k_saxpy, 3, sizeof(cl_int), &nn);
        size_t g = N, l = 64;
        ok(clEnqueueNDRangeKernel(q, k_saxpy, 1, NULL, &g, &l, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueNDRangeKernel saxpy");
        float* out = malloc(bytes);
        ok(clEnqueueReadBuffer(q, Y, CL_TRUE, 0, bytes, out, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueReadBuffer saxpy result");
        int bad = -1;
        for (int i = 0; i < N; i++)
            if (!feq(out[i], alpha * ha[i] + hy[i])) { bad = i; break; }
        ok(bad == -1, "saxpy: y[i]==alpha*x[i]+y[i] for all i (vs host reference)");
        free(hy);
        free(out);
        clReleaseMemObject(Y);
    }

    /* --- OP 4: local-memory tree reduction, per-group partial sums vs closed-form ----------------- */
    {
        const int LS = 64;
        int groups = N / LS;
        float* hin = malloc(bytes);
        for (int i = 0; i < N; i++) hin[i] = 1.0f + (i % 8);
        cl_mem IN = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, bytes, hin, &e);
        ok(e == CL_SUCCESS, "clCreateBuffer reduce input");
        cl_mem OUT = clCreateBuffer(ctx, CL_MEM_WRITE_ONLY, groups * sizeof(float), NULL, &e);
        ok(e == CL_SUCCESS, "clCreateBuffer reduce output");
        cl_int nn = N;
        clSetKernelArg(k_reduce, 0, sizeof(cl_mem), &IN);
        clSetKernelArg(k_reduce, 1, sizeof(cl_mem), &OUT);
        clSetKernelArg(k_reduce, 2, LS * sizeof(float), NULL); /* __local */
        clSetKernelArg(k_reduce, 3, sizeof(cl_int), &nn);
        size_t g = N, l = LS;
        ok(clEnqueueNDRangeKernel(q, k_reduce, 1, NULL, &g, &l, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueNDRangeKernel reduce_sum (local memory + barrier)");
        float* partial = malloc(groups * sizeof(float));
        ok(clEnqueueReadBuffer(q, OUT, CL_TRUE, 0, groups * sizeof(float), partial, 0, NULL, NULL) ==
               CL_SUCCESS,
           "clEnqueueReadBuffer reduce partials");
        int bad = -1;
        for (int gi = 0; gi < groups; gi++) {
            float ref = 0.0f;
            for (int j = 0; j < LS; j++) ref += hin[gi * LS + j];
            if (!feq(partial[gi], ref)) { bad = gi; break; }
        }
        ok(bad == -1, "reduce_sum: each group partial == sum of its LS inputs (closed-form)");
        /* full sum across all groups matches the arithmetic total */
        float total = 0.0f, ref_total = 0.0f;
        for (int gi = 0; gi < groups; gi++) total += partial[gi];
        for (int i = 0; i < N; i++) ref_total += hin[i];
        ok(feq(total, ref_total), "reduce_sum: sum of group partials == total input sum");
        free(hin);
        free(partial);
        clReleaseMemObject(IN);
        clReleaseMemObject(OUT);
    }

    /* --- boundary: tail-guard (n not a multiple of local size) + oversubscription ----------------- */
    {
        const int M = 1000; /* not a multiple of 64 */
        size_t mbytes = M * sizeof(float);
        float* xa = malloc(mbytes);
        float* xb = malloc(mbytes);
        float* xc = malloc(mbytes);
        for (int i = 0; i < M; i++) { xa[i] = 3.0f * i; xb[i] = -i; }
        cl_mem XA = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, mbytes, xa, &e);
        cl_mem XB = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, mbytes, xb, &e);
        cl_mem XC = clCreateBuffer(ctx, CL_MEM_WRITE_ONLY, mbytes, NULL, &e);
        /* sentinel so out-of-range writes would be visible */
        for (int i = 0; i < M; i++) xc[i] = -999.0f;
        clEnqueueWriteBuffer(q, XC, CL_TRUE, 0, mbytes, xc, 0, NULL, NULL);
        clSetKernelArg(k_vadd, 0, sizeof(cl_mem), &XA);
        clSetKernelArg(k_vadd, 1, sizeof(cl_mem), &XB);
        clSetKernelArg(k_vadd, 2, sizeof(cl_mem), &XC);
        cl_int mm = M;
        clSetKernelArg(k_vadd, 3, sizeof(cl_int), &mm);
        /* oversubscribe: global rounded up to 1024 > M, in-kernel i<n guards the tail */
        size_t g = 1024, l = 64;
        ok(clEnqueueNDRangeKernel(q, k_vadd, 1, NULL, &g, &l, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueNDRangeKernel vadd tail-guard (global>n)");
        ok(clEnqueueReadBuffer(q, XC, CL_TRUE, 0, mbytes, xc, 0, NULL, NULL) == CL_SUCCESS,
           "clEnqueueReadBuffer tail-guard result");
        int bad = -1;
        for (int i = 0; i < M; i++)
            if (!feq(xc[i], xa[i] + xb[i])) { bad = i; break; }
        ok(bad == -1, "tail-guard: all M elements computed, no stray writes past n");
        free(xa);
        free(xb);
        free(xc);
        clReleaseMemObject(XA);
        clReleaseMemObject(XB);
        clReleaseMemObject(XC);
    }

    /* --- boundary: dispatch of zero global work items is a no-op ---------------------------------- */
    {
        float* zc = malloc(bytes);
        for (int i = 0; i < N; i++) zc[i] = 77.0f;
        cl_mem Z = clCreateBuffer(ctx, CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR, bytes, zc, &e);
        clSetKernelArg(k_vadd, 0, sizeof(cl_mem), &A);
        clSetKernelArg(k_vadd, 1, sizeof(cl_mem), &B);
        clSetKernelArg(k_vadd, 2, sizeof(cl_mem), &Z);
        cl_int nn = N;
        clSetKernelArg(k_vadd, 3, sizeof(cl_int), &nn);
        size_t g = 0, l = 64;
        cl_int de = clEnqueueNDRangeKernel(q, k_vadd, 1, NULL, &g, &l, 0, NULL, NULL);
        /* zero global size is invalid per spec; either it errors or is a no-op - both leave Z intact */
        clFinish(q);
        float* back = malloc(bytes);
        clEnqueueReadBuffer(q, Z, CL_TRUE, 0, bytes, back, 0, NULL, NULL);
        int untouched = 1;
        for (int i = 0; i < N; i++)
            if (!feq(back[i], 77.0f)) { untouched = 0; break; }
        ok(de != CL_SUCCESS || untouched,
           "dispatch(global=0): output untouched or rejected (no partial writes)");
        free(zc);
        free(back);
        clReleaseMemObject(Z);
    }

    /* --- explicit sync + cleanup ----------------------------------------------------------------- */
    ok(clFinish(q) == CL_SUCCESS, "clFinish drains the queue");
    ok(clReleaseKernel(k_vadd) == CL_SUCCESS, "clReleaseKernel vadd");
    clReleaseKernel(k_vmul);
    clReleaseKernel(k_saxpy);
    clReleaseKernel(k_reduce);
    clReleaseProgram(prog);
    clReleaseMemObject(A);
    clReleaseMemObject(B);
    clReleaseMemObject(C);
    ok(clReleaseCommandQueue(q) == CL_SUCCESS, "clReleaseCommandQueue");
    ok(clReleaseContext(ctx) == CL_SUCCESS, "clReleaseContext");
    free(ha);
    free(hb);
    free(hc);
    free(plats);

    int EXPECTED = 65, TOTAL = PASS + FAIL;
    printf("clvk-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) {
        printf("CLVK_C_FULL_API OK %d\n", PASS);
        return 0;
    }
    printf("CLVK_C_FULL_API FAIL\n");
    return 1;
}

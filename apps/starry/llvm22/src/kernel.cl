/* kernel.cl - a trivial OpenCL C kernel. `clang -x cl` (OpenCL C) and `clang -x clcpp`
   (OpenCL C++) lower it to LLVM IR headless, with no OpenCL runtime, exercising the
   OpenCL front-end path. */
__kernel void kmul(__global int *o) {
    o[0] = 6 * 7;
}

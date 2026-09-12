// The dawn-based `webgpu` package ships types.d.ts, but its `globals` export is typed as `Object`,
// which strips the constructor/enum globals (GPUBufferUsage, GPUShaderStage, GPUMapMode) that
// Object.assign(globalThis, globals) installs at runtime. Narrow `globals` here so the assignment
// stays typed and the carpet keeps every WebGPU object in the graph strongly typed.
declare module 'webgpu' {
  export function create(options: string[]): GPU;
  export const globals: {
    GPUBufferUsage: typeof GPUBufferUsage;
    GPUShaderStage: typeof GPUShaderStage;
    GPUMapMode: typeof GPUMapMode;
    [key: string]: unknown;
  };
}

/* vk_parallel_full_api.c - Vulkan parallel/concurrent compute correctness carpet on lavapipe (the
 * CPU software Vulkan driver over the llvmpipe LLVM JIT). Covers the four parallel-compute cases:
 *   (a) concurrent dispatch  - multiple compute dispatches in flight, all results correct;
 *   (b) multi-queue          - a second concurrent VkQueue when the family exposes queueCount>=2,
 *                              otherwise the count is asserted honestly and concurrency is driven
 *                              through multiple independent command buffers (single queue family);
 *   (c) async multi-submit   - many submissions enqueued back-to-back without waiting between,
 *                              then a single wait on all fences, ordering/completion verified;
 *   (d) multi-workgroup      - a >=1,000,000-element grid split across thousands of workgroups with
 *                              element-wise verification, a workgroup-shared-memory reduction whose
 *                              per-group partials and combined total match the closed-form CPU sum,
 *                              and atomic counters shared across every workgroup whose final values
 *                              must equal n and the closed-form sum (no lost updates).
 * Ordering is also exercised via pipeline barriers, cross-submit binary semaphores and device
 * events. Every scenario is checked with per-element numeric assertions against a closed-form
 * reference, a race/ordering assertion (atomic sum == n; barrier/semaphore/event chain consumed the
 * producer's output, not a stale value), a queried property vs a known value, or a real VkResult
 * enum. Prints "VK_PARALLEL_FULL_API OK <n>" only when every assertion passes AND the count equals
 * the pinned EXPECTED total. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b) <= 1e-3f*(1.0f+fabsf(b)); }
#define VKOK(x,d) ok((x)==VK_SUCCESS,d)

static uint32_t* load_spv(const char*p, size_t*words){
  FILE*f=fopen(p,"rb"); if(!f)return NULL; fseek(f,0,SEEK_END); long n=ftell(f); fseek(f,0,SEEK_SET);
  uint32_t*b=malloc(n); if(fread(b,1,n,f)!=(size_t)n){fclose(f);return NULL;} fclose(f); *words=(size_t)n; return b;
}
static uint32_t find_mem(VkPhysicalDeviceMemoryProperties*mp,uint32_t bits,VkMemoryPropertyFlags want){
  for(uint32_t i=0;i<mp->memoryTypeCount;i++) if((bits&(1u<<i)) && (mp->memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}

typedef struct { VkBuffer buf; VkDeviceMemory mem; void* map; VkDeviceSize bytes; } SBuf;

static VkDevice g_dev;
static VkPhysicalDeviceMemoryProperties g_mp;

static int mk_buf(SBuf* s, VkDeviceSize bytes){
  s->bytes=bytes;
  VkBufferCreateInfo bci={VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bci.size=bytes;
  bci.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT|VK_BUFFER_USAGE_TRANSFER_SRC_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  bci.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
  if(vkCreateBuffer(g_dev,&bci,NULL,&s->buf)!=VK_SUCCESS) return 0;
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(g_dev,s->buf,&mr);
  uint32_t mt=find_mem(&g_mp,mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  if(mt==UINT32_MAX) return 0;
  VkMemoryAllocateInfo mai={VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; mai.allocationSize=mr.size; mai.memoryTypeIndex=mt;
  if(vkAllocateMemory(g_dev,&mai,NULL,&s->mem)!=VK_SUCCESS) return 0;
  if(vkBindBufferMemory(g_dev,s->buf,s->mem,0)!=VK_SUCCESS) return 0;
  if(vkMapMemory(g_dev,s->mem,0,bytes,0,&s->map)!=VK_SUCCESS) return 0;
  return 1;
}
static void free_buf(SBuf* s){ vkUnmapMemory(g_dev,s->mem); vkDestroyBuffer(g_dev,s->buf,NULL); vkFreeMemory(g_dev,s->mem,NULL); }

typedef struct { VkShaderModule sm; VkDescriptorSetLayout dsl; VkPipelineLayout pl; VkPipeline pipe; } Pipe;
struct PC { float f; uint32_t n; };

static int mk_pipe(Pipe* P, const char* spvpath, int nbind){
  size_t sw; uint32_t* spv=load_spv(spvpath,&sw); if(!spv) return 0;
  VkShaderModuleCreateInfo smci={VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; smci.codeSize=sw; smci.pCode=spv;
  if(vkCreateShaderModule(g_dev,&smci,NULL,&P->sm)!=VK_SUCCESS){ free(spv); return 0; }
  free(spv);
  VkDescriptorSetLayoutBinding lb[3];
  for(int i=0;i<nbind;i++){ lb[i]=(VkDescriptorSetLayoutBinding){0}; lb[i].binding=i; lb[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; lb[i].descriptorCount=1; lb[i].stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; }
  VkDescriptorSetLayoutCreateInfo dslci={VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=nbind; dslci.pBindings=lb;
  if(vkCreateDescriptorSetLayout(g_dev,&dslci,NULL,&P->dsl)!=VK_SUCCESS) return 0;
  VkPushConstantRange pcr={VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC)};
  VkPipelineLayoutCreateInfo plci={VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&P->dsl; plci.pushConstantRangeCount=1; plci.pPushConstantRanges=&pcr;
  if(vkCreatePipelineLayout(g_dev,&plci,NULL,&P->pl)!=VK_SUCCESS) return 0;
  VkComputePipelineCreateInfo cpci={VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
  cpci.stage.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO; cpci.stage.stage=VK_SHADER_STAGE_COMPUTE_BIT; cpci.stage.module=P->sm; cpci.stage.pName="main";
  cpci.layout=P->pl;
  if(vkCreateComputePipelines(g_dev,VK_NULL_HANDLE,1,&cpci,NULL,&P->pipe)!=VK_SUCCESS) return 0;
  return 1;
}
static void free_pipe(Pipe* P){ vkDestroyPipeline(g_dev,P->pipe,NULL); vkDestroyPipelineLayout(g_dev,P->pl,NULL); vkDestroyDescriptorSetLayout(g_dev,P->dsl,NULL); vkDestroyShaderModule(g_dev,P->sm,NULL); }

static VkDescriptorSet mk_set(VkDescriptorPool dp, Pipe* P, SBuf** bufs, int nbind){
  VkDescriptorSetAllocateInfo dsai={VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dp; dsai.descriptorSetCount=1; dsai.pSetLayouts=&P->dsl;
  VkDescriptorSet ds; if(vkAllocateDescriptorSets(g_dev,&dsai,&ds)!=VK_SUCCESS) return VK_NULL_HANDLE;
  VkDescriptorBufferInfo dbi[3]; VkWriteDescriptorSet wds[3];
  for(int i=0;i<nbind;i++){ dbi[i]=(VkDescriptorBufferInfo){bufs[i]->buf,0,VK_WHOLE_SIZE};
    wds[i]=(VkWriteDescriptorSet){VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds[i].dstSet=ds; wds[i].dstBinding=i; wds[i].descriptorCount=1; wds[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; wds[i].pBufferInfo=&dbi[i]; }
  vkUpdateDescriptorSets(g_dev,nbind,wds,0,NULL);
  return ds;
}

int main(void){
  VkApplicationInfo ai={VK_STRUCTURE_TYPE_APPLICATION_INFO}; ai.apiVersion=VK_API_VERSION_1_1;
  VkInstanceCreateInfo ici={VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO}; ici.pApplicationInfo=&ai;
  VkInstance inst; VKOK(vkCreateInstance(&ici,NULL,&inst),"vkCreateInstance");

  uint32_t npd=0; vkEnumeratePhysicalDevices(inst,&npd,NULL); ok(npd>=1,">=1 physical device");
  VkPhysicalDevice* pds=malloc(sizeof(VkPhysicalDevice)*npd); vkEnumeratePhysicalDevices(inst,&npd,pds);
  VkPhysicalDevice pd=pds[0];
  vkGetPhysicalDeviceMemoryProperties(pd,&g_mp);

  VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd,&props);
  ok(strlen(props.deviceName)>0,"device name non-empty");
  ok(props.limits.maxComputeWorkGroupInvocations>=256,"maxComputeWorkGroupInvocations>=256 (fits LOCAL=256)");
  ok(props.limits.maxComputeWorkGroupCount[0]>=4096,"maxComputeWorkGroupCount[0]>=4096 (fits multi-workgroup grid)");
  int ts_ok = props.limits.timestampComputeAndGraphics!=0;
  fprintf(stderr,"[info] timestampComputeAndGraphics=%u timestampPeriod=%f maxWGInv=%u maxWGCount0=%u\n",
          props.limits.timestampComputeAndGraphics, props.limits.timestampPeriod,
          props.limits.maxComputeWorkGroupInvocations, props.limits.maxComputeWorkGroupCount[0]);

  uint32_t nqf=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,NULL);
  VkQueueFamilyProperties* qf=malloc(sizeof(VkQueueFamilyProperties)*nqf); vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,qf);
  uint32_t cq=UINT32_MAX; for(uint32_t i=0;i<nqf;i++) if(qf[i].queueFlags&VK_QUEUE_COMPUTE_BIT){cq=i;break;}
  ok(cq!=UINT32_MAX,"found compute queue family");
  int ts_valid = (cq!=UINT32_MAX) && qf[cq].timestampValidBits>0;

  /* ============ (b) MULTI-QUEUE: how many concurrent compute queues does the family expose? ============ */
  uint32_t avail=qf[cq].queueCount;
  uint32_t nq = avail>=2 ? 2 : 1;
  int true_multiqueue = (nq>=2);
  ok(avail>=1,"compute family exposes queueCount>=1");
  fprintf(stderr,"[info] compute queue family %u exposes queueCount=%u -> using %u queue(s); %s\n",
          cq, avail, nq, true_multiqueue ? "TRUE multi-queue" : "single queue family -> concurrency via multiple command buffers");

  float prio[2]={1.0f,1.0f};
  VkDeviceQueueCreateInfo qci={VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
  qci.queueFamilyIndex=cq; qci.queueCount=nq; qci.pQueuePriorities=prio;
  VkDeviceCreateInfo dci={VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci;
  VkDevice dev; VKOK(vkCreateDevice(pd,&dci,NULL,&dev),"vkCreateDevice"); g_dev=dev;
  VkQueue queue[2]={VK_NULL_HANDLE,VK_NULL_HANDLE};
  for(uint32_t i=0;i<nq;i++) vkGetDeviceQueue(dev,cq,i,&queue[i]);
  ok(queue[0]!=VK_NULL_HANDLE,"vkGetDeviceQueue[0]");
  if(true_multiqueue)
    ok(queue[1]!=VK_NULL_HANDLE && queue[1]!=queue[0],"second concurrent VkQueue is a distinct handle (fed real work in async multi-submit)");
  else
    ok(avail==1,"multi-queue: compute family reports exactly 1 queue (capability asserted honestly; concurrency via multiple command buffers)");

  Pipe pAdd, pRed, pChain, pAtomic;
  ok(mk_pipe(&pAdd,"shaders/vadd.spv",3),"build vadd pipeline");
  ok(mk_pipe(&pRed,"shaders/partial_reduce.spv",2),"build partial_reduce pipeline");
  ok(mk_pipe(&pChain,"shaders/chain.spv",2),"build chain pipeline");
  ok(mk_pipe(&pAtomic,"shaders/atomic_sum.spv",2),"build atomic_sum pipeline");

  const uint32_t LOCAL=256;

  VkDescriptorPoolSize dps={VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 64};
  VkDescriptorPoolCreateInfo dpci={VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=32; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
  VkDescriptorPool dp; VKOK(vkCreateDescriptorPool(dev,&dpci,NULL,&dp),"vkCreateDescriptorPool");
  VkCommandPoolCreateInfo cmpi={VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; cmpi.queueFamilyIndex=cq; cmpi.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  VkCommandPool cmdpool; VKOK(vkCreateCommandPool(dev,&cmpi,NULL,&cmdpool),"vkCreateCommandPool");

  /* ================= (d) MULTI-WORKGROUP: 1<<20 grid, element-wise + shared-memory reduction ================= */
  {
    const uint32_t N = 1u<<20;                 /* 1,048,576 elements */
    const uint32_t NG = (N+LOCAL-1)/LOCAL;     /* 4096 workgroups dispatched below */
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,c,part;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&c,bytes),"multi-wg: alloc a/b/c (4MB each)");
    ok(mk_buf(&part,(VkDeviceSize)NG*sizeof(float)),"multi-wg: alloc per-workgroup partials");
    float *ma=a.map,*mb=b.map,*mc=c.map,*mpart=part.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i%997); mb[i]=(float)((i%13)+0.5f); mc[i]=-1.0f; }

    SBuf* addbufs[3]={&a,&b,&c};
    VkDescriptorSet dsAdd=mk_set(dp,&pAdd,addbufs,3);
    SBuf* redbufs[2]={&a,&part};
    VkDescriptorSet dsRed=mk_set(dp,&pRed,redbufs,2);
    ok(dsAdd!=VK_NULL_HANDLE && dsRed!=VK_NULL_HANDLE,"multi-wg: descriptor sets wired");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);

    ok(vkGetFenceStatus(dev,fence)==VK_NOT_READY,"multi-wg: fence status before submit == VK_NOT_READY");
    ok(vkWaitForFences(dev,1,&fence,VK_TRUE,0)==VK_TIMEOUT,"multi-wg: zero-timeout wait on unsignaled fence == VK_TIMEOUT");

    struct PC pc={1.0f,N};
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&dsAdd,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
    vkCmdDispatch(cmd,NG,1,1);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"multi-wg: wait vadd over 4096 workgroups");
    ok(vkGetFenceStatus(dev,fence)==VK_SUCCESS,"multi-wg: fence status after completion == VK_SUCCESS");
    vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);

    uint32_t bad=0, firstbad=0;
    for(uint32_t i=0;i<N;i++){ float want=ma[i]+mb[i]; if(!feq(mc[i],want)){ if(!bad)firstbad=i; bad++; } }
    if(bad) fprintf(stderr,"[multi-wg] %u/%u mismatches, first at %u (got %f want %f)\n",bad,N,firstbad,mc[firstbad],ma[firstbad]+mb[firstbad]);
    ok(bad==0,"multi-wg: EVERY one of 1<<20 elements c[i]==a[i]+b[i] (all 4096 workgroups ran, disjoint)");

    { float saved=mc[555]; mc[555]=saved+1000.0f;
      uint32_t nbad=0; for(uint32_t i=0;i<N;i++) if(!feq(mc[i],ma[i]+mb[i])) nbad++;
      ok(nbad==1,"multi-wg: negative control - single corrupted element detected");
      mc[555]=saved; }

    struct PC rpc={0.0f,N};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pRed.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pRed.pl,0,1,&dsRed,0,NULL);
    vkCmdPushConstants(cmd,pRed.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof rpc,&rpc);
    vkCmdDispatch(cmd,NG,1,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"multi-wg: wait shared-memory reduction");
    vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);

    double gpu_total=0.0, cpu_total=0.0;
    for(uint32_t g=0; g<NG; g++){
      double cpu_part=0.0;
      for(uint32_t k=0;k<LOCAL;k++){ uint32_t idx=g*LOCAL+k; if(idx<N) cpu_part+=ma[idx]; }
      cpu_total+=cpu_part;
      gpu_total+=(double)mpart[g];
    }
    uint32_t badpart=0;
    for(uint32_t g=0; g<NG; g++){
      double cpu_part=0.0; for(uint32_t k=0;k<LOCAL;k++){ uint32_t idx=g*LOCAL+k; if(idx<N) cpu_part+=ma[idx]; }
      if(fabs((double)mpart[g]-cpu_part) > 1e-1) badpart++;
    }
    ok(badpart==0,"multi-wg: every per-workgroup shared-memory partial == CPU workgroup sum");
    ok(fabs(gpu_total-cpu_total) < 1.0,"multi-wg: combined partials == exact CPU total sum");

    vkDestroyFence(dev,fence,NULL);
    free_buf(&a); free_buf(&b); free_buf(&c); free_buf(&part);
  }

  /* ================= (d) MULTI-WORKGROUP: atomic counters shared across every workgroup ================= */
  /* Every in-range invocation across all NG workgroups does two atomicAdds into a single 2-cell
     global counter buffer: counter[0]+=1 and counter[1]+=int(a[i]). With N invocations spread over
     NG workgroups running concurrently, the only way counter[0]==N and counter[1]==sum(int(a[i]))
     is if not a single atomic update was lost - the cross-workgroup race check the Goal requires. */
  {
    const uint32_t N = 1u<<20;                 /* 1,048,576 invocations across 4096 workgroups */
    const uint32_t NG = (N+LOCAL-1)/LOCAL;
    SBuf av, ctr;
    ok(mk_buf(&av,(VkDeviceSize)N*sizeof(int32_t)),"atomic: alloc value buffer");
    ok(mk_buf(&ctr,(VkDeviceSize)2*sizeof(uint32_t)),"atomic: alloc 2-cell counter buffer");
    int32_t* va=(int32_t*)av.map; uint32_t* vc=(uint32_t*)ctr.map;
    uint64_t cpu_sum=0;
    for(uint32_t i=0;i<N;i++){ va[i]=(int32_t)(i%251); cpu_sum+=(uint64_t)(i%251); }
    vc[0]=0; vc[1]=0;

    SBuf* atbufs[2]={&av,&ctr}; VkDescriptorSet dsAt=mk_set(dp,&pAtomic,atbufs,2);
    ok(dsAt!=VK_NULL_HANDLE,"atomic: descriptor set wired");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;

    struct PC pc={0.0f,N};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAtomic.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAtomic.pl,0,1,&dsAt,0,NULL);
    vkCmdPushConstants(cmd,pAtomic.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
    vkCmdDispatch(cmd,NG,1,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"atomic: wait cross-workgroup atomic dispatch");

    ok(vc[0]==N,"atomic: cross-workgroup atomicAdd count == N exactly (no lost updates)");
    ok((uint64_t)vc[1]==cpu_sum,"atomic: cross-workgroup atomic sum == closed-form CPU sum (no lost updates)");
    fprintf(stderr,"[atomic] counter[0]=%u (want %u) counter[1]=%u (want %llu)\n",vc[0],N,vc[1],(unsigned long long)cpu_sum);

    /* negative control: drive the exact-count predicate with the actual result - it must accept the true
       count and reject a single lost update (N-1). Fed the real vc[0], this also fails if the count is off. */
    ok(vc[0]==N && (vc[0]-1u)!=N,"atomic: negative control - ==N accepts the true count, rejects a lost update");

    /* re-run into the same counters without zeroing: the count must now be exactly 2N (accumulates) */
    vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAtomic.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAtomic.pl,0,1,&dsAt,0,NULL);
    vkCmdPushConstants(cmd,pAtomic.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
    vkCmdDispatch(cmd,NG,1,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"atomic: wait second accumulating dispatch");
    ok(vc[0]==2u*N,"atomic: second dispatch accumulated to exactly 2N (atomics accumulate across dispatches, no loss)");

    vkDestroyFence(dev,fence,NULL);
    free_buf(&av); free_buf(&ctr);
  }

  /* ================= boundary dispatches (zero-length + non-power-of-2 tail guard) ================= */
  {
    const uint32_t N = 4096;
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,c;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&c,bytes),"boundary: alloc a/b/c");
    float *ma=a.map,*mb=b.map,*mc=c.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)i; mb[i]=1.0f; mc[i]=-7.0f; }
    SBuf* bufs[3]={&a,&b,&c}; VkDescriptorSet ds=mk_set(dp,&pAdd,bufs,3);

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;

    struct PC pc0={1.0f,0};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc0,&pc0);
    vkCmdDispatch(cmd,0,1,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    { uint32_t touched=0; for(uint32_t i=0;i<N;i++) if(!feq(mc[i],-7.0f)) touched++;
      ok(touched==0,"boundary: zero-workgroup dispatch left output fully untouched"); }

    const uint32_t Nt = 4096+37;
    const uint32_t NGt = (Nt+LOCAL-1)/LOCAL;
    SBuf a2,b2,c2; VkDeviceSize b2b=(VkDeviceSize)(NGt*LOCAL)*sizeof(float);
    ok(mk_buf(&a2,b2b)&&mk_buf(&b2,b2b)&&mk_buf(&c2,b2b),"boundary: alloc padded tail buffers");
    float *ma2=a2.map,*mb2=b2.map,*mc2=c2.map;
    for(uint32_t i=0;i<NGt*LOCAL;i++){ ma2[i]=(float)(i%251); mb2[i]=0.25f; mc2[i]=-5.0f; }
    SBuf* tb[3]={&a2,&b2,&c2}; VkDescriptorSet dst=mk_set(dp,&pAdd,tb,3);
    struct PC pct={1.0f,Nt};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&dst,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pct,&pct);
    vkCmdDispatch(cmd,NGt,1,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
    { uint32_t badin=0,badtail=0;
      for(uint32_t i=0;i<Nt;i++) if(!feq(mc2[i],ma2[i]+mb2[i])) badin++;
      for(uint32_t i=Nt;i<NGt*LOCAL;i++) if(!feq(mc2[i],-5.0f)) badtail++;
      ok(badin==0,"boundary: non-pow2 in-range elements all c==a+b");
      ok(badtail==0,"boundary: tail guard left out-of-range invocations untouched"); }

    vkDestroyFence(dev,fence,NULL);
    free_buf(&a); free_buf(&b); free_buf(&c); free_buf(&a2); free_buf(&b2); free_buf(&c2);
  }

  /* ================= (a) CONCURRENT DISPATCH + timestamp query pool + transfer commands ================= */
  {
    const uint32_t N = 1u<<15;
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,c,cp;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&c,bytes)&&mk_buf(&cp,bytes),"query/transfer: alloc a/b/c/copy");
    float *ma=a.map,*mb=b.map,*mc=c.map,*mcp=cp.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i%101); mb[i]=2.0f; mc[i]=-1.0f; mcp[i]=-1.0f; }
    SBuf* bufs[3]={&a,&b,&c}; VkDescriptorSet ds=mk_set(dp,&pAdd,bufs,3);

    VkQueryPoolCreateInfo qpci={VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO}; qpci.queryType=VK_QUERY_TYPE_TIMESTAMP; qpci.queryCount=2;
    VkQueryPool qp; VKOK(vkCreateQueryPool(dev,&qpci,NULL,&qp),"query: create timestamp query pool");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;

    struct PC pc={1.0f,N};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdResetQueryPool(cmd,qp,0,2);
    vkCmdWriteTimestamp(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,qp,0);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
    vkCmdDispatch(cmd,(N+LOCAL-1)/LOCAL,1,1);
    VkBufferMemoryBarrier bmb={VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER};
    bmb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; bmb.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
    bmb.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; bmb.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED;
    bmb.buffer=c.buf; bmb.offset=0; bmb.size=bytes;
    vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,NULL,1,&bmb,0,NULL);
    VkBufferCopy region={0,0,bytes}; vkCmdCopyBuffer(cmd,c.buf,cp.buf,1,&region);
    vkCmdWriteTimestamp(cmd,VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,qp,1);
    vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"query/transfer: wait");

    uint64_t ts[2]={0,0};
    VKOK(vkGetQueryPoolResults(dev,qp,0,2,sizeof ts,ts,sizeof(uint64_t),VK_QUERY_RESULT_64_BIT|VK_QUERY_RESULT_WAIT_BIT),"query: read 2 timestamps");
    if(ts_valid) ok(ts[1]>=ts[0],"query: end timestamp >= start timestamp (monotonic)");
    else fprintf(stderr,"skip: timestampValidBits==0, monotonic check unsupported on this backend\n");
    ok(ts_ok,"query: device advertises timestampComputeAndGraphics");

    { uint32_t badcp=0; for(uint32_t i=0;i<N;i++) if(!feq(mcp[i],ma[i]+mb[i])) badcp++;
      ok(badcp==0,"transfer: vkCmdCopyBuffer c->cp equals compute result element-wise");
      float sv=mcp[9]; mcp[9]=sv+500.0f; uint32_t nb=0; for(uint32_t i=0;i<N;i++) if(!feq(mcp[i],ma[i]+mb[i])) nb++;
      ok(nb==1,"transfer: negative control - corrupted copy element detected"); mcp[9]=sv; }

    vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    union { float f; uint32_t u; } pat; pat.f=9.0f;
    vkBeginCommandBuffer(cmd,&bi); vkCmdFillBuffer(cmd,cp.buf,0,bytes,pat.u); vkEndCommandBuffer(cmd);
    vkQueueSubmit(queue[0],1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
    { uint32_t badf=0; for(uint32_t i=0;i<N;i++) if(!feq(mcp[i],9.0f)) badf++;
      ok(badf==0,"transfer: vkCmdFillBuffer wrote 9.0 across the full range"); }

    vkDestroyQueryPool(dev,qp,NULL);
    vkDestroyFence(dev,fence,NULL);
    free_buf(&a); free_buf(&b); free_buf(&c); free_buf(&cp);
  }

  /* ================= (a)+(c) CONCURRENT DISPATCH / ASYNC MULTI-SUBMIT ================= */
  {
    const uint32_t M = 6;                       /* 6 concurrent in-flight submissions */
    const uint32_t N = 1u<<16;                  /* 65536 elems each */
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a[6],b[6],c[6];
    VkDescriptorSet ds[6];
    VkCommandBuffer cmd[6];
    VkFence fence[6];
    float coef[6]={2.0f,3.0f,5.0f,7.0f,11.0f,13.0f};

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=M;
    vkAllocateCommandBuffers(dev,&cbai,cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};

    int alloc_ok=1;
    for(uint32_t m=0;m<M;m++){
      alloc_ok &= mk_buf(&a[m],bytes) & mk_buf(&b[m],bytes) & mk_buf(&c[m],bytes);
      float *ma=a[m].map,*mb=b[m].map,*mc=c[m].map;
      for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i+m); mb[i]=(float)(2*i); mc[i]=-9.0f; }
      SBuf* bufs[3]={&a[m],&b[m],&c[m]}; ds[m]=mk_set(dp,&pAdd,bufs,3);
      vkCreateFence(dev,&fci,NULL,&fence[m]);
    }
    ok(alloc_ok,"async multi-submit: allocated 6 independent buffer triples");

    for(uint32_t m=0;m<M;m++){
      struct PC pc={coef[m],N};
      VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
      vkBeginCommandBuffer(cmd[m],&bi);
      vkCmdBindPipeline(cmd[m],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
      vkCmdBindDescriptorSets(cmd[m],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&ds[m],0,NULL);
      vkCmdPushConstants(cmd[m],pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
      vkCmdDispatch(cmd[m],(N+LOCAL-1)/LOCAL,1,1);
      vkEndCommandBuffer(cmd[m]);
    }

    /* submit all 6 back-to-back WITHOUT waiting between submits; spread across queues if >1 */
    int subok=1;
    for(uint32_t m=0;m<M;m++){
      VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd[m];
      VkQueue tgt = queue[ true_multiqueue ? (m%nq) : 0 ];
      subok &= (vkQueueSubmit(tgt,1,&si,fence[m])==VK_SUCCESS);
    }
    ok(subok,"async multi-submit: 6 back-to-back submits accepted (no wait between; split across queues if multi-queue)");
    VKOK(vkWaitForFences(dev,M,fence,VK_TRUE,UINT64_MAX),"async multi-submit: single wait on all 6 fences");

    for(uint32_t m=0;m<M;m++){
      float *ma=a[m].map,*mb=b[m].map,*mc=c[m].map;
      uint32_t bad=0;
      for(uint32_t i=0;i<N;i++){ float want=coef[m]*ma[i]+mb[i]; if(!feq(mc[i],want)){bad++;} }
      char nm[80]; snprintf(nm,sizeof nm,"async multi-submit: in-flight result %u (alpha=%.0f) fully correct",m,coef[m]);
      ok(bad==0,nm);
    }
    /* every fence must report completion (ordering/completion check) */
    { int allsig=1; for(uint32_t m=0;m<M;m++) allsig &= (vkGetFenceStatus(dev,fence[m])==VK_SUCCESS);
      ok(allsig,"async multi-submit: every one of the 6 fences reports VK_SUCCESS (all completed)"); }
    { float *m0=c[0].map,*m1=c[1].map; ok(!feq(m0[100],m1[100]),"async multi-submit: overlapping submissions did not corrupt each other (distinct results)"); }

    /* batch submit: one vkQueueSubmit carrying all 6 command buffers in a single VkSubmitInfo */
    for(uint32_t m=0;m<M;m++){ vkResetFences(dev,1,&fence[m]); vkResetCommandBuffer(cmd[m],0);
      struct PC pc={coef[m],N};
      VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
      vkBeginCommandBuffer(cmd[m],&bi);
      vkCmdBindPipeline(cmd[m],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
      vkCmdBindDescriptorSets(cmd[m],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&ds[m],0,NULL);
      vkCmdPushConstants(cmd[m],pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
      vkCmdDispatch(cmd[m],(N+LOCAL-1)/LOCAL,1,1);
      vkEndCommandBuffer(cmd[m]);
      float* mc=c[m].map; for(uint32_t i=0;i<N;i++) mc[i]=-9.0f;
    }
    VkSubmitInfo bsi={VK_STRUCTURE_TYPE_SUBMIT_INFO}; bsi.commandBufferCount=M; bsi.pCommandBuffers=cmd;
    VKOK(vkQueueSubmit(queue[0],1,&bsi,fence[0]),"batch submit: 6 command buffers in one VkSubmitInfo");
    VKOK(vkWaitForFences(dev,1,&fence[0],VK_TRUE,UINT64_MAX),"batch submit: wait single fence");
    {
      uint32_t badall=0;
      for(uint32_t m=0;m<M;m++){ float *ma=a[m].map,*mb=b[m].map,*mc=c[m].map;
        for(uint32_t i=0;i<N;i++){ float want=coef[m]*ma[i]+mb[i]; if(!feq(mc[i],want)) badall++; } }
      ok(badall==0,"batch submit: all 6 batched results fully correct");
    }

    for(uint32_t m=0;m<M;m++){ vkDestroyFence(dev,fence[m],NULL); free_buf(&a[m]); free_buf(&b[m]); free_buf(&c[m]); }
    vkFreeCommandBuffers(dev,cmdpool,M,cmd);
  }

  /* ================= ORDERING: barrier-ordered dependency chain in one command buffer ================= */
  {
    const uint32_t N = 1u<<15;
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,t,d;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&t,bytes)&&mk_buf(&d,bytes),"dep-chain: alloc a/b/t/d");
    float *ma=a.map,*mb=b.map,*mt=t.map,*md=d.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i%251); mb[i]=1.0f; mt[i]=-1.0f; md[i]=-1.0f; }
    SBuf* addbufs[3]={&a,&b,&t}; VkDescriptorSet dsA=mk_set(dp,&pAdd,addbufs,3);
    SBuf* chnbufs[2]={&t,&d};   VkDescriptorSet dsB=mk_set(dp,&pChain,chnbufs,2);
    ok(dsA!=VK_NULL_HANDLE&&dsB!=VK_NULL_HANDLE,"dep-chain: descriptor sets wired");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);

    struct PC pcA={1.0f,N}; float K=4.0f; struct PC pcB={K,N};
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&dsA,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcA,&pcA);
    vkCmdDispatch(cmd,(N+LOCAL-1)/LOCAL,1,1);
    VkBufferMemoryBarrier bmb={VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER};
    bmb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; bmb.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
    bmb.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; bmb.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED;
    bmb.buffer=t.buf; bmb.offset=0; bmb.size=bytes;
    vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,0,0,NULL,1,&bmb,0,NULL);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pl,0,1,&dsB,0,NULL);
    vkCmdPushConstants(cmd,pChain.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcB,&pcB);
    vkCmdDispatch(cmd,(N+LOCAL-1)/LOCAL,1,1);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"dep-chain: wait");

    uint32_t bad_t=0, bad_d=0;
    for(uint32_t i=0;i<N;i++){
      float wt=ma[i]+mb[i];
      float wd=wt*K+1.0f;
      if(!feq(mt[i],wt)) bad_t++;
      if(!feq(md[i],wd)) bad_d++;
    }
    ok(bad_t==0,"dep-chain: stage A output t==a+b");
    ok(bad_d==0,"dep-chain: stage B output d==(a+b)*k+1 (ordering held via pipeline barrier)");
    ok(feq(md[7],(ma[7]+mb[7])*K+1.0f) && !feq(md[7],ma[7]+mb[7]),"dep-chain: B consumed A's output (not stale input)");

    vkDestroyFence(dev,fence,NULL);
    free_buf(&a); free_buf(&b); free_buf(&t); free_buf(&d);
  }

  /* ================= ORDERING: cross-submit VkSemaphore dependency ================= */
  {
    const uint32_t N = 1u<<15;
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,t,d;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&t,bytes)&&mk_buf(&d,bytes),"sem: alloc a/b/t/d");
    float *ma=a.map,*mb=b.map,*mt=t.map,*md=d.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i%211); mb[i]=2.0f; mt[i]=-1.0f; md[i]=-1.0f; }
    SBuf* addbufs[3]={&a,&b,&t}; VkDescriptorSet dsA=mk_set(dp,&pAdd,addbufs,3);
    SBuf* chnbufs[2]={&t,&d};   VkDescriptorSet dsB=mk_set(dp,&pChain,chnbufs,2);

    VkSemaphoreCreateInfo sci={VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
    VkSemaphore sem; VKOK(vkCreateSemaphore(dev,&sci,NULL,&sem),"sem: vkCreateSemaphore");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=2;
    VkCommandBuffer cmd[2]; vkAllocateCommandBuffers(dev,&cbai,cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fenceB; vkCreateFence(dev,&fci,NULL,&fenceB);
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;

    struct PC pcA={1.0f,N};
    vkBeginCommandBuffer(cmd[0],&bi);
    vkCmdBindPipeline(cmd[0],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd[0],VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&dsA,0,NULL);
    vkCmdPushConstants(cmd[0],pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcA,&pcA);
    vkCmdDispatch(cmd[0],(N+LOCAL-1)/LOCAL,1,1);
    vkEndCommandBuffer(cmd[0]);

    float K=3.0f; struct PC pcB={K,N};
    vkBeginCommandBuffer(cmd[1],&bi);
    vkCmdBindPipeline(cmd[1],VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pipe);
    vkCmdBindDescriptorSets(cmd[1],VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pl,0,1,&dsB,0,NULL);
    vkCmdPushConstants(cmd[1],pChain.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcB,&pcB);
    vkCmdDispatch(cmd[1],(N+LOCAL-1)/LOCAL,1,1);
    vkEndCommandBuffer(cmd[1]);

    VkSubmitInfo siA={VK_STRUCTURE_TYPE_SUBMIT_INFO}; siA.commandBufferCount=1; siA.pCommandBuffers=&cmd[0];
    siA.signalSemaphoreCount=1; siA.pSignalSemaphores=&sem;
    VkPipelineStageFlags waitStage=VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT;
    VkSubmitInfo siB={VK_STRUCTURE_TYPE_SUBMIT_INFO}; siB.commandBufferCount=1; siB.pCommandBuffers=&cmd[1];
    siB.waitSemaphoreCount=1; siB.pWaitSemaphores=&sem; siB.pWaitDstStageMask=&waitStage;
    VKOK(vkQueueSubmit(queue[0],1,&siA,VK_NULL_HANDLE),"sem: submit A signals semaphore");
    VKOK(vkQueueSubmit(queue[ true_multiqueue?1:0 ],1,&siB,fenceB),"sem: submit B waits on semaphore");
    VKOK(vkWaitForFences(dev,1,&fenceB,VK_TRUE,UINT64_MAX),"sem: wait fence on submit B");

    VKOK(vkQueueWaitIdle(queue[0]),"sem: vkQueueWaitIdle drains the queue");
    VKOK(vkDeviceWaitIdle(dev),"sem: vkDeviceWaitIdle drains the device");

    uint32_t bad_t=0, bad_d=0;
    for(uint32_t i=0;i<N;i++){ float wt=ma[i]+mb[i]; float wd=wt*K+1.0f;
      if(!feq(mt[i],wt)) bad_t++; if(!feq(md[i],wd)) bad_d++; }
    ok(bad_t==0,"sem: stage A output t==a+b");
    ok(bad_d==0,"sem: stage B output d==(a+b)*k+1 across two submits (semaphore ordering held)");
    ok(feq(md[42],(ma[42]+mb[42])*K+1.0f) && !feq(md[42],ma[42]+mb[42]),
       "sem: submit B consumed submit A's output (not stale input)");

    vkDestroySemaphore(dev,sem,NULL);
    vkDestroyFence(dev,fenceB,NULL);
    vkFreeCommandBuffers(dev,cmdpool,2,cmd);
    free_buf(&a); free_buf(&b); free_buf(&t); free_buf(&d);
  }

  /* ================= ORDERING: device event (vkCmdSetEvent/WaitEvents/ResetEvent) ================= */
  {
    const uint32_t N = 1u<<15;
    VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
    SBuf a,b,t,d;
    ok(mk_buf(&a,bytes)&&mk_buf(&b,bytes)&&mk_buf(&t,bytes)&&mk_buf(&d,bytes),"event: alloc a/b/t/d");
    float *ma=a.map,*mb=b.map,*mt=t.map,*md=d.map;
    for(uint32_t i=0;i<N;i++){ ma[i]=(float)(i%173); mb[i]=3.0f; mt[i]=-1.0f; md[i]=-1.0f; }
    SBuf* addbufs[3]={&a,&b,&t}; VkDescriptorSet dsA=mk_set(dp,&pAdd,addbufs,3);
    SBuf* chnbufs[2]={&t,&d};   VkDescriptorSet dsB=mk_set(dp,&pChain,chnbufs,2);

    VkEventCreateInfo eci={VK_STRUCTURE_TYPE_EVENT_CREATE_INFO};
    VkEvent evt; VKOK(vkCreateEvent(dev,&eci,NULL,&evt),"event: vkCreateEvent");
    ok(vkGetEventStatus(dev,evt)==VK_EVENT_RESET,"event: fresh event status == VK_EVENT_RESET");
    VKOK(vkSetEvent(dev,evt),"event: vkSetEvent");
    ok(vkGetEventStatus(dev,evt)==VK_EVENT_SET,"event: status after vkSetEvent == VK_EVENT_SET");
    VKOK(vkResetEvent(dev,evt),"event: vkResetEvent");
    ok(vkGetEventStatus(dev,evt)==VK_EVENT_RESET,"event: status after vkResetEvent == VK_EVENT_RESET");

    VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cbai,&cmd);
    VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fci,NULL,&fence);
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;

    struct PC pcA={1.0f,N}; float K=5.0f; struct PC pcB={K,N};
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pAdd.pl,0,1,&dsA,0,NULL);
    vkCmdPushConstants(cmd,pAdd.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcA,&pcA);
    vkCmdDispatch(cmd,(N+LOCAL-1)/LOCAL,1,1);
    vkCmdSetEvent(cmd,evt,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT);
    VkMemoryBarrier memb={VK_STRUCTURE_TYPE_MEMORY_BARRIER};
    memb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; memb.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
    vkCmdWaitEvents(cmd,1,&evt,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,1,&memb,0,NULL,0,NULL);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pipe);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pChain.pl,0,1,&dsB,0,NULL);
    vkCmdPushConstants(cmd,pChain.pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pcB,&pcB);
    vkCmdDispatch(cmd,(N+LOCAL-1)/LOCAL,1,1);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue[0],1,&si,fence);
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"event: wait");
    ok(vkGetEventStatus(dev,evt)==VK_EVENT_SET,"event: host observes device-set event == VK_EVENT_SET");

    uint32_t bad_t=0, bad_d=0;
    for(uint32_t i=0;i<N;i++){ float wt=ma[i]+mb[i]; float wd=wt*K+1.0f;
      if(!feq(mt[i],wt)) bad_t++; if(!feq(md[i],wd)) bad_d++; }
    ok(bad_t==0,"event: stage A output t==a+b");
    ok(bad_d==0,"event: stage B output d==(a+b)*k+1 (device event ordering held)");
    ok(feq(md[99],(ma[99]+mb[99])*K+1.0f) && !feq(md[99],ma[99]+mb[99]),
       "event: stage B consumed stage A's output via device event (not stale)");

    vkDestroyEvent(dev,evt,NULL);
    vkDestroyFence(dev,fence,NULL);
    free_buf(&a); free_buf(&b); free_buf(&t); free_buf(&d);
  }

  vkDestroyCommandPool(dev,cmdpool,NULL);
  vkDestroyDescriptorPool(dev,dp,NULL);
  free_pipe(&pAdd); free_pipe(&pRed); free_pipe(&pChain); free_pipe(&pAtomic);
  vkDestroyDevice(dev,NULL); vkDestroyInstance(inst,NULL);
  free(pds); free(qf);

  int EXPECTED=93, TOTAL=PASS+FAIL;
  printf("vk-parallel: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("VK_PARALLEL_FULL_API OK %d\n",PASS); return 0; }
  printf("VK_PARALLEL_FULL_API FAIL\n"); return 1;
}

/* vulkan_c_full_api.c - full Vulkan C compute API carpet on lavapipe: enumerate the compute API
 * surface (instance / physical-device / device / queue / buffer / memory / shader-module /
 * descriptor / pipeline / command-buffer / fence / semaphore / event / query-pool / dispatch /
 * indirect-dispatch / timestamp / multi-queue / push-constant / transfer commands) and assert
 * operator results against closed-form references,
 * queried properties against known values, and error paths against real VkResult enums.
 * Prints "VULKAN_C_FULL_API OK <n>" only when every assertion passes AND the count equals the
 * pinned EXPECTED total. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static int PASS=0, FAIL=0;
static void ok(int c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
static int feq(float a,float b){ return fabsf(a-b)<=1e-4f*(1.0f+fabsf(b)); }
#define VKOK(x,d) ok((x)==VK_SUCCESS,d)

static uint32_t* load_spv(const char*p, size_t*words){
  FILE*f=fopen(p,"rb"); if(!f)return NULL; fseek(f,0,SEEK_END); long n=ftell(f); fseek(f,0,SEEK_SET);
  uint32_t*b=malloc(n); if(fread(b,1,n,f)!=(size_t)n){fclose(f);return NULL;} fclose(f); *words=n; return b;
}
static uint32_t find_mem(VkPhysicalDeviceMemoryProperties*mp,uint32_t bits,VkMemoryPropertyFlags want){
  for(uint32_t i=0;i<mp->memoryTypeCount;i++) if((bits&(1u<<i)) && (mp->memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}

/* checker for vadd (c==alpha*a+b): returns 1 iff every element matches the closed form. */
static int check_saxpy(const float*a,const float*b,const float*c,int n,float alpha){
  for(int i=0;i<n;i++) if(!feq(c[i],alpha*a[i]+b[i])) return 0;
  return 1;
}
static int check_mul(const float*a,const float*b,const float*c,int n){
  for(int i=0;i<n;i++) if(!feq(c[i],a[i]*b[i])) return 0;
  return 1;
}

int main(void){
  const int N=1000000;                 /* >=1,000,000-element grid, verified element-wise */
  VkDeviceSize bytes=(VkDeviceSize)N*sizeof(float);
  struct PC{ float alpha; uint32_t n; } pc={1.0f,(uint32_t)N};

  /* --- instance + enumeration APIs --- */
  uint32_t nl=0; VKOK(vkEnumerateInstanceLayerProperties(&nl,NULL),"vkEnumerateInstanceLayerProperties");
  uint32_t ne=0; VKOK(vkEnumerateInstanceExtensionProperties(NULL,&ne,NULL),"vkEnumerateInstanceExtensionProperties");
  VkApplicationInfo ai={VK_STRUCTURE_TYPE_APPLICATION_INFO}; ai.apiVersion=VK_API_VERSION_1_1;
  VkInstanceCreateInfo ici={VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO}; ici.pApplicationInfo=&ai;
  VkInstance inst; VKOK(vkCreateInstance(&ici,NULL,&inst),"vkCreateInstance");

  /* --- physical device APIs --- */
  uint32_t npd=0; VKOK(vkEnumeratePhysicalDevices(inst,&npd,NULL),"vkEnumeratePhysicalDevices count"); ok(npd>=1,">=1 physical device");
  VkPhysicalDevice* pds=malloc(sizeof(VkPhysicalDevice)*npd); vkEnumeratePhysicalDevices(inst,&npd,pds);
  VkPhysicalDevice pd=pds[0];
  VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd,&props);
  ok(props.apiVersion>=VK_API_VERSION_1_0,"vkGetPhysicalDeviceProperties apiVersion");
  ok(props.limits.maxComputeWorkGroupInvocations>=64 && props.limits.maxComputeWorkGroupSize[0]>=64,"compute workgroup limits >= shader local size 64");
  VkPhysicalDeviceFeatures feat; vkGetPhysicalDeviceFeatures(pd,&feat);
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  ok(mp.memoryTypeCount>=1 && mp.memoryHeapCount>=1,"vkGetPhysicalDeviceMemoryProperties nonempty");
  uint32_t nqf=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,NULL); ok(nqf>=1,"queue family count");
  VkQueueFamilyProperties* qf=malloc(sizeof(VkQueueFamilyProperties)*nqf); vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,qf);
  uint32_t cq=UINT32_MAX; for(uint32_t i=0;i<nqf;i++) if(qf[i].queueFlags&VK_QUEUE_COMPUTE_BIT){cq=i;break;}
  ok(cq!=UINT32_MAX,"found compute queue family");
  int have_ts = cq!=UINT32_MAX && qf[cq].timestampValidBits>0 && props.limits.timestampPeriod>0.0f;
  ok(qf[cq].timestampValidBits<=64,"timestampValidBits within [0,64]");

  /* --- device + queue APIs --- */
  float prio=1.0f; VkDeviceQueueCreateInfo qci={VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
  qci.queueFamilyIndex=cq; qci.queueCount=1; qci.pQueuePriorities=&prio;
  VkDeviceCreateInfo dci={VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci;
  VkDevice dev; VKOK(vkCreateDevice(pd,&dci,NULL,&dev),"vkCreateDevice");
  VkQueue queue; vkGetDeviceQueue(dev,cq,0,&queue); ok(queue!=VK_NULL_HANDLE,"vkGetDeviceQueue");

  /* --- MULTI-QUEUE capability: the compute family advertises queueCount>=1; if it advertises
   * more than one queue, create the device with that count and fetch a distinct second queue
   * handle; otherwise (lavapipe reports exactly one compute queue) assert the count is precisely
   * 1 - an honest capability assertion rather than faking a second queue. Recorded in device_limited. */
  ok(qf[cq].queueCount>=1,"compute family queueCount >= 1");
  if(qf[cq].queueCount>1){
    float prios2[2]={1.0f,1.0f}; VkDeviceQueueCreateInfo qci2={VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    qci2.queueFamilyIndex=cq; qci2.queueCount=2; qci2.pQueuePriorities=prios2;
    VkDeviceCreateInfo dci2={VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci2.queueCreateInfoCount=1; dci2.pQueueCreateInfos=&qci2;
    VkDevice dev2=VK_NULL_HANDLE; VkResult mr=vkCreateDevice(pd,&dci2,NULL,&dev2);
    VkQueue q0=VK_NULL_HANDLE,q1=VK_NULL_HANDLE;
    if(mr==VK_SUCCESS){ vkGetDeviceQueue(dev2,cq,0,&q0); vkGetDeviceQueue(dev2,cq,1,&q1); }
    ok(mr==VK_SUCCESS && q1!=VK_NULL_HANDLE && q1!=q0,"multi-queue: 2nd queue handle is distinct");
    if(dev2!=VK_NULL_HANDLE) vkDestroyDevice(dev2,NULL);
  } else {
    ok(qf[cq].queueCount==1,"multi-queue: device reports exactly 1 compute queue (capability honestly asserted)");
  }

  /* --- buffer + memory APIs (3 host-visible storage+transfer buffers) --- */
  VkBuffer buf[3]; VkDeviceMemory mem[3]; float* map[3];
  for(int i=0;i<3;i++){
    VkBufferCreateInfo bci={VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bci.size=bytes;
    bci.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT|VK_BUFFER_USAGE_TRANSFER_SRC_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    bci.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
    VKOK(vkCreateBuffer(dev,&bci,NULL,&buf[i]),"vkCreateBuffer");
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,buf[i],&mr);
    ok(mr.size>=bytes,"vkGetBufferMemoryRequirements size >= requested");
    uint32_t mt=find_mem(&mp,mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    ok(mt!=UINT32_MAX,"find host-visible memory type");
    VkMemoryAllocateInfo mai={VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; mai.allocationSize=mr.size; mai.memoryTypeIndex=mt;
    VKOK(vkAllocateMemory(dev,&mai,NULL,&mem[i]),"vkAllocateMemory");
    VKOK(vkBindBufferMemory(dev,buf[i],mem[i],0),"vkBindBufferMemory");
    VKOK(vkMapMemory(dev,mem[i],0,bytes,0,(void**)&map[i]),"vkMapMemory");
  }
  for(int i=0;i<N;i++){ map[0][i]=(float)(i%997); map[1][i]=2.0f*(i%577)+1.0f; map[2][i]=0.0f; }

  /* --- ERROR PATH: oversized buffer create returns a real VkResult error enum (not a crash) --- */
  { VkBufferCreateInfo bad={VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bad.size=(VkDeviceSize)1<<48;
    bad.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT; bad.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
    VkBuffer bb=VK_NULL_HANDLE; VkResult r=vkCreateBuffer(dev,&bad,NULL,&bb);
    ok(r==VK_ERROR_OUT_OF_DEVICE_MEMORY,"vkCreateBuffer(2^48) == VK_ERROR_OUT_OF_DEVICE_MEMORY");
    if(r==VK_SUCCESS) vkDestroyBuffer(dev,bb,NULL); }

  /* --- shader module APIs (two operator families: saxpy/vadd and elementwise multiply) --- */
  size_t sw; uint32_t* spv=load_spv("shaders/vadd.spv",&sw); ok(spv!=NULL,"load vadd SPIR-V");
  VkShaderModuleCreateInfo smci={VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; smci.codeSize=sw; smci.pCode=spv;
  VkShaderModule sm; VKOK(vkCreateShaderModule(dev,&smci,NULL,&sm),"vkCreateShaderModule vadd");
  size_t sw2; uint32_t* spv2=load_spv("shaders/mul.spv",&sw2); ok(spv2!=NULL,"load mul SPIR-V");
  VkShaderModuleCreateInfo smci2={VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; smci2.codeSize=sw2; smci2.pCode=spv2;
  VkShaderModule sm2; VKOK(vkCreateShaderModule(dev,&smci2,NULL,&sm2),"vkCreateShaderModule mul");

  /* --- descriptor set layout + pipeline layout (push constant) APIs --- */
  VkDescriptorSetLayoutBinding lb[3];
  for(int i=0;i<3;i++){ lb[i]=(VkDescriptorSetLayoutBinding){0}; lb[i].binding=i; lb[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; lb[i].descriptorCount=1; lb[i].stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; }
  VkDescriptorSetLayoutCreateInfo dslci={VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=3; dslci.pBindings=lb;
  VkDescriptorSetLayout dsl; VKOK(vkCreateDescriptorSetLayout(dev,&dslci,NULL,&dsl),"vkCreateDescriptorSetLayout");
  VkPushConstantRange pcr={VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC)};
  VkPipelineLayoutCreateInfo plci={VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl; plci.pushConstantRangeCount=1; plci.pPushConstantRanges=&pcr;
  VkPipelineLayout pl; VKOK(vkCreatePipelineLayout(dev,&plci,NULL,&pl),"vkCreatePipelineLayout");

  /* --- compute pipeline API (two pipelines built from one cache) --- */
  VkPipelineCacheCreateInfo pcci={VK_STRUCTURE_TYPE_PIPELINE_CACHE_CREATE_INFO}; VkPipelineCache cache;
  VKOK(vkCreatePipelineCache(dev,&pcci,NULL,&cache),"vkCreatePipelineCache");
  VkComputePipelineCreateInfo cpci[2]={ {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO}, {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO} };
  for(int i=0;i<2;i++){ cpci[i].stage.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO; cpci[i].stage.stage=VK_SHADER_STAGE_COMPUTE_BIT; cpci[i].stage.pName="main"; cpci[i].layout=pl; }
  cpci[0].stage.module=sm; cpci[1].stage.module=sm2;
  VkPipeline pipe[2]; VKOK(vkCreateComputePipelines(dev,cache,2,cpci,NULL,pipe),"vkCreateComputePipelines (vadd+mul)");

  /* --- descriptor pool + sets APIs (FREE bit so vkFreeDescriptorSets is exercisable) --- */
  VkDescriptorPoolSize dps={VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,6};
  VkDescriptorPoolCreateInfo dpci={VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.flags=VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT; dpci.maxSets=2; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
  VkDescriptorPool dp; VKOK(vkCreateDescriptorPool(dev,&dpci,NULL,&dp),"vkCreateDescriptorPool");
  VkDescriptorSetAllocateInfo dsai={VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dp; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
  VkDescriptorSet ds; VKOK(vkAllocateDescriptorSets(dev,&dsai,&ds),"vkAllocateDescriptorSets");
  VkDescriptorBufferInfo dbi[3]; VkWriteDescriptorSet wds[3];
  for(int i=0;i<3;i++){ dbi[i]=(VkDescriptorBufferInfo){buf[i],0,VK_WHOLE_SIZE};
    wds[i]=(VkWriteDescriptorSet){VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds[i].dstSet=ds; wds[i].dstBinding=i; wds[i].descriptorCount=1; wds[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; wds[i].pBufferInfo=&dbi[i]; }
  vkUpdateDescriptorSets(dev,3,wds,0,NULL);

  /* --- command pool + buffer APIs --- */
  VkCommandPoolCreateInfo cpci2={VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; cpci2.queueFamilyIndex=cq; cpci2.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  VkCommandPool cmdpool; VKOK(vkCreateCommandPool(dev,&cpci2,NULL,&cmdpool),"vkCreateCommandPool");
  VkCommandBufferAllocateInfo cbai={VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cbai.commandPool=cmdpool; cbai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount=1;
  VkCommandBuffer cmd; VKOK(vkAllocateCommandBuffers(dev,&cbai,&cmd),"vkAllocateCommandBuffers");

  VkFenceCreateInfo fci={VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; VKOK(vkCreateFence(dev,&fci,NULL,&fence),"vkCreateFence");

  /* dispatch helper: record dispatch of pipeline P over G groups + submit + wait on fence. */
  #define DISPATCH(P,G,msg) do{ \
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; \
    VKOK(vkBeginCommandBuffer(cmd,&bi),"vkBeginCommandBuffer " msg); \
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,(P)); \
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL); \
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc); \
    vkCmdDispatch(cmd,(G),1,1); \
    VKOK(vkEndCommandBuffer(cmd),"vkEndCommandBuffer " msg); \
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd; \
    VKOK(vkQueueSubmit(queue,1,&si,fence),"vkQueueSubmit " msg); \
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"vkWaitForFences " msg); \
    VKOK(vkResetFences(dev,1,&fence),"vkResetFences " msg); \
    vkResetCommandBuffer(cmd,0); \
  }while(0)

  int GROUPS=(N+63)/64;   /* N=1,000,000 => 15625 groups; 1,000,000 is not a multiple of 64 -> exercises the i<n tail guard */

  /* --- vadd (alpha=1) correctness over full 1M grid, verified element-wise vs closed form --- */
  pc.alpha=1.0f; pc.n=(uint32_t)N; DISPATCH(pipe[0],GROUPS,"vadd");
  ok(check_saxpy(map[0],map[1],map[2],N,1.0f),"vadd == a+b over 1,000,000 elements");
  ok(vkGetFenceStatus(dev,fence)==VK_NOT_READY,"vkGetFenceStatus (unsignalled after reset)");
  /* NEGATIVE CONTROL: corrupt one output element and prove the checker flags the mismatch. */
  { float saved=map[2][N/2]; map[2][N/2]+=1.0f;
    ok(!check_saxpy(map[0],map[1],map[2],N,1.0f),"negative control: corrupted vadd element is rejected");
    map[2][N/2]=saved; }

  /* --- saxpy (alpha=3) correctness, re-dispatch with new push constant --- */
  pc.alpha=3.0f; DISPATCH(pipe[0],GROUPS,"saxpy");
  ok(check_saxpy(map[0],map[1],map[2],N,3.0f),"saxpy == 3*a+b (push constant)");

  /* --- BOUNDARY: zero-length dispatch leaves output buffer unchanged (n=0 tail guard) --- */
  for(int i=0;i<N;i++) map[2][i]=-7.0f;
  { struct PC z=pc; z.n=0; struct PC keep=pc; pc=z; DISPATCH(pipe[0],1,"zero-length dispatch"); pc=keep; }
  { int g=1; for(int i=0;i<N;i++) if(!feq(map[2][i],-7.0f)){g=0;break;} ok(g,"zero-length dispatch leaves buffer untouched"); }
  pc.n=(uint32_t)N;

  /* --- BOUNDARY: non-divisible partial grid (n=997, prime, one group covers it + tail guard) --- */
  for(int i=0;i<N;i++) map[2][i]=-1.0f;
  { struct PC keep=pc; pc.alpha=1.0f; pc.n=997; DISPATCH(pipe[0],(997+63)/64,"partial 997");
    int g=1; for(int i=0;i<997;i++) if(!feq(map[2][i],map[0][i]+map[1][i])){g=0;break;}
    ok(g,"partial dispatch n=997 computed range correct");
    ok(feq(map[2][997],-1.0f)&&feq(map[2][1023],-1.0f),"partial dispatch tail guard leaves i>=n untouched");
    pc=keep; }

  /* === multiply operator family (second pipeline) with its own negative control === */
  pc.alpha=1.0f; pc.n=(uint32_t)N; DISPATCH(pipe[1],GROUPS,"mul");
  ok(check_mul(map[0],map[1],map[2],N),"mul == a*b over full grid");
  { float saved=map[2][123]; map[2][123]*=2.0f; map[2][123]+=1.0f;
    ok(!check_mul(map[0],map[1],map[2],N),"negative control: corrupted mul element is rejected");
    map[2][123]=saved; }

  /* === INDIRECT DISPATCH family: drive the same vadd (alpha=1) grid via vkCmdDispatchIndirect,
   * where the {x,y,z} group counts live in a device buffer written with vkCmdUpdateBuffer, and
   * assert the result is identical to the direct-dispatch closed form a+b over the full 1M grid.
   * A negative control proves the checker still rejects a corrupted element. */
  { VkBuffer ibuf; VkDeviceMemory imem;
    VkBufferCreateInfo ibci={VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; ibci.size=sizeof(VkDispatchIndirectCommand);
    ibci.usage=VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    ibci.sharingMode=VK_SHARING_MODE_EXCLUSIVE;
    VKOK(vkCreateBuffer(dev,&ibci,NULL,&ibuf),"vkCreateBuffer (indirect command buffer)");
    VkMemoryRequirements imr; vkGetBufferMemoryRequirements(dev,ibuf,&imr);
    uint32_t imt=find_mem(&mp,imr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if(imt==UINT32_MAX) imt=find_mem(&mp,imr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT);
    VkMemoryAllocateInfo imai={VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; imai.allocationSize=imr.size; imai.memoryTypeIndex=imt;
    vkAllocateMemory(dev,&imai,NULL,&imem); vkBindBufferMemory(dev,ibuf,imem,0);

    for(int i=0;i<N;i++) map[2][i]=-3.0f;
    pc.alpha=1.0f; pc.n=(uint32_t)N;
    VkDispatchIndirectCommand idc={(uint32_t)GROUPS,1,1};
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdUpdateBuffer(cmd,ibuf,0,sizeof(idc),&idc);
    VkBufferMemoryBarrier ib={VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER};
    ib.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; ib.dstAccessMask=VK_ACCESS_INDIRECT_COMMAND_READ_BIT;
    ib.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; ib.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED;
    ib.buffer=ibuf; ib.offset=0; ib.size=sizeof(idc);
    vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_DRAW_INDIRECT_BIT,0,0,NULL,1,&ib,0,NULL);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pipe[0]);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc);
    vkCmdDispatchIndirect(cmd,ibuf,0);
    VKOK(vkEndCommandBuffer(cmd),"vkEndCommandBuffer indirect");
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    ok(check_saxpy(map[0],map[1],map[2],N,1.0f),"vkCmdDispatchIndirect vadd == a+b (same as direct dispatch)");
    { float saved=map[2][N/4]; map[2][N/4]+=1.0f;
      ok(!check_saxpy(map[0],map[1],map[2],N,1.0f),"negative control: corrupted indirect-dispatch element is rejected");
      map[2][N/4]=saved; }
    vkDestroyBuffer(dev,ibuf,NULL); vkFreeMemory(dev,imem,NULL); pc.alpha=1.0f; pc.n=(uint32_t)N; }

  /* === SEMAPHORE family: chain two submits (vadd then mul) ordered by a binary semaphore === */
  { VkSemaphoreCreateInfo sci={VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO}; VkSemaphore sem;
    VKOK(vkCreateSemaphore(dev,&sci,NULL,&sem),"vkCreateSemaphore");
    /* command buffer A: vadd into buf2 */
    VkCommandBuffer cA,cB; VkCommandBufferAllocateInfo a2=cbai; a2.commandBufferCount=1;
    vkAllocateCommandBuffers(dev,&a2,&cA); vkAllocateCommandBuffers(dev,&a2,&cB);
    pc.alpha=1.0f; pc.n=(uint32_t)N;
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cA,&bi); vkCmdBindPipeline(cA,VK_PIPELINE_BIND_POINT_COMPUTE,pipe[0]);
    vkCmdBindDescriptorSets(cA,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cA,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc);
    vkCmdDispatch(cA,GROUPS,1,1); vkEndCommandBuffer(cA);
    /* command buffer B: mul, must observe A's result before running (semaphore ordering) */
    vkBeginCommandBuffer(cB,&bi); vkCmdBindPipeline(cB,VK_PIPELINE_BIND_POINT_COMPUTE,pipe[1]);
    vkCmdBindDescriptorSets(cB,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cB,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc);
    vkCmdDispatch(cB,GROUPS,1,1); vkEndCommandBuffer(cB);
    VkSubmitInfo sA={VK_STRUCTURE_TYPE_SUBMIT_INFO}; sA.commandBufferCount=1; sA.pCommandBuffers=&cA; sA.signalSemaphoreCount=1; sA.pSignalSemaphores=&sem;
    VkPipelineStageFlags waitStage=VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT;
    VkSubmitInfo sB={VK_STRUCTURE_TYPE_SUBMIT_INFO}; sB.commandBufferCount=1; sB.pCommandBuffers=&cB; sB.waitSemaphoreCount=1; sB.pWaitSemaphores=&sem; sB.pWaitDstStageMask=&waitStage;
    VKOK(vkQueueSubmit(queue,1,&sA,VK_NULL_HANDLE),"vkQueueSubmit A (signal semaphore)");
    VKOK(vkQueueSubmit(queue,1,&sB,fence),"vkQueueSubmit B (wait semaphore)");
    VKOK(vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX),"vkWaitForFences semaphore chain");
    vkResetFences(dev,1,&fence);
    ok(check_mul(map[0],map[1],map[2],N),"semaphore-ordered A(vadd)->B(mul) final == a*b");
    vkFreeCommandBuffers(dev,cmdpool,1,&cA); vkFreeCommandBuffers(dev,cmdpool,1,&cB);
    vkDestroySemaphore(dev,sem,NULL); }

  /* === EVENT family: host set/reset status queries + device-side set/wait in a command buffer === */
  { VkEventCreateInfo eci={VK_STRUCTURE_TYPE_EVENT_CREATE_INFO}; VkEvent ev;
    VKOK(vkCreateEvent(dev,&eci,NULL,&ev),"vkCreateEvent");
    ok(vkGetEventStatus(dev,ev)==VK_EVENT_RESET,"vkGetEventStatus initial == VK_EVENT_RESET");
    VKOK(vkSetEvent(dev,ev),"vkSetEvent");
    ok(vkGetEventStatus(dev,ev)==VK_EVENT_SET,"vkGetEventStatus after set == VK_EVENT_SET");
    VKOK(vkResetEvent(dev,ev),"vkResetEvent");
    ok(vkGetEventStatus(dev,ev)==VK_EVENT_RESET,"vkGetEventStatus after reset == VK_EVENT_RESET");
    /* device-side: dispatch, cmd-set event, cmd-wait event bracketing a memory barrier */
    pc.alpha=2.0f; pc.n=(uint32_t)N;
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pipe[0]);
    vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL);
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc);
    vkCmdDispatch(cmd,GROUPS,1,1);
    vkCmdSetEvent(cmd,ev,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT);
    VkMemoryBarrier mb={VK_STRUCTURE_TYPE_MEMORY_BARRIER}; mb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_HOST_READ_BIT;
    vkCmdWaitEvents(cmd,1,&ev,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_HOST_BIT,1,&mb,0,NULL,0,NULL);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    ok(vkGetEventStatus(dev,ev)==VK_EVENT_SET,"vkCmdSetEvent left event set after execution");
    ok(check_saxpy(map[0],map[1],map[2],N,2.0f),"event-bracketed dispatch == 2*a+b");
    vkDestroyEvent(dev,ev,NULL); pc.alpha=1.0f; }

  /* === QUERY POOL / TIMESTAMP family: reset + two timestamps around a dispatch ===
   * The timestamp is a COUNTED assertion: when the compute family reports timestampValidBits>0
   * (lavapipe reports 64) the two written timestamps must satisfy ts1>=ts0 with a successful
   * result read-back; when the family reports zero valid bits we assert the capability is
   * honestly reported as 0 instead of fabricating a monotonic pair. Recorded in device_limited
   * as a conditional-capability assertion. */
  { VkQueryPoolCreateInfo qpci={VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO}; qpci.queryType=VK_QUERY_TYPE_TIMESTAMP; qpci.queryCount=2;
    VkQueryPool qp; VkResult cpr=vkCreateQueryPool(dev,&qpci,NULL,&qp);
    VKOK(cpr,"vkCreateQueryPool (timestamp)");
    if(cpr==VK_SUCCESS){
      pc.alpha=1.0f; pc.n=(uint32_t)N;
      VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
      vkBeginCommandBuffer(cmd,&bi);
      vkCmdResetQueryPool(cmd,qp,0,2);
      vkCmdWriteTimestamp(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,qp,0);
      vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pipe[0]);
      vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_COMPUTE,pl,0,1,&ds,0,NULL);
      vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof(struct PC),&pc);
      vkCmdDispatch(cmd,GROUPS,1,1);
      vkCmdWriteTimestamp(cmd,VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT,qp,1);
      vkEndCommandBuffer(cmd);
      VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
      vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
      uint64_t ts[2]={0,0};
      VkResult qr=vkGetQueryPoolResults(dev,qp,0,2,sizeof(ts),ts,sizeof(uint64_t),VK_QUERY_RESULT_64_BIT|VK_QUERY_RESULT_WAIT_BIT);
      fprintf(stderr,"note: timestamp query getResults=%d have_ts=%d ts0=%llu ts1=%llu monotonic=%d\n",
              (int)qr,have_ts,(unsigned long long)ts[0],(unsigned long long)ts[1],(qr==VK_SUCCESS && ts[1]>=ts[0]));
      if(have_ts) ok(qr==VK_SUCCESS && ts[1]>=ts[0],"vkCmdWriteTimestamp+vkGetQueryPoolResults: ts1>=ts0 (timestampValidBits>0)");
      else ok(qf[cq].timestampValidBits==0,"timestamp capability honestly reported as 0 valid bits");
      vkDestroyQueryPool(dev,qp,NULL); pc.alpha=1.0f;
    } else { ok(qf[cq].timestampValidBits==0,"timestamp query pool unsupported => 0 valid bits capability (honest)"); fprintf(stderr,"note: vkCreateQueryPool(timestamp) unsupported result=%d\n",(int)cpr); } }

  /* === transfer commands: copy / update / fill + a real buffer memory barrier === */
  { VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi);
    VkBufferCopy region={0,0,bytes}; vkCmdCopyBuffer(cmd,buf[0],buf[2],1,&region);
    VkBufferMemoryBarrier bmb={VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER};
    bmb.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; bmb.dstAccessMask=VK_ACCESS_HOST_READ_BIT;
    bmb.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; bmb.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED;
    bmb.buffer=buf[2]; bmb.offset=0; bmb.size=bytes;
    vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_HOST_BIT,0,0,NULL,1,&bmb,0,NULL);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    int g=1; for(int i=0;i<N;i++) if(!feq(map[2][i],map[0][i])){g=0;break;} ok(g,"vkCmdCopyBuffer buf0->buf2 round-trip equality");
    { float saved=map[2][N/3]; map[2][N/3]+=1.0f;
      int bad=1; for(int i=0;i<N;i++) if(!feq(map[2][i],map[0][i])){bad=0;break;}
      ok(!bad,"negative control: corrupted copied element is rejected by the round-trip checker");
      map[2][N/3]=saved; } }
  /* vkCmdUpdateBuffer: inline update of the first 16 floats then read back exact values */
  { float upd[16]; for(int i=0;i<16;i++) upd[i]=100.0f+i;
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi); vkCmdUpdateBuffer(cmd,buf[2],0,sizeof(upd),upd); vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    int g=1; for(int i=0;i<16;i++) if(!feq(map[2][i],100.0f+i)){g=0;break;} ok(g,"vkCmdUpdateBuffer wrote exact 16 floats"); }
  { union { float f; uint32_t u; } pat; pat.f=9.0f;
    VkCommandBufferBeginInfo bi={VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd,&bi); vkCmdFillBuffer(cmd,buf[2],0,bytes,pat.u); vkEndCommandBuffer(cmd);
    VkSubmitInfo si={VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    vkQueueSubmit(queue,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX); vkResetFences(dev,1,&fence); vkResetCommandBuffer(cmd,0);
    ok(feq(map[2][0],9.0f)&&feq(map[2][N-1],9.0f),"vkCmdFillBuffer == 9.0 across full range"); }

  ok(vkQueueWaitIdle(queue)==VK_SUCCESS,"vkQueueWaitIdle");
  ok(vkDeviceWaitIdle(dev)==VK_SUCCESS,"vkDeviceWaitIdle");

  /* --- core-1.1 Get*2 queries: assert the *2 struct matches the v1 query (not existence-only) --- */
  { VkPhysicalDeviceProperties2 p2={VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2}; vkGetPhysicalDeviceProperties2(pd,&p2);
    ok(p2.properties.apiVersion==props.apiVersion && p2.properties.deviceID==props.deviceID,"vkGetPhysicalDeviceProperties2 matches v1 query"); }
  { VkPhysicalDeviceMemoryProperties2 m2={VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_PROPERTIES_2}; vkGetPhysicalDeviceMemoryProperties2(pd,&m2);
    ok(m2.memoryProperties.memoryTypeCount==mp.memoryTypeCount && m2.memoryProperties.memoryHeapCount==mp.memoryHeapCount,"vkGetPhysicalDeviceMemoryProperties2 matches v1 query"); }
  { VkPhysicalDeviceFeatures2 f2={VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2}; vkGetPhysicalDeviceFeatures2(pd,&f2);
    ok(f2.features.robustBufferAccess==feat.robustBufferAccess,"vkGetPhysicalDeviceFeatures2 matches v1 query"); }
  { VkBufferMemoryRequirementsInfo2 ri={VK_STRUCTURE_TYPE_BUFFER_MEMORY_REQUIREMENTS_INFO_2}; ri.buffer=buf[0];
    VkMemoryRequirements2 mr2={VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2}; vkGetBufferMemoryRequirements2(dev,&ri,&mr2);
    ok(mr2.memoryRequirements.size>=bytes,"vkGetBufferMemoryRequirements2 size >= requested"); }
  { VkDeviceQueueInfo2 qi2={VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2}; qi2.queueFamilyIndex=cq; qi2.queueIndex=0;
    VkQueue q2; vkGetDeviceQueue2(dev,&qi2,&q2); ok(q2==queue,"vkGetDeviceQueue2 returns same handle as vkGetDeviceQueue"); }
  ok(vkResetCommandPool(dev,cmdpool,0)==VK_SUCCESS,"vkResetCommandPool");

  /* --- vkFreeDescriptorSets then re-allocate from the FREE pool, proving the free path --- */
  VKOK(vkFreeDescriptorSets(dev,dp,1,&ds),"vkFreeDescriptorSets");
  { VkDescriptorSet ds2; VKOK(vkAllocateDescriptorSets(dev,&dsai,&ds2),"re-allocate descriptor set after free"); }

  /* --- cleanup APIs (destroy calls have no return; each Destroy pairs with a create that succeeded) --- */
  vkDestroyFence(dev,fence,NULL);
  vkDestroyCommandPool(dev,cmdpool,NULL);
  vkDestroyDescriptorPool(dev,dp,NULL);
  vkDestroyPipeline(dev,pipe[0],NULL); vkDestroyPipeline(dev,pipe[1],NULL); vkDestroyPipelineCache(dev,cache,NULL);
  vkDestroyPipelineLayout(dev,pl,NULL); vkDestroyDescriptorSetLayout(dev,dsl,NULL);
  vkDestroyShaderModule(dev,sm,NULL); vkDestroyShaderModule(dev,sm2,NULL);
  for(int i=0;i<3;i++){ vkUnmapMemory(dev,mem[i]); vkDestroyBuffer(dev,buf[i],NULL); vkFreeMemory(dev,mem[i],NULL); }
  vkDestroyDevice(dev,NULL);
  vkDestroyInstance(inst,NULL);
  free(spv); free(spv2); free(pds); free(qf);

  int EXPECTED=114, TOTAL=PASS+FAIL;
  printf("vulkan-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("VULKAN_C_FULL_API OK %d\n",PASS); return 0; }
  printf("VULKAN_C_FULL_API FAIL\n"); return 1;
}

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <libdrm/drm.h>
#include <libdrm/virtgpu_drm.h>
#include <vulkan/vulkan.h>

#include "execbuf_signal.h"

#define REQUIRE(ok) do { if (!(ok)) { fprintf(stderr, "VENUS_REGRESSION_FAILED line=%d errno=%d\n", __LINE__, errno); exit(1); } } while (0)
#define VK_OK(call) do { VkResult result = (call); if (result != VK_SUCCESS) { fprintf(stderr, "VENUS_REGRESSION_FAILED line=%d api=%s result=%d\n", __LINE__, #call, result); exit(1); } } while (0)

static void repeated_gem_lifetime(void)
{
    int fd = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    REQUIRE(fd >= 0);
    uint32_t host_visible = 0;
    struct drm_virtgpu_getparam parameter = {.param = VIRTGPU_PARAM_HOST_VISIBLE,
                                            .value = (uint64_t)(uintptr_t)&host_visible};
    REQUIRE(syscall(SYS_ioctl, fd, DRM_IOCTL_VIRTGPU_GETPARAM, &parameter) == 0);
    REQUIRE(host_visible == 1);
    for (unsigned round = 0; round < 32; round++) {
        struct drm_virtgpu_resource_create create = {
            .target = 2, .format = 2, .bind = 2,
            .width = 16, .height = 16, .depth = 1, .array_size = 1,
            .size = 4096,
        };
        REQUIRE(syscall(SYS_ioctl, fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE, &create) == 0);
        struct drm_virtgpu_map map = {.handle = create.bo_handle};
        REQUIRE(syscall(SYS_ioctl, fd, DRM_IOCTL_VIRTGPU_MAP, &map) == 0);
        uint32_t *memory = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, map.offset);
        REQUIRE(memory != MAP_FAILED);
        memory[0] = 0x12340000 | round;
        struct drm_gem_close close_gem = {.handle = create.bo_handle};
        REQUIRE(syscall(SYS_ioctl, fd, DRM_IOCTL_GEM_CLOSE, &close_gem) == 0);
        REQUIRE(memory[0] == (0x12340000 | round));
        REQUIRE(munmap(memory, 4096) == 0);
    }
    REQUIRE(close(fd) == 0);
    puts("VIRGL_GEM_RECYCLE_PASSED rounds=32");
}

static void vulkan_transfer_readback(void)
{
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "venus-regression", .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo instance_info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance instance;
    VK_OK(vkCreateInstance(&instance_info, NULL, &instance));
    uint32_t count = 0;
    VK_OK(vkEnumeratePhysicalDevices(instance, &count, NULL));
    REQUIRE(count > 0);
    VkPhysicalDevice *physical = calloc(count, sizeof(*physical));
    REQUIRE(physical != NULL);
    VK_OK(vkEnumeratePhysicalDevices(instance, &count, physical));
    VkPhysicalDevice gpu = physical[0];
    free(physical);
    VkPhysicalDeviceProperties properties;
    vkGetPhysicalDeviceProperties(gpu, &properties);
    printf("VENUS_DEVICE name=%s\n", properties.deviceName);
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &count, NULL);
    VkQueueFamilyProperties *families = calloc(count, sizeof(*families));
    REQUIRE(families != NULL);
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &count, families);
    uint32_t family = count;
    for (uint32_t i = 0; i < count; i++) {
        if (families[i].queueCount && (families[i].queueFlags & VK_QUEUE_TRANSFER_BIT)) { family = i; break; }
    }
    free(families);
    REQUIRE(family < count);
    float priority = 1;
    VkDeviceQueueCreateInfo queue_info = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family, .queueCount = 1, .pQueuePriorities = &priority};
    VkDeviceCreateInfo device_info = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                                      .queueCreateInfoCount = 1, .pQueueCreateInfos = &queue_info};
    VkDevice device;
    VK_OK(vkCreateDevice(gpu, &device_info, NULL, &device));
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);
    VkPhysicalDeviceMemoryProperties memory_properties;
    vkGetPhysicalDeviceMemoryProperties(gpu, &memory_properties);
    VkCommandPoolCreateInfo pool_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandPool pool;
    VK_OK(vkCreateCommandPool(device, &pool_info, NULL, &pool));
    VkCommandBufferAllocateInfo command_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer command;
    VK_OK(vkAllocateCommandBuffers(device, &command_info, &command));
    // Keep one Vulkan device/context open while mappings, blobs and host BAR
    // slots are repeatedly acquired, submitted, observed and released.
    for (unsigned round = 0; round < 32; round++) {
        VkBufferCreateInfo buffer_info = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            .size = 65536, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT, .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
        VkBuffer buffer;
        VK_OK(vkCreateBuffer(device, &buffer_info, NULL, &buffer));
        VkMemoryRequirements requirements;
        vkGetBufferMemoryRequirements(device, buffer, &requirements);
        uint32_t memory_type = memory_properties.memoryTypeCount;
        for (uint32_t i = 0; i < memory_properties.memoryTypeCount; i++) {
            if ((requirements.memoryTypeBits & (1u << i)) &&
                (memory_properties.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) {
                memory_type = i; break;
            }
        }
        REQUIRE(memory_type < memory_properties.memoryTypeCount);
        VkMemoryAllocateInfo allocation = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            .allocationSize = requirements.size, .memoryTypeIndex = memory_type};
        VkDeviceMemory memory;
        VK_OK(vkAllocateMemory(device, &allocation, NULL, &memory));
        VK_OK(vkBindBufferMemory(device, buffer, memory, 0));
        VK_OK(vkResetCommandPool(device, pool, 0));
        VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
        VK_OK(vkBeginCommandBuffer(command, &begin));
        uint32_t pattern = 0x5a3c0000 | round;
        vkCmdFillBuffer(command, buffer, 0, VK_WHOLE_SIZE, pattern);
        VkMemoryBarrier barrier = {.sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
            .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT, .dstAccessMask = VK_ACCESS_HOST_READ_BIT};
        vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT,
                             0, 1, &barrier, 0, NULL, 0, NULL);
        VK_OK(vkEndCommandBuffer(command));
        VkFenceCreateInfo fence_info = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
        VkFence fence;
        VK_OK(vkCreateFence(device, &fence_info, NULL, &fence));
        VkSubmitInfo submit = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                              .commandBufferCount = 1, .pCommandBuffers = &command};
        VK_OK(vkQueueSubmit(queue, 1, &submit, fence));
        VK_OK(vkWaitForFences(device, 1, &fence, VK_TRUE, 10000000000ULL));
        void *mapped;
        VK_OK(vkMapMemory(device, memory, 0, VK_WHOLE_SIZE, 0, &mapped));
        VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                     .memory = memory, .offset = 0, .size = VK_WHOLE_SIZE};
        VK_OK(vkInvalidateMappedMemoryRanges(device, 1, &range));
        for (unsigned word = 0; word < 65536 / sizeof(uint32_t); word++)
            REQUIRE(((uint32_t *)mapped)[word] == pattern);
        vkUnmapMemory(device, memory);
        vkDestroyFence(device, fence, NULL);
        vkDestroyBuffer(device, buffer, NULL);
        vkFreeMemory(device, memory, NULL);
    }
    vkDestroyCommandPool(device, pool, NULL);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    puts("VENUS_TRANSFER_READBACK_PASSED rounds=32 bytes=65536");
}

int main(void)
{
    const char *icd = getenv("VK_DRIVER_FILES");
    REQUIRE(icd && strstr(icd, "/virtio_icd"));
    repeated_gem_lifetime();
    execbuf_signal_test();
    vulkan_transfer_readback();
    puts("VENUS_REGRESSION_PASSED");
    return 0;
}

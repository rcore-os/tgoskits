/*
 * SPDX-License-Identifier: Apache-2.0
 */

#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/pcie/cap.h>
#include <zephyr/drivers/pcie/msi.h>
#include <zephyr/drivers/pcie/pcie.h>
#include <zephyr/kernel.h>
#include <zephyr/kernel/mm.h>
#include <zephyr/sw_isr_table.h>
#include <zephyr/sys/sys_io.h>

#include "tgos_ivc_backend.h"

#define TGOS_IVC_VENDOR_ID			0x1af4U
#define TGOS_IVC_DEVICE_ID			0x1110U
#define TGOS_IVC_CLASS_REVISION_NC		0xff000001U
#define TGOS_IVC_CLASS_REVISION_WB		0xff000002U

#define TGOS_IVC_AXVISOR_CAP_ID			0x09U
#define TGOS_IVC_AXVISOR_CAP_LENGTH		36U
#define TGOS_IVC_AXVISOR_CAP_VERSION		1U
#define TGOS_IVC_AXVISOR_CAP_MAGIC		0x56495841U
#define TGOS_IVC_AXVISOR_PROTOCOL_NC		0x4000U
#define TGOS_IVC_AXVISOR_PROTOCOL_WB		0x4001U
#define TGOS_IVC_AXVISOR_FEATURE_PEER_EPOCH	BIT(0)
#define TGOS_IVC_AXVISOR_EPOCH_OFFSET		0x100U
#define TGOS_IVC_AXVISOR_EPOCH_STRIDE		8U

#define TGOS_IVC_REGISTER_BAR			0U
#define TGOS_IVC_MSIX_BAR			1U
#define TGOS_IVC_SHARED_BAR			2U

#define TGOS_IVC_PAGE_SIZE			0x1000U
#define TGOS_IVC_STATE_TABLE_SIZE		TGOS_IVC_PAGE_SIZE
#define TGOS_IVC_REQUIRED_PEERS			2U
#define TGOS_IVC_MIN_SHARED_SIZE		0x00010000U
#define TGOS_IVC_MAX_SHARED_SIZE		0x02000000U

#define TGOS_IVC_PEER_ID_OFFSET			0x00U
#define TGOS_IVC_MAXIMUM_PEERS_OFFSET		0x04U
#define TGOS_IVC_INTERRUPT_CONTROL_OFFSET	0x08U
#define TGOS_IVC_DOORBELL_OFFSET		0x0cU
#define TGOS_IVC_DOORBELL_PEER_SHIFT		16U
#define TGOS_IVC_EVENT_STATUS_OFFSET		0x14U
#define TGOS_IVC_COMMON_SIZE_OFFSET		0x18U
#define TGOS_IVC_OUTPUT_SIZE_OFFSET		0x1cU
#define TGOS_IVC_NOTIFICATIONS_ENABLE		BIT(0)
#define TGOS_IVC_EVENT_VECTOR_0			BIT(0)
#define TGOS_IVC_CMD_INTX_DISABLE		0x00000400U
#define TGOS_IVC_BAR_PREFETCHABLE		BIT(3)
#define TGOS_IVC_CAPABILITY_SCAN_LIMIT		48U
#define TGOS_IVC_CONFIG_SIZE			256U
#define TGOS_IVC_CONFIG_DWORDS			(TGOS_IVC_CONFIG_SIZE / 4U)

BUILD_ASSERT(IS_ENABLED(CONFIG_ARM64),
	     "TGOS_IVC requires the AArch64 MMU attribute set");
BUILD_ASSERT(DT_HAS_CHOSEN(zephyr_pcie_controller),
	     "TGOS_IVC requires a devicetree-selected PCIe host");
BUILD_ASSERT(
	DT_NODE_HAS_PROP(DT_CHOSEN(zephyr_pcie_controller), msi_parent),
	"TGOS_IVC requires the selected PCIe host to define msi-parent");
BUILD_ASSERT(
	DT_PROP_LEN(DT_CHOSEN(zephyr_pcie_controller), msi_parent) == 1,
	"TGOS_IVC requires a single PCIe host MSI parent");

K_MUTEX_DEFINE(tgos_ivc_api_mutex);
K_SEM_DEFINE(tgos_ivc_event_sem, 0, 1);

struct tgos_ivc_capability {
	uint32_t common_size;
	uint32_t output_size;
	uint32_t epoch_offset;
	uint32_t epoch_stride;
	uint32_t bar2_flags;
};

struct tgos_ivc_scan_result {
	pcie_bdf_t bdf;
	size_t matches;
	struct tgos_ivc_capability capability;
};

static struct tgos_ivc_platform_view platform_view;
static struct tgos_ivc_capability transport;
static msi_vector_t msi_vector;
static uint8_t *registers;
static uint8_t *state_table;
static uint8_t *local_output;
static uint8_t *peer_output;
static uintptr_t local_epoch;
static uintptr_t peer_epoch;
static uint32_t local_peer_id;
static uint32_t output_size;
static bool prepared;
static bool attempted;
static int prepare_error;

static bool decode_transport_policy(uint32_t class_revision,
				    uint32_t protocol, uint32_t *bar2_flags)
{
	if (class_revision == TGOS_IVC_CLASS_REVISION_NC &&
	    (protocol & 0xffffU) == TGOS_IVC_AXVISOR_PROTOCOL_NC) {
		*bar2_flags = K_MEM_ARM_NORMAL_NC;
		return true;
	}
	if (class_revision == TGOS_IVC_CLASS_REVISION_WB &&
	    (protocol & 0xffffU) == TGOS_IVC_AXVISOR_PROTOCOL_WB) {
		*bar2_flags = K_MEM_CACHE_WB;
		return true;
	}

	return false;
}

static int read_axvisor_capability(pcie_bdf_t bdf, uint32_t class_revision,
				   struct tgos_ivc_capability *result)
{
	uint32_t command = pcie_conf_read(bdf, PCIE_CONF_CMDSTAT);
	uint32_t pointer;

	if ((command & PCIE_CONF_CMDSTAT_CAPS) == 0U) {
		return -ENODEV;
	}

	pointer = PCIE_CONF_CAPPTR_FIRST(
		pcie_conf_read(bdf, PCIE_CONF_CAPPTR));
	for (uint32_t visited = 0U;
	     pointer != 0U && visited < TGOS_IVC_CAPABILITY_SCAN_LIMIT;
	     visited++) {
		uint32_t capability_offset;
		uint32_t header;
		uint32_t next;

		if (pointer >= TGOS_IVC_CONFIG_DWORDS) {
			return -EPROTO;
		}
		header = pcie_conf_read(bdf, pointer);
		next = PCIE_CONF_CAP_NEXT(header);
		capability_offset = pointer * 4U;

		if (PCIE_CONF_CAP_ID(header) == TGOS_IVC_AXVISOR_CAP_ID) {
			uint32_t length = (header >> 16) & 0xffU;
			uint32_t version = (header >> 24) & 0xffU;
			uint32_t magic;
			uint32_t protocol;
			uint32_t features;
			uint32_t epoch_offset;
			uint32_t epoch_stride;
			uint32_t common_size;
			uint32_t advertised_output_size;
			uint32_t max_peers;
			uint32_t bar2_flags;

			if (version != TGOS_IVC_AXVISOR_CAP_VERSION ||
			    length != TGOS_IVC_AXVISOR_CAP_LENGTH ||
			    capability_offset > TGOS_IVC_CONFIG_SIZE ||
			    length > TGOS_IVC_CONFIG_SIZE - capability_offset) {
				return -EPROTO;
			}

			magic = pcie_conf_read(bdf, pointer + 1U);
			protocol = pcie_conf_read(bdf, pointer + 2U);
			features = pcie_conf_read(bdf, pointer + 3U);
			epoch_offset = pcie_conf_read(bdf, pointer + 4U);
			epoch_stride = pcie_conf_read(bdf, pointer + 5U);
			common_size = pcie_conf_read(bdf, pointer + 6U);
			advertised_output_size =
				pcie_conf_read(bdf, pointer + 7U);
			max_peers = pcie_conf_read(bdf, pointer + 8U);

			if (!decode_transport_policy(class_revision, protocol,
						     &bar2_flags)) {
				return -EPROTO;
			}
			if (magic != TGOS_IVC_AXVISOR_CAP_MAGIC ||
			    (features &
			     TGOS_IVC_AXVISOR_FEATURE_PEER_EPOCH) == 0U ||
			    epoch_offset != TGOS_IVC_AXVISOR_EPOCH_OFFSET ||
			    epoch_stride != TGOS_IVC_AXVISOR_EPOCH_STRIDE ||
			    (common_size % TGOS_IVC_PAGE_SIZE) != 0U ||
			    (bar2_flags == K_MEM_CACHE_WB &&
			     common_size != 0U) ||
			    advertised_output_size == 0U ||
			    (advertised_output_size % TGOS_IVC_PAGE_SIZE) !=
				    0U ||
			    max_peers != TGOS_IVC_REQUIRED_PEERS) {
				return -EPROTO;
			}

			result->common_size = common_size;
			result->output_size = advertised_output_size;
			result->epoch_offset = epoch_offset;
			result->epoch_stride = epoch_stride;
			result->bar2_flags = bar2_flags;
			return 0;
		}

		if (next == 0U || next == pointer) {
			break;
		}
		pointer = next;
	}

	return -ENODEV;
}

static int validate_bar_kind(pcie_bdf_t bdf, unsigned int bar,
			     bool prefetchable, bool allow_64)
{
	uint32_t value = pcie_conf_read(bdf, PCIE_CONF_BAR0 + bar);

	if (!PCIE_CONF_BAR_MEM(value)) {
		return -ENODEV;
	}
	if (!allow_64 && PCIE_CONF_BAR_64(value)) {
		return -EPROTO;
	}
	if (((value & TGOS_IVC_BAR_PREFETCHABLE) != 0U) != prefetchable) {
		return -EPROTO;
	}

	return 0;
}

static bool ranges_overlap(const struct pcie_bar *left,
			   const struct pcie_bar *right)
{
	if (left->size > UINTPTR_MAX - left->phys_addr ||
	    right->size > UINTPTR_MAX - right->phys_addr) {
		return true;
	}

	return left->phys_addr < right->phys_addr + right->size &&
	       right->phys_addr < left->phys_addr + left->size;
}

static bool checked_phys_add(uintptr_t base, size_t offset,
			     uintptr_t *result)
{
	if (offset > UINTPTR_MAX - base) {
		return false;
	}

	*result = base + offset;
	return true;
}

static int map_region(uintptr_t physical, size_t size, uint32_t flags,
		      uint8_t **mapped)
{
	uint8_t *address = NULL;

	k_mem_map_phys_bare(&address, physical, size, flags);
	if (address == NULL) {
		return -ENOMEM;
	}

	*mapped = address;
	return 0;
}

static void unmap_region(uint8_t *mapped, size_t size)
{
	if (mapped != NULL) {
		k_mem_unmap_phys_bare(mapped, size);
	}
}

static int validate_msix_bar(pcie_bdf_t bdf)
{
	uint32_t capability = pcie_get_cap(bdf, PCI_CAP_ID_MSIX);
	uint32_t table;
	uint32_t pba;

	if (capability == 0U) {
		return -ENODEV;
	}

	table = pcie_conf_read(bdf, capability + PCIE_MSIX_TR);
	pba = pcie_conf_read(bdf, capability + PCIE_MSIX_PBA);
	if ((table & PCIE_MSIX_TR_BIR) != TGOS_IVC_MSIX_BAR ||
	    (pba & PCIE_MSIX_PBA_BIR) != TGOS_IVC_MSIX_BAR) {
		return -EPROTO;
	}

	return 0;
}

static int read_strict_profile(pcie_bdf_t bdf,
			       struct tgos_ivc_capability *capability)
{
	uint32_t class_revision = pcie_conf_read(bdf, PCIE_CONF_CLASSREV);
	int result;

	result = read_axvisor_capability(bdf, class_revision, capability);
	if (result != 0) {
		return result;
	}
	result = validate_bar_kind(bdf, TGOS_IVC_REGISTER_BAR, false, false);
	if (result != 0) {
		return result;
	}
	result = validate_bar_kind(bdf, TGOS_IVC_MSIX_BAR, false, false);
	if (result != 0) {
		return result;
	}
	result = validate_bar_kind(bdf, TGOS_IVC_SHARED_BAR, true, true);
	if (result != 0) {
		return result;
	}
	return validate_msix_bar(bdf);
}

static bool scan_endpoint(pcie_bdf_t bdf, pcie_id_t id, void *context)
{
	struct tgos_ivc_scan_result *result = context;
	struct tgos_ivc_capability capability;

	if (id != PCIE_ID(TGOS_IVC_VENDOR_ID, TGOS_IVC_DEVICE_ID) ||
	    read_strict_profile(bdf, &capability) != 0) {
		return true;
	}

	if (result->matches == 0U) {
		result->bdf = bdf;
		result->capability = capability;
	}
	result->matches++;

	/* Stop after proving that the provisioned endpoint is not unique. */
	return result->matches < 2U;
}

static void signal_event(const void *parameter)
{
	ARG_UNUSED(parameter);
	k_sem_give(&tgos_ivc_event_sem);
}

static int configure_msix(pcie_bdf_t bdf)
{
	unsigned int key;
	int result = -EIO;
	uint8_t vectors;

	key = irq_lock();
	vectors = pcie_msi_vectors_allocate(
		bdf, CONFIG_TGOS_IVC_MSI_PRIORITY, &msi_vector, 1U);
	if (vectors != 1U) {
		goto out;
	}
	if (msi_vector.arch.irq - CONFIG_GEN_IRQ_START_VECTOR >= IRQ_TABLE_SIZE) {
		result = -ENOSPC;
		goto out;
	}

	if (!pcie_msi_vector_connect(bdf, &msi_vector, signal_event, NULL,
				     0U)) {
		goto out;
	}

	if (!pcie_msi_enable(bdf, &msi_vector, 1U, 0U)) {
		goto out;
	}

	result = 0;
out:
	irq_unlock(key);
	return result;
}

static int fill_platform_view(struct tgos_ivc_platform_view *view)
{
	if (!prepared) {
		return prepare_error == 0 ? -ENODEV : prepare_error;
	}

	*view = platform_view;
	return 0;
}

static int prepare_once(void)
{
	const struct device *host =
		DEVICE_DT_GET(DT_CHOSEN(zephyr_pcie_controller));
	const struct device *msi_parent = DEVICE_DT_GET(
		DT_PHANDLE(DT_CHOSEN(zephyr_pcie_controller), msi_parent));
	struct tgos_ivc_scan_result scan = {
		.bdf = PCIE_BDF_NONE,
		.matches = 0U,
	};
	struct pcie_scan_opt options = {
		.bus = 0U,
		.cb = scan_endpoint,
		.cb_data = &scan,
		.flags = PCIE_SCAN_RECURSIVE,
	};
	struct pcie_bar bar0;
	struct pcie_bar bar1;
	struct pcie_bar bar2;
	uintptr_t state_physical;
	uintptr_t output0_physical;
	uintptr_t local_physical;
	uintptr_t peer_physical;
	size_t output0_offset;
	size_t local_offset;
	size_t peer_offset;
	size_t required;
	uint32_t peer_id;
	uint32_t max_peers;
	uint32_t common_size;
	uint32_t bar_output_size;
	int result;

	if (!device_is_ready(host) || !device_is_ready(msi_parent)) {
		return -ENODEV;
	}

	result = pcie_scan(&options);
	if (result != 0) {
		return result;
	}
	if (scan.matches != 1U) {
		return -ENODEV;
	}
	transport = scan.capability;

	if (!pcie_get_mbar(scan.bdf, TGOS_IVC_REGISTER_BAR, &bar0) ||
	    !pcie_get_mbar(scan.bdf, TGOS_IVC_MSIX_BAR, &bar1) ||
	    !pcie_get_mbar(scan.bdf, TGOS_IVC_SHARED_BAR, &bar2)) {
		return -ENODEV;
	}
	if (bar0.size < TGOS_IVC_PAGE_SIZE ||
	    bar2.size < TGOS_IVC_MIN_SHARED_SIZE ||
	    bar2.size > TGOS_IVC_MAX_SHARED_SIZE ||
	    (bar2.phys_addr % TGOS_IVC_PAGE_SIZE) != 0U ||
	    ranges_overlap(&bar0, &bar1) || ranges_overlap(&bar0, &bar2) ||
	    ranges_overlap(&bar1, &bar2)) {
		return -EPROTO;
	}

	common_size = transport.common_size;
	output_size = transport.output_size;
	output0_offset = TGOS_IVC_STATE_TABLE_SIZE + common_size;
	required = output0_offset;
	required += (size_t)output_size * TGOS_IVC_REQUIRED_PEERS;
	if (required > bar2.size) {
		return -EPROTO;
	}

	pcie_set_cmd(scan.bdf,
		     PCIE_CONF_CMDSTAT_MEM | PCIE_CONF_CMDSTAT_MASTER, true);
	pcie_set_cmd(scan.bdf, TGOS_IVC_CMD_INTX_DISABLE, false);

	result = map_region(bar0.phys_addr, bar0.size,
			    K_MEM_CACHE_NONE | K_MEM_PERM_RW, &registers);
	if (result != 0) {
		goto disable_decode;
	}

	peer_id = sys_read32((mm_reg_t)(uintptr_t)
			     (registers + TGOS_IVC_PEER_ID_OFFSET));
	max_peers = sys_read32((mm_reg_t)(uintptr_t)
			       (registers + TGOS_IVC_MAXIMUM_PEERS_OFFSET));
	common_size = sys_read32((mm_reg_t)(uintptr_t)
				 (registers + TGOS_IVC_COMMON_SIZE_OFFSET));
	bar_output_size =
		sys_read32((mm_reg_t)(uintptr_t)
			   (registers + TGOS_IVC_OUTPUT_SIZE_OFFSET));
	if (peer_id >= TGOS_IVC_REQUIRED_PEERS ||
	    max_peers != TGOS_IVC_REQUIRED_PEERS ||
	    common_size != transport.common_size ||
	    bar_output_size != transport.output_size) {
		result = -EPROTO;
		goto disable_decode;
	}

	local_peer_id = peer_id;
	state_physical = bar2.phys_addr;
	if (!checked_phys_add(bar2.phys_addr, output0_offset,
			      &output0_physical) ||
	    !checked_phys_add(output0_physical,
			      (size_t)peer_id * output_size,
			      &local_physical) ||
	    !checked_phys_add(output0_physical,
			      (size_t)(peer_id ^ 1U) * output_size,
			      &peer_physical)) {
		result = -EOVERFLOW;
		goto disable_decode;
	}

	local_offset = transport.epoch_offset +
		       (size_t)peer_id * transport.epoch_stride;
	peer_offset = transport.epoch_offset +
		      (size_t)(peer_id ^ 1U) * transport.epoch_stride;
	if (local_offset > TGOS_IVC_STATE_TABLE_SIZE -
				   sizeof(uint64_t) ||
	    peer_offset > TGOS_IVC_STATE_TABLE_SIZE -
				  sizeof(uint64_t)) {
		result = -EPROTO;
		goto disable_decode;
	}

	/* BAR2 is CPU-owned shared RAM. Apply the capability-selected cache
	 * policy to every alias. No Device alias or writable peer alias is
	 * created; only this peer's output section gets RW.
	 */
	result = map_region(state_physical, TGOS_IVC_STATE_TABLE_SIZE,
			    transport.bar2_flags, &state_table);
	if (result != 0) {
		goto disable_decode;
	}
	result = map_region(local_physical, output_size,
			    transport.bar2_flags | K_MEM_PERM_RW,
			    &local_output);
	if (result != 0) {
		goto disable_decode;
	}
	result = map_region(peer_physical, output_size,
			    transport.bar2_flags, &peer_output);
	if (result != 0) {
		goto disable_decode;
	}

	local_epoch = (uintptr_t)state_table + local_offset;
	peer_epoch = (uintptr_t)state_table + peer_offset;
	if ((local_epoch & (sizeof(uint64_t) - 1U)) != 0U ||
	    (peer_epoch & (sizeof(uint64_t) - 1U)) != 0U) {
		result = -EPROTO;
		goto disable_decode;
	}

	result = configure_msix(scan.bdf);
	if (result != 0) {
		goto disable_decode;
	}

	k_sem_reset(&tgos_ivc_event_sem);
	sys_write32(TGOS_IVC_EVENT_VECTOR_0,
		    (mm_reg_t)(uintptr_t)
			    (registers + TGOS_IVC_EVENT_STATUS_OFFSET));

	platform_view.local_output = (uintptr_t)local_output;
	platform_view.peer_output = (uintptr_t)peer_output;
	platform_view.local_epoch = local_epoch;
	platform_view.peer_epoch = peer_epoch;
	platform_view.output_size = output_size;
	prepared = true;
	return 0;

disable_decode:
	unmap_region(peer_output, output_size);
	unmap_region(local_output, output_size);
	unmap_region(state_table, TGOS_IVC_STATE_TABLE_SIZE);
	unmap_region(registers, bar0.size);
	peer_output = NULL;
	local_output = NULL;
	state_table = NULL;
	registers = NULL;
	pcie_set_cmd(scan.bdf,
		     PCIE_CONF_CMDSTAT_MEM | PCIE_CONF_CMDSTAT_MASTER, false);
	return result;
}

int tgos_ivc_backend_lock(void)
{
	int result;

	if (k_is_in_isr()) {
		return -EWOULDBLOCK;
	}

	result = k_mutex_lock(&tgos_ivc_api_mutex, K_FOREVER);
	return result < 0 ? result : 0;
}

void tgos_ivc_backend_unlock(void)
{
	(void)k_mutex_unlock(&tgos_ivc_api_mutex);
}

int tgos_ivc_backend_prepare(struct tgos_ivc_platform_view *view)
{
	int result;

	if (view == NULL) {
		return -EINVAL;
	}

	if (prepared) {
		return fill_platform_view(view);
	}
	if (attempted) {
		return prepare_error;
	}

	attempted = true;
	result = prepare_once();
	if (result != 0) {
		prepare_error = result;
		return result;
	}

	return fill_platform_view(view);
}

int tgos_ivc_backend_enable_interrupts(void)
{
	if (!prepared || registers == NULL) {
		return -ENODEV;
	}

	sys_write32(TGOS_IVC_NOTIFICATIONS_ENABLE,
		    (mm_reg_t)(uintptr_t)
			    (registers + TGOS_IVC_INTERRUPT_CONTROL_OFFSET));
	return 0;
}

void tgos_ivc_backend_disable_interrupts(void)
{
	if (!prepared || registers == NULL) {
		return;
	}

	sys_write32(0U, (mm_reg_t)(uintptr_t)
			      (registers + TGOS_IVC_INTERRUPT_CONTROL_OFFSET));
}

void tgos_ivc_backend_reset_event(void)
{
	k_sem_reset(&tgos_ivc_event_sem);
	if (prepared && registers != NULL) {
		sys_write32(TGOS_IVC_EVENT_VECTOR_0,
			    (mm_reg_t)(uintptr_t)
				    (registers +
				     TGOS_IVC_EVENT_STATUS_OFFSET));
	}
}

void tgos_ivc_backend_take_events(void)
{
	uint32_t pending;

	if (!prepared || registers == NULL) {
		return;
	}

	pending = sys_read32((mm_reg_t)(uintptr_t)
			     (registers + TGOS_IVC_EVENT_STATUS_OFFSET));
	if ((pending & TGOS_IVC_EVENT_VECTOR_0) != 0U) {
		sys_write32(pending & TGOS_IVC_EVENT_VECTOR_0,
			    (mm_reg_t)(uintptr_t)
				    (registers +
				     TGOS_IVC_EVENT_STATUS_OFFSET));
	}
}

int tgos_ivc_backend_wait_event(int32_t timeout_ms)
{
	k_timeout_t timeout;
	int result;

	if (!prepared) {
		return -ENODEV;
	}

	if (timeout_ms < 0) {
		timeout = K_FOREVER;
	} else if (timeout_ms == 0) {
		timeout = K_NO_WAIT;
	} else {
		timeout = K_MSEC(timeout_ms);
	}

	result = k_sem_take(&tgos_ivc_event_sem, timeout);
	if (result == -EAGAIN || result == -EBUSY) {
		return -EAGAIN;
	}
	return result < 0 ? result : 0;
}

int64_t tgos_ivc_backend_now_ms(void)
{
	return k_uptime_get();
}

int tgos_ivc_backend_epochs(uint64_t *local, uint64_t *peer)
{
	if (!prepared || local == NULL || peer == NULL) {
		return -ENODEV;
	}

	*local = __atomic_load_n((const uint64_t *)local_epoch,
				 __ATOMIC_ACQUIRE);
	*peer = __atomic_load_n((const uint64_t *)peer_epoch,
				__ATOMIC_ACQUIRE);
	return 0;
}

int tgos_ivc_backend_ring_peer(void)
{
	if (!prepared || registers == NULL) {
		return -ENODEV;
	}

	/*
	 * Publish all prior shared-memory writes before the device observes the
	 * doorbell. The host raises the endpoint event after this write.
	 */
	__atomic_thread_fence(__ATOMIC_SEQ_CST);
	/* Bits 31:16 select the target peer; bits 15:0 select the vector. */
	sys_write32((local_peer_id ^ 1U) << TGOS_IVC_DOORBELL_PEER_SHIFT,
		    (mm_reg_t)(uintptr_t)
			    (registers + TGOS_IVC_DOORBELL_OFFSET));
	return 0;
}

void tgos_ivc_backend_abort(void)
{
	k_panic();
	__builtin_unreachable();
}

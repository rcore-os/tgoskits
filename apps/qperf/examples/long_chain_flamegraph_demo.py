#!/usr/bin/env python3
"""Generate a synthetic long-call-chain flamegraph demo.

This script is intentionally synthetic: it demonstrates the SVG renderer's
vertical expansion when folded stacks contain deep call chains. It is not a
qperf measurement and should not be used for performance conclusions.
"""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path


BASE_STACKS: list[tuple[list[str], int]] = [
    (
        [
            "os::syscall::task::sys_execve",
            "os::task::spawn_user_task",
            "os::task::Task::exec",
            "os::mm::memory_set::MemorySet::from_elf",
            "os::mm::memory_set::MemorySet::map_area",
            "alloc::sync::Arc<T,A>::drop_slow",
            "core::ptr::drop_in_place<os::mm::memory_set::MemorySet>",
            "core::ptr::drop_in_place<alloc::sync::Arc<spin::rw_lock::RwLock<os::mm::memory_set::MemorySet>>>",
            "alloc::collections::btree::map::BTreeMap<K,V,A>::drop",
            "alloc::collections::btree::node::NodeRef::drop",
            "alloc::collections::btree::node::Handle::drop",
            "core::ptr::drop_in_place<alloc::collections::btree::node::Handle<alloc::alloc::Global>>",
            "core::mem::drop",
            "os::mm::area::MapArea::drop",
            "os::mm::page::Page::drop",
            "ax_alloc::buddy_slab::GlobalAllocator::dealloc",
        ],
        5000,
    ),
    (
        [
            "os::syscall::fs::sys_read",
            "starry_process::fd_manager::FileLike::read",
            "ax_fs_ng::fs::ext4::Ext4File::read_at",
            "rsext4::file::File::read_at",
            "rsext4::cache::data_block::DataBlockCache::get",
            "rsext4::cache::data_block::DataBlockCache::evict_lru",
            "alloc::collections::btree::map::BTreeMap<K,V,A>::get",
            "alloc::collections::btree::search::search_tree",
            "rd_block::BlockDevice::read_blocks",
            "ax_driver::virtio::block::VirtIoBlkDev::read_block",
            "virtio_drivers::device::blk::VirtIOBlk::read_blocks",
            "virtio_drivers::queue::VirtQueue::add_notify_wait_pop",
            "virtio_drivers::queue::VirtQueue::add",
            "virtio_drivers::transport::pci::PciTransport::notify",
            "virtio_drivers::queue::VirtQueue::pop_used",
        ],
        3600,
    ),
    (
        [
            "os::syscall::net::sys_recvfrom",
            "ax_net_ng::tcp::Socket::recv",
            "smoltcp::socket::tcp::Socket::recv_slice",
            "rd_net::VirtioNetDriver::receive",
            "ax_driver::virtio::net::VirtIoNetDev::receive",
            "virtio_drivers::device::net::VirtIONetRaw::receive",
            "virtio_drivers::queue::VirtQueue::pop_used",
            "ax_driver::virtio::net::RxBuffer::copy_within",
            "compiler_builtins::mem::memmove",
            "compiler_builtins::mem::memcpy",
        ],
        2600,
    ),
    (
        [
            "os::syscall::task::sys_waitpid",
            "starry_process::thread::Thread::wait",
            "ax_task::wait_queue::WaitQueue::wait_until",
            "ax_task::run_queue::AxRunQueue::block_current",
            "ax_task::run_queue::AxRunQueue::resched",
            "ax_task::run_queue::AxRunQueue::switch_to",
            "riscv::register::sstatus::set_spie",
            "ax_hal::arch::context::TaskContext::switch",
        ],
        1800,
    ),
]


def variant_stack(base: list[str], index: int) -> list[str]:
    stack = list(base)
    if "BTreeMap<K,V,A>::drop" in stack[-8]:
        stack.insert(-4, f"alloc::collections::btree::node::drop_child::{index % 17}")
    if index % 5 == 0:
        stack.append("core::ptr::drop_in_place<alloc::boxed::Box<[u8]>>")
    if index % 7 == 0:
        stack.append("compiler_builtins::mem::memset")
    if index % 11 == 0:
        stack.insert(1, "os::task::current")
    return stack


def write_folded(path: Path, variants: int) -> tuple[int, int]:
    path.parent.mkdir(parents=True, exist_ok=True)
    max_depth = 0
    total_samples = 0
    with path.open("w", encoding="utf-8") as output:
        for base, count in BASE_STACKS:
            for index in range(variants):
                stack = variant_stack(base, index)
                sample_count = max(1, count // variants + (index % 13))
                output.write(f"{';'.join(stack)} {sample_count}\n")
                max_depth = max(max_depth, len(stack))
                total_samples += sample_count
    return max_depth, total_samples


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", default="target/qperf-long-chain-demo")
    parser.add_argument("--variants", type=int, default=80)
    parser.add_argument("--no-svg", action="store_true")
    args = parser.parse_args()

    out_dir = Path(args.output_dir)
    folded = out_dir / "stack.folded"
    svg = out_dir / "flamegraph.svg"
    max_depth, total_samples = write_folded(folded, args.variants)

    if not args.no_svg:
        qperf_root = Path(__file__).resolve().parents[1]
        subprocess.run(
            [
                "cargo",
                "run",
                "--manifest-path",
                str(qperf_root / "analyzer/Cargo.toml"),
                "--features",
                "flamegraph",
                "--",
                "diff",
                "--baseline",
                str(folded),
                "--compare",
                str(folded),
                "--top",
                "0",
                "--flamegraph",
                str(svg),
            ],
            check=True,
        )

    print(f"folded: {folded}")
    print(f"flamegraph: {svg}")
    print(f"synthetic: true")
    print(f"max_depth: {max_depth}")
    print(f"total_samples: {total_samples}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

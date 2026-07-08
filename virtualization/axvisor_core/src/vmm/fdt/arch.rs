#[cfg(not(target_arch = "riscv64"))]
use alloc::vec::Vec;

#[cfg(not(target_arch = "riscv64"))]
use ax_errno::AxResult;
#[cfg(not(target_arch = "riscv64"))]
use fdt_parser::Node;

#[cfg(not(target_arch = "riscv64"))]
use super::vm_fdt::{FdtWriter, FdtWriterNode};

#[cfg(target_arch = "riscv64")]
pub(super) type FdtRewriter<'a> = super::riscv::RiscvFdtRewriter<'a>;

#[cfg(not(target_arch = "riscv64"))]
pub(super) struct FdtRewriter;

#[cfg(not(target_arch = "riscv64"))]
impl FdtRewriter {
    pub(super) fn new(_nodes: &[Node<'_>]) -> Self {
        Self
    }

    pub(super) fn before_node(
        &mut self,
        _node_path: &str,
        _phys_cpu_ids: &[usize],
        _previous_node_level: &mut usize,
        _node_stack: &mut Vec<FdtWriterNode>,
        _fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        Ok(())
    }

    pub(super) fn observe_cpu_node(&mut self, _node: Node<'_>, _node_path: &str) {}

    pub(super) fn write_rewritten_property(
        &self,
        _node: &Node<'_>,
        _node_path: &str,
        _prop_name: &str,
        _prop_value: &[u8],
        _phys_cpu_ids: &[usize],
        _fdt_writer: &mut FdtWriter,
    ) -> AxResult<bool> {
        Ok(false)
    }

    pub(super) fn after_node(&mut self, _node_path: &str) {}

    pub(super) fn finish(
        &mut self,
        _phys_cpu_ids: &[usize],
        _previous_node_level: &mut usize,
        _node_stack: &mut Vec<FdtWriterNode>,
        _fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        Ok(())
    }
}

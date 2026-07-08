use alloc::{collections::BTreeMap, string::String, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use fdt_parser::Node;

use super::{
    create::{cpu_node_id, fdt_write_err, should_skip_guest_cpu_prop},
    vm_fdt::{FdtWriter, FdtWriterNode},
};

/// RISC-V guest FDT rewriting backend.
///
/// RISC-V CPU topology is described by `/cpus/cpu@...` nodes plus one local
/// interrupt-controller phandle per hart. The PLIC references those phandles in
/// `interrupts-extended`, so adding guest harts also requires rebuilding the
/// PLIC context list.
pub(super) struct RiscvFdtRewriter<'a> {
    cpu_template: Option<Node<'a>>,
    intc_template: Option<Node<'a>>,
    copied_cpu_ids: Vec<usize>,
    cpu_intc_phandles: BTreeMap<usize, u32>,
    next_phandle: u32,
    added_missing_cpus: bool,
    previous_node_path: String,
}

impl<'a> RiscvFdtRewriter<'a> {
    pub(super) fn new(nodes: &[Node<'a>]) -> Self {
        let mut max_phandle = 0;
        for node in nodes {
            for prop in node.propertys() {
                if matches!(prop.name, "phandle" | "linux,phandle")
                    && let Some(phandle) = read_be_u32(prop.raw_value())
                {
                    max_phandle = max_phandle.max(phandle);
                }
            }
        }

        Self {
            cpu_template: None,
            intc_template: None,
            copied_cpu_ids: Vec::new(),
            cpu_intc_phandles: BTreeMap::new(),
            next_phandle: max_phandle.saturating_add(1),
            added_missing_cpus: false,
            previous_node_path: String::new(),
        }
    }

    pub(super) fn before_node(
        &mut self,
        node_path: &str,
        phys_cpu_ids: &[usize],
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        if self.previous_node_path.starts_with("/cpus")
            && !node_path.starts_with("/cpus")
            && !self.added_missing_cpus
        {
            self.close_cpu_children(previous_node_level, node_stack, fdt_writer)?;
            self.add_missing_cpu_nodes(phys_cpu_ids, fdt_writer)?;
        }
        Ok(())
    }

    pub(super) fn observe_cpu_node(&mut self, node: Node<'a>, node_path: &str) {
        if let Some(cpu_id) = cpu_node_id(node_path) {
            self.cpu_template.get_or_insert(node.clone());
            if !self.copied_cpu_ids.contains(&cpu_id) {
                self.copied_cpu_ids.push(cpu_id);
            }
        }

        if node_path.contains("/interrupt-controller") {
            self.intc_template.get_or_insert(node.clone());
            if let Some(cpu_id) = cpu_node_id(node_path)
                && let Some(phandle) = node
                    .propertys()
                    .find(|prop| prop.name == "phandle")
                    .and_then(|p| read_be_u32(p.raw_value()))
            {
                self.cpu_intc_phandles.insert(cpu_id, phandle);
            }
        }
    }

    pub(super) fn write_rewritten_property(
        &self,
        node: &Node,
        node_path: &str,
        prop_name: &str,
        prop_value: &[u8],
        phys_cpu_ids: &[usize],
        fdt_writer: &mut FdtWriter,
    ) -> AxResult<bool> {
        if node_path.starts_with("/cpus/cpu@")
            && prop_name == "reg"
            && let Some(cpu_id) = cpu_node_id(node_path)
        {
            write_cpu_reg_property(fdt_writer, prop_value, cpu_id)?;
            return Ok(true);
        }

        if is_riscv_plic_node(node)
            && prop_name == "interrupts-extended"
            && let Some(context_interrupts) = plic_context_interrupts(prop_value)
        {
            self.write_plic_interrupts_extended(fdt_writer, phys_cpu_ids, &context_interrupts)?;
            return Ok(true);
        }

        Ok(false)
    }

    pub(super) fn after_node(&mut self, node_path: &str) {
        self.previous_node_path.clear();
        self.previous_node_path.push_str(node_path);
    }

    pub(super) fn finish(
        &mut self,
        phys_cpu_ids: &[usize],
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        if self.previous_node_path.starts_with("/cpus") && !self.added_missing_cpus {
            self.close_cpu_children(previous_node_level, node_stack, fdt_writer)?;
            self.add_missing_cpu_nodes(phys_cpu_ids, fdt_writer)?;
        }
        Ok(())
    }

    fn close_cpu_children(
        &mut self,
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        while *previous_node_level > 2 {
            let end_node = node_stack
                .pop()
                .ok_or_else(|| ax_err_type!(InvalidData, "Guest FDT CPU node stack is empty"))?;
            fdt_writer.end_node(end_node).map_err(fdt_write_err)?;
            *previous_node_level -= 1;
        }
        Ok(())
    }

    fn add_missing_cpu_nodes(
        &mut self,
        phys_cpu_ids: &[usize],
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        self.added_missing_cpus = true;

        let Some(cpu_template) = self.cpu_template.clone() else {
            return Ok(());
        };
        let Some(intc_template) = self.intc_template.clone() else {
            return Ok(());
        };

        for &cpu_id in phys_cpu_ids {
            if self.copied_cpu_ids.contains(&cpu_id) {
                continue;
            }

            let cpu = fdt_writer
                .begin_node(&alloc::format!("cpu@{cpu_id:x}"))
                .map_err(fdt_write_err)?;
            for prop in cpu_template.propertys() {
                if should_skip_guest_cpu_prop(prop.name) {
                    continue;
                }
                if prop.name == "reg" {
                    write_cpu_reg_property(fdt_writer, prop.raw_value(), cpu_id)?;
                } else if !matches!(prop.name, "phandle" | "linux,phandle") {
                    fdt_writer
                        .property(prop.name, prop.raw_value())
                        .map_err(fdt_write_err)?;
                }
            }

            let intc_phandle = self.alloc_phandle();
            let intc = fdt_writer
                .begin_node(intc_template.name())
                .map_err(fdt_write_err)?;
            for prop in intc_template.propertys() {
                match prop.name {
                    "phandle" | "linux,phandle" => fdt_writer
                        .property_u32(prop.name, intc_phandle)
                        .map_err(fdt_write_err)?,
                    _ => fdt_writer
                        .property(prop.name, prop.raw_value())
                        .map_err(fdt_write_err)?,
                }
            }
            fdt_writer.end_node(intc).map_err(fdt_write_err)?;
            fdt_writer.end_node(cpu).map_err(fdt_write_err)?;

            self.cpu_intc_phandles.insert(cpu_id, intc_phandle);
            self.copied_cpu_ids.push(cpu_id);
        }

        Ok(())
    }

    fn write_plic_interrupts_extended(
        &self,
        fdt_writer: &mut FdtWriter,
        phys_cpu_ids: &[usize],
        context_interrupts: &[u32],
    ) -> AxResult {
        let mut cells = Vec::with_capacity(phys_cpu_ids.len() * context_interrupts.len() * 2);
        for cpu_id in phys_cpu_ids {
            let Some(phandle) = self.cpu_intc_phandles.get(cpu_id) else {
                continue;
            };
            for interrupt in context_interrupts {
                cells.push(*phandle);
                cells.push(*interrupt);
            }
        }

        fdt_writer
            .property_array_u32("interrupts-extended", &cells)
            .map_err(fdt_write_err)?;
        Ok(())
    }

    fn alloc_phandle(&mut self) -> u32 {
        let phandle = self.next_phandle;
        self.next_phandle = self.next_phandle.saturating_add(1);
        phandle
    }
}

fn is_riscv_plic_node(node: &Node) -> bool {
    node.compatibles()
        .any(|compat| compat.contains("riscv,plic"))
}

fn plic_context_interrupts(raw: &[u8]) -> Option<Vec<u32>> {
    let mut cells = raw.chunks_exact(4).filter_map(read_be_u32);
    let first_phandle = cells.next()?;
    let first_interrupt = cells.next()?;
    let mut interrupts = vec![first_interrupt];

    while let Some(phandle) = cells.next() {
        let Some(interrupt) = cells.next() else {
            break;
        };
        if phandle != first_phandle {
            break;
        }
        interrupts.push(interrupt);
    }

    Some(interrupts)
}

fn write_cpu_reg_property(
    fdt_writer: &mut FdtWriter,
    template_value: &[u8],
    cpu_id: usize,
) -> AxResult {
    match template_value.len() {
        4 => fdt_writer
            .property_u32("reg", cpu_id as u32)
            .map_err(fdt_write_err),
        8 => fdt_writer
            .property_u64("reg", cpu_id as u64)
            .map_err(fdt_write_err),
        _ => fdt_writer
            .property("reg", template_value)
            .map_err(fdt_write_err),
    }
}

fn read_be_u32(raw: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = raw.get(..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

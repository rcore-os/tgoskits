//! RISC-V PLIC guest FDT normalization.

use alloc::{string::String, vec::Vec};

use fdt_edit::{Fdt, Phandle};

const PLIC_COMPATIBLES: &[&str] = &["riscv,plic0", "sifive,plic-1.0.0", "starfive,jh7110-plic"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PlicFdtError {
    #[error("matching PLIC node is missing from the host or guest tree")]
    MissingHostNode,
    #[error("PLIC interrupts-extended property is missing")]
    MissingInterruptsExtended,
    #[error("PLIC interrupts-extended is not an array of complete u32 cells")]
    InvalidCellEncoding,
    #[error("interrupt provider phandle {phandle:#x} is missing from the host tree")]
    MissingProvider { phandle: u32 },
    #[error("interrupt provider phandle {phandle:#x} has no #interrupt-cells")]
    MissingInterruptCells { phandle: u32 },
    #[error("interrupt provider phandle {phandle:#x} has zero #interrupt-cells")]
    ZeroInterruptCells { phandle: u32 },
    #[error(
        "interrupt tuple for provider {phandle:#x} needs {expected_cells} cells but only \
         {remaining_cells} remain"
    )]
    TruncatedTuple {
        phandle: u32,
        expected_cells: usize,
        remaining_cells: usize,
    },
}

pub(crate) fn normalize_interrupts_extended(
    host_fdt: &Fdt,
    guest_fdt: &mut Fdt,
) -> Result<(), PlicFdtError> {
    let plic_paths = guest_fdt
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = guest_fdt.node(node_id)?;
            node.compatibles()
                .any(|compatible| PLIC_COMPATIBLES.contains(&compatible))
                .then(|| guest_fdt.path_of(node_id))
        })
        .collect::<Vec<String>>();

    for path in plic_paths {
        normalize_plic_node(host_fdt, guest_fdt, &path)?;
    }
    Ok(())
}

fn normalize_plic_node(
    host_fdt: &Fdt,
    guest_fdt: &mut Fdt,
    path: &str,
) -> Result<(), PlicFdtError> {
    let guest_node_id = guest_fdt
        .get_by_path_id(path)
        .ok_or(PlicFdtError::MissingHostNode)?;
    if guest_fdt
        .node(guest_node_id)
        .and_then(|node| node.get_property("interrupts-extended"))
        .is_none()
    {
        return Ok(());
    }

    let host_node_id = host_fdt
        .get_by_path_id(path)
        .ok_or(PlicFdtError::MissingHostNode)?;
    let host_property = host_fdt
        .node(host_node_id)
        .and_then(|node| node.get_property("interrupts-extended"))
        .ok_or(PlicFdtError::MissingInterruptsExtended)?;
    if host_property.data.len() % size_of::<u32>() != 0 {
        return Err(PlicFdtError::InvalidCellEncoding);
    }

    let host_cells = host_property.get_u32_iter().collect::<Vec<_>>();
    let retained_cells = retain_guest_provider_tuples(host_fdt, guest_fdt, &host_cells)?;
    guest_fdt
        .node_mut(guest_node_id)
        .and_then(|node| node.get_property_mut("interrupts-extended"))
        .ok_or(PlicFdtError::MissingInterruptsExtended)?
        .set_u32_ls(&retained_cells);
    Ok(())
}

fn retain_guest_provider_tuples(
    host_fdt: &Fdt,
    guest_fdt: &Fdt,
    cells: &[u32],
) -> Result<Vec<u32>, PlicFdtError> {
    let mut retained = Vec::new();
    let mut offset = 0;

    while offset < cells.len() {
        let phandle = cells[offset];
        let provider = host_fdt
            .get_by_phandle(Phandle::from(phandle))
            .ok_or(PlicFdtError::MissingProvider { phandle })?;
        let interrupt_cells = provider
            .as_node()
            .get_property("#interrupt-cells")
            .and_then(|property| property.get_u32())
            .ok_or(PlicFdtError::MissingInterruptCells { phandle })?
            as usize;
        if interrupt_cells == 0 {
            return Err(PlicFdtError::ZeroInterruptCells { phandle });
        }

        let tuple_end = offset + 1 + interrupt_cells;
        if tuple_end > cells.len() {
            return Err(PlicFdtError::TruncatedTuple {
                phandle,
                expected_cells: interrupt_cells,
                remaining_cells: cells.len().saturating_sub(offset + 1),
            });
        }
        if contains_phandle(guest_fdt, phandle) {
            retained.extend_from_slice(&cells[offset..tuple_end]);
        }
        offset = tuple_end;
    }

    Ok(retained)
}

fn contains_phandle(fdt: &Fdt, raw_phandle: u32) -> bool {
    fdt.iter_node_ids().any(|node_id| {
        fdt.node(node_id)
            .and_then(|node| node.phandle())
            .is_some_and(|phandle| phandle.raw() == raw_phandle)
    })
}

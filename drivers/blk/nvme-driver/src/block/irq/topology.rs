use super::*;

pub(in crate::block) fn vector_for_queue(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> Option<u16> {
    if msix_interrupts {
        vectors.get(queue_id).copied()
    } else {
        Some(0)
    }
}

pub(in crate::block) fn queue_interrupt_sources(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> IdList {
    let mut sources = IdList::none();
    if let Some(source_id) = vector_for_queue(msix_interrupts, vectors, queue_id) {
        sources.insert(usize::from(source_id));
    }
    sources
}

pub(in crate::block) fn source_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    source_id: usize,
    queue_bits: u64,
) -> u64 {
    if !msix_interrupts {
        return if source_id == 0 { queue_bits } else { 0 };
    }

    let mut bits = 0;
    for queue_id in 0..u64::BITS as usize {
        if queue_bits & (1 << queue_id) == 0 {
            continue;
        }
        if vector_for_queue(msix_interrupts, vectors, queue_id) == Some(source_id as u16) {
            bits |= 1 << queue_id;
        }
    }
    bits
}

pub(in crate::block) fn irq_sources_from_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_bits: u64,
) -> IrqSourceList {
    if !msix_interrupts {
        return vec![IrqSourceInfo::legacy(IdList::from_bits(queue_bits))];
    }

    let mut sources = Vec::new();
    for vector in unique_interrupt_vectors(vectors) {
        let queues = source_queue_bits(msix_interrupts, vectors, usize::from(vector), queue_bits);
        if queues != 0 {
            sources.push(IrqSourceInfo::new(
                usize::from(vector),
                IdList::from_bits(queues),
            ));
        }
    }
    sources
}

pub(in crate::block) fn unique_interrupt_vectors(vectors: &[u16]) -> Vec<u16> {
    let mut unique = Vec::new();
    for vector in vectors {
        if !unique.contains(vector) {
            unique.push(*vector);
        }
    }
    unique
}

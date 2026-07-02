// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ax_errno::AxResult;
use tock_registers::interfaces::{Readable, Writeable};

use super::SvmVcpu;
use crate::{
    kvm::{KVM_REGS_SIZE, KVM_SREGS_SIZE, KvmDtable, KvmRegs, KvmSegment, KvmSregs},
    svm::vmcb::VmcbSegment,
};

impl SvmVcpu {
    pub(super) fn encode_kvm_regs(&self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_REGS_SIZE {
            return ax_errno::ax_err!(InvalidInput);
        }
        let regs = self.regs();
        let state = unsafe { &self.vmcb.as_vmcb_ref().state };
        KvmRegs {
            rax: regs.rax,
            rbx: regs.rbx,
            rcx: regs.rcx,
            rdx: regs.rdx,
            rsi: regs.rsi,
            rdi: regs.rdi,
            rsp: state.rsp.get(),
            rbp: regs.rbp,
            r8: regs.r8,
            r9: regs.r9,
            r10: regs.r10,
            r11: regs.r11,
            r12: regs.r12,
            r13: regs.r13,
            r14: regs.r14,
            r15: regs.r15,
            rip: state.rip.get(),
            rflags: state.rflags.get(),
        }
        .encode(buf)
    }

    pub(super) fn decode_kvm_regs(&mut self, buf: &[u8]) -> AxResult {
        let kvm_regs = KvmRegs::decode(buf)?;
        let regs = self.regs_mut();
        regs.rax = kvm_regs.rax;
        regs.rbx = kvm_regs.rbx;
        regs.rcx = kvm_regs.rcx;
        regs.rdx = kvm_regs.rdx;
        regs.rsi = kvm_regs.rsi;
        regs.rdi = kvm_regs.rdi;
        regs.rbp = kvm_regs.rbp;
        regs.r8 = kvm_regs.r8;
        regs.r9 = kvm_regs.r9;
        regs.r10 = kvm_regs.r10;
        regs.r11 = kvm_regs.r11;
        regs.r12 = kvm_regs.r12;
        regs.r13 = kvm_regs.r13;
        regs.r14 = kvm_regs.r14;
        regs.r15 = kvm_regs.r15;
        let state = unsafe { &mut self.vmcb.as_vmcb().state };
        state.rsp.set(kvm_regs.rsp);
        state.rip.set(kvm_regs.rip);
        state.rflags.set(kvm_regs.rflags);
        Ok(())
    }

    pub(super) fn encode_kvm_sregs(&self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_SREGS_SIZE {
            return ax_errno::ax_err!(InvalidInput);
        }
        let state = unsafe { &self.vmcb.as_vmcb_ref().state };
        KvmSregs {
            cs: read_segment(&state.cs),
            ds: read_segment(&state.ds),
            es: read_segment(&state.es),
            fs: read_segment(&state.fs),
            gs: read_segment(&state.gs),
            ss: read_segment(&state.ss),
            tr: read_segment(&state.tr),
            ldt: read_segment(&state.ldtr),
            gdt: KvmDtable {
                base: state.gdtr.base.get(),
                limit: state.gdtr.limit.get() as u16,
            },
            idt: KvmDtable {
                base: state.idtr.base.get(),
                limit: state.idtr.limit.get() as u16,
            },
            cr0: state.cr0.get(),
            cr2: state.cr2.get(),
            cr3: state.cr3.get(),
            cr4: state.cr4.get(),
            cr8: 0,
            efer: self.guest_visible_efer(),
            apic_base: 0,
            interrupt_bitmap: [0; 4],
        }
        .encode(buf)
    }

    pub(super) fn decode_kvm_sregs(&mut self, buf: &[u8]) -> AxResult {
        let sregs = KvmSregs::decode(buf)?;
        {
            let state = unsafe { &mut self.vmcb.as_vmcb().state };
            write_segment(&mut state.cs, sregs.cs);
            write_segment(&mut state.ds, sregs.ds);
            write_segment(&mut state.es, sregs.es);
            write_segment(&mut state.fs, sregs.fs);
            write_segment(&mut state.gs, sregs.gs);
            write_segment(&mut state.ss, sregs.ss);
            write_segment(&mut state.tr, sregs.tr);
            write_segment(&mut state.ldtr, sregs.ldt);
            state.gdtr.base.set(sregs.gdt.base);
            state.gdtr.limit.set(sregs.gdt.limit as u32);
            state.idtr.base.set(sregs.idt.base);
            state.idtr.limit.set(sregs.idt.limit as u32);
            state.cr2.set(sregs.cr2);
        }
        self.set_cr(0, sregs.cr0)?;
        self.set_cr(3, sregs.cr3)?;
        self.set_cr(4, sregs.cr4)?;
        self.set_guest_efer(sregs.efer);
        Ok(())
    }
}

fn read_segment(segment: &VmcbSegment) -> KvmSegment {
    let attr = segment.attr.get();
    KvmSegment {
        base: segment.base.get(),
        limit: segment.limit.get(),
        selector: segment.selector.get(),
        type_: (attr & 0xf) as u8,
        s: ((attr >> 4) & 1) as u8,
        dpl: ((attr >> 5) & 0x3) as u8,
        present: ((attr >> 7) & 1) as u8,
        avl: ((attr >> 8) & 1) as u8,
        l: ((attr >> 9) & 1) as u8,
        db: ((attr >> 10) & 1) as u8,
        g: ((attr >> 11) & 1) as u8,
        unusable: u8::from(attr & (1 << 7) == 0),
    }
}

fn write_segment(segment: &mut VmcbSegment, value: KvmSegment) {
    let present = value.present & u8::from(value.unusable == 0);
    let attr = (value.type_ as u16)
        | ((value.s as u16) << 4)
        | ((value.dpl as u16) << 5)
        | ((present as u16) << 7)
        | ((value.avl as u16) << 8)
        | ((value.l as u16) << 9)
        | ((value.db as u16) << 10)
        | ((value.g as u16) << 11);

    segment.selector.set(value.selector);
    segment.base.set(value.base);
    segment.limit.set(value.limit);
    segment.attr.set(attr);
}

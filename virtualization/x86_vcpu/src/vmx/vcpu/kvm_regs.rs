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

use super::VmxVcpu;
use crate::{
    kvm::{
        KVM_REGS_SIZE, KVM_SREGS_SIZE, KvmDtable, KvmRegs, KvmSegment, KvmSregs, map_kvm_uapi_error,
    },
    vmx::{
        vmcs,
        vmcs::{VmcsGuest16, VmcsGuest32, VmcsGuest64, VmcsGuestNW},
    },
};

impl VmxVcpu {
    pub(super) fn encode_kvm_regs(&self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_REGS_SIZE {
            return ax_errno::ax_err!(InvalidInput);
        }
        self.bind_to_current_processor()?;
        let result = self.encode_kvm_regs_loaded(buf);
        finish_vmcs_access(self, result)
    }

    fn encode_kvm_regs_loaded(&self, buf: &mut [u8]) -> AxResult {
        let regs = self.regs();
        KvmRegs {
            rax: regs.rax,
            rbx: regs.rbx,
            rcx: regs.rcx,
            rdx: regs.rdx,
            rsi: regs.rsi,
            rdi: regs.rdi,
            rsp: self.stack_pointer() as u64,
            rbp: regs.rbp,
            r8: regs.r8,
            r9: regs.r9,
            r10: regs.r10,
            r11: regs.r11,
            r12: regs.r12,
            r13: regs.r13,
            r14: regs.r14,
            r15: regs.r15,
            rip: VmcsGuestNW::RIP.read()? as u64,
            rflags: VmcsGuestNW::RFLAGS.read()? as u64,
        }
        .encode(buf)
        .map_err(map_kvm_uapi_error)
    }

    pub(super) fn decode_kvm_regs(&mut self, buf: &[u8]) -> AxResult {
        self.bind_to_current_processor()?;
        let result = self.decode_kvm_regs_loaded(buf);
        finish_vmcs_access(self, result)
    }

    fn decode_kvm_regs_loaded(&mut self, buf: &[u8]) -> AxResult {
        let kvm_regs = KvmRegs::decode(buf).map_err(map_kvm_uapi_error)?;
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
        self.set_stack_pointer(kvm_regs.rsp as usize);
        VmcsGuestNW::RIP.write(kvm_regs.rip as usize)?;
        VmcsGuestNW::RFLAGS.write(kvm_regs.rflags as usize)?;
        Ok(())
    }

    pub(super) fn encode_kvm_sregs(&self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_SREGS_SIZE {
            return ax_errno::ax_err!(InvalidInput);
        }
        self.bind_to_current_processor()?;
        let result = self.encode_kvm_sregs_loaded(buf);
        finish_vmcs_access(self, result)
    }

    fn encode_kvm_sregs_loaded(&self, buf: &mut [u8]) -> AxResult {
        KvmSregs {
            cs: read_segment(
                VmcsGuest16::CS_SELECTOR,
                VmcsGuestNW::CS_BASE,
                VmcsGuest32::CS_LIMIT,
                VmcsGuest32::CS_ACCESS_RIGHTS,
            )?,
            ds: read_segment(
                VmcsGuest16::DS_SELECTOR,
                VmcsGuestNW::DS_BASE,
                VmcsGuest32::DS_LIMIT,
                VmcsGuest32::DS_ACCESS_RIGHTS,
            )?,
            es: read_segment(
                VmcsGuest16::ES_SELECTOR,
                VmcsGuestNW::ES_BASE,
                VmcsGuest32::ES_LIMIT,
                VmcsGuest32::ES_ACCESS_RIGHTS,
            )?,
            fs: read_segment(
                VmcsGuest16::FS_SELECTOR,
                VmcsGuestNW::FS_BASE,
                VmcsGuest32::FS_LIMIT,
                VmcsGuest32::FS_ACCESS_RIGHTS,
            )?,
            gs: read_segment(
                VmcsGuest16::GS_SELECTOR,
                VmcsGuestNW::GS_BASE,
                VmcsGuest32::GS_LIMIT,
                VmcsGuest32::GS_ACCESS_RIGHTS,
            )?,
            ss: read_segment(
                VmcsGuest16::SS_SELECTOR,
                VmcsGuestNW::SS_BASE,
                VmcsGuest32::SS_LIMIT,
                VmcsGuest32::SS_ACCESS_RIGHTS,
            )?,
            tr: read_segment(
                VmcsGuest16::TR_SELECTOR,
                VmcsGuestNW::TR_BASE,
                VmcsGuest32::TR_LIMIT,
                VmcsGuest32::TR_ACCESS_RIGHTS,
            )?,
            ldt: read_segment(
                VmcsGuest16::LDTR_SELECTOR,
                VmcsGuestNW::LDTR_BASE,
                VmcsGuest32::LDTR_LIMIT,
                VmcsGuest32::LDTR_ACCESS_RIGHTS,
            )?,
            gdt: KvmDtable {
                base: VmcsGuestNW::GDTR_BASE.read()? as u64,
                limit: VmcsGuest32::GDTR_LIMIT.read()? as u16,
            },
            idt: KvmDtable {
                base: VmcsGuestNW::IDTR_BASE.read()? as u64,
                limit: VmcsGuest32::IDTR_LIMIT.read()? as u16,
            },
            cr0: self.cr(0) as u64,
            cr2: 0,
            cr3: self.cr(3) as u64,
            cr4: self.cr(4) as u64,
            cr8: 0,
            efer: VmcsGuest64::IA32_EFER.read()?,
            apic_base: 0,
            interrupt_bitmap: [0; 4],
        }
        .encode(buf)
        .map_err(map_kvm_uapi_error)
    }

    pub(super) fn decode_kvm_sregs(&mut self, buf: &[u8]) -> AxResult {
        self.bind_to_current_processor()?;
        let result = self.decode_kvm_sregs_loaded(buf);
        finish_vmcs_access(self, result)
    }

    fn decode_kvm_sregs_loaded(&mut self, buf: &[u8]) -> AxResult {
        let sregs = KvmSregs::decode(buf).map_err(map_kvm_uapi_error)?;
        write_segment(
            VmcsGuest16::CS_SELECTOR,
            VmcsGuestNW::CS_BASE,
            VmcsGuest32::CS_LIMIT,
            VmcsGuest32::CS_ACCESS_RIGHTS,
            sregs.cs,
        )?;
        write_segment(
            VmcsGuest16::DS_SELECTOR,
            VmcsGuestNW::DS_BASE,
            VmcsGuest32::DS_LIMIT,
            VmcsGuest32::DS_ACCESS_RIGHTS,
            sregs.ds,
        )?;
        write_segment(
            VmcsGuest16::ES_SELECTOR,
            VmcsGuestNW::ES_BASE,
            VmcsGuest32::ES_LIMIT,
            VmcsGuest32::ES_ACCESS_RIGHTS,
            sregs.es,
        )?;
        write_segment(
            VmcsGuest16::FS_SELECTOR,
            VmcsGuestNW::FS_BASE,
            VmcsGuest32::FS_LIMIT,
            VmcsGuest32::FS_ACCESS_RIGHTS,
            sregs.fs,
        )?;
        write_segment(
            VmcsGuest16::GS_SELECTOR,
            VmcsGuestNW::GS_BASE,
            VmcsGuest32::GS_LIMIT,
            VmcsGuest32::GS_ACCESS_RIGHTS,
            sregs.gs,
        )?;
        write_segment(
            VmcsGuest16::SS_SELECTOR,
            VmcsGuestNW::SS_BASE,
            VmcsGuest32::SS_LIMIT,
            VmcsGuest32::SS_ACCESS_RIGHTS,
            sregs.ss,
        )?;
        write_segment(
            VmcsGuest16::TR_SELECTOR,
            VmcsGuestNW::TR_BASE,
            VmcsGuest32::TR_LIMIT,
            VmcsGuest32::TR_ACCESS_RIGHTS,
            sregs.tr,
        )?;
        write_segment(
            VmcsGuest16::LDTR_SELECTOR,
            VmcsGuestNW::LDTR_BASE,
            VmcsGuest32::LDTR_LIMIT,
            VmcsGuest32::LDTR_ACCESS_RIGHTS,
            sregs.ldt,
        )?;
        VmcsGuestNW::GDTR_BASE.write(sregs.gdt.base as usize)?;
        VmcsGuest32::GDTR_LIMIT.write(sregs.gdt.limit as u32)?;
        VmcsGuestNW::IDTR_BASE.write(sregs.idt.base as usize)?;
        VmcsGuest32::IDTR_LIMIT.write(sregs.idt.limit as u32)?;
        self.set_cr(0, sregs.cr0);
        self.set_cr(3, sregs.cr3);
        self.set_cr(4, sregs.cr4);
        VmcsGuest64::IA32_EFER.write(sregs.efer)?;
        vmcs::update_efer()?;
        Ok(())
    }
}

fn finish_vmcs_access(vcpu: &VmxVcpu, result: AxResult) -> AxResult {
    let unbind_result = vcpu.unbind_from_current_processor();
    match result {
        Ok(()) => unbind_result,
        Err(err) => {
            let _ = unbind_result;
            Err(err)
        }
    }
}

fn read_segment(
    selector_field: VmcsGuest16,
    base_field: VmcsGuestNW,
    limit_field: VmcsGuest32,
    access_rights_field: VmcsGuest32,
) -> AxResult<KvmSegment> {
    Ok(KvmSegment::from_access_rights(
        selector_field.read()?,
        base_field.read()? as u64,
        limit_field.read()?,
        access_rights_field.read()?,
    ))
}

fn write_segment(
    selector_field: VmcsGuest16,
    base_field: VmcsGuestNW,
    limit_field: VmcsGuest32,
    access_rights_field: VmcsGuest32,
    segment: KvmSegment,
) -> AxResult {
    selector_field.write(segment.selector)?;
    base_field.write(segment.base as usize)?;
    limit_field.write(segment.limit)?;
    access_rights_field.write(segment.access_rights())?;
    Ok(())
}

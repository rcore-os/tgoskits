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

use ax_errno::{AxError, AxResult};

pub(crate) const KVM_REGS_SIZE: usize = 18 * 8;
pub(crate) const KVM_SREGS_SIZE: usize = 312;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KvmRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KvmSegment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
    pub unusable: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KvmDtable {
    pub base: u64,
    pub limit: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KvmSregs {
    pub cs: KvmSegment,
    pub ds: KvmSegment,
    pub es: KvmSegment,
    pub fs: KvmSegment,
    pub gs: KvmSegment,
    pub ss: KvmSegment,
    pub tr: KvmSegment,
    pub ldt: KvmSegment,
    pub gdt: KvmDtable,
    pub idt: KvmDtable,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
    pub interrupt_bitmap: [u64; 4],
}

impl KvmRegs {
    pub(crate) fn decode(buf: &[u8]) -> AxResult<Self> {
        if buf.len() != KVM_REGS_SIZE {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            rax: read_u64(buf, 0),
            rbx: read_u64(buf, 8),
            rcx: read_u64(buf, 16),
            rdx: read_u64(buf, 24),
            rsi: read_u64(buf, 32),
            rdi: read_u64(buf, 40),
            rsp: read_u64(buf, 48),
            rbp: read_u64(buf, 56),
            r8: read_u64(buf, 64),
            r9: read_u64(buf, 72),
            r10: read_u64(buf, 80),
            r11: read_u64(buf, 88),
            r12: read_u64(buf, 96),
            r13: read_u64(buf, 104),
            r14: read_u64(buf, 112),
            r15: read_u64(buf, 120),
            rip: read_u64(buf, 128),
            rflags: read_u64(buf, 136),
        })
    }

    pub(crate) fn encode(self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_REGS_SIZE {
            return Err(AxError::InvalidInput);
        }
        write_u64(buf, 0, self.rax);
        write_u64(buf, 8, self.rbx);
        write_u64(buf, 16, self.rcx);
        write_u64(buf, 24, self.rdx);
        write_u64(buf, 32, self.rsi);
        write_u64(buf, 40, self.rdi);
        write_u64(buf, 48, self.rsp);
        write_u64(buf, 56, self.rbp);
        write_u64(buf, 64, self.r8);
        write_u64(buf, 72, self.r9);
        write_u64(buf, 80, self.r10);
        write_u64(buf, 88, self.r11);
        write_u64(buf, 96, self.r12);
        write_u64(buf, 104, self.r13);
        write_u64(buf, 112, self.r14);
        write_u64(buf, 120, self.r15);
        write_u64(buf, 128, self.rip);
        write_u64(buf, 136, self.rflags);
        Ok(())
    }
}

impl KvmSregs {
    pub(crate) fn decode(buf: &[u8]) -> AxResult<Self> {
        if buf.len() != KVM_SREGS_SIZE {
            return Err(AxError::InvalidInput);
        }

        let mut interrupt_bitmap = [0u64; 4];
        for (index, value) in interrupt_bitmap.iter_mut().enumerate() {
            *value = read_u64(buf, 280 + index * 8);
        }

        Ok(Self {
            cs: KvmSegment::decode(buf, 0),
            ds: KvmSegment::decode(buf, 24),
            es: KvmSegment::decode(buf, 48),
            fs: KvmSegment::decode(buf, 72),
            gs: KvmSegment::decode(buf, 96),
            ss: KvmSegment::decode(buf, 120),
            tr: KvmSegment::decode(buf, 144),
            ldt: KvmSegment::decode(buf, 168),
            gdt: KvmDtable::decode(buf, 192),
            idt: KvmDtable::decode(buf, 208),
            cr0: read_u64(buf, 224),
            cr2: read_u64(buf, 232),
            cr3: read_u64(buf, 240),
            cr4: read_u64(buf, 248),
            cr8: read_u64(buf, 256),
            efer: read_u64(buf, 264),
            apic_base: read_u64(buf, 272),
            interrupt_bitmap,
        })
    }

    pub(crate) fn encode(self, buf: &mut [u8]) -> AxResult {
        if buf.len() != KVM_SREGS_SIZE {
            return Err(AxError::InvalidInput);
        }
        buf.fill(0);
        self.cs.encode(buf, 0);
        self.ds.encode(buf, 24);
        self.es.encode(buf, 48);
        self.fs.encode(buf, 72);
        self.gs.encode(buf, 96);
        self.ss.encode(buf, 120);
        self.tr.encode(buf, 144);
        self.ldt.encode(buf, 168);
        self.gdt.encode(buf, 192);
        self.idt.encode(buf, 208);
        write_u64(buf, 224, self.cr0);
        write_u64(buf, 232, self.cr2);
        write_u64(buf, 240, self.cr3);
        write_u64(buf, 248, self.cr4);
        write_u64(buf, 256, self.cr8);
        write_u64(buf, 264, self.efer);
        write_u64(buf, 272, self.apic_base);
        for (index, value) in self.interrupt_bitmap.iter().enumerate() {
            write_u64(buf, 280 + index * 8, *value);
        }
        Ok(())
    }
}

#[cfg_attr(not(feature = "vmx"), allow(dead_code))]
impl KvmSegment {
    pub(crate) fn from_access_rights(
        selector: u16,
        base: u64,
        limit: u32,
        access_rights: u32,
    ) -> Self {
        Self {
            base,
            limit,
            selector,
            type_: (access_rights & 0xf) as u8,
            s: ((access_rights >> 4) & 1) as u8,
            dpl: ((access_rights >> 5) & 0x3) as u8,
            present: ((access_rights >> 7) & 1) as u8,
            avl: ((access_rights >> 12) & 1) as u8,
            l: ((access_rights >> 13) & 1) as u8,
            db: ((access_rights >> 14) & 1) as u8,
            g: ((access_rights >> 15) & 1) as u8,
            unusable: ((access_rights >> 16) & 1) as u8,
        }
    }

    pub(crate) fn access_rights(self) -> u32 {
        (self.type_ as u32)
            | ((self.s as u32) << 4)
            | ((self.dpl as u32) << 5)
            | ((self.present as u32) << 7)
            | ((self.avl as u32) << 12)
            | ((self.l as u32) << 13)
            | ((self.db as u32) << 14)
            | ((self.g as u32) << 15)
            | ((self.unusable as u32) << 16)
    }

    fn decode(buf: &[u8], offset: usize) -> Self {
        Self {
            base: read_u64(buf, offset),
            limit: read_u32(buf, offset + 8),
            selector: read_u16(buf, offset + 12),
            type_: buf[offset + 14],
            present: buf[offset + 15],
            dpl: buf[offset + 16],
            db: buf[offset + 17],
            s: buf[offset + 18],
            l: buf[offset + 19],
            g: buf[offset + 20],
            avl: buf[offset + 21],
            unusable: buf[offset + 22],
        }
    }

    fn encode(self, buf: &mut [u8], offset: usize) {
        write_u64(buf, offset, self.base);
        write_u32(buf, offset + 8, self.limit);
        write_u16(buf, offset + 12, self.selector);
        buf[offset + 14] = self.type_;
        buf[offset + 15] = self.present;
        buf[offset + 16] = self.dpl;
        buf[offset + 17] = self.db;
        buf[offset + 18] = self.s;
        buf[offset + 19] = self.l;
        buf[offset + 20] = self.g;
        buf[offset + 21] = self.avl;
        buf[offset + 22] = self.unusable;
    }
}

impl KvmDtable {
    fn decode(buf: &[u8], offset: usize) -> Self {
        Self {
            base: read_u64(buf, offset),
            limit: read_u16(buf, offset + 8),
        }
    }

    fn encode(self, buf: &mut [u8], offset: usize) {
        write_u64(buf, offset, self.base);
        write_u16(buf, offset + 8, self.limit);
    }
}

fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap())
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(buf[offset..offset + 8].try_into().unwrap())
}

fn write_u16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
}

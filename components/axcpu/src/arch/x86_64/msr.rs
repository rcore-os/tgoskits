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

//! Typed hardware model-specific register identifiers.

/// Architectural and vendor MSR identifiers used by CPU and VM control owners.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Msr {
    /// IA32_FEATURE_CONTROL (Intel SDM / AMD APM register identifier).
    Ia32FeatureControl   = 0x3a,
    /// IA32_SYSENTER_CS (Intel SDM / AMD APM register identifier).
    Ia32SysenterCs       = 0x174,
    /// IA32_SYSENTER_ESP (Intel SDM / AMD APM register identifier).
    Ia32SysenterEsp      = 0x175,
    /// IA32_SYSENTER_EIP (Intel SDM / AMD APM register identifier).
    Ia32SysenterEip      = 0x176,
    /// IA32_PAT (Intel SDM / AMD APM register identifier).
    Ia32Pat              = 0x277,
    /// IA32_VMX_BASIC (Intel SDM / AMD APM register identifier).
    Ia32VmxBasic         = 0x480,
    /// IA32_VMX_PINBASED_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxPinbasedCtls  = 0x481,
    /// IA32_VMX_PROCBASED_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxProcbasedCtls = 0x482,
    /// IA32_VMX_EXIT_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxExitCtls      = 0x483,
    /// IA32_VMX_ENTRY_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxEntryCtls     = 0x484,
    /// IA32_VMX_MISC (Intel SDM / AMD APM register identifier).
    Ia32VmxMisc          = 0x485,
    /// IA32_VMX_CR0_FIXED0 (Intel SDM / AMD APM register identifier).
    Ia32VmxCr0Fixed0     = 0x486,
    /// IA32_VMX_CR0_FIXED1 (Intel SDM / AMD APM register identifier).
    Ia32VmxCr0Fixed1     = 0x487,
    /// IA32_VMX_CR4_FIXED0 (Intel SDM / AMD APM register identifier).
    Ia32VmxCr4Fixed0     = 0x488,
    /// IA32_VMX_CR4_FIXED1 (Intel SDM / AMD APM register identifier).
    Ia32VmxCr4Fixed1     = 0x489,
    /// IA32_VMX_PROCBASED_CTLS2 (Intel SDM / AMD APM register identifier).
    Ia32VmxProcbasedCtls2 = 0x48b,
    /// IA32_VMX_EPT_VPID_CAP (Intel SDM / AMD APM register identifier).
    Ia32VmxEptVpidCap    = 0x48c,
    /// IA32_VMX_TRUE_PINBASED_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxTruePinbasedCtls = 0x48d,
    /// IA32_VMX_TRUE_PROCBASED_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxTrueProcbasedCtls = 0x48e,
    /// IA32_VMX_TRUE_EXIT_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxTrueExitCtls  = 0x48f,
    /// IA32_VMX_TRUE_ENTRY_CTLS (Intel SDM / AMD APM register identifier).
    Ia32VmxTrueEntryCtls = 0x490,
    /// IA32_XSS (Intel SDM / AMD APM register identifier).
    Ia32Xss              = 0xda0,
    /// IA32_EFER (Intel SDM / AMD APM register identifier).
    Ia32Efer             = 0xc000_0080,
    /// IA32_STAR (Intel SDM / AMD APM register identifier).
    Ia32Star             = 0xc000_0081,
    /// IA32_LSTAR (Intel SDM / AMD APM register identifier).
    Ia32Lstar            = 0xc000_0082,
    /// IA32_CSTAR (Intel SDM / AMD APM register identifier).
    Ia32Cstar            = 0xc000_0083,
    /// IA32_FMASK (Intel SDM / AMD APM register identifier).
    Ia32Fmask            = 0xc000_0084,
    /// IA32_FS_BASE (Intel SDM / AMD APM register identifier).
    Ia32FsBase           = 0xc000_0100,
    /// IA32_GS_BASE (Intel SDM / AMD APM register identifier).
    Ia32GsBase           = 0xc000_0101,
    /// IA32_KERNEL_GSBASE (Intel SDM / AMD APM register identifier).
    Ia32KernelGsbase     = 0xc000_0102,
    /// VM_CR (Intel SDM / AMD APM register identifier).
    VmCr                 = 0xc001_0114,
    /// IGNNE (Intel SDM / AMD APM register identifier).
    Ignne                = 0xc001_0115,
    /// VM_HSAVE_PA (Intel SDM / AMD APM register identifier).
    VmHsavePa            = 0xc001_0117,
    /// PERF_EVT_SEL0 (Intel SDM / AMD APM register identifier).
    PerfEvtSel0          = 0xc001_0200,
    /// PERF_EVT_SEL1 (Intel SDM / AMD APM register identifier).
    PerfEvtSel1          = 0xc001_0202,
    /// PERF_EVT_SEL2 (Intel SDM / AMD APM register identifier).
    PerfEvtSel2          = 0xc001_0204,
    /// PERF_EVT_SEL3 (Intel SDM / AMD APM register identifier).
    PerfEvtSel3          = 0xc001_0206,
    /// PERF_EVT_SEL4 (Intel SDM / AMD APM register identifier).
    PerfEvtSel4          = 0xc001_0208,
    /// PERF_EVT_SEL5 (Intel SDM / AMD APM register identifier).
    PerfEvtSel5          = 0xc001_020a,
}

impl Msr {
    /// Reads the complete register value.
    ///
    /// # Safety
    /// Execute at ring 0 on a CPU implementing this register with access
    /// permitted by any higher virtualization layer.
    pub unsafe fn read(self) -> u64 {
        // SAFETY: the caller establishes the register's privileged availability.
        unsafe { x86::msr::rdmsr(self as u32) }
    }

    /// Writes a validated register value.
    ///
    /// # Safety
    /// The caller must own this register's machine-state transition, establish
    /// its availability and reserved-bit rules, and retain referenced memory
    /// and execution state for as long as the hardware can use the value.
    pub unsafe fn write(self, value: u64) {
        // SAFETY: the caller owns the register and the complete state lifetime.
        unsafe { x86::msr::wrmsr(self as u32, value) };
    }
}

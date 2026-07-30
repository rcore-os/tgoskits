use std::collections::BTreeMap;

use anyhow::{Context, anyhow};
use qemu_plugin::RegisterDescriptor;

#[derive(Default)]
pub struct AllRegs(BTreeMap<String, RegisterDescriptor<'static>>);

impl AllRegs {
    pub fn read(&self, name: &str) -> anyhow::Result<u64> {
        let value = self
            .0
            .get(name)
            .context(format!("Register {name} not found"))?
            .read()?;

        value
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|v| anyhow!("Unexpected size for register {name}: {}", v.len()))
    }
}

impl From<Vec<RegisterDescriptor<'static>>> for AllRegs {
    fn from(regs: Vec<RegisterDescriptor<'static>>) -> Self {
        let map = regs
            .into_iter()
            .map(|reg| (reg.name.clone(), reg))
            .collect();
        AllRegs(map)
    }
}

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use ax_lazyinit::LazyInit;

static KALLSYMS_TABLE: LazyInit<KallsymsTable> = LazyInit::new();

struct KallsymsTable {
    symbols: Vec<KallsymEntry>,
}

struct KallsymEntry {
    addr: u64,
    name: String,
}

impl KallsymsTable {
    fn from_kallsyms_str(data: &str) -> Self {
        let mut symbols = Vec::new();
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Each line is `<hex addr> <type> <name>`; the type column is
            // not retained because the only consumer (perf kprobe) resolves
            // by name.
            let parts: Vec<&str> = line.splitn(3, ' ').collect();
            if parts.len() < 3 {
                continue;
            }
            let addr = match u64::from_str_radix(parts[0], 16) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let name = parts[2].to_string();
            symbols.push(KallsymEntry { addr, name });
        }
        symbols.sort_by_key(|e| e.addr);
        Self { symbols }
    }

    fn lookup_name(&self, name: &str) -> Option<u64> {
        self.symbols.iter().find(|e| e.name == name).map(|e| e.addr)
    }
}

/// Populate the symbol table from a textual kallsyms dump (`addr type name`
/// per line). Feeding real data is a follow-up; until then the table stays
/// empty and [`kallsyms_lookup_name`] returns `None`.
pub fn kallsyms_init(data: &str) {
    if data.trim().is_empty() {
        info!("kallsyms: no symbol data available, skipping initialization");
        return;
    }
    let table = KallsymsTable::from_kallsyms_str(data);
    let count = table.symbols.len();
    KALLSYMS_TABLE.init_once(table);
    info!("kallsyms: initialized with {count} symbols");
}

/// Resolve a symbol name to its address, or `None` if unknown / not yet
/// initialized. Used by the perf kprobe path to resolve probe targets.
pub fn kallsyms_lookup_name(name: &str) -> Option<u64> {
    KALLSYMS_TABLE.get().and_then(|t| t.lookup_name(name))
}

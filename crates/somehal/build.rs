use std::{fs, io::Write, path::PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(efi)");

    let target = std::env::var("TARGET").unwrap();

    if target.contains("windows") || target.contains("linux") || target.contains("darwin") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out_dir.display());

    if std::env::var("CARGO_FEATURE_EFI").is_ok() {
        println!("cargo:rustc-cfg=efi");
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let uspace = std::env::var("CARGO_FEATURE_USPACE").is_ok();
    let hv = std::env::var("CARGO_FEATURE_HV").is_ok();

    let mut build = Build {
        arch: Arch::from(arch.as_str()),
        out_dir,
        kernel_vaddr: 0x200000,
        kernel_liner_offset: 0,
        uspace,
        hv,
    };

    build.prepare();
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Arch {
    #[default]
    Loongarch64,
    Arch64,
}

impl From<&str> for Arch {
    fn from(s: &str) -> Self {
        match s {
            "loongarch64" => Arch::Loongarch64,
            "aarch64" => Arch::Arch64,
            _ => todo!(),
        }
    }
}

#[derive(Default)]
struct Build {
    arch: Arch,
    out_dir: PathBuf,
    kernel_vaddr: u64,
    kernel_liner_offset: u64,
    uspace: bool,
    hv: bool,
}

impl Build {
    const LD_NAME: &'static str = "somehal.x";

    fn prepare(&mut self) {
        match self.arch {
            Arch::Loongarch64 => self.prepare_loongarch64(),
            Arch::Arch64 => self.prepare_aarch64(),
        }
    }

    fn prepare_aarch64(&mut self) {
        println!("cargo::rustc-check-cfg=cfg(hard_float)");

        let ld_src = "src/arch/aarch64/link.ld";

        if self.hv {
            self.uspace = false;
        }
        if self.uspace {
            self.kernel_liner_offset = 0xFFFF_0000_0000_0000;
        }

        let kernel_vaddr = self.kernel_vaddr as usize;
        let kernel_liner_offset = self.kernel_liner_offset as usize;

        let ld = include_str!("src/arch/aarch64/link.ld")
            .replace("${kernel_load_vaddr}", &format!("{kernel_vaddr:#x}"));

        println!("cargo:rerun-if-changed={ld_src}");
        if std::env::var("CARGO_FEATURE_EFI").is_ok() {
            println!("cargo:rustc-cfg=efi");
        }
        let ld_dst = self.out_dir.join(Self::LD_NAME);

        fs::write(ld_dst, ld).unwrap();

        let defines = quote::quote! {
            pub const KERNEL_LINER_OFFSET: usize = #kernel_liner_offset;
        };
        let syntax_tree = syn::parse2(defines).unwrap();
        let formatted = prettyplease::unparse(&syntax_tree);
        let mut out_file = fs::File::create(self.out_dir.join("defines.rs")).unwrap();
        out_file.write_all(formatted.as_bytes()).unwrap();
    }

    fn prepare_loongarch64(&mut self) {
        let ld_src = "src/arch/loongarch64/link.ld";

        self.kernel_vaddr = 0x9000000000200000;

        let kernel_load_vaddr = self.kernel_vaddr as usize;

        let ld = include_str!("src/arch/loongarch64/link.ld")
            .replace("${kernel_load_vaddr}", &format!("{kernel_load_vaddr:#x}"));

        println!("cargo:rerun-if-changed={ld_src}");
        println!("cargo:rustc-cfg=efi");

        let ld_dst = self.out_dir.join(Self::LD_NAME);

        fs::write(ld_dst, ld).unwrap();

        let defines = quote::quote! {
            pub const VMLINUX_LOAD_ADDRESS: usize = #kernel_load_vaddr;
        };
        let syntax_tree = syn::parse2(defines).unwrap();
        let formatted = prettyplease::unparse(&syntax_tree);
        let mut out_file = fs::File::create(self.out_dir.join("defines.rs")).unwrap();
        out_file.write_all(formatted.as_bytes()).unwrap();
    }
}

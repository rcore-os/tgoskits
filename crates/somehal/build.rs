use std::{fs, io::Write, path::PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(efi)");
    println!("cargo::rustc-check-cfg=cfg(page_size_4k)");
    println!("cargo::rustc-check-cfg=cfg(page_size_16k)");

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
        uspace,
        hv,
        page_size: 4096,
    };

    build.prepare();

    if build.page_size == 4096 {
        println!("cargo:rustc-cfg=page_size_4k");
    } else if build.page_size == 16384 {
        println!("cargo:rustc-cfg=page_size_16k");
    }
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
    uspace: bool,
    hv: bool,
    page_size: usize,
}

impl Build {
    const LD_NAME: &'static str = "somehal.x";

    fn prepare(&mut self) {
        match self.arch {
            Arch::Loongarch64 => self.prepare_loongarch64(),
            Arch::Arch64 => self.prepare_aarch64(),
        }

        self.gen_defines();
    }

    fn prepare_aarch64(&mut self) {
        println!("cargo::rustc-check-cfg=cfg(hard_float)");

        let ld_src = "src/arch/aarch64/link.ld";

        if self.hv {
            self.uspace = false;
        }
        if self.uspace {
            self.kernel_vaddr = 0xFFFF_9000_0020_0000;
        }

        let kernel_vaddr = self.kernel_vaddr as usize;

        let ld = include_str!("src/arch/aarch64/link.ld")
            .replace("${kernel_load_vaddr}", &format!("{kernel_vaddr:#x}"));

        println!("cargo:rerun-if-changed={ld_src}");
        if std::env::var("CARGO_FEATURE_EFI").is_ok() {
            println!("cargo:rustc-cfg=efi");
        }
        let ld_dst = self.out_dir.join(Self::LD_NAME);

        fs::write(ld_dst, ld).unwrap();
    }

    fn prepare_loongarch64(&mut self) {
        let ld_src = "src/arch/loongarch64/link.ld";

        self.kernel_vaddr = 0xffff_ffff_8000_0000;

        let kernel_load_vaddr = self.kernel_vaddr as usize;

        let ld = include_str!("src/arch/loongarch64/link.ld")
            .replace("${kernel_load_vaddr}", &format!("{kernel_load_vaddr:#x}"));

        println!("cargo:rerun-if-changed={ld_src}");
        println!("cargo:rustc-cfg=efi");

        let ld_dst = self.out_dir.join(Self::LD_NAME);

        fs::write(ld_dst, ld).unwrap();
    }

    fn gen_defines(&self) {
        let kernel_load_vaddr = self.kernel_vaddr as usize;
        let defines = quote::quote! {
            pub const VM_LOAD_ADDRESS: usize = #kernel_load_vaddr;
        };
        let syntax_tree = syn::parse2(defines).unwrap();
        let formatted = prettyplease::unparse(&syntax_tree);
        let mut out_file = fs::File::create(self.out_dir.join("defines.rs")).unwrap();
        out_file.write_all(formatted.as_bytes()).unwrap();
    }
}

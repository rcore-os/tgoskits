use clap::Parser;
use serde_json::Value;
use toml_edit::DocumentMut;

/// Display information about a platform package
#[derive(Parser, Debug)]
#[command(long_about = "Display information about a platform package")]
pub struct CommandInfo {
    /// Package to inspect
    ///
    /// Requires that the package has been added to dependencies,
    /// e.g, by using `axplat-cli add <PLATFORM>`.
    #[arg(required = true, value_name = "PLATFORM")]
    platform: String,

    /// Display the platform name
    #[arg(short = 'p', long = "platform")]
    plat: bool,

    /// Display the architecture of the platform
    #[arg(short = 'a', long = "arch")]
    arch: bool,

    /// Display the version of the platform package
    #[arg(short = 'v', long = "version")]
    version: bool,

    /// Display the source of the platform package
    #[arg(short = 's', long = "source")]
    source: bool,

    /// Display the path to Cargo.toml of the platform package
    #[arg(short = 'm', long = "manifest-path")]
    manifest_path: bool,

    /// Display the path to the platform configuration file
    #[arg(short = 'c', long = "config-path")]
    config_path: bool,
}

fn get_info_from_metadata(metadata: &str, name: &str) -> Result<PlatformInfo, PlatformInfoErr> {
    let json: Value = serde_json::from_str(&metadata).map_err(|_| PlatformInfoErr::ParseError)?;
    let packages = json["packages"]
        .as_array()
        .ok_or(PlatformInfoErr::ParseError)?;
    for p in packages {
        if p["name"] == name {
            return PlatformInfo::new(p);
        }
    }
    Err(PlatformInfoErr::PackageNotFound)
}

fn parse_config(config_path: &str) -> Result<(String, String), PlatformInfoErr> {
    let toml = std::fs::read_to_string(config_path)
        .map_err(|_| PlatformInfoErr::NoConfig(config_path.into()))?;
    (|| {
        let config = toml.parse::<DocumentMut>().ok()?;
        let plat_name = config["platform"].as_str()?.to_string();
        let arch = config["arch"].as_str()?.to_string();
        Some((plat_name, arch))
    })()
    .ok_or_else(|| PlatformInfoErr::InvalidConfig(config_path.into()))
}

pub fn platform_info(args: CommandInfo) {
    let metadata = crate::run_cargo_command("metadata", |cmd| {
        cmd.arg("--all-features").arg("--format-version").arg("1");
    });

    match get_info_from_metadata(&metadata, &args.platform) {
        Ok(info) => {
            if args.plat
                || args.arch
                || args.version
                || args.source
                || args.manifest_path
                || args.config_path
            {
                info.display(&args);
            } else {
                info.display_all();
            }
        }
        Err(PlatformInfoErr::ParseError) => {
            eprintln!("error: failed to parse cargo metadata");
            std::process::exit(1);
        }
        Err(PlatformInfoErr::PackageNotFound) => {
            eprintln!(
                "error: platform `{}` not found in dependencies",
                args.platform
            );
            std::process::exit(1);
        }
        Err(PlatformInfoErr::NoConfig(path)) => {
            eprintln!("error: configuration file not found at `{}`", path);
            std::process::exit(1);
        }
        Err(PlatformInfoErr::InvalidConfig(path)) => {
            eprintln!("error: invalid configuration file `{}`", path);
            std::process::exit(1);
        }
    }
}

enum PlatformInfoErr {
    ParseError,
    PackageNotFound,
    NoConfig(String),
    InvalidConfig(String),
}

struct PlatformInfo {
    platform: String,
    arch: String,
    version: String,
    source: String,
    manifest_path: String,
    config_path: String,
}

impl PlatformInfo {
    fn new(package: &Value) -> Result<Self, PlatformInfoErr> {
        let version = package["version"]
            .as_str()
            .ok_or(PlatformInfoErr::ParseError)?
            .to_string();
        let manifest_path = package["manifest_path"]
            .as_str()
            .ok_or(PlatformInfoErr::ParseError)?
            .to_string();

        let source = package["source"]
            .as_str()
            .or_else(|| package["id"].as_str())
            .ok_or(PlatformInfoErr::ParseError)?
            .to_string();

        let root_dir = manifest_path.strip_suffix("/Cargo.toml").unwrap();
        let config_path = format!("{}/axconfig.toml", root_dir);
        let (platform, arch) = parse_config(&config_path)?;
        Ok(Self {
            platform,
            arch,
            version,
            source,
            manifest_path,
            config_path,
        })
    }

    fn display(&self, args: &CommandInfo) {
        if args.plat {
            println!("{}", self.platform);
        }
        if args.arch {
            println!("{}", self.arch);
        }
        if args.version {
            println!("{}", self.version);
        }
        if args.source {
            println!("{}", self.source);
        }
        if args.manifest_path {
            println!("{}", self.manifest_path);
        }
        if args.config_path {
            println!("{}", self.config_path);
        }
    }

    fn display_all(&self) {
        println!("platform: {}", self.platform);
        println!("arch: {}", self.arch);
        println!("version: {}", self.version);
        println!("source: {}", self.source);
        println!("manifest_path: {}", self.manifest_path);
        println!("config_path: {}", self.config_path);
    }
}

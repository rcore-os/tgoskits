use axconfig_gen::{Config, ConfigValue, OutputFormat};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Paths to the config specification files
    #[arg(required = true)]
    spec: Vec<String>,

    /// Path to the old config file
    #[arg(short = 'c', long)]
    oldconfig: Option<String>,

    /// Path to the output config file
    #[arg(short, long)]
    output: Option<String>,

    /// The output format
    #[arg(
        short, long,
        default_value_t = OutputFormat::Toml,
        value_parser = PossibleValuesParser::new(["toml", "rust"])
            .map(|s| s.parse::<OutputFormat>().unwrap()),
    )]
    fmt: OutputFormat,

    /// Getting a config item with format `table.key`
    #[arg(short, long, value_name = "RD_CONFIG")]
    read: Vec<String>,

    /// Setting a config item with format `table.key=value`
    #[arg(short, long, value_name = "WR_CONFIG")]
    write: Vec<String>,

    /// Verbose mode
    #[arg(short, long)]
    verbose: bool,
}

fn parse_config_read_arg(arg: &str) -> Result<(String, String), String> {
    if let Some((table, key)) = arg.split_once('.') {
        Ok((table.into(), key.into()))
    } else {
        Ok((Config::GLOBAL_TABLE_NAME.into(), arg.into()))
    }
}

fn parse_config_write_arg(arg: &str) -> Result<(String, String, String), String> {
    let (item, value) = arg.split_once('=').ok_or_else(|| {
        format!(
            "Invalid config setting command `{}`, expected `table.key=value`",
            arg
        )
    })?;
    if let Some((table, key)) = item.split_once('.') {
        Ok((table.into(), key.into(), value.into()))
    } else {
        Ok((Config::GLOBAL_TABLE_NAME.into(), item.into(), value.into()))
    }
}

macro_rules! unwrap {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    };
}

fn main() {
    let args = Args::parse();

    macro_rules! debug {
        ($($arg:tt)*) => {
            if args.verbose {
                eprintln!($($arg)*);
            }
        };
    }

    let mut config = Config::new();
    for spec in &args.spec {
        debug!("[DEBUG] Loading config specification from {:?}", spec);
        let spec_toml = unwrap!(std::fs::read_to_string(spec).inspect_err(|_| {
            eprintln!("Failed to read config specification file {:?}", spec);
        }));
        let sub_config = unwrap!(Config::from_toml(&spec_toml));
        unwrap!(config.merge(&sub_config));
    }

    if let Some(oldconfig_path) = &args.oldconfig {
        debug!("[DEBUG] Loading old config from {:?}", oldconfig_path);
        let oldconfig_toml = unwrap!(std::fs::read_to_string(oldconfig_path).inspect_err(|_| {
            eprintln!("Failed to read old config file {:?}", oldconfig_path);
        }));
        let oldconfig = unwrap!(Config::from_toml(&oldconfig_toml));

        let (untouched, extra) = unwrap!(config.update(&oldconfig));
        for item in &untouched {
            eprintln!(
                "[WARN] config item `{}` not set in the old config, using default value",
                item.item_name(),
            );
        }
        for item in &extra {
            eprintln!(
                "[WARN] config item `{}` not found in the specification, ignoring",
                item.item_name(),
            );
        }
    }

    for arg in &args.write {
        let (table, key, value) = unwrap!(parse_config_write_arg(arg));
        if table == Config::GLOBAL_TABLE_NAME {
            debug!("[DEBUG] Setting config item `{}` to `{}`", key, value);
        } else {
            debug!(
                "[DEBUG] Setting config item `{}.{}` to `{}`",
                table, key, value
            );
        }
        let new_value = unwrap!(ConfigValue::new(&value));
        let item = unwrap!(config
            .config_at_mut(&table, &key)
            .ok_or_else(|| format!("Config item `{}` not found", arg)));
        unwrap!(item.value_mut().update(new_value));
    }

    for arg in &args.read {
        let (table, key) = unwrap!(parse_config_read_arg(arg));
        if table == Config::GLOBAL_TABLE_NAME {
            debug!("[DEBUG] Getting config item `{}`", key);
        } else {
            debug!("[DEBUG] Getting config item `{}.{}`", table, key);
        }
        let item = unwrap!(config
            .config_at(&table, &key)
            .ok_or_else(|| format!("Config item `{}` not found", arg)));
        println!("{}", item.value().to_toml_value());
    }

    if !args.read.is_empty() {
        debug!("[DEBUG] In reading mode, no output");
        return;
    }

    let output = unwrap!(config.dump(args.fmt));
    if let Some(path) = args.output.as_ref().map(std::path::Path::new) {
        if let Ok(oldconfig) = std::fs::read_to_string(path) {
            // If the output is the same as the old config, do nothing
            if oldconfig == output {
                return;
            }
            // Calculate the path to the backup file
            let bak_path = if let Some(ext) = path.extension() {
                path.with_extension(format!("old.{}", ext.to_string_lossy()))
            } else {
                path.with_extension("old")
            };
            // Backup the old config file
            unwrap!(std::fs::write(bak_path, oldconfig));
        }
        unwrap!(std::fs::write(path, output));
    } else {
        println!("{}", output);
    }
}

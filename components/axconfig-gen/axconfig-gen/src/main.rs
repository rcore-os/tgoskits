use ax_config_gen::{
    GenerateOptions, OutputFormat, generate_config, load_config_specs, parse_config_read_arg,
};
use clap::{
    Parser,
    builder::{PossibleValuesParser, TypedValueParser},
};

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

    let specs = args.spec.iter().map(Into::into).collect::<Vec<_>>();

    if !args.read.is_empty() {
        let config = unwrap!(load_config_specs(&specs));
        for arg in &args.read {
            if args.verbose {
                eprintln!("[DEBUG] Getting config item `{}`", arg);
            }
            let (table, key) = unwrap!(parse_config_read_arg(arg));
            let item = unwrap!(config.config_at(&table, &key).ok_or_else(|| {
                ax_config_gen::ConfigErr::Other(format!("Config item `{}` not found", arg))
            }));
            println!("{}", item.value().to_toml_value());
        }

        if args.verbose {
            eprintln!("[DEBUG] In reading mode, no output");
        }
        return;
    }

    let options = GenerateOptions {
        specs,
        oldconfig: args.oldconfig.map(Into::into),
        output: args.output.map(Into::into),
        fmt: args.fmt,
        writes: args.write,
        keep_backup: true,
    };
    let report = unwrap!(generate_config(&options));
    for item in &report.untouched {
        eprintln!(
            "[WARN] config item `{}` not set in the old config, using default value",
            item.item_name(),
        );
    }
    for item in &report.extra {
        eprintln!(
            "[WARN] config item `{}` not found in the specification, ignoring",
            item.item_name(),
        );
    }
    if options.output.is_none() {
        println!("{}", report.output);
    }
}

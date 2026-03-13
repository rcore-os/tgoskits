use crate::{ConfigErr, ConfigItem, ConfigResult, ConfigType};

/// The format of the generated file.
#[derive(Debug, Clone)]
pub enum OutputFormat {
    /// Output is in TOML format.
    Toml,
    /// Output is Rust code.
    Rust,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Toml => "toml",
            Self::Rust => "rust",
        };
        s.fmt(f)
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "toml" => Ok(Self::Toml),
            "rust" => Ok(Self::Rust),
            _ => Err(s.into()),
        }
    }
}

/// The output writer.
pub struct Output {
    fmt: OutputFormat,
    indent: usize,
    result: String,
}

impl Output {
    pub fn new(fmt: OutputFormat) -> Self {
        Self {
            fmt,
            indent: 0,
            result: String::new(),
        }
    }

    pub fn result(&self) -> &str {
        &self.result
    }

    pub fn println_fmt(&mut self, fmt: std::fmt::Arguments) {
        self.result += &format!("{:indent$}{}\n", "", fmt, indent = self.indent);
    }

    pub fn println(&mut self, s: &str) {
        self.println_fmt(format_args!("{}", s));
    }

    pub fn print_lines(&mut self, s: &str, line_op: impl Fn(&str) -> String) {
        for line in s.lines() {
            let line = line_op(line);
            if !line.is_empty() {
                self.println(&line);
            }
        }
    }

    pub fn table_begin(&mut self, name: &str, comments: &str) {
        if !self.result.is_empty() {
            self.println("");
        }
        match self.fmt {
            OutputFormat::Toml => {
                self.print_lines(comments, |l| l.trim().into());
                self.println(&format!("[{}]", name));
            }
            OutputFormat::Rust => {
                self.print_lines(comments, |l| l.trim().replacen("#", "///", 1));
                self.println_fmt(format_args!("pub mod {} {{", mod_name(name)));
                self.indent += 4;
            }
        }
    }

    pub fn table_end(&mut self) {
        if let OutputFormat::Rust = self.fmt {
            self.indent -= 4;
            self.println("}");
        }
    }

    pub fn write_item(&mut self, item: &ConfigItem) -> ConfigResult<()> {
        match self.fmt {
            OutputFormat::Toml => {
                self.print_lines(item.comments(), |l| l.trim().into());
                self.println_fmt(format_args!(
                    "{} = {}{}",
                    item.key(),
                    item.value().to_toml_value(),
                    if let Some(ty) = item.value().ty() {
                        format!(" # {}", ty)
                    } else {
                        "".into()
                    },
                ));
            }
            OutputFormat::Rust => {
                self.print_lines(item.comments(), |l| l.trim().replacen("#", "///", 1));
                let key = const_name(item.key());
                let val = item.value();
                let ty = if let Some(ty) = val.ty() {
                    ty.clone()
                } else {
                    val.inferred_type()?
                };

                if matches!(ty, ConfigType::Unknown) {
                    return Err(ConfigErr::Other(format!(
                        "Unknown type for key `{}`",
                        item.key()
                    )));
                }
                self.println_fmt(format_args!(
                    "pub const {}: {} = {};",
                    key,
                    ty.to_rust_type(),
                    val.to_rust_value(&ty, self.indent)?,
                ));
            }
        }
        Ok(())
    }
}

fn mod_name(name: &str) -> String {
    name.replace("-", "_")
}

fn const_name(name: &str) -> String {
    name.to_uppercase().replace('-', "_")
}

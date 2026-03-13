use std::collections::{BTreeMap, BTreeSet};
use toml_edit::{Decor, DocumentMut, Item, Table, Value};

use crate::output::{Output, OutputFormat};
use crate::{ConfigErr, ConfigResult, ConfigType, ConfigValue};

type ConfigTable = BTreeMap<String, ConfigItem>;

/// A structure representing a config item.
///
/// It contains the config key, value and comments.
#[derive(Debug, Clone)]
pub struct ConfigItem {
    table_name: String,
    key: String,
    value: ConfigValue,
    comments: String,
}

impl ConfigItem {
    fn new(table_name: &str, table: &Table, key: &str, value: &Value) -> ConfigResult<Self> {
        let inner = || {
            let item = table.key(key).unwrap();
            let comments = prefix_comments(item.leaf_decor())
                .unwrap_or_default()
                .to_string();
            let suffix = suffix_comments(value.decor()).unwrap_or_default().trim();
            let value = if !suffix.is_empty() {
                let ty_str = suffix.trim_start_matches('#');
                let ty = ConfigType::new(ty_str)?;
                ConfigValue::from_raw_value_type(value, ty)?
            } else {
                ConfigValue::from_raw_value(value)?
            };
            Ok(Self {
                table_name: table_name.into(),
                key: key.into(),
                value,
                comments,
            })
        };
        let res = inner();
        if let Err(e) = &res {
            eprintln!("Parsing error at key `{}`: {:?}", key, e);
        }
        res
    }

    fn new_global(table: &Table, key: &str, value: &Value) -> ConfigResult<Self> {
        Self::new(Config::GLOBAL_TABLE_NAME, table, key, value)
    }

    /// Returns the unique name of the config item.
    ///
    /// If the item is contained in the global table, it returns the iten key.
    /// Otherwise, it returns a string with the format `table.key`.
    pub fn item_name(&self) -> String {
        if self.table_name == Config::GLOBAL_TABLE_NAME {
            self.key.clone()
        } else {
            format!("{}.{}", self.table_name, self.key)
        }
    }

    /// Returns the table name of the config item.
    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// Returns the key of the config item.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the value of the config item.
    pub fn value(&self) -> &ConfigValue {
        &self.value
    }

    /// Returns the comments of the config item.
    pub fn comments(&self) -> &str {
        &self.comments
    }

    /// Returns the mutable reference to the value of the config item.
    pub fn value_mut(&mut self) -> &mut ConfigValue {
        &mut self.value
    }
}

/// A structure storing all config items.
///
/// It contains a global table and multiple named tables, each table is a map
/// from key to value, the key is a string and the value is a [`ConfigItem`].
#[derive(Default, Debug)]
pub struct Config {
    global: ConfigTable,
    tables: BTreeMap<String, ConfigTable>,
    table_comments: BTreeMap<String, String>,
}

impl Config {
    /// The name of the global table of the config.
    pub const GLOBAL_TABLE_NAME: &'static str = "$GLOBAL";

    /// Create a new empty config object.
    pub fn new() -> Self {
        Self {
            global: ConfigTable::new(),
            tables: BTreeMap::new(),
            table_comments: BTreeMap::new(),
        }
    }

    /// Returns whether the config object contains no items.
    pub fn is_empty(&self) -> bool {
        self.global.is_empty() && self.tables.is_empty()
    }

    fn new_table(&mut self, name: &str, comments: &str) -> ConfigResult<&mut ConfigTable> {
        if name == Self::GLOBAL_TABLE_NAME {
            return Err(ConfigErr::Other(format!(
                "Table name `{}` is reserved",
                Self::GLOBAL_TABLE_NAME
            )));
        }
        if self.tables.contains_key(name) {
            return Err(ConfigErr::Other(format!("Duplicate table name `{}`", name)));
        }
        self.tables.insert(name.into(), ConfigTable::new());
        self.table_comments.insert(name.into(), comments.into());
        Ok(self.tables.get_mut(name).unwrap())
    }

    /// Returns the global table of the config.
    pub fn global_table(&self) -> &BTreeMap<String, ConfigItem> {
        &self.global
    }

    /// Returns the reference to the table with the specified name.
    pub fn table_at(&self, name: &str) -> Option<&BTreeMap<String, ConfigItem>> {
        if name == Self::GLOBAL_TABLE_NAME {
            Some(&self.global)
        } else {
            self.tables.get(name)
        }
    }

    /// Returns the mutable reference to the table with the specified name.
    pub fn table_at_mut(&mut self, name: &str) -> Option<&mut BTreeMap<String, ConfigItem>> {
        if name == Self::GLOBAL_TABLE_NAME {
            Some(&mut self.global)
        } else {
            self.tables.get_mut(name)
        }
    }

    /// Returns the reference to the config item with the specified table name and key.
    pub fn config_at(&self, table: &str, key: &str) -> Option<&ConfigItem> {
        self.table_at(table).and_then(|t| t.get(key))
    }

    /// Returns the mutable reference to the config item with the specified
    /// table name and key.
    pub fn config_at_mut(&mut self, table: &str, key: &str) -> Option<&mut ConfigItem> {
        self.table_at_mut(table).and_then(|t| t.get_mut(key))
    }

    /// Returns the comments of the table with the specified name.
    pub fn table_comments_at(&self, name: &str) -> Option<&str> {
        self.table_comments.get(name).map(|s| s.as_str())
    }

    /// Returns the iterator of all tables.
    ///
    /// The iterator returns a tuple of table name, table and comments. The
    /// global table is named `$GLOBAL`.
    pub fn table_iter(&self) -> impl Iterator<Item = (&str, &ConfigTable, &str)> {
        let global_iter = [(Self::GLOBAL_TABLE_NAME, &self.global, "")].into_iter();
        let other_iter = self.tables.iter().map(|(name, configs)| {
            (
                name.as_str(),
                configs,
                self.table_comments.get(name).unwrap().as_str(),
            )
        });
        global_iter.chain(other_iter)
    }

    /// Returns the iterator of all config items.
    ///
    /// The iterator returns a tuple of table name, key and config item. The
    /// global table is named `$GLOBAL`.
    pub fn iter(&self) -> impl Iterator<Item = &ConfigItem> {
        self.table_iter().flat_map(|(_, c, _)| c.values())
    }
}

impl Config {
    /// Parse a toml string into a config object.
    pub fn from_toml(toml: &str) -> ConfigResult<Self> {
        let doc = toml.parse::<DocumentMut>()?;
        let table = doc.as_table();

        let mut result = Self::new();
        for (key, item) in table.iter() {
            match item {
                Item::Value(val) => {
                    result
                        .global
                        .insert(key.into(), ConfigItem::new_global(table, key, val)?);
                }
                Item::Table(table) => {
                    let table_name = key;
                    let comments = prefix_comments(table.decor());
                    let configs = result.new_table(key, comments.unwrap_or_default())?;
                    for (key, item) in table.iter() {
                        if let Item::Value(val) = item {
                            configs
                                .insert(key.into(), ConfigItem::new(table_name, table, key, val)?);
                        } else {
                            return Err(ConfigErr::InvalidValue);
                        }
                    }
                }
                Item::None => {}
                _ => {
                    return Err(ConfigErr::Other(format!(
                        "Object array `[[{}]]` is not supported",
                        key
                    )))
                }
            }
        }
        Ok(result)
    }

    /// Dump the config into a string with the specified format.
    pub fn dump(&self, fmt: OutputFormat) -> ConfigResult<String> {
        let mut output = Output::new(fmt);
        for (name, table, comments) in self.table_iter() {
            if name != Self::GLOBAL_TABLE_NAME {
                output.table_begin(name, comments);
            }
            for (key, item) in table.iter() {
                if let Err(e) = output.write_item(item) {
                    eprintln!("Dump config `{}` failed: {:?}", key, e);
                }
            }
            if name != Self::GLOBAL_TABLE_NAME {
                output.table_end();
            }
        }
        Ok(output.result().into())
    }

    /// Dump the config into TOML format.
    pub fn dump_toml(&self) -> ConfigResult<String> {
        self.dump(OutputFormat::Toml)
    }

    /// Dump the config into Rust code.
    pub fn dump_rs(&self) -> ConfigResult<String> {
        self.dump(OutputFormat::Rust)
    }

    /// Merge the other config into `self`, if there is a duplicate key, return an error.
    pub fn merge(&mut self, other: &Self) -> ConfigResult<()> {
        for (name, other_table, table_comments) in other.table_iter() {
            let self_table = if let Some(table) = self.table_at_mut(name) {
                table
            } else {
                self.new_table(name, table_comments)?
            };
            for (key, item) in other_table.iter() {
                if self_table.contains_key(key) {
                    return Err(ConfigErr::Other(format!("Duplicate key `{}`", key)));
                } else {
                    self_table.insert(key.into(), item.clone());
                }
            }
        }
        Ok(())
    }

    /// Update the values of `self` with the other config, if there is a key not
    /// found in `self`, skip it.
    ///
    /// It returns two vectors of `ConfigItem`, the first contains the keys that
    /// are included in `self` but not in `other`, the second contains the keys
    /// that are included in `other` but not in `self`.
    pub fn update(&mut self, other: &Self) -> ConfigResult<(Vec<ConfigItem>, Vec<ConfigItem>)> {
        let mut touched = BTreeSet::new(); // included in both `self` and `other`
        let mut extra = Vec::new(); // included in `other` but not in `self`

        for other_item in other.iter() {
            let table_name = other_item.table_name.clone();
            let key = other_item.key.clone();
            let self_table = if let Some(table) = self.table_at_mut(&table_name) {
                table
            } else {
                extra.push(other_item.clone());
                continue;
            };

            if let Some(self_item) = self_table.get_mut(&key) {
                self_item.value.update(other_item.value.clone())?;
                touched.insert(self_item.item_name());
            } else {
                extra.push(other_item.clone());
            }
        }

        // included in `self` but not in `other`
        let untouched = self
            .iter()
            .filter(|item| !touched.contains(&item.item_name()))
            .cloned()
            .collect::<Vec<_>>();
        Ok((untouched, extra))
    }
}

fn prefix_comments(decor: &Decor) -> Option<&str> {
    decor.prefix().and_then(|s| s.as_str())
}

fn suffix_comments(decor: &Decor) -> Option<&str> {
    decor.suffix().and_then(|s| s.as_str())
}

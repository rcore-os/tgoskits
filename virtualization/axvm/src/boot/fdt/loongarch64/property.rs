use alloc::vec::Vec;

use fdt_edit::Property;

pub(crate) fn prop_null(name: &str) -> Property {
    Property::new(name, Vec::new())
}

pub(crate) fn prop_u32(name: &str, value: u32) -> Property {
    prop_u32_array(name, &[value])
}

pub(crate) fn prop_u32_array(name: &str, values: &[u32]) -> Property {
    let mut prop = Property::new(name, Vec::new());
    prop.set_u32_ls(values);
    prop
}

pub(crate) fn prop_string(name: &str, value: &str) -> Property {
    let mut prop = Property::new(name, Vec::new());
    prop.set_string(value);
    prop
}

pub(crate) fn prop_string_list(name: &str, values: &[&str]) -> Property {
    let mut prop = Property::new(name, Vec::new());
    prop.set_string_ls(values);
    prop
}

use std::{mem, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DriveArg {
    options: QemuOptions,
}

impl DriveArg {
    pub(super) fn parse(value: &str) -> Self {
        Self {
            options: QemuOptions::parse(value),
        }
    }

    pub(super) fn file(&self) -> Option<&str> {
        self.options.value("file")
    }

    pub(super) fn id(&self) -> Option<&str> {
        self.options.value("id")
    }

    pub(super) fn interface(&self) -> Option<&str> {
        self.options.value("if")
    }

    pub(super) fn set_file(&mut self, path: &Path) {
        self.options.set_value("file", &path.display().to_string());
    }

    pub(super) fn snapshot_conflict(&self) -> Option<&str> {
        self.options.value_other_than("snapshot", "off")
    }

    pub(super) fn set_snapshot_on(&mut self) {
        self.options.set_value("snapshot", "on");
    }

    pub(super) fn is_file_backed_block_drive(&self) -> bool {
        self.file().is_some_and(|file| !file.starts_with("fat:"))
            && self.interface() != Some("pflash")
    }

    pub(super) fn render(&self) -> String {
        self.options.render()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeviceArg {
    options: QemuOptions,
}

impl DeviceArg {
    pub(super) fn parse(value: &str) -> Self {
        Self {
            options: QemuOptions::parse(value),
        }
    }

    pub(super) fn drive(&self) -> Option<&str> {
        self.options.value("drive")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QemuOptions {
    fields: Vec<String>,
}

impl QemuOptions {
    fn parse(value: &str) -> Self {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut chars = value.chars().peekable();

        while let Some(character) = chars.next() {
            if character != ',' {
                field.push(character);
                continue;
            }
            if chars.peek() == Some(&',') {
                chars.next();
                field.push(',');
                continue;
            }
            fields.push(mem::take(&mut field));
        }
        fields.push(field);

        Self { fields }
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.fields.iter().find_map(|field| {
            let (field_key, value) = field.split_once('=')?;
            (field_key == key).then_some(value)
        })
    }

    fn value_other_than(&self, key: &str, allowed: &str) -> Option<&str> {
        self.fields.iter().find_map(|field| {
            let (field_key, value) = field.split_once('=')?;
            (field_key == key && value != allowed).then_some(value)
        })
    }

    fn set_value(&mut self, key: &str, value: &str) {
        let mut replaced = false;
        for field in &mut self.fields {
            if field
                .split_once('=')
                .is_some_and(|(field_key, _)| field_key == key)
            {
                *field = format!("{key}={value}");
                replaced = true;
            }
        }
        if !replaced {
            self.fields.push(format!("{key}={value}"));
        }
    }

    fn render(&self) -> String {
        self.fields
            .iter()
            .map(|field| field.replace(',', ",,"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_arg_decodes_and_encodes_qemu_escaped_commas() {
        let mut drive = DriveArg::parse(
            "cache=none,file=/tmp/rootfs,,old.img,id=disk0,serial=name,,with,,commas",
        );

        assert_eq!(drive.file(), Some("/tmp/rootfs,old.img"));
        assert_eq!(drive.id(), Some("disk0"));

        drive.set_file(Path::new("/tmp/rootfs,new.img"));
        assert_eq!(
            drive.render(),
            "cache=none,file=/tmp/rootfs,,new.img,id=disk0,serial=name,,with,,commas"
        );
    }
}

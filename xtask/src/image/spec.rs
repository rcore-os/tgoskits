//! Image spec: owned and reference types for (name, version).
//!
//! Parses specs as `name` or `name:version` (ASCII colon) and provides
//! [`ImageSpec`] (owned) and [`ImageSpecRef`] (reference) with common trait impls.

use std::fmt;

/// Owned image spec: name and optional version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub struct ImageSpec {
    /// Image name (e.g. `evm3588_arceos`).
    pub name: String,
    /// Image version; `None` means "latest" or default path.
    pub version: Option<String>,
}

/// Reference image spec: borrowed name and optional version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageSpecRef<'a> {
    /// Image name.
    pub name: &'a str,
    /// Image version; `None` means "latest" or default path.
    pub version: Option<&'a str>,
}

#[allow(dead_code)]
impl ImageSpec {
    /// Parses a spec string: `name` or `name:version` (ASCII colon).
    ///
    /// # Arguments
    ///
    /// * `s` - Spec string (e.g. `evm3588_arceos` or `evm3588_arceos:0.0.22`)
    pub fn parse(s: &str) -> ImageSpec {
        let r = ImageSpecRef::parse(s);
        r.to_owned()
    }

    /// Creates an owned spec from a reference spec.
    ///
    /// # Arguments
    ///
    /// * `r` - Reference spec to clone
    pub fn from_ref(r: ImageSpecRef<'_>) -> ImageSpec {
        ImageSpec {
            name: r.name.to_string(),
            version: r.version.map(String::from),
        }
    }

    /// Borrows this spec as an [`ImageSpecRef`].
    pub fn as_ref(&self) -> ImageSpecRef<'_> {
        ImageSpecRef {
            name: &self.name,
            version: self.version.as_deref(),
        }
    }
}

impl<'a> ImageSpecRef<'a> {
    /// Parses a spec string: `name` or `name:version` (ASCII colon).
    ///
    /// The returned spec borrows from `s`.
    ///
    /// # Arguments
    ///
    /// * `s` - Spec string (e.g. `evm3588_arceos` or `evm3588_arceos:0.0.22`)
    pub fn parse(s: &'a str) -> ImageSpecRef<'a> {
        match s.split_once(':') {
            Some((name, version)) => ImageSpecRef {
                name,
                version: Some(version),
            },
            None => ImageSpecRef {
                name: s,
                version: None,
            },
        }
    }

    /// Converts to an owned [`ImageSpec`].
    pub fn to_owned(self) -> ImageSpec {
        ImageSpec::from_ref(self)
    }
}

impl fmt::Display for ImageSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{}:{}", self.name, v),
            None => write!(f, "{}", self.name),
        }
    }
}

impl fmt::Display for ImageSpecRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            Some(v) => write!(f, "{}:{}", self.name, v),
            None => write!(f, "{}", self.name),
        }
    }
}

impl From<ImageSpecRef<'_>> for ImageSpec {
    fn from(r: ImageSpecRef<'_>) -> Self {
        ImageSpec::from_ref(r)
    }
}

impl<'a> From<&'a str> for ImageSpecRef<'a> {
    fn from(s: &'a str) -> Self {
        ImageSpecRef::parse(s)
    }
}

impl<'a> From<&'a String> for ImageSpecRef<'a> {
    fn from(s: &'a String) -> Self {
        ImageSpecRef::parse(s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_only() {
        let r = ImageSpecRef::parse("evm3588_arceos");
        assert_eq!(r.name, "evm3588_arceos");
        assert_eq!(r.version, None);
    }

    #[test]
    fn parse_name_and_version() {
        let r = ImageSpecRef::parse("evm3588_arceos:0.0.22");
        assert_eq!(r.name, "evm3588_arceos");
        assert_eq!(r.version, Some("0.0.22"));
    }

    #[test]
    fn display_ref() {
        assert_eq!(ImageSpecRef::parse("a").to_string(), "a");
        assert_eq!(ImageSpecRef::parse("a:b").to_string(), "a:b");
    }

    #[test]
    fn as_ref() {
        let spec = ImageSpec::parse("x:1");
        let r = spec.as_ref();
        assert_eq!(r.name, "x");
        assert_eq!(r.version, Some("1"));
    }
}

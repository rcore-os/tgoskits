use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageSpec {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageSpecRef<'a> {
    pub name: &'a str,
    pub version: Option<&'a str>,
}

impl ImageSpec {
    pub fn parse(s: &str) -> ImageSpec {
        ImageSpecRef::parse(s).to_owned()
    }

    pub fn from_ref(r: ImageSpecRef<'_>) -> ImageSpec {
        ImageSpec {
            name: r.name.to_string(),
            version: r.version.map(String::from),
        }
    }

    pub fn as_ref(&self) -> ImageSpecRef<'_> {
        ImageSpecRef {
            name: &self.name,
            version: self.version.as_deref(),
        }
    }
}

impl<'a> ImageSpecRef<'a> {
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

    pub fn to_owned(self) -> ImageSpec {
        ImageSpec::from_ref(self)
    }
}

impl fmt::Display for ImageSpecRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            Some(version) => write!(f, "{}:{}", self.name, version),
            None => write!(f, "{}", self.name),
        }
    }
}

impl From<ImageSpecRef<'_>> for ImageSpec {
    fn from(value: ImageSpecRef<'_>) -> Self {
        ImageSpec::from_ref(value)
    }
}

impl<'a> From<&'a str> for ImageSpecRef<'a> {
    fn from(value: &'a str) -> Self {
        ImageSpecRef::parse(value)
    }
}

impl<'a> From<&'a String> for ImageSpecRef<'a> {
    fn from(value: &'a String) -> Self {
        ImageSpecRef::parse(value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_without_version() {
        let spec = ImageSpecRef::parse("linux");
        assert_eq!(spec.name, "linux");
        assert_eq!(spec.version, None);
    }

    #[test]
    fn parses_name_with_version() {
        let spec = ImageSpecRef::parse("linux:0.0.1");
        assert_eq!(spec.name, "linux");
        assert_eq!(spec.version, Some("0.0.1"));
    }
}

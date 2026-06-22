use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBuildWrapper {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    pub(crate) build_config_path: PathBuf,
    pub(super) variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredQemuCase {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedQemuCase {
    pub(crate) name: String,
    pub(crate) archs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListQemuCasesErrorKind {
    EmptyGroup,
    UnknownSelectedCase,
    Unexpected,
}

#[derive(Debug)]
pub(crate) struct ListQemuCasesError {
    kind: ListQemuCasesErrorKind,
    message: String,
}

impl ListQemuCasesError {
    pub(super) fn new(kind: ListQemuCasesErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    pub(crate) fn kind(&self) -> ListQemuCasesErrorKind {
        self.kind
    }
}

impl std::fmt::Display for ListQemuCasesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for ListQemuCasesError {}

impl From<anyhow::Error> for ListQemuCasesError {
    fn from(err: anyhow::Error) -> Self {
        list_qemu_cases_unexpected_error(err)
    }
}

pub(crate) type ListQemuCasesResult<T> = Result<T, ListQemuCasesError>;

pub(crate) struct QemuCaseGroup<'a, T> {
    pub(crate) build_group: &'a str,
    pub(crate) build_config_path: &'a Path,
    pub(crate) cases: Vec<&'a T>,
}

pub(crate) struct QemuCaseBuildGroup<'a, T, R> {
    pub(crate) group: QemuCaseGroup<'a, T>,
    pub(crate) request: R,
    pub(crate) cargo: Cargo,
}

pub(crate) trait BuildConfigRef {
    fn build_group(&self) -> &str;
    fn build_config_path(&self) -> &Path;
}

#[derive(Debug, Deserialize)]
pub(crate) struct QemuCaseExtraConfig {
    #[serde(default)]
    pub(crate) test_commands: Vec<String>,
    #[serde(default)]
    pub(crate) host_symbolize_success_regex: Vec<String>,
    #[serde(default)]
    pub(crate) host_http_server: Option<HostHttpServerConfig>,
}

pub(super) fn list_qemu_cases_unexpected_error(err: anyhow::Error) -> ListQemuCasesError {
    ListQemuCasesError::new(ListQemuCasesErrorKind::Unexpected, err.to_string())
}

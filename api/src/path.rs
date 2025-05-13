use core::{ffi::c_int, fmt, ops::Deref};

use alloc::{
    collections::btree_map::BTreeMap,
    string::{String, ToString},
};
use axerrno::{AxError, AxResult, LinuxError, LinuxResult};
use axfs::api::canonicalize;
use linux_raw_sys::general::AT_FDCWD;
use spin::RwLock;

use crate::file::{Directory, File, FileLike};

/// 一个规范化的文件路径表示
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct FilePath(String);

impl FilePath {
    /// 从路径字符串创建一个新的 `FilePath`，路径将被规范化。
    /// 输入路径可以是绝对路径或相对路径。
    pub fn new<P: AsRef<str>>(path: P) -> AxResult<Self> {
        let path = path.as_ref();
        let canonical = canonicalize(path).map_err(|_| AxError::NotFound)?;
        let mut new_path = canonical.trim().to_string();

        // 如果原始路径以 '/' 结尾，那么规范化后的路径也应以 '/' 结尾
        if path.ends_with('/') && !new_path.ends_with('/') {
            new_path.push('/');
        }

        assert!(
            new_path.starts_with('/'),
            "canonical path should start with /"
        );

        Ok(Self(HARDLINK_MANAGER.real_path(&new_path)))
    }

    /// 返回底层路径的字符串切片
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 返回父目录路径
    pub fn parent(&self) -> AxResult<&str> {
        if self.is_root() {
            return Ok("/");
        }

        // 查找最后一个斜杠，考虑可能的尾部斜杠
        let mut path = self.as_str();
        if path.ends_with('/') {
            path = path.strip_suffix('/').unwrap();
        }
        let pos = path.rfind('/').ok_or(AxError::NotFound)?;

        Ok(&path[..=pos])
    }

    /// 返回文件名或目录名组件
    pub fn name(&self) -> AxResult<&str> {
        if self.is_root() {
            return Ok("/");
        }

        let mut path = self.as_str();
        if path.ends_with('/') {
            path = path.strip_suffix('/').unwrap();
        }
        let start_pos = path.rfind('/').ok_or(AxError::NotFound)?;

        let end_pos = if path.ends_with('/') {
            path.len() - 1
        } else {
            path.len()
        };
        Ok(&path[start_pos + 1..end_pos])
    }

    /// 判断是否为根目录
    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    /// 判断是否为目录（以 '/' 结尾）
    pub fn is_dir(&self) -> bool {
        self.0.ends_with('/')
    }

    /// 判断是否为常规文件（不以 '/' 结尾）
    pub fn is_file(&self) -> bool {
        !self.is_dir()
    }

    /// Whether the path exists
    pub fn exists(&self) -> bool {
        axfs::api::absolute_path_exists(&self.0)
    }

    /// 判断此路径是否以给定前缀路径开头
    pub fn starts_with(&self, prefix: &FilePath) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// 判断此路径是否以给定后缀路径结尾
    pub fn ends_with(&self, suffix: &FilePath) -> bool {
        self.0.ends_with(&suffix.0)
    }

    /// 将此路径与相对路径组件连接
    pub fn join<P: AsRef<str>>(&self, path: P) -> AxResult<Self> {
        let mut new_path = self.0.clone();
        if !new_path.ends_with('/') {
            new_path.push('/');
        }
        new_path.push_str(path.as_ref());
        FilePath::new(new_path)
    }

    /// 返回此路径组件的迭代器
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.trim_matches('/').split('/')
    }
}

impl fmt::Display for FilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for FilePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for FilePath {
    fn from(s: &str) -> Self {
        FilePath::new(s).unwrap()
    }
}

impl Deref for FilePath {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// 错误类型
#[derive(Debug)]
pub enum LinkError {
    LinkExists,  // 链接已存在
    InvalidPath, // 无效路径
    NotFound,    // 文件不存在
    NotFile,     // 不是文件
}

impl From<LinkError> for AxError {
    fn from(err: LinkError) -> AxError {
        match err {
            LinkError::LinkExists => AxError::AlreadyExists,
            LinkError::InvalidPath => AxError::InvalidInput,
            LinkError::NotFound => AxError::NotFound,
            LinkError::NotFile => AxError::InvalidInput,
        }
    }
}

impl From<LinkError> for LinuxError {
    fn from(err: LinkError) -> LinuxError {
        AxError::from(err).into()
    }
}

/// A global hardlink manager
pub static HARDLINK_MANAGER: HardlinkManager = HardlinkManager::new();

/// A manager for hardlinks
pub struct HardlinkManager {
    inner: RwLock<LinkManagerInner>,
}
struct LinkManagerInner {
    links: BTreeMap<String, String>,
    ref_counts: BTreeMap<String, usize>,
}

// 关于innner的操作都在atomic_开头的函数中
impl HardlinkManager {
    const fn new() -> Self {
        Self {
            inner: RwLock::new(LinkManagerInner {
                links: BTreeMap::new(),
                ref_counts: BTreeMap::new(),
            }),
        }
    }

    /// 创建链接
    /// 如果目标路径不存在，则返回 `LinkError::NotFound`
    /// 如果目标路径不是文件，则返回 `LinkError::NotFile`
    pub fn create_link(&self, src: &FilePath, dst: &FilePath) -> Result<(), LinkError> {
        if !dst.exists() {
            return Err(LinkError::NotFound);
        }
        if !dst.is_dir() {
            return Err(LinkError::NotFile);
        }

        let mut inner = self.inner.write();
        self.atomic_link_update(&mut inner, src, dst);
        Ok(())
    }

    /// 移除链接
    /// 链接数量为零 或 没有链接时， 删除文件
    /// 如果路径对应的链接不存在 或 路径对应的文件不存在，则返回 `None`
    /// 否则返回链接的目标路径
    pub fn remove_link(&self, src: &FilePath) -> Option<String> {
        let mut inner = self.inner.write();
        self.atomic_link_remove(&mut inner, src).or_else(|| {
            axfs::api::remove_file(src.as_str())
                .ok()
                .map(|_| src.to_string())
        })
    }

    pub fn real_path(&self, path: &str) -> String {
        self.inner
            .read()
            .links
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_string())
    }

    pub fn link_count(&self, path: &FilePath) -> usize {
        let inner = self.inner.read();
        inner
            .ref_counts
            .get(path.as_str())
            .copied()
            .unwrap_or_else(|| if path.exists() { 1 } else { 0 })
    }

    // 原子操作helpers

    /// 创建或更新链接
    /// 如果链接已存在，则更新目标路径
    /// 如果目标路径不存在，则返回 `LinkError::NotFound`
    fn atomic_link_update(&self, inner: &mut LinkManagerInner, src: &FilePath, dst: &FilePath) {
        if let Some(old_dst) = inner.links.get(src.as_str()) {
            if old_dst == dst.as_str() {
                return;
            }
            self.decrease_ref_count(inner, &old_dst.to_string());
        }
        inner.links.insert(src.to_string(), dst.to_string());
        *inner.ref_counts.entry(dst.to_string()).or_insert(0) += 1;
    }

    /// 移除链接
    /// 如果链接不存在，则返回 `None`，否则返回链接的目标路径
    fn atomic_link_remove(&self, inner: &mut LinkManagerInner, src: &FilePath) -> Option<String> {
        inner.links.remove(src.as_str()).inspect(|dst| {
            self.decrease_ref_count(inner, dst);
        })
    }

    /// 减少引用计数
    /// 如果引用计数为零，则删除链接，并删除文件，如果删除文件失败，则返回 `None`
    /// 如果链接不存在，则返回 `None`
    fn decrease_ref_count(&self, inner: &mut LinkManagerInner, path: &str) -> Option<()> {
        match inner.ref_counts.get_mut(path) {
            Some(count) => {
                *count -= 1;
                if *count == 0 {
                    inner.ref_counts.remove(path);
                    axfs::api::remove_file(path).ok()?
                }
                Some(())
            }
            None => {
                axlog::error!("link exists but ref count is zero");
                None
            }
        }
    }
}

pub fn handle_file_path(dirfd: c_int, path: &str) -> LinuxResult<FilePath> {
    if path.starts_with('/') {
        Ok(FilePath::new(path)?)
    } else if path.is_empty() {
        Ok(FilePath::new(File::from_fd(dirfd)?.path())?)
    } else {
        let base = if dirfd == AT_FDCWD {
            FilePath::new("")?
        } else {
            FilePath::new(Directory::from_fd(dirfd)?.path())?
        };
        Ok(base.join(path)?)
    }
}

//! Common utilities and helper functions.
//!
//! This module provides utility types and functions used throughout ostool,
//! including command execution helpers and string processing utilities.

use std::{
    ffi::OsStr,
    ops::{Deref, DerefMut},
    path::Path,
};

use anyhow::Context;
use anyhow::bail;
use colored::Colorize;

/// A command builder wrapper with variable substitution support.
///
/// `Command` wraps `std::process::Command` and adds support for automatic
/// variable replacement in arguments and environment values.
pub struct Command {
    inner: std::process::Command,
    value_replace: Box<dyn Fn(&OsStr) -> String>,
}

impl Deref for Command {
    type Target = std::process::Command;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Command {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Command {
    /// Creates a new command builder.
    ///
    /// # Arguments
    ///
    /// * `program` - The program to execute.
    /// * `workdir` - The working directory for the command.
    /// * `value_replace` - Function to perform variable substitution on arguments.
    pub fn new<S>(
        program: S,
        workdir: &Path,
        value_replace: impl Fn(&OsStr) -> String + 'static,
    ) -> Command
    where
        S: AsRef<OsStr>,
    {
        let mut cmd = std::process::Command::new(program);
        cmd.current_dir(workdir);
        cmd.env("WORKSPACE_FOLDER", workdir.display().to_string());

        Self {
            inner: cmd,
            value_replace: Box::new(value_replace),
        }
    }

    /// Prints the command to stdout with colored formatting.
    pub fn print_cmd(&self) {
        let program = self.get_program().to_string_lossy();
        let mut cmd_str = program.into_owned();
        for arg in self.get_args() {
            cmd_str.push(' ');
            cmd_str.push_str(arg.to_string_lossy().as_ref());
        }

        println!("{}", cmd_str.purple().bold());
    }

    pub fn into_std(self) -> std::process::Command {
        self.inner
    }

    /// Executes the command and waits for it to complete.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails to execute or exits with non-zero status.
    pub fn run(&mut self) -> anyhow::Result<()> {
        self.print_cmd();
        let status = self.status()?;
        if !status.success() {
            bail!("failed with status: {status}");
        }
        Ok(())
    }

    /// Adds an argument to the command with variable substitution.
    pub fn arg<S>(&mut self, arg: S) -> &mut Command
    where
        S: AsRef<OsStr>,
    {
        let value = (self.value_replace)(arg.as_ref());
        self.inner.arg(value);
        self
    }

    /// Adds multiple arguments to the command with variable substitution.
    pub fn args<I, S>(&mut self, args: I) -> &mut Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg.as_ref());
        }
        self
    }

    /// Sets an environment variable for the command with variable substitution.
    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Command
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let value = (self.value_replace)(val.as_ref());
        self.inner.env(key, value);
        self
    }
}

/// Adds file-system path context to fallible operations.
pub trait PathResultExt<T> {
    /// Attach an operation label and the relevant path while preserving the original error as source.
    fn with_path<P>(self, action: &'static str, path: P) -> anyhow::Result<T>
    where
        P: AsRef<Path>;
}

impl<T, E> PathResultExt<T> for Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn with_path<P>(self, action: &'static str, path: P) -> anyhow::Result<T>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref().to_path_buf();
        self.with_context(move || format!("{action}: {}", path.display()))
    }
}

/// Replaces environment variable placeholders in a string.
///
/// Placeholders use the format `${env:VAR_NAME}` where `VAR_NAME` is the
/// name of an environment variable. If the variable is not set, the
/// placeholder is replaced with an empty string.
///
/// # Example
///
/// ```rust
/// use ostool::utils::replace_env_placeholders;
///
/// unsafe { std::env::set_var("MY_VAR", "hello"); }
/// let result = replace_env_placeholders("Value: ${env:MY_VAR}").unwrap();
/// assert_eq!(result, "Value: hello");
/// ```
pub fn replace_env_placeholders(input: &str) -> anyhow::Result<String> {
    replace_placeholders(input, |placeholder| {
        if let Some(env_var_name) = placeholder.strip_prefix("env:") {
            return Ok(Some(std::env::var(env_var_name).unwrap_or_default()));
        }

        Ok(None)
    })
}

/// Replaces placeholders in a string using a caller-provided resolver.
///
/// Placeholders use the format `${name}`. The resolver can choose to replace
/// a placeholder by returning `Some(value)` or keep it unchanged with `None`.
/// This function preserves malformed or unknown placeholders as-is.
pub fn replace_placeholders<F>(input: &str, mut resolver: F) -> anyhow::Result<String>
where
    F: FnMut(&str) -> anyhow::Result<Option<String>>,
{
    let mut result = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            // 开始可能的占位符
            chars.next(); // 消耗 '{'
            let mut placeholder = String::new();
            let mut brace_count = 1;
            let mut found_closing_brace = false;

            // 收集占位符内容
            for ch in chars.by_ref() {
                if ch == '{' {
                    brace_count += 1;
                    placeholder.push(ch);
                } else if ch == '}' {
                    brace_count -= 1;
                    if brace_count == 0 {
                        found_closing_brace = true;
                        break;
                    } else {
                        placeholder.push(ch);
                    }
                } else {
                    placeholder.push(ch);
                }
            }

            if found_closing_brace {
                if let Some(value) = resolver(&placeholder)? {
                    result.push_str(&value);
                } else {
                    result.push_str("${");
                    result.push_str(&placeholder);
                    result.push('}');
                }
            } else {
                result.push_str("${");
                result.push_str(&placeholder);
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_replace_placeholders_supports_custom_variables() {
        unsafe {
            env::set_var("OSTOOL_TEST_CUSTOM_ENV", "env-value");
        }

        let result = replace_placeholders(
            "workspace=${workspace}, package=${package}, env=${env:OSTOOL_TEST_CUSTOM_ENV}",
            |placeholder| {
                Ok(match placeholder {
                    "workspace" => Some("/tmp/workspace".into()),
                    "package" => Some("/tmp/workspace/kernel".into()),
                    p if p.starts_with("env:") => Some(env::var(&p[4..]).unwrap_or_default()),
                    _ => None,
                })
            },
        )
        .unwrap();

        assert_eq!(
            result,
            "workspace=/tmp/workspace, package=/tmp/workspace/kernel, env=env-value"
        );
    }

    #[test]
    fn test_replace_env_placeholders() {
        // 设置测试环境变量
        unsafe {
            env::set_var("OSTOOL_TEST_HOME_REPLACE", "/home/test");
            env::set_var("OSTOOL_TEST_PATH_REPLACE", "/usr/local/bin");
        }

        // 测试简单的环境变量替换
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL_TEST_HOME_REPLACE}").unwrap(),
            "/home/test"
        );

        // 测试多个环境变量
        assert_eq!(
            replace_env_placeholders(
                "${env:OSTOOL_TEST_HOME_REPLACE}:${env:OSTOOL_TEST_PATH_REPLACE}"
            )
            .unwrap(),
            "/home/test:/usr/local/bin"
        );

        // 测试不存在的环境变量 - 应该返回空字符串而不是错误
        let result = replace_env_placeholders("${env:NON_EXISTENT}");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");

        // 测试混合内容
        assert_eq!(
            replace_env_placeholders("Path: ${env:OSTOOL_TEST_HOME_REPLACE}/bin").unwrap(),
            "Path: /home/test/bin"
        );

        // 测试非环境变量占位符
        assert_eq!(
            replace_env_placeholders("${not_env:placeholder}").unwrap(),
            "${not_env:placeholder}"
        );

        // 测试无占位符的字符串
        assert_eq!(
            replace_env_placeholders("Just a normal string").unwrap(),
            "Just a normal string"
        );

        // 测试空字符串
        assert_eq!(replace_env_placeholders("").unwrap(), "");
    }

    #[test]
    fn test_nested_braces() {
        unsafe {
            env::set_var("OSTOOL_TEST_VAR_NESTED", "value");
        }

        // 测试嵌套大括号的情况
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL_TEST_VAR_NESTED} and ${other:placeholder}")
                .unwrap(),
            "value and ${other:placeholder}"
        );
    }

    #[test]
    fn test_replace_placeholders_keeps_unknown_and_legacy_placeholders() {
        let result = replace_placeholders(
            "${workspaceFolder}:${unknown}:${workspace}",
            |placeholder| {
                Ok(match placeholder {
                    "workspaceFolder" => Some("/legacy".into()),
                    "workspace" => Some("/modern".into()),
                    _ => None,
                })
            },
        )
        .unwrap();

        assert_eq!(result, "/legacy:${unknown}:/modern");
    }

    #[test]
    fn test_real_env_vars() {
        // 测试真实的环境变量（如果存在）
        if let Ok(home) = env::var("HOME") {
            assert_eq!(replace_env_placeholders("${env:HOME}").unwrap(), home);
        }
    }

    #[test]
    fn test_edge_cases() {
        // 测试不完整的占位符
        assert_eq!(replace_env_placeholders("${").unwrap(), "${");
        assert_eq!(replace_env_placeholders("${env").unwrap(), "${env");
        assert_eq!(replace_env_placeholders("${env:").unwrap(), "${env:");
        assert_eq!(replace_env_placeholders("${env:VAR").unwrap(), "${env:VAR");

        // 测试空的env变量名 - 应该返回空字符串而不是错误
        let result = replace_env_placeholders("${env:}");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");

        // 测试只包含$的字符串
        assert_eq!(replace_env_placeholders("$").unwrap(), "$");
        assert_eq!(replace_env_placeholders("$$").unwrap(), "$$");

        // 测试包含特殊字符的环境变量名
        unsafe {
            env::set_var("OSTOOL-TEST-VAR-EDGE", "dash-value");
            env::set_var("OSTOOL_TEST_VAR_EDGE", "underscore-value");
        }
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL-TEST-VAR-EDGE}").unwrap(),
            "dash-value"
        );
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL_TEST_VAR_EDGE}").unwrap(),
            "underscore-value"
        );

        // 测试空的环境变量值
        unsafe {
            env::set_var("OSTOOL_EMPTY_VAR_EDGE", "");
        }
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL_EMPTY_VAR_EDGE}").unwrap(),
            ""
        );
    }

    #[test]
    fn test_malformed_placeholders() {
        // 测试格式错误的占位符
        assert_eq!(replace_env_placeholders("${env:VAR").unwrap(), "${env:VAR");
        assert_eq!(replace_env_placeholders("${env}").unwrap(), "${env}");
        assert_eq!(replace_env_placeholders("${:VAR}").unwrap(), "${:VAR}");

        // 设置测试环境变量
        unsafe {
            env::set_var("OSTOOL_VAR_MALFORMED", "value");
        }

        // 测试混合的大括号
        // 当遇到完整的占位符后停止，剩余字符由主循环继续处理
        assert_eq!(
            replace_env_placeholders("${env:OSTOOL_VAR_MALFORMED}}").unwrap(),
            "value}"
        );

        // 测试其他格式错误的情况
        assert_eq!(replace_env_placeholders("{env:VAR}").unwrap(), "{env:VAR}");
        assert_eq!(replace_env_placeholders("$env:VAR}").unwrap(), "$env:VAR}");
    }
}

use std::path::Path;

use regex::Regex;

pub(crate) const RESULT_BEGIN_MARKER: &str = "AXTEST_RESULT_BEGIN";
pub(crate) const RESULT_END_MARKER: &str = "AXTEST_RESULT_END";

pub(crate) fn render_vm_create_cmd(config_path: &Path) -> String {
    format!("vm create {}", config_path.display())
}

pub(crate) fn render_vm_start_cmd(vm_id: usize) -> String {
    format!("vm start {vm_id}")
}

pub(crate) fn render_vm_stop_cmd(vm_id: usize) -> String {
    format!("vm stop {vm_id}")
}

pub(crate) fn render_vm_delete_cmd(vm_id: usize) -> String {
    format!("vm delete {vm_id} --force")
}

pub(crate) fn render_vm_list_json_cmd() -> &'static str {
    "vm list --format json"
}

pub(crate) fn contains_shell_prompt(buffer: &str) -> bool {
    shell_prompt_regex().is_match(buffer)
}

pub(crate) fn extract_result_payload(buffer: &str) -> Option<String> {
    let start = buffer.find(RESULT_BEGIN_MARKER)?;
    let payload_start = start + RESULT_BEGIN_MARKER.len();
    let rest = &buffer[payload_start..];
    let end_rel = rest.find(RESULT_END_MARKER)?;
    let payload = &rest[..end_rel];
    Some(payload.trim().to_string())
}

pub(crate) fn parse_created_vm_ids(output: &str) -> Vec<usize> {
    created_vm_regex()
        .captures_iter(output)
        .filter_map(|caps| caps.get(1))
        .filter_map(|value| value.as_str().parse::<usize>().ok())
        .collect()
}

fn shell_prompt_regex() -> &'static Regex {
    static PROMPT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PROMPT.get_or_init(|| Regex::new(r"(?m)(?:^|\n)axvisor:[^\r\n$]*\$ ").unwrap())
}

fn created_vm_regex() -> &'static Regex {
    static CREATED_VM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    CREATED_VM.get_or_init(|| Regex::new(r"Successfully created VM\[(\d+)\] from config").unwrap())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn render_vm_commands_match_shell_shape() {
        assert_eq!(
            render_vm_create_cmd(Path::new("/axcases/cases/case-1/vm.toml")),
            "vm create /axcases/cases/case-1/vm.toml"
        );
        assert_eq!(render_vm_start_cmd(3), "vm start 3");
        assert_eq!(render_vm_stop_cmd(3), "vm stop 3");
        assert_eq!(render_vm_delete_cmd(3), "vm delete 3 --force");
        assert_eq!(render_vm_list_json_cmd(), "vm list --format json");
    }

    #[test]
    fn contains_shell_prompt_matches_fs_and_non_fs_shapes() {
        assert!(contains_shell_prompt("axvisor:/$ "));
        assert!(contains_shell_prompt("some log\naxvisor:/guest$ "));
        assert!(contains_shell_prompt("axvisor:$ "));
        assert!(!contains_shell_prompt("\raxvisor:/$ v"));
        assert!(!contains_shell_prompt("no prompt here"));
    }

    #[test]
    fn extract_result_payload_returns_trimmed_inner_payload() {
        let output = r#"
noise before
AXTEST_RESULT_BEGIN
{
  "case_id": "cpu.tlb",
  "status": "ok"
}
AXTEST_RESULT_END
noise after
"#;

        let payload = extract_result_payload(output).unwrap();
        assert!(payload.starts_with('{'));
        assert!(payload.contains("\"case_id\": \"cpu.tlb\""));
        assert!(payload.ends_with('}'));
    }

    #[test]
    fn parse_created_vm_ids_collects_all_created_ids() {
        let output = r#"
Creating VM from config: /axcases/cases/case1/vm.toml
✓ Successfully created VM[2] from config: /axcases/cases/case1/vm.toml
Creating VM from config: /axcases/cases/case2/vm.toml
✓ Successfully created VM[4] from config: /axcases/cases/case2/vm.toml
Successfully created 2 VM(s)
"#;

        assert_eq!(parse_created_vm_ids(output), vec![2, 4]);
    }
}

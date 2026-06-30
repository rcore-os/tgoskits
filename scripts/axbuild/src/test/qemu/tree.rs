use super::*;

#[derive(Default)]
pub(super) struct CaseTreeNode {
    children: BTreeMap<String, CaseTreeNode>,
    labels: BTreeSet<String>,
}

pub(super) fn insert_case_tree_path(node: &mut CaseTreeNode, path: &str) {
    insert_case_tree_path_with_label(node, path, None);
}

pub(super) fn insert_case_tree_path_with_label(
    node: &mut CaseTreeNode,
    path: &str,
    label: Option<String>,
) {
    let mut current = node;
    for part in path.split('/').filter(|part| !part.is_empty()) {
        current = current.children.entry(part.to_string()).or_default();
    }
    if let Some(label) = label {
        current.labels.insert(label);
    }
}

pub(super) fn render_case_tree_node(node: &CaseTreeNode, prefix: &str, lines: &mut Vec<String>) {
    let total = node.children.len();
    for (index, (name, child)) in node.children.iter().enumerate() {
        let is_last = index + 1 == total;
        let branch = if is_last { "└── " } else { "├── " };
        let label = if child.labels.is_empty() {
            String::new()
        } else {
            format!(
                " [{}]",
                child
                    .labels
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("{prefix}{branch}{name}{label}"));

        let child_prefix = if is_last { "    " } else { "│   " };
        render_case_tree_node(child, &format!("{prefix}{child_prefix}"), lines);
    }
}

pub(crate) fn render_case_tree<I, S>(group: &str, cases: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut root = CaseTreeNode::default();
    for case in cases {
        insert_case_tree_path(&mut root, case.as_ref());
    }

    let mut lines = vec![group.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
}

pub(crate) fn render_qemu_case_forest<I, G, C>(suite: &str, groups: I) -> String
where
    I: IntoIterator<Item = (G, C)>,
    G: AsRef<str>,
    C: IntoIterator<Item = ListedQemuCase>,
{
    let mut root = CaseTreeNode::default();
    for (group, cases) in groups {
        let group_node = root.children.entry(group.as_ref().to_string()).or_default();
        for case in cases {
            let label = if case.archs.is_empty() {
                None
            } else {
                Some(case.archs.join(", "))
            };
            insert_case_tree_path_with_label(group_node, &case.name, label);
        }
    }

    let mut lines = vec![suite.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
}

pub(crate) fn render_labeled_case_forest<I, G, C, N, L>(suite: &str, groups: I) -> String
where
    I: IntoIterator<Item = (G, C)>,
    G: AsRef<str>,
    C: IntoIterator<Item = (N, L)>,
    N: AsRef<str>,
    L: AsRef<str>,
{
    let mut root = CaseTreeNode::default();
    for (group, cases) in groups {
        let group_node = root.children.entry(group.as_ref().to_string()).or_default();
        for (case, label) in cases {
            insert_case_tree_path_with_label(
                group_node,
                case.as_ref(),
                Some(label.as_ref().to_string()),
            );
        }
    }

    let mut lines = vec![suite.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
}

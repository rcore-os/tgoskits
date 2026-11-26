use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use super::CgroupNode;

fn ancestry(node: &Arc<CgroupNode>) -> Vec<Arc<CgroupNode>> {
    let mut nodes = Vec::new();
    let mut current = Some(Arc::clone(node));
    while let Some(node) = current {
        current = node.parent();
        nodes.push(node);
    }
    nodes
}

pub(crate) fn relative_path(root: &Arc<CgroupNode>, target: &Arc<CgroupNode>) -> String {
    if Arc::ptr_eq(root, target) {
        return "/".to_string();
    }

    let root_path = ancestry(root);
    let target_path = ancestry(target);
    let mut root_unique = root_path.len();
    let mut target_unique = target_path.len();
    while root_unique > 0
        && target_unique > 0
        && Arc::ptr_eq(&root_path[root_unique - 1], &target_path[target_unique - 1])
    {
        root_unique -= 1;
        target_unique -= 1;
    }

    let mut path = String::from("/");
    for index in 0..root_unique {
        if index != 0 {
            path.push('/');
        }
        path.push_str("..");
    }
    for node in target_path[..target_unique].iter().rev() {
        if path != "/" && !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(node.name());
    }
    path
}

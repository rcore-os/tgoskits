const X86_CONTEXT: &str = include_str!("../src/x86_64/context.rs");
const X86_LOCAL_STATE: &str = include_str!("../src/x86_64/local_state.rs");
const RUNTIME_USER_ENTRY: &str =
    include_str!("../../../os/arceos/modules/axruntime/src/task/user_entry.rs");

#[test]
fn x86_user_fpu_follows_linux_owner_and_return_to_user_boundaries() {
    assert!(
        X86_CONTEXT.contains("current_user_fp_is_owner(current)"),
        "userspace scheduler switches must save only the physical FPU owner",
    );
    assert!(
        X86_CONTEXT.contains(
            "self.ext_state.save();\n                \
             super::local_state::clear_current_user_fp_owner_after_save(current);",
        ),
        "the owner must remain published until its physical image has been saved",
    );
    assert!(
        X86_CONTEXT.contains("pub fn prepare_user_return_fp(&self)"),
        "incoming user FPU state must be restored at the return-to-user boundary",
    );
    assert!(
        X86_LOCAL_STATE.contains("user_fp_owner"),
        "the physical FPU image requires one CPU-local owner source",
    );
    assert!(
        RUNTIME_USER_ENTRY.contains(
            "crate::guard::prepare_user_return()?;\n        self.binding.prepare_user_fp_return();",
        ),
        "the context binding must prepare the FPU owner after the final IRQ-off snapshot",
    );
}

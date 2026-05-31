use axvisor_api::process::ProcessIf;

struct ProcessImpl;

#[axvisor_api::api_impl]
impl ProcessIf for ProcessImpl {
    fn exit(exit_code: i32) -> ! {
        std::process::exit(exit_code)
    }
}

extern crate axvisor_api;
extern crate memory_addr;
use axvisor_api::__priv;

pub mod some_demo {
    use memory_addr::MemoryAddr;
    pub use memory_addr::PhysAddr;

    #[axvisor_api::api_def]
    pub trait SomeDemoIf {
        /// Some function provided by the implementer
        fn some_func() -> PhysAddr;
        /// Another function provided by the implementer
        fn another_func(addr: PhysAddr);
    }

    /// Some function provided by the API definer
    pub fn provided_func() -> PhysAddr {
        some_func().add(0x1000)
    }
}

mod some_demo_impl {
    use crate::some_demo::SomeDemoIf;

    pub struct SomeDemoImpl;

    #[axvisor_api::api_impl]
    impl SomeDemoIf for SomeDemoImpl {
        fn some_func() -> memory_addr::PhysAddr {
            memory_addr::pa!(0x42)
        }

        fn another_func(addr: memory_addr::PhysAddr) {
            println!("Wow, the answer is {:?}", addr);
        }
    }
}

fn main() {
    some_demo::another_func(some_demo::some_func());
    some_demo::another_func(some_demo::provided_func());
}

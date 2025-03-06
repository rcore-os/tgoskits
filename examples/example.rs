extern crate axvisor_api;
extern crate memory_addr;
use axvisor_api::{__priv, api_mod, api_mod_impl};

api_mod! {
    /// Memory-related API
    pub mod some_demo {
        pub use memory_addr::PhysAddr;

        /// Some function
        extern fn some_func() -> PhysAddr;
        /// Another function
        extern fn another_func(addr: PhysAddr);
    }
}

#[api_mod_impl(some_demo)]
mod some_impl {
    use memory_addr::{PhysAddr, pa};

    extern fn some_func() -> PhysAddr {
        return pa!(0x42);
    }

    extern fn another_func(addr: PhysAddr) {
        println!("Wow, the answer is {:?}", addr);
    }
}

fn main() {
    some_demo::another_func(some_demo::some_func());
}

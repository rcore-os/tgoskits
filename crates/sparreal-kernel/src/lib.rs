#![no_std]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

pub mod __export;
pub mod hal;
mod lang;
pub mod os;

use hal::setup::start_kernel;
pub use sparreal_macros::entry;

static LOGO: &str = r#"
     _____                                         __
    / ___/ ____   ____ _ _____ _____ ___   ____ _ / /
    \__ \ / __ \ / __ `// ___// ___// _ \ / __ `// / 
   ___/ // /_/ // /_/ // /   / /   /  __// /_/ // /  
  /____// .___/ \__,_//_/   /_/    \___/ \__,_//_/   
       /_/                                           
"#;

pub fn run_kernel() -> ! {
    println!("{LOGO}");
    start_kernel()
}

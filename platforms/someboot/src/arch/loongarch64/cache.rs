// /*
//  * LoongArch maintains ICache/DCache coherency by hardware,
//  * we just need "ibar" to avoid instruction hazard here.
//  */
// pub fn local_flush_icache_range(_start: usize, _end: usize) {
//     unsafe {
//         core::arch::asm!("ibar 0");
//     }
// }

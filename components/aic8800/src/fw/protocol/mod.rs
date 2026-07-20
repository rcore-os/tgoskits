//! IPC 协议实现模块

mod ipc_msg;

pub use ipc_msg::{
    IpcTransport, ipc_mem_block_write, ipc_mem_mask_write, ipc_mem_read, ipc_mem_write,
    ipc_mem_write_probe, ipc_start_app,
};

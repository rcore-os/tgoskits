mod mem;
mod task;

cfg_fs! {
    mod fs;
    pub use fs::*;
}

cfg_net! {
    mod net;
    pub use net::*;
}

cfg_display! {
    mod display;
    pub use display::*;
}

mod stdio {
    use core::fmt::{self, Write};

    #[cfg(all(feature = "irq", feature = "multitask"))]
    fn runtime_input() -> &'static ax_runtime::RuntimeResult<ax_runtime::console::TaskConsoleInput>
    {
        static INPUT: ax_lazyinit::LazyLock<
            ax_runtime::RuntimeResult<ax_runtime::console::TaskConsoleInput>,
        > = ax_lazyinit::LazyLock::new(ax_runtime::console::take_input);
        &INPUT
    }

    pub fn ax_console_read_bytes(buf: &mut [u8]) -> crate::ApiResult<usize> {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            match runtime_input() {
                Ok(input) => {
                    let mut read = 0;
                    let mut items = [ax_runtime::console::RxItem::default(); 64];
                    while read < buf.len() {
                        let limit = items.len().min(buf.len() - read);
                        let count = input.try_read(&mut items[..limit]);
                        if count == 0 {
                            break;
                        }
                        for item in &items[..count] {
                            if let ax_runtime::console::RxItem::Byte { byte, .. } = *item {
                                buf[read] = if byte == b'\r' { b'\n' } else { byte };
                                read += 1;
                            }
                        }
                    }
                    return Ok(read);
                }
                Err(ax_runtime::RuntimeError::SerialNotStarted) => {}
                Err(error) => return Err((*error).into()),
            }
        }

        let len = ax_hal::console::read_bytes(buf);
        for c in &mut buf[..len] {
            if *c == b'\r' {
                *c = b'\n';
            }
        }
        Ok(len)
    }

    pub fn ax_console_write_bytes(buf: &[u8]) -> crate::ApiResult<usize> {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            match ax_runtime::console::output() {
                Ok(output) => return Ok(output.write_text_all(buf)?),
                Err(ax_runtime::RuntimeError::SerialNotStarted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        ax_hal::console::write_text_bytes(buf);
        Ok(buf.len())
    }

    pub fn ax_console_write_fmt(args: fmt::Arguments) -> fmt::Result {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            match ax_runtime::console::output() {
                Ok(output) => return output.write_fmt(args),
                Err(ax_runtime::RuntimeError::SerialNotStarted) => {}
                Err(_) => return Err(fmt::Error),
            }
        }
        PlatformConsoleWriter.write_fmt(args)
    }

    pub fn ax_console_wait_readable() -> crate::ApiResult {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        match runtime_input() {
            Ok(input) => return Ok(input.wait_readable()?),
            Err(ax_runtime::RuntimeError::SerialNotStarted) => ax_task::yield_now(),
            Err(error) => return Err((*error).into()),
        }
        #[cfg(all(feature = "multitask", not(feature = "irq")))]
        ax_task::yield_now();
        #[cfg(not(feature = "multitask"))]
        core::hint::spin_loop();
        Ok(())
    }

    pub fn ax_console_flush() -> crate::ApiResult {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        match ax_runtime::console::output() {
            Ok(output) => output.drain()?,
            Err(ax_runtime::RuntimeError::SerialNotStarted) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    struct PlatformConsoleWriter;

    impl Write for PlatformConsoleWriter {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            ax_hal::console::write_text_bytes(text.as_bytes());
            Ok(())
        }
    }
}

mod sys {
    pub use ax_hal::cpu_num as ax_get_cpu_num;
    pub use ax_runtime::terminate as ax_terminate;
}

mod time {
    pub use ax_hal::time::{
        TimeValue as AxTimeValue, monotonic_time as ax_monotonic_time, wall_time as ax_wall_time,
    };
}

pub use ax_io::PollState as AxPollState;
pub use ax_runtime::terminate as ax_terminate;

pub use self::{mem::*, stdio::*, sys::*, task::*, time::*};

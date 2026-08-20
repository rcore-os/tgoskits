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
    fn runtime_input() -> ax_runtime::RuntimeResult<&'static ax_runtime::console::TaskConsoleInput>
    {
        static INPUT: ax_lazyinit::OnceLock<ax_runtime::console::TaskConsoleInput> =
            ax_lazyinit::OnceLock::new();
        INPUT.get_or_try_init(ax_runtime::console::take_input)
    }

    pub fn ax_console_read_bytes(buf: &mut [u8]) -> crate::ApiResult<usize> {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            let input = runtime_input()?;
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
            Ok(read)
        }

        #[cfg(not(all(feature = "irq", feature = "multitask")))]
        {
            let len = ax_hal::console::read_bytes(buf);
            for c in &mut buf[..len] {
                if *c == b'\r' {
                    *c = b'\n';
                }
            }
            Ok(len)
        }
    }

    pub fn ax_console_write_bytes(buf: &[u8]) -> crate::ApiResult<usize> {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            let output = ax_runtime::console::output()?;
            Ok(output.write_text_all(buf)?)
        }
        #[cfg(not(all(feature = "irq", feature = "multitask")))]
        {
            ax_hal::console::write_text_bytes(buf);
            Ok(buf.len())
        }
    }

    pub fn ax_console_write_fmt(args: fmt::Arguments) -> fmt::Result {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            ax_runtime::console::output()
                .map_err(|_| fmt::Error)?
                .write_fmt(args)
        }
        #[cfg(not(all(feature = "irq", feature = "multitask")))]
        {
            PlatformConsoleWriter.write_fmt(args)
        }
    }

    pub fn ax_console_wait_readable() -> crate::ApiResult {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            runtime_input()?.wait_readable()?;
        }
        #[cfg(all(feature = "multitask", not(feature = "irq")))]
        ax_task::yield_now();
        #[cfg(not(feature = "multitask"))]
        core::hint::spin_loop();
        Ok(())
    }

    pub fn ax_console_flush() -> crate::ApiResult {
        #[cfg(all(feature = "irq", feature = "multitask"))]
        {
            ax_runtime::console::output()?.drain()?;
        }
        Ok(())
    }

    #[cfg(not(all(feature = "irq", feature = "multitask")))]
    struct PlatformConsoleWriter;

    #[cfg(not(all(feature = "irq", feature = "multitask")))]
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

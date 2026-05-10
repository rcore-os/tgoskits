#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

mod display;

use std::println;

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle, Triangle},
    text::{Alignment, Text},
};

use self::display::Display;

const INIT_X: i32 = 80;
const INIT_Y: i32 = 400;
const RECT_SIZE: u32 = 150;

pub struct DrawingBoard {
    disp: Display,
    latest_pos: Point,
}

impl Default for DrawingBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl DrawingBoard {
    pub fn new() -> Self {
        Self {
            disp: Display::new(),
            latest_pos: Point::new(INIT_X, INIT_Y),
        }
    }

    fn paint(&mut self) {
        Rectangle::with_center(self.latest_pos, Size::new(RECT_SIZE, RECT_SIZE))
            .into_styled(PrimitiveStyle::with_stroke(Rgb888::RED, 10))
            .draw(&mut self.disp)
            .ok();
        Circle::new(self.latest_pos + Point::new(-70, -300), 150)
            .into_styled(PrimitiveStyle::with_fill(Rgb888::BLUE))
            .draw(&mut self.disp)
            .ok();
        Triangle::new(
            self.latest_pos + Point::new(0, 150),
            self.latest_pos + Point::new(80, 200),
            self.latest_pos + Point::new(-120, 300),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb888::GREEN, 10))
        .draw(&mut self.disp)
        .ok();
        let text = "ArceOS";
        Text::with_alignment(
            text,
            self.latest_pos + Point::new(0, 300),
            MonoTextStyle::new(&FONT_10X20, Rgb888::YELLOW),
            Alignment::Center,
        )
        .draw(&mut self.disp)
        .ok();
    }
}

fn test_gpu() {
    let mut board = DrawingBoard::new();
    board.disp.clear(Rgb888::BLACK).unwrap();
    for _ in 0..5 {
        board.latest_pos.x += RECT_SIZE as i32 + 20;
        board.paint();
        board.disp.flush();
    }
    println!("Display tests run OK!");
    #[cfg(feature = "ax-std")]
    std::os::arceos::modules::ax_hal::power::system_off();
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() -> ! {
    test_gpu();
    loop {
        core::hint::spin_loop();
    }
}

}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

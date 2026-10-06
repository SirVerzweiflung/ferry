//! Platform glue: notifications, clipboard, file picker, main loop.
#[cfg(not(win))]
mod unix;
#[cfg(not(win))]
pub use unix::*;

#[cfg(win)]
mod win;
#[cfg(win)]
pub use win::*;

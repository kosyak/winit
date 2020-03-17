#![cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd",
           target_os = "netbsd", target_os = "openbsd"))]

pub use self::window::Window;
pub use self::event_loop::{EventLoop, EventLoopProxy, EventLoopWindowTarget, MonitorHandle, WindowEventsSink};

use gbm::{Surface, AsRaw};

mod event_loop;
mod window;
mod card;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(usize);

#[inline]
fn make_wid(s: &Surface<card::Card>) -> WindowId {
    WindowId(s.as_raw() as usize)
}

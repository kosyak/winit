use drm::control::{Device as ControlDevice, connector::Info as ConnectorInfo, Mode, ResourceInfo,
                   encoder::Info as EncoderInfo, crtc::Info as CrtcInfo};

use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsRawFd, RawFd};

#[derive(Debug)]
// This is our customized struct that implements the traits in drm.
pub struct Card(File);

// Need to implement AsRawFd before we can implement drm::Device
impl AsRawFd for Card {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub fn open(path: &str) -> Self {
        let mut options = OpenOptions::new();
        options.read(true);
        options.write(true);
        Card(options.open(path).unwrap())
    }

    pub fn open_global() -> Self {
        Self::open(&std::env::var("CARD").unwrap_or("/dev/dri/card0".to_string()))
    }

    // fn open_control() -> Self {
    //     Self::open("/dev/dri/controlD64")
    // }

    pub fn try_clone(&self) -> std::io::Result<Self> {
        let file = self.0.try_clone()?;
        Ok(Card(file))
    }

    pub fn get_resources(&self) -> (ConnectorInfo, Mode, EncoderInfo, CrtcInfo) {
        let resources = self.resource_handles().unwrap();

        let connector = resources.connectors().iter().find_map(|&c| {
            if let Ok(c) = ConnectorInfo::load_from_device(self, c) {
                if c.connection_state() == drm::control::connector::State::Connected &&
                    c.size().0 > 0 && c.size().1 > 0
                {
                    return Some(c);
                }
            }

            None
        }).unwrap();

        let connector_clone = connector.clone();
        let modes = connector_clone.modes();
        let modes = &mut modes.to_owned();
        modes.sort_by(|a, b| {
            /*if a.is_preferred() != b.is_preferred() {
                a.is_preferred().cmp(&b.is_preferred()).reverse()
            } else*/ if a.size().0 as u32 * a.size().1 as u32 != b.size().0 as u32 * b.size().1 as u32 {
                (a.size().0 as u32 * a.size().1 as u32).cmp(&(b.size().0 as u32 * b.size().1 as u32)).reverse()
            } else {
                a.vrefresh().cmp(&b.vrefresh()).reverse()
            }
        });

        let mode = modes.iter().next().unwrap();

        let encoder =
            EncoderInfo::load_from_device(self, connector.current_encoder().unwrap()).unwrap();

        let ctrc_handle = encoder.current_crtc().unwrap();
        let crtc = CrtcInfo::load_from_device(self, ctrc_handle).unwrap();

        (connector, *mode, encoder, crtc)
    }
}

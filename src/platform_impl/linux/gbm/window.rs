use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};

use crate::{
    dpi::{LogicalPosition, LogicalSize},
    error::{ExternalError, NotSupportedError, OsError as RootOsError},
    monitor::MonitorHandle as RootMonitorHandle,
    platform_impl::{
        MonitorHandle as PlatformMonitorHandle,
        PlatformSpecificWindowBuilderAttributes as PlAttributes,
    },
    window::{CursorIcon, Fullscreen, WindowAttributes},
};

use super::{make_wid, EventLoopWindowTarget, MonitorHandle, WindowId, card::Card};
use crate::platform_impl::platform::gbm::event_loop::{available_monitors, primary_monitor, page_flip};

use gbm::{Surface, Format, BufferObjectFlags, SurfaceBufferHandle, Device as GbmDevice};
use drm::control::{crtc, connector::Info as ConnectorInfo, Mode, ResourceInfo, encoder::Info as EncoderInfo};

pub(crate) struct Frame {
    pub(crate) bo: Option<SurfaceBufferHandle<Card>>,
    pub(crate) crtc: crtc::Info,
    pub(crate) connector: ConnectorInfo,
    pub(crate) encoder: EncoderInfo,
    pub(crate) mode: Mode,
}

pub struct Window {
    surface: Arc<Surface<Card>>,
    frame: Arc<Mutex<Frame>>,
    // outputs: OutputMgr, // Access to info for all monitors
    display: Arc<GbmDevice<Card>>,
    size: Arc<Mutex<(u32, u32)>>,
    kill_switch: (Arc<Mutex<bool>>, Arc<Mutex<bool>>),
    need_frame_refresh: Arc<Mutex<bool>>,
    need_refresh: Arc<Mutex<bool>>,
    fullscreen: Arc<Mutex<bool>>,
}

impl Window {
    pub fn new<T>(evlp: &EventLoopWindowTarget<T>, attributes: WindowAttributes, _pl_attribs: PlAttributes)
        -> Result<Window, RootOsError>
    {
        let (width, height) = if let Some(fullscreen) = attributes.fullscreen {
            match fullscreen {
                Fullscreen::Exclusive(_) => {
                    panic!("GBM doesn't support exclusive fullscreen")
                }
                Fullscreen::Borderless(RootMonitorHandle {
                    inner: PlatformMonitorHandle::Gbm(ref monitor_id),
                }) => *&monitor_id.size().into(),
                Fullscreen::Borderless(_) => unreachable!(),
            }
        } else {
            attributes.inner_size.map(Into::into).unwrap_or((800, 600))
        };
        // Create the window
        let size = Arc::new(Mutex::new((width, height)));
        let fullscreen = Arc::new(Mutex::new(true));

        let _window_store = evlp.store.clone();
        let gbm = evlp.display.clone();

        let (connector, mode, encoder, crtc) = gbm.get_resources();

        let surface = gbm.create_surface::<Card>(
            width,
            height,
            Format::ARGB8888,
            BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING
        ).unwrap();

        // let window_store = evlp.store.clone();
        // let my_surface = surface.clone();
        // let mut frame = SWindow::<ConceptFrame>::init_from_env(
        //     &evlp.env,
        //     surface.clone(),
        //     (width, height),
        //     move |event| match event {
        //         WEvent::Configure { new_size, states } => {
        //             let mut store = window_store.lock().unwrap();
        //             let is_fullscreen = states.contains(&WState::Fullscreen);

        //             for window in &mut store.windows {
        //                 if window.surface.as_ref().equals(&my_surface.as_ref()) {
        //                     window.newsize = new_size;
        //                     *(window.need_refresh.lock().unwrap()) = true;
        //                     *(window.fullscreen.lock().unwrap()) = is_fullscreen;
        //                     *(window.need_frame_refresh.lock().unwrap()) = true;
        //                     return;
        //                 }
        //             }
        //         }
        //         WEvent::Refresh => {
        //             let store = window_store.lock().unwrap();
        //             for window in &store.windows {
        //                 if window.surface.as_ref().equals(&my_surface.as_ref()) {
        //                     *(window.need_frame_refresh.lock().unwrap()) = true;
        //                     return;
        //                 }
        //             }
        //         }
        //         WEvent::Close => {
        //             let mut store = window_store.lock().unwrap();
        //             for window in &mut store.windows {
        //                 if window.surface.as_ref().equals(&my_surface.as_ref()) {
        //                     window.closed = true;
        //                     return;
        //                 }
        //             }
        //         }
        //     },
        // )
        // .unwrap();

        // if let Some(app_id) = pl_attribs.app_id {
        //     frame.set_app_id(app_id);
        // }

        // frame.set_title(attributes.title);

        // for &(_, ref seat) in evlp.seats.lock().unwrap().iter() {
        //     frame.new_seat(seat);
        // }

        // frame.set_resizable(attributes.resizable);

        // // set decorations
        // frame.set_decorate(attributes.decorations);

        // // min-max dimensions
        // frame.set_min_size(attributes.min_dimensions.map(Into::into));
        // frame.set_max_size(attributes.max_dimensions.map(Into::into));

        let kill_switch = Arc::new(Mutex::new(false));
        let need_frame_refresh = Arc::new(Mutex::new(true));
        let frame = Arc::new(Mutex::new(Frame {
            bo: None,
            crtc,
            connector,
            encoder,
            mode
        }));
        let need_refresh = Arc::new(Mutex::new(true));
        let surface = Arc::new(surface);

        evlp.store.lock().unwrap().windows.push(InternalWindow {
            closed: false,
            newsize: None,
            size: size.clone(),
            need_refresh: need_refresh.clone(),
            fullscreen: fullscreen.clone(),
            need_frame_refresh: need_frame_refresh.clone(),
            surface: surface.clone(),
            kill_switch: kill_switch.clone(),
            frame: Arc::downgrade(&frame),
            current_dpi: 1,
            new_dpi: None,
        });
//        evlp.evq.borrow_mut().sync_roundtrip().unwrap();

        Ok(Window {
            surface,
            frame,
            display: gbm,
            size,
            kill_switch: (kill_switch, evlp.cleanup_needed.clone()),
            need_frame_refresh,
            need_refresh,
            fullscreen,
        })
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        make_wid(&self.surface)
    }

    pub fn set_title(&self, _title: &str) {
        // self.frame.lock().unwrap().set_title(title.into());
    }

    pub fn set_visible(&self, _visible: bool) {
        // TODO
    }

    #[inline]
    pub fn outer_position(&self) -> Result<LogicalPosition, NotSupportedError> {
        Err(NotSupportedError::new())
    }

    #[inline]
    pub fn inner_position(&self) -> Result<LogicalPosition, NotSupportedError> {
        Err(NotSupportedError::new())
    }

    #[inline]
    pub fn set_outer_position(&self, _pos: LogicalPosition) {
        // Not possible with gbm
    }

    pub fn inner_size(&self) -> LogicalSize {
        self.size.lock().unwrap().clone().into()
    }

    pub fn request_redraw(&self) {
        *self.need_refresh.lock().unwrap() = true;
    }

    #[inline]
    pub fn outer_size(&self) -> LogicalSize {
        self.size.lock().unwrap().clone().into()
    }

    #[inline]
    // NOTE: This will only resize the borders, the contents must be updated by the user
    pub fn set_inner_size(&self, size: LogicalSize) {
        let (w, h) = size.into();
        // self.frame.lock().unwrap().resize(w, h);
        *(self.size.lock().unwrap()) = (w, h);
    }

    #[inline]
    pub fn set_min_inner_size(&self, _dimensions: Option<LogicalSize>) {
        // self.frame
        //     .lock()
        //     .unwrap()
        //     .set_min_size(dimensions.map(Into::into));
    }

    #[inline]
    pub fn set_max_inner_size(&self, _dimensions: Option<LogicalSize>) {
        // self.frame
        //     .lock()
        //     .unwrap()
        //     .set_max_size(dimensions.map(Into::into));
    }

    #[inline]
    pub fn set_resizable(&self, _resizable: bool) {
        // self.frame.lock().unwrap().set_resizable(resizable);
    }

    #[inline]
    pub fn scale_factor(&self) -> i32 {
        1
    }

    pub fn set_decorations(&self, _decorate: bool) {
        // self.frame.lock().unwrap().set_decorate(decorate);
        *(self.need_frame_refresh.lock().unwrap()) = true;
    }

    pub fn set_maximized(&self, maximized: bool) {
        if maximized {
            // self.frame.lock().unwrap().set_maximized();
        } else {
            // self.frame.lock().unwrap().unset_maximized();
        }
    }

    pub fn fullscreen(&self) -> Option<Fullscreen> {
        if *(self.fullscreen.lock().unwrap()) {
            Some(Fullscreen::Borderless(RootMonitorHandle {
                inner: PlatformMonitorHandle::Gbm(self.current_monitor()),
            }))
        } else {
            None
        }
    }

    pub fn set_fullscreen(&self, _fullscreen: Option<Fullscreen>) {
        unimplemented!()
        // if let Some(RootMonitorHandle {
        //     inner: PlatformMonitorHandle::gbm(ref monitor_id),
        // }) = monitor
        // {
        //     self.frame
        //         .lock()
        //         .unwrap()
        //         .set_fullscreen(Some(&monitor_id.proxy));
        // } else {
        //     // self.frame.lock().unwrap().unset_fullscreen();
        // }
    }

    #[inline]
    pub fn set_cursor_icon(&self, _cursor: CursorIcon) {
        // TODO
    }

    #[inline]
    pub fn set_cursor_visible(&self, _visible: bool) {
        // TODO: This isn't possible on Gbm yet
    }

    #[inline]
    pub fn set_cursor_grab(&self, _grab: bool) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    #[inline]
    pub fn set_cursor_position(&self, _pos: LogicalPosition) -> Result<(), ExternalError> {
        Err(ExternalError::NotSupported(NotSupportedError::new()))
    }

    pub fn display(&self) -> &GbmDevice<Card> {
        &*self.display
    }

    pub fn surface(&self) -> &Surface<Card> {
        &*self.surface
    }

    pub fn current_monitor(&self) -> MonitorHandle {
        MonitorHandle { handle: self.frame.lock().unwrap().connector.handle(), card: self.display.try_clone().unwrap() }
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        available_monitors(&self.display)
    }

    pub fn primary_monitor(&self) -> MonitorHandle {
        primary_monitor(&self.display)
    }

    pub fn page_flip(&self) {
        let mut mutex_lock = self.frame.as_ref().lock().unwrap();

        page_flip(&self.display, &self.surface, &mut *mutex_lock);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        *(self.kill_switch.0.lock().unwrap()) = true;
        *(self.kill_switch.1.lock().unwrap()) = true;
    }
}

/*
 * Internal store for windows
 */

struct InternalWindow {
    surface: Arc<Surface<Card>>,
    newsize: Option<(u32, u32)>,
    size: Arc<Mutex<(u32, u32)>>,
    need_refresh: Arc<Mutex<bool>>,
    fullscreen: Arc<Mutex<bool>>,
    need_frame_refresh: Arc<Mutex<bool>>,
    closed: bool,
    kill_switch: Arc<Mutex<bool>>,
    frame: Weak<Mutex<Frame>>,
    current_dpi: i32,
    new_dpi: Option<i32>,
}

pub struct WindowStore {
    windows: Vec<InternalWindow>,
}

impl WindowStore {
    pub fn new() -> WindowStore {
        WindowStore {
            windows: Vec::new(),
        }
    }

    pub fn cleanup(&mut self) -> Vec<WindowId> {
        let mut pruned = Vec::new();
        self.windows.retain(|w| {
            if *w.kill_switch.lock().unwrap() {
                // window is dead, cleanup
                pruned.push(make_wid(&w.surface));
                // w.surface.destroy();
                false
            } else {
                true
            }
        });
        pruned
    }

    pub(crate) fn for_each<F>(&mut self, mut f: F)
        where
            F: FnMut(Option<(u32, u32)>, &mut (u32, u32), Option<i32>,
                bool, bool, bool, &Surface<Card>, Option<&mut Frame>),
    {
        for window in &mut self.windows {
            let opt_arc = window.frame.upgrade();
            let mut opt_mutex_lock = opt_arc.as_ref().map(|m| m.lock().unwrap());
            f(
                window.newsize.take(),
                &mut *(window.size.lock().unwrap()),
                window.new_dpi,
                ::std::mem::replace(&mut *window.need_refresh.lock().unwrap(), false),
                ::std::mem::replace(&mut *window.need_frame_refresh.lock().unwrap(), false),
                window.closed,
                &window.surface,
                opt_mutex_lock.as_mut().map(|m| &mut **m),
            );
            if let Some(dpi) = window.new_dpi.take() {
                window.current_dpi = dpi;
            }
            // avoid re-spamming the event
            window.closed = false;
        }
    }
}

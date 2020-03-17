use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt,
    io::{Error as IoError},
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::{ControlFlow, EventLoopClosed, EventLoopWindowTarget as RootELW},
    monitor::VideoMode,
    platform_impl::platform::sticky_exit_callback,
};

use super::{window::{WindowStore, Frame}, DeviceId, card::Card, WindowId};

use gbm::{Surface, Device as GbmDevice};
use drm::control::{Device, crtc, framebuffer, connector::Info as ConnectorInfo, ResourceInfo};

pub struct WindowEventsSink<T> {
    buffer: VecDeque<crate::event::Event<T>>,
}

impl<T> WindowEventsSink<T> {
    pub fn new() -> WindowEventsSink<T> {
        WindowEventsSink {
            buffer: VecDeque::new(),
        }
    }

    pub fn send_event(&mut self, evt: crate::event::Event<T>) {
        self.buffer.push_back(evt);
    }

    pub fn send_window_event(&mut self, evt: crate::event::WindowEvent, wid: WindowId) {
        self.buffer.push_back(crate::event::Event::WindowEvent {
            event: evt,
            window_id: crate::window::WindowId(crate::platform_impl::WindowId::Gbm(wid)),
        });
    }

    pub fn send_device_event(&mut self, evt: crate::event::DeviceEvent, dev_id: DeviceId) {
        self.buffer.push_back(crate::event::Event::DeviceEvent {
            event: evt,
            device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Gbm(dev_id)),
        });
    }

    fn empty_with<F>(&mut self, mut callback: F)
    where
        F: FnMut(crate::event::Event<T>),
    {
        for evt in self.buffer.drain(..) {
            callback(evt)
        }
    }
}

pub struct EventLoop<T: 'static> {
    // The loop
    inner_loop: ::calloop::EventLoop<()>,
    // The gbm display
    pub display: Arc<GbmDevice<Card>>,
    // the output manager
    // pub outputs: OutputMgr,
    // our sink, shared with some handlers, buffering the events
    sink: Arc<Mutex<WindowEventsSink<T>>>,
    pending_user_events: Rc<RefCell<VecDeque<T>>>,
    _user_source: ::calloop::Source<::calloop::channel::Channel<T>>,
    user_sender: ::calloop::channel::Sender<T>,
    _kbd_source: ::calloop::Source<
        ::calloop::channel::Channel<(crate::event::WindowEvent, super::WindowId)>,
    >,
    window_target: RootELW<T>,
}

// A handle that can be sent across threads and used to wake up the `EventLoop`.
//
// We should only try and wake up the `EventLoop` if it still exists, so we hold Weak ptrs.
pub struct EventLoopProxy<T: 'static> {
    user_sender: ::calloop::channel::Sender<T>,
}

pub struct EventLoopWindowTarget<T> {
    // the event queue
    // pub evq: RefCell<::calloop::Source<EventQueue>>,
    // The window store
    pub store: Arc<Mutex<WindowStore>>,
    // the env
    // pub env: Environment,
    // a cleanup switch to prune dead windows
    pub cleanup_needed: Arc<Mutex<bool>>,
    // The wayland display
    pub display: Arc<GbmDevice<Card>>,
    _marker: ::std::marker::PhantomData<T>,
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        EventLoopProxy {
            user_sender: self.user_sender.clone(),
        }
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed> {
        self.user_sender.send(event).map_err(|_| EventLoopClosed)
    }
}

impl<T: 'static> EventLoop<T> {
    pub fn new() -> Result<EventLoop<T>, IoError> {
        let display = GbmDevice::new(Card::open_global()).unwrap();
        let display = Arc::new(display);

        let sink = Arc::new(Mutex::new(WindowEventsSink::new()));
        let store = Arc::new(Mutex::new(WindowStore::new()));

        let inner_loop = ::calloop::EventLoop::new().unwrap();

        let (_kbd_sender, kbd_channel) = ::calloop::channel::channel();
        let kbd_sink = sink.clone();
        let kbd_source = inner_loop
            .handle()
            .insert_source(kbd_channel, move |evt, &mut ()| {
                if let ::calloop::channel::Event::Msg((evt, wid)) = evt {
                    kbd_sink.lock().unwrap().send_window_event(evt, wid);
                }
            })
            .unwrap();

        let pending_user_events = Rc::new(RefCell::new(VecDeque::new()));
        let pending_user_events2 = pending_user_events.clone();

        let (user_sender, user_channel) = ::calloop::channel::channel();

        let user_source = inner_loop
            .handle()
            .insert_source(user_channel, move |evt, &mut ()| {
                if let ::calloop::channel::Event::Msg(msg) = evt {
                    pending_user_events2.borrow_mut().push_back(msg);
                }
            })
            .unwrap();

        Ok(EventLoop {
            inner_loop,
            sink,
            pending_user_events,
            display: display.clone(),
            // outputs: env.outputs.clone(),
            _user_source: user_source,
            user_sender,
            _kbd_source: kbd_source,
            window_target: RootELW {
                p: crate::platform_impl::EventLoopWindowTarget::Gbm(EventLoopWindowTarget {
                    // evq: RefCell::new(source),
                    store,
                    // env,
                    cleanup_needed: Arc::new(Mutex::new(false)),
                    display: display.clone(),
                    _marker: ::std::marker::PhantomData,
                }),
                _marker: ::std::marker::PhantomData,
            },
        })
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy {
            user_sender: self.user_sender.clone(),
        }
    }

    pub fn run<F>(mut self, callback: F) -> !
    where
        F: 'static + FnMut(crate::event::Event<T>, &RootELW<T>, &mut ControlFlow),
    {
        self.run_return(callback);
        ::std::process::exit(0);
    }

    pub fn run_return<F>(&mut self, mut callback: F)
    where
        F: FnMut(crate::event::Event<T>, &RootELW<T>, &mut ControlFlow),
    {
        // send pending events to the server
        self.display_flush();

        let mut control_flow = ControlFlow::default();

        let sink = self.sink.clone();
        let user_events = self.pending_user_events.clone();

        callback(
            crate::event::Event::NewEvents(crate::event::StartCause::Init),
            &self.window_target,
            &mut control_flow,
        );

        loop {
            self.post_dispatch_triggers();

            // empty buffer of events
            {
                let mut guard = sink.lock().unwrap();
                guard.empty_with(|evt| {
                    sticky_exit_callback(
                        evt,
                        &self.window_target,
                        &mut control_flow,
                        &mut callback,
                    );
                });
            }
            // empty user events
            {
                let mut guard = user_events.borrow_mut();
                for evt in guard.drain(..) {
                    sticky_exit_callback(
                        crate::event::Event::UserEvent(evt),
                        &self.window_target,
                        &mut control_flow,
                        &mut callback,
                    );
                }
            }
            // do a second run of post-dispatch-triggers, to handle user-generated "request-redraw"
            // in response of resize & friends
            self.post_dispatch_triggers();
            {
                let mut guard = sink.lock().unwrap();
                guard.empty_with(|evt| {
                    sticky_exit_callback(
                        evt,
                        &self.window_target,
                        &mut control_flow,
                        &mut callback,
                    );
                });
            }
            // send Events cleared
            {
                sticky_exit_callback(
                    crate::event::Event::EventsCleared,
                    &self.window_target,
                    &mut control_flow,
                    &mut callback,
                );
            }

            // send pending events to the server
            self.display_flush();

            match control_flow {
                ControlFlow::Exit => break,
                ControlFlow::Poll => {
                    // non-blocking dispatch
                    self.inner_loop
                        .dispatch(Some(::std::time::Duration::from_millis(0)), &mut ())
                        .unwrap();
                    callback(
                        crate::event::Event::NewEvents(crate::event::StartCause::Poll),
                        &self.window_target,
                        &mut control_flow,
                    );
                }
                ControlFlow::Wait => {
                    self.inner_loop.dispatch(None, &mut ()).unwrap();
                    callback(
                        crate::event::Event::NewEvents(crate::event::StartCause::WaitCancelled {
                            start: Instant::now(),
                            requested_resume: None,
                        }),
                        &self.window_target,
                        &mut control_flow,
                    );
                }
                ControlFlow::WaitUntil(deadline) => {
                    let start = Instant::now();
                    // compute the blocking duration
                    let duration = if deadline > start {
                        deadline - start
                    } else {
                        ::std::time::Duration::from_millis(0)
                    };
                    self.inner_loop.dispatch(Some(duration), &mut ()).unwrap();
                    let now = Instant::now();
                    if now < deadline {
                        callback(
                            crate::event::Event::NewEvents(
                                crate::event::StartCause::WaitCancelled {
                                    start,
                                    requested_resume: Some(deadline),
                                },
                            ),
                            &self.window_target,
                            &mut control_flow,
                        );
                    } else {
                        callback(
                            crate::event::Event::NewEvents(
                                crate::event::StartCause::ResumeTimeReached {
                                    start,
                                    requested_resume: deadline,
                                },
                            ),
                            &self.window_target,
                            &mut control_flow,
                        );
                    }
                }
            }
        }

        callback(
            crate::event::Event::LoopDestroyed,
            &self.window_target,
            &mut control_flow,
        );
    }

    pub fn primary_monitor(&self) -> MonitorHandle {
        primary_monitor(&self.display)
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        available_monitors(&self.display)
    }

    pub fn display(&self) -> &GbmDevice<Card> {
        &*self.display
    }

    pub fn window_target(&self) -> &RootELW<T> {
        &self.window_target
    }
}

/*
 * Private EventLoop Internals
 */

impl<T> EventLoop<T> {
    fn post_dispatch_triggers(&mut self) {
        let mut sink = self.sink.lock().unwrap();
        let window_target = match self.window_target.p {
            crate::platform_impl::EventLoopWindowTarget::Gbm(ref wt) => wt,
            _ => unreachable!(),
        };
        // prune possible dead windows
        {
            let mut cleanup_needed = window_target.cleanup_needed.lock().unwrap();
            if *cleanup_needed {
                let pruned = window_target.store.lock().unwrap().cleanup();
                *cleanup_needed = false;
                for wid in pruned {
                    sink.send_window_event(crate::event::WindowEvent::Destroyed, wid);
                }
            }
        }
        // process pending resize/refresh
        window_target.store.lock().unwrap().for_each(
            |newsize, _size, new_dpi, refresh, frame_refresh, closed, surface, frame| {
                use crate::platform_impl::platform::gbm::make_wid;

                if let Some(frame) = frame {
                    if let Some((w, h)) = newsize {
                        // frame.resize(w, h);
                        // frame.refresh();
                        // let logical_size = ::LogicalSize::new(w as f64, h as f64);
                        // sink.send_window_event(crate::event::Event::WindowEvent::Resized(logical_size), wid);
                        // *size = (w, h);
                    } else if frame_refresh {
                        // if !refresh {
                        //     frame.surface().commit()
                        // }
                    }
                }
                if let Some(dpi) = new_dpi {
                    sink.send_window_event(crate::event::WindowEvent::HiDpiFactorChanged(dpi as f64), make_wid(surface));
                }
                if refresh {
                    sink.send_window_event(crate::event::WindowEvent::RedrawRequested, make_wid(surface));
                }
                if closed {
                    sink.send_window_event(crate::event::WindowEvent::CloseRequested, make_wid(surface));
                }
            },
        )
    }

    fn display_flush(&self) {

    }
}

/*
 * Monitor stuff
 */

pub struct MonitorHandle {
    pub(crate) handle: drm::control::connector::Handle,
    pub(crate) card: Card,
}

impl Clone for MonitorHandle {
    fn clone(&self) -> MonitorHandle {
        MonitorHandle {
            handle: self.handle.clone(),
            card: self.card.try_clone().unwrap(),
        }
    }
}

impl PartialEq for MonitorHandle {
    fn eq(&self, other: &Self) -> bool {
        unimplemented!()
        // self.native_identifier() == other.native_identifier()
    }
}

impl Eq for MonitorHandle {}

impl PartialOrd for MonitorHandle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        unimplemented!()
        // Some(self.cmp(&other))
    }
}

impl Ord for MonitorHandle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        unimplemented!()
        // self.native_identifier().cmp(&other.native_identifier())
    }
}

impl std::hash::Hash for MonitorHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        unimplemented!()
        // self.native_identifier().hash(state);
    }
}

impl fmt::Debug for MonitorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[derive(Debug)]
        struct MonitorHandle {
            name: Option<String>,
            native_identifier: u32,
            size: PhysicalSize,
            position: PhysicalPosition,
            hidpi_factor: i32,
        }

        let monitor_id_proxy = MonitorHandle {
            name: self.name(),
            native_identifier: self.native_identifier(),
            size: self.size(),
            position: self.position(),
            hidpi_factor: self.hidpi_factor(),
        };

        monitor_id_proxy.fmt(f)
    }
}

impl MonitorHandle {
    pub fn name(&self) -> Option<String> {
        let info = drm::control::connector::Info::load_from_device(&self.card, self.handle).unwrap();
        Some(format!("{} ({:?}, {:?})", "DRM", info.connector_type(), info.connection_state()))
    }

    #[inline]
    pub fn native_identifier(&self) -> u32 {
        self.handle.into()
    }

    pub fn size(&self) -> PhysicalSize {
        let (_connector, mode, _encoder, _crtc) = self.card.get_resources();
        (mode.size().0 as u32, mode.size().1 as u32).into()
    }

    pub fn position(&self) -> PhysicalPosition {
        (0, 0).into()
    }

    #[inline]
    pub fn hidpi_factor(&self) -> i32 {
        1
    }

    #[inline]
    pub fn video_modes(&self) -> impl Iterator<Item = VideoMode> {
        vec![].into_iter()
        // self.mgr
        //     .with_info(&self.proxy, |_, info| info.modes.clone())
        //     .unwrap_or(vec![])
        //     .into_iter()
        //     .map(|x| VideoMode {
        //         size: (x.dimensions.0 as u32, x.dimensions.1 as u32),
        //         refresh_rate: (x.refresh_rate as f32 / 1000.0).round() as u16,
        //         bit_depth: 32,
        //     })
    }
}

pub fn primary_monitor(display: &GbmDevice<Card>) -> MonitorHandle {
    available_monitors(display).iter().next().unwrap().clone()
}

pub fn available_monitors(display: &GbmDevice<Card>) -> VecDeque<MonitorHandle> {
    display.resource_handles()
        .unwrap()
        .connectors()
        .iter()
        .filter_map(|&handle| {
            ConnectorInfo::load_from_device(display, handle).ok().and_then(|info|
                if info.connection_state() == drm::control::connector::State::Connected &&
                    info.size().0 * info.size().1 > 0
                {
                    Some(MonitorHandle { handle: handle, card: display.try_clone().unwrap() })
                } else {
                    None
                }
            )
        })
        .collect()
}

pub(crate) fn page_flip(display: &GbmDevice<Card>, surface: &Surface<Card>, frame: &mut Frame) {
    let mut next_bo = unsafe { surface.lock_front_buffer() }.unwrap();
    let fb_handle = framebuffer::create(display, &*next_bo).unwrap().handle();

    // * Here you could also update drm plane layers if you want
    // * hw composition

    if set_crtc(display, surface, fb_handle, frame) {
        frame.bo = Some(next_bo);
        return;
    }

    crtc::page_flip(
        display,
        frame.crtc.handle(),
        fb_handle,
        &[crtc::PageFlipFlags::PageFlipEvent],
    ).expect("Failed to queue Page Flip");

    let mut events: crtc::Events;
    let mut waiting_for_flip = true;
    while waiting_for_flip {
        events = crtc::receive_events(display).unwrap();
        for event in events {
            match event {
                crtc::Event::Vblank(s) => { dbg!("VblankEvent:{}", s.frame); },
                crtc::Event::PageFlip(s) => { waiting_for_flip = false; }
                crtc::Event::Unknown(s) => { dbg!("unkonw event:{:?}", s); },
            }
        }
    }

    frame.bo = Some(next_bo);
}

fn set_crtc(display: &GbmDevice<Card>, surface: &Surface<Card>, fb_handle: framebuffer::Handle, frame: &mut Frame) -> bool {
    if frame.bo.is_some() {
        false
    } else {
        let _ = crtc::set(
            display,
            frame.crtc.handle(),
            fb_handle,
            &[frame.connector.handle()],
            (0, 0),
            Some(frame.mode)
        ).unwrap();

        true
    }
}

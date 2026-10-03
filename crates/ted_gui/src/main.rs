//! The ted desktop app: a winit window drawn with softbuffer, turning window events into
//! editor keys and mouse input and drawing each `Frame` the core renders.

mod keys;
mod renderer;
mod shell_env;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::event::{ElementState, Event, KeyEvent as WinitKeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winit::platform::wayland::WindowBuilderExtWayland;
use winit::window::WindowBuilder;

use renderer::GuiRenderer;
use ted_core::{Editor, FaceId, Frame, Metrics, Modifiers, StartupOptions};

/// Presses this close in time and space count as one multi-click.
const MULTI_CLICK_TIME: Duration = Duration::from_millis(400);
const MULTI_CLICK_SLOP: f32 = 4.0;

/// Counts quick presses in one spot, since winit reports every press on its own.
#[derive(Default)]
struct ClickCounter {
    last: Option<(Instant, f32, f32)>,
    count: u32,
}

impl ClickCounter {
    fn press(&mut self, now: Instant, x: f32, y: f32) -> u32 {
        let repeat = self.last.is_some_and(|(at, lx, ly)| {
            now.duration_since(at) <= MULTI_CLICK_TIME
                && (x - lx).abs() <= MULTI_CLICK_SLOP
                && (y - ly).abs() <= MULTI_CLICK_SLOP
        });
        self.count = if repeat { self.count + 1 } else { 1 };
        self.last = Some((now, x, y));
        self.count
    }
}

/// Passes a restarting ted's session to the process replacing it (`reload-ted`).
const RESTORE_SESSION: &str = "--restore-session";

fn main() {
    let (initial_files, session) = match parse_args() {
        Launch::Editor { files, session } => (files, session),
        Launch::TermHost(socket) => {
            if let Err(e) = ted_term::host::run(&socket) {
                eprintln!("ted: cannot run the terminal host: {}", e);
            }
            return;
        }
    };
    // A restart inherits the environment its predecessor already imported.
    if session.is_none() {
        shell_env::import_if_launched_from_desktop();
    }
    // Resolved now: once a rebuild replaces the binary, Linux reports this one as deleted.
    let exe = std::env::current_exe();

    let event_loop = EventLoopBuilder::<()>::with_user_event().build().expect("Failed to create event loop");
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("ted")
            // Wayland app_id and X11 WM_CLASS (winit shares the field): must match ted.desktop so
            // launchers and docks associate the window with the installed entry and its icon.
            .with_name("ted", "ted")
            // The size it restores to when unmaximized.
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            .with_maximized(true)
            .build(&event_loop)
            .expect("Failed to create window"),
    );

    // Terminals run in the terminal host, this binary started with `--term-host`.
    let term = match &exe {
        Ok(exe) => ted_term::TermPlugin::default().persistent(exe.clone()),
        Err(_) => ted_term::TermPlugin::default(),
    };
    let plugins: Vec<Box<dyn ted_core::Plugin>> =
        vec![Box::new(ted_git::GitPlugin), Box::new(term), Box::new(ted_lsp::LspPlugin)];
    let options = StartupOptions { plugins, ..StartupOptions::user() };
    let mut editor = Editor::with_options(&initial_files, options);
    // Background jobs wake the event loop when they have results.
    let proxy = Mutex::new(event_loop.create_proxy());
    editor.set_waker(move || {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(());
        }
    });
    if let Some(session) = &session {
        ted_core::session::resume(&mut editor, session);
    }

    let mut renderer = GuiRenderer::new(window.clone(), editor.font_metrics());
    let (mut width, mut height) = (1280u32, 800u32);
    let mut modifiers = Modifiers::default();
    let (mut mouse_x, mut mouse_y, mut mouse_down) = (0.0f32, 0.0f32, false);
    let mut clicks = ClickCounter::default();
    // Wheel travel not yet scrolled, in lines: touchpads send fractions of a line.
    let mut wheel_lines = 0.0f64;
    let mut frame = Frame::new(width as f32, height as f32, editor.faces.bg(FaceId::DEFAULT));
    renderer.resize(width, height);
    let mut restart = None;
    let restart_to = &mut restart;
    let mut title = String::new();

    event_loop
        .run(move |event, target| {
            let mut redraw = false;
            match event {
                Event::UserEvent(()) | Event::AboutToWait => {
                    redraw = editor.poll(Instant::now());
                    target.set_control_flow(match editor.next_deadline() {
                        Some(deadline) => ControlFlow::WaitUntil(deadline),
                        None => ControlFlow::Wait,
                    });
                }
                // Sent once, as the loop ends: events keep arriving after `exit` until then.
                Event::LoopExiting => *restart_to = editor.restart.take(),
                Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                    WindowEvent::Focused(focused) => {
                        editor.focused = focused;
                        if focused {
                            editor.check_external_changes();
                        }
                        redraw = true;
                    }
                    WindowEvent::CloseRequested => {
                        editor.request_exit();
                        redraw = true;
                    }
                    WindowEvent::Resized(size) => {
                        width = size.width.max(1);
                        height = size.height.max(1);
                        renderer.resize(width, height);
                        redraw = true;
                    }
                    WindowEvent::ModifiersChanged(mods) => {
                        let state = mods.state();
                        modifiers =
                            Modifiers { ctrl: state.control_key(), alt: state.alt_key(), shift: state.shift_key() };
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        (mouse_x, mouse_y) = (position.x as f32, position.y as f32);
                        if mouse_down {
                            editor.handle_mouse_drag(mouse_x, mouse_y);
                            redraw = true;
                        }
                    }
                    WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                        mouse_down = state == ElementState::Pressed;
                        if mouse_down {
                            let count = clicks.press(Instant::now(), mouse_x, mouse_y);
                            editor.handle_mouse_press(mouse_x, mouse_y, count);
                            redraw = true;
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        wheel_lines -= match delta {
                            MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
                            MouseScrollDelta::PixelDelta(pos) => pos.y / renderer.line_h.max(1.0) as f64,
                        };
                        let lines = wheel_lines.trunc();
                        if lines != 0.0 {
                            wheel_lines -= lines;
                            editor.handle_scroll(mouse_x, mouse_y, lines as isize);
                            redraw = true;
                        }
                    }
                    WindowEvent::KeyboardInput {
                        event: WinitKeyEvent { state: ElementState::Pressed, logical_key, text, physical_key, .. },
                        ..
                    } => {
                        if let Some(key) = keys::translate(physical_key, &logical_key, text.as_deref(), modifiers) {
                            editor.handle_key(key);
                            redraw = true;
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        renderer.set_font(editor.font_metrics());
                        frame.clear(width as f32, height as f32, editor.faces.bg(FaceId::DEFAULT));
                        editor.render(&mut frame, Metrics::new(renderer.char_w, renderer.line_h));
                        renderer.render(&frame, width, height);
                    }
                    _ => {}
                },
                _ => {}
            }
            if !editor.running {
                target.exit();
            } else if redraw {
                let now = editor.title();
                if now != title {
                    window.set_title(&now);
                    title = now;
                }
                window.request_redraw();
            }
        })
        .expect("Event loop error");

    // The editor is gone by now, and with it its terminals and language servers.
    if let Some(session) = restart {
        let err = match exe {
            Ok(exe) => Command::new(exe).arg(RESTORE_SESSION).arg(&session).exec(),
            Err(e) => e,
        };
        eprintln!("ted: cannot restart: {}", err);
    }
}

/// What the command line asks the process to be.
enum Launch {
    /// The editor, with the files to open, and the session to restore instead when
    /// restarting.
    Editor { files: Vec<PathBuf>, session: Option<PathBuf> },
    /// The terminal host, listening on the socket.
    TermHost(PathBuf),
}

fn parse_args() -> Launch {
    let (mut files, mut session) = (Vec::new(), None);
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == ted_term::host::FLAG {
            return Launch::TermHost(args.next().map(PathBuf::from).unwrap_or_default());
        } else if arg == RESTORE_SESSION {
            session = args.next().map(PathBuf::from);
        } else {
            files.push(PathBuf::from(arg));
        }
    }
    Launch::Editor { files, session }
}

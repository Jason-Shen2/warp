//! The TUI runtime, additive behind the `tui` feature: the alternate-screen
//! lifecycle and the draw + event loop that drives a [`TuiView`] through the
//! shared [`App`].
//!
//! Placement: the GUI has no in-core analog of this module — its runtime is
//! the platform event loop in the `warpui` crate — so the TUI runtime stands
//! alone as an additive top-level module rather than a backend submodule of an
//! existing one.
//!
//! [`TuiRuntime`] mirrors the GUI's invalidate→redraw flow. On
//! [`enter`](TuiRuntime::enter) it puts the host terminal into raw mode + the
//! alternate screen (restored on drop) and subscribes to the window's
//! invalidation signal; [`run_until`](TuiRuntime::run_until) then repeatedly
//! redraws when dirty and polls crossterm for input, converting each event with
//! [`crossterm_event_to_warp_event`] and dispatching it — first through the
//! shared keymap (the focused view's responder chain, exactly like the GUI
//! window event path), then through the rendered element tree.
//!
//! The host terminal is abstracted behind [`TuiTerminal`] so the loop and the
//! frame renderer can be exercised headlessly against an in-memory writer
//! without a real tty. The concrete [`CrosstermTerminal`] is the production
//! implementation.

use std::cell::{Cell, RefCell};
use std::io::{self, stdout, Stdout, Write};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};

use crate::elements::tui::{TuiEventContext, TuiLayoutContext, TuiRect, TuiSize};
use crate::platform::TerminationMode;
use crate::presenter::tui::TuiPresenter;
use crate::r#async::block_on;
use crate::r#async::executor::ForegroundTask;
use crate::{App, AppContext, Event, TuiView, ViewHandle, WindowId};

mod event_conversion;
mod renderer;

pub use event_conversion::crossterm_event_to_warp_event;
pub use renderer::TuiFrameRenderer;

/// The host terminal the runtime draws to and reads input from. Abstracted so
/// the draw + event loop is testable against an in-memory target.
pub trait TuiTerminal {
    /// The current terminal size in cells (each axis at least 1).
    fn size(&self) -> io::Result<TuiSize>;

    /// Blocks up to `timeout` for the next input event, returning `None` on
    /// timeout.
    fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<CrosstermEvent>>;

    /// The writer the renderer flushes frames to.
    fn writer(&mut self) -> &mut dyn Write;
}

/// Drives a single [`TuiView`] window: redraws it when invalidated and routes
/// input events back through the shared core.
pub struct TuiRuntime<T, R = CrosstermTerminal>
where
    R: TuiTerminal,
{
    window_id: WindowId,
    root_view: ViewHandle<T>,
    presenter: TuiPresenter,
    renderer: TuiFrameRenderer,
    terminal: R,
    dirty: Rc<Cell<bool>>,
    last_size: Option<TuiSize>,
}

impl<T> TuiRuntime<T, CrosstermTerminal>
where
    T: TuiView,
{
    /// Enters the alternate screen + raw mode and prepares to drive `root_view`.
    /// The terminal is restored when the returned runtime is dropped.
    pub fn enter(app: &App, window_id: WindowId, root_view: ViewHandle<T>) -> io::Result<Self> {
        let terminal = CrosstermTerminal::enter()?;
        Ok(Self::with_terminal(app, window_id, root_view, terminal))
    }
}

/// Owns raw mode and alternate-screen state independently of a terminal writer.
pub struct TuiTerminalGuard(RawModeGuard<CrosstermModeControl>);

impl TuiTerminalGuard {
    /// Enters raw mode and the alternate screen.
    pub fn enter() -> io::Result<Self> {
        Ok(Self(RawModeGuard::enter(CrosstermModeControl)?))
    }
}

/// Keeps an app-owned TUI driver and terminal-mode guard alive.
pub struct TuiDriverHandle {
    _task: ForegroundTask,
    _reader: thread::JoinHandle<()>,
    _guard: TuiTerminalGuard,
}

struct AppTuiScreen<T: TuiView> {
    window_id: WindowId,
    root_view: ViewHandle<T>,
    presenter: TuiPresenter,
    renderer: TuiFrameRenderer,
    terminal: CrosstermTerminal,
}

impl<T: TuiView> AppTuiScreen<T> {
    fn new(window_id: WindowId, root_view: ViewHandle<T>) -> Self {
        Self {
            window_id,
            root_view,
            presenter: TuiPresenter::new(),
            renderer: TuiFrameRenderer::new(),
            terminal: CrosstermTerminal::new(),
        }
    }

    fn draw(&mut self, ctx: &mut AppContext) -> io::Result<()> {
        let size = self.terminal.size()?;
        let area = TuiRect::new(0, 0, size.width, size.height);
        let invalidation = ctx.take_all_invalidations_for_window(self.window_id);
        self.presenter
            .invalidate(&invalidation, ctx, self.window_id);
        let frame = self.presenter.present(ctx, &self.root_view, area);
        let mut writer = self.terminal.writer();
        self.renderer.draw(&mut writer, &frame.buffer, frame.cursor)
    }

    fn dispatch_event(&mut self, ctx: &mut AppContext, event: &Event) -> bool {
        if let Event::KeyDown {
            keystroke,
            is_composing,
            ..
        } = event
        {
            let responder_chain = ctx.get_responder_chain(self.window_id);
            match ctx.dispatch_keystroke(self.window_id, &responder_chain, keystroke, *is_composing)
            {
                Ok(true) => return true,
                Ok(false) => {}
                Err(error) => log::error!("error dispatching TUI keystroke: {error}"),
            }
        }

        let Some(element) = self.presenter.last_element.as_mut() else {
            return false;
        };
        let size = self.terminal.size().unwrap_or_default();
        let area = TuiRect::new(0, 0, size.width, size.height);
        let root_view_id = self.root_view.id();
        let mut event_ctx = TuiEventContext::default();
        event_ctx.set_origin_view(Some(root_view_id));
        let handled = {
            let app = &*ctx;
            let mut layout_ctx =
                TuiLayoutContext::with_app(app, &mut self.presenter.rendered_views);
            element.dispatch_event(event, area, &mut event_ctx, &mut layout_ctx, app)
        };
        let _ = event_ctx.take_updates();
        for action in event_ctx.take_typed_actions() {
            ctx.dispatch_typed_action_for_view(
                self.window_id,
                action.origin_view_id,
                action.action.as_ref(),
            );
        }
        handled
    }
}

/// Starts an invalidation-driven TUI driver inside Warp's shared app loop.
pub fn spawn_tui_driver<T: TuiView>(
    ctx: &mut AppContext,
    window_id: WindowId,
    root_view: ViewHandle<T>,
) -> io::Result<TuiDriverHandle> {
    let guard = TuiTerminalGuard::enter()?;
    let screen = Rc::new(RefCell::new(AppTuiScreen::new(window_id, root_view)));
    {
        let screen = screen.clone();
        ctx.on_window_invalidated(window_id, move |_, ctx| {
            let Ok(mut screen) = screen.try_borrow_mut() else {
                return;
            };
            if let Err(error) = screen.draw(ctx) {
                log::error!("failed to draw a TUI frame: {error}");
            }
        });
    }
    if let Err(error) = screen.borrow_mut().draw(ctx) {
        log::error!("failed to draw the initial TUI frame: {error}");
    }

    let weak_app = ctx.weak_app();
    let (sender, receiver) = async_channel::unbounded::<CrosstermEvent>();
    let screen_for_input = screen.clone();
    let reader = thread::Builder::new()
        .name("warp-tui-input".to_owned())
        .spawn(move || loop {
            match event::read() {
                Ok(event) => {
                    if block_on(sender.send(event)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    log::error!("failed to read a terminal event: {error}");
                    break;
                }
            }
        })?;

    let task = ctx.foreground_executor().spawn(async move {
        while let Ok(event) = receiver.recv().await {
            let Some(app) = weak_app.upgrade() else {
                break;
            };
            let mut ctx = app.borrow_mut();
            if is_ctrl_c(&event) {
                ctx.terminate_app(TerminationMode::ForceTerminate, None);
                continue;
            }
            let Some(event) = crossterm_event_to_warp_event(event) else {
                continue;
            };
            let handled = screen_for_input
                .borrow_mut()
                .dispatch_event(&mut ctx, &event);
            if handled {
                ctx.invalidate_all_views();
            }
        }
    });

    Ok(TuiDriverHandle {
        _task: task,
        _reader: reader,
        _guard: guard,
    })
}

fn is_ctrl_c(event: &CrosstermEvent) -> bool {
    matches!(
        event,
        CrosstermEvent::Key(key)
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
    )
}

impl<T, R> TuiRuntime<T, R>
where
    T: TuiView,
    R: TuiTerminal,
{
    /// Builds a runtime over an arbitrary [`TuiTerminal`]. Subscribes to the
    /// window's invalidation signal so a `notify` schedules a redraw, and marks
    /// the runtime dirty so the first loop iteration paints.
    pub fn with_terminal(
        app: &App,
        window_id: WindowId,
        root_view: ViewHandle<T>,
        terminal: R,
    ) -> Self {
        let dirty = Rc::new(Cell::new(true));
        let dirty_for_callback = dirty.clone();
        app.on_window_invalidated(window_id, move |_, _| dirty_for_callback.set(true));
        Self {
            window_id,
            root_view,
            presenter: TuiPresenter::new(),
            renderer: TuiFrameRenderer::new(),
            terminal,
            dirty,
            last_size: None,
        }
    }

    /// Runs the draw + input loop until `should_quit` returns `true`, redrawing
    /// when invalidated (or resized) and dispatching converted input events.
    pub fn run_until(
        &mut self,
        app: &mut App,
        mut should_quit: impl FnMut(&App) -> bool,
    ) -> io::Result<()> {
        while !should_quit(app) {
            self.draw_if_dirty(app)?;
            // 250 ms is a standard event-poll heartbeat: short enough to feel
            // responsive to resize, long enough to avoid busy-waiting. A timeout
            // is not an error — `poll_event` returns `Ok(None)`, making the loop
            // iteration a no-op before the next draw-if-dirty check.
            self.poll_and_dispatch(app, Duration::from_millis(250))?;
        }
        Ok(())
    }

    /// The terminal this runtime draws to. Primarily useful for inspecting an
    /// in-memory terminal's captured output in tests.
    pub fn terminal(&self) -> &R {
        &self.terminal
    }

    fn draw_if_dirty(&mut self, app: &mut App) -> io::Result<()> {
        let size = self.terminal.size()?;
        if self.last_size != Some(size) {
            self.dirty.set(true);
        }
        if !self.dirty.replace(false) {
            return Ok(());
        }

        // Lay out and paint the view through the dedicated presenter, which
        // resolves the root (and any embedded child views) through the app,
        // reports the discovered view embeddings into the shared hierarchy,
        // and returns a composited frame (buffer + cursor).
        let area = TuiRect::new(0, 0, size.width, size.height);
        let window_id = self.window_id;
        let presenter = &mut self.presenter;
        let root_view = &self.root_view;
        let frame = app.update(|ctx| {
            // Re-render only the views that changed this frame, then present
            // the full tree (unchanged views reuse their cached elements).
            let invalidation = ctx.take_all_invalidations_for_window(window_id);
            presenter.invalidate(&invalidation, ctx, window_id);
            presenter.present(ctx, root_view, area)
        });

        let mut writer = self.terminal.writer();
        self.renderer
            .draw(&mut writer, &frame.buffer, frame.cursor)?;
        self.last_size = Some(size);
        Ok(())
    }

    fn poll_and_dispatch(&mut self, app: &mut App, timeout: Duration) -> io::Result<()> {
        let Some(event) = self.terminal.poll_event(timeout)? else {
            return Ok(());
        };

        match event {
            CrosstermEvent::Resize(_, _) => self.dirty.set(true),
            event => {
                if let Some(warp_event) = crossterm_event_to_warp_event(event) {
                    // Redraws are triggered by views calling `ctx.notify()`, which
                    // fires `on_window_invalidated` and sets the dirty flag. An event
                    // being handled is not itself a reason to redraw.
                    self.dispatch_event(app, &warp_event);
                }
            }
        }
        Ok(())
    }

    fn dispatch_event(&mut self, app: &mut App, event: &Event) -> bool {
        // Keymap pass (GUI parity): offer a keystroke to the focused view's
        // responder chain first, exactly like the GUI window event path.
        if let Event::KeyDown {
            keystroke,
            is_composing,
            ..
        } = event
        {
            let window_id = self.window_id;
            match app.update(|ctx| {
                let responder_chain = ctx.get_responder_chain(window_id);
                ctx.dispatch_keystroke(window_id, &responder_chain, keystroke, *is_composing)
            }) {
                Ok(true) => return true,
                Ok(false) => {}
                Err(error) => log::error!("error dispatching keystroke: {error}"),
            }
        }

        // Element-tree pass: walk the last rendered+laid-out element tree
        // (cached by the presenter from the most recent draw). Access the two
        // presenter fields directly so Rust can see they are disjoint borrows.
        let Some(element) = self.presenter.last_element.as_mut() else {
            return false; // no draw has happened yet
        };
        let size = self.last_size.unwrap_or_default();
        let area = TuiRect::new(0, 0, size.width, size.height);

        let root_view_id = self.root_view.id();
        let mut event_ctx = TuiEventContext::default();
        event_ctx.set_origin_view(Some(root_view_id));
        let handled = app.read(|app_ctx| {
            let mut ctx = TuiLayoutContext::with_app(app_ctx, &mut self.presenter.rendered_views);
            element.dispatch_event(event, area, &mut event_ctx, &mut ctx, app_ctx)
        });

        for update in event_ctx.take_updates() {
            update(app);
        }
        for action in event_ctx.take_typed_actions() {
            // Dispatch through the shared responder chain (the origin view's
            // ancestors in the neutral view hierarchy), so an action raised
            // inside an embedded child view bubbles to ancestor handlers.
            app.update(|ctx| {
                ctx.dispatch_typed_action_for_view(
                    self.window_id,
                    action.origin_view_id,
                    action.action.as_ref(),
                )
            });
        }
        if handled {
            self.dirty.set(true);
        }
        handled
    }
}

/// The production [`TuiTerminal`]: reads from / writes to the real terminal and
/// keeps it in the alternate screen + raw mode for the runtime's lifetime.
pub struct CrosstermTerminal {
    stdout: Stdout,
    _mode_guard: Option<RawModeGuard<CrosstermModeControl>>,
}

impl CrosstermTerminal {
    /// Creates a terminal writer without changing terminal modes.
    pub fn new() -> Self {
        Self {
            stdout: stdout(),
            _mode_guard: None,
        }
    }
    /// Enables raw mode and switches to the alternate screen, restoring the
    /// terminal when the returned value is dropped.
    pub fn enter() -> io::Result<Self> {
        let mode_guard = RawModeGuard::enter(CrosstermModeControl)?;
        Ok(Self {
            stdout: stdout(),
            _mode_guard: Some(mode_guard),
        })
    }
}

impl Default for CrosstermTerminal {
    fn default() -> Self {
        Self::new()
    }
}
impl TuiTerminal for CrosstermTerminal {
    fn size(&self) -> io::Result<TuiSize> {
        let (width, height) = terminal::size()?;
        Ok(TuiSize::new(width.max(1), height.max(1)))
    }

    fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<CrosstermEvent>> {
        if event::poll(timeout)? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    }

    fn writer(&mut self) -> &mut dyn Write {
        &mut self.stdout
    }
}

/// The alternate-screen + raw-mode operations a [`RawModeGuard`] toggles.
/// Behind a trait so the guard's enter/leave lifecycle can be exercised without
/// a real terminal.
trait TerminalModeControl {
    fn enter(&mut self) -> io::Result<()>;
    fn leave(&mut self);
}

struct CrosstermModeControl;

impl TerminalModeControl for CrosstermModeControl {
    fn enter(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        let mut out = stdout();
        if let Err(error) = execute!(out, EnterAlternateScreen, EnableMouseCapture, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(())
    }

    fn leave(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, Show, DisableMouseCapture, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// Restores the host terminal on drop, so a panic or early return never strands
/// it in the alternate screen or raw mode.
struct RawModeGuard<C: TerminalModeControl> {
    control: C,
}

impl<C: TerminalModeControl> RawModeGuard<C> {
    fn enter(mut control: C) -> io::Result<Self> {
        control.enter()?;
        Ok(Self { control })
    }
}

impl<C: TerminalModeControl> Drop for RawModeGuard<C> {
    fn drop(&mut self) {
        self.control.leave();
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

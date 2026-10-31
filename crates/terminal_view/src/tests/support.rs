//! The rig of the GPUI tests: `lectern_ui::init_fixed`, a real kernel with a
//! plugin that takes the `ServiceRef` and the token the pane needs, the fake
//! `dyn Terminals`, and a window whose root has the `Workspace` key context
//! and the actions an outer `workspace` binds (`Escape`, ⌘W).

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, InteractiveElement, IntoElement,
    KeyBinding, KeyBindingContextPredicate, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, WindowHandle, actions, div,
};
use lectern_i18n::{Locale, Strings};
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{
    Activation, ActivationToken, Active, Context as KernelContext, Manifest, Plugin, PluginError,
    PluginHandle, ServiceRef, service,
};
use lectern_services::terminal::{Frame, Input, TerminalId, Terminals};
use lectern_ui::{Appearance, init_fixed};

use crate::paint::Op;
use crate::testing::FakeTerminals;
use crate::{TerminalPane, key_bindings};

actions!(terminal_view_test, [OuterEscape, OuterClose]);

/// The terminal every test uses.
pub const ID: TerminalId = TerminalId(7);

type Slot = Arc<Mutex<Option<(ServiceRef<dyn Terminals>, ActivationToken)>>>;

/// The host plugin of the tests: it requires `dyn Terminals` and keeps the
/// reference and its own token, as `conversation`'s terminal plugin does.
struct Host {
    slot: Slot,
}

struct Taken {
    service: ServiceRef<dyn Terminals>,
    slot: Slot,
}

impl Plugin for Host {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Terminals>()],
            activation: vec![Activation::Manual],
        }
    }

    fn activate(&self, ctx: &mut KernelContext<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(Taken {
            service: ctx.service::<dyn Terminals>(),
            slot: self.slot.clone(),
        }))
    }
}

impl Active for Taken {
    fn started(&mut self, handle: PluginHandle) {
        *self.slot.lock().unwrap() = Some((self.service.clone(), handle.token()));
    }
}

/// The root of a test window: the context and the actions of `workspace`.
pub struct Root {
    child: Option<Entity<TerminalPane>>,
    pub escapes: Arc<AtomicUsize>,
    pub closes: Arc<AtomicUsize>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let escapes = self.escapes.clone();
        let closes = self.closes.clone();
        div()
            .id("root")
            .key_context("Workspace")
            .size_full()
            .on_action(cx.listener(move |_, _: &OuterEscape, _, _| {
                escapes.fetch_add(1, Ordering::SeqCst);
            }))
            .on_action(cx.listener(move |_, _: &OuterClose, _, _| {
                closes.fetch_add(1, Ordering::SeqCst);
            }))
            .children(self.child.clone())
    }
}

pub struct Rig {
    pub terminals: Arc<FakeTerminals>,
    /// Kept alive: the host plugin's token must stay live.
    _kernel: TestKernel,
    service: ServiceRef<dyn Terminals>,
    token: ActivationToken,
}

pub struct Opened {
    pub window: WindowHandle<Root>,
    pub pane: Entity<TerminalPane>,
    pub escapes: Arc<AtomicUsize>,
    pub closes: Arc<AtomicUsize>,
}

impl Rig {
    pub fn new(cx: &mut TestAppContext) -> Rig {
        Rig::with_appearance(cx, Appearance::Light)
    }

    pub fn with_appearance(cx: &mut TestAppContext, appearance: Appearance) -> Rig {
        // The kernel thread wakes foreground tasks; the test scheduler
        // accepts a wake from another thread only with parking allowed.
        cx.executor().allow_parking();
        cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), appearance, false));
        let terminals = FakeTerminals::new();
        let service_object: Arc<dyn Terminals> = terminals.clone();
        let slot: Slot = Arc::default();
        let kernel =
            TestKernel::new(Host { slot: slot.clone() }).mock::<dyn Terminals>(service_object);
        kernel.activate().expect("the host plugin activates");
        let (service, token) = slot
            .lock()
            .unwrap()
            .take()
            .expect("the host plugin started");
        Rig {
            terminals,
            _kernel: kernel,
            service,
            token,
        }
    }

    /// Opens a window with a pane for `ID`. `frame` is what the fake service
    /// answers `frame(ID)` with; the pane has the keyboard focus and is
    /// drawn.
    pub fn open(&self, cx: &mut TestAppContext, frame: Option<Arc<Frame>>) -> Opened {
        if let Some(frame) = frame {
            self.terminals.set_frame(ID, frame);
        }
        cx.update(bind_keys);
        let escapes = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let (root_escapes, root_closes) = (escapes.clone(), closes.clone());
        let window = cx.add_window(|_, _| Root {
            child: None,
            escapes: root_escapes,
            closes: root_closes,
        });
        let (service, token) = (self.service.clone(), self.token.clone());
        let pane = window
            .update(cx, |root, window, cx| {
                let pane = cx.new(|cx| TerminalPane::new(service, token, ID, window, cx));
                root.child = Some(pane.clone());
                pane.read(cx).focus_handle().focus(window, cx);
                cx.notify();
                pane
            })
            .expect("the window is open");
        // The test platform opens a window inactive.
        window
            .update(cx, |_, window, _| window.activate_window())
            .expect("the window is open");
        cx.run_until_parked();
        let opened = Opened {
            window,
            pane,
            escapes,
            closes,
        };
        opened.draw(cx);
        opened
    }

    /// The kernel behind the test kernel, to switch a plugin off.
    pub fn kernel_handle(&self) -> Option<Arc<lectern_kernel::Kernel>> {
        self._kernel.inner()
    }

    /// The inputs the pane sent since the last call.
    pub fn inputs(&self) -> Vec<Input> {
        self.terminals.take_inputs()
    }
}

/// What `workspace` and the host plugin bind: the outer Esc and ⌘W on the
/// `Workspace` context, and `key_bindings()`.
fn bind_keys(cx: &mut App) {
    let mut bindings = vec![
        KeyBinding::new("escape", OuterEscape, Some("Workspace")),
        KeyBinding::new("cmd-w", OuterClose, Some("Workspace")),
    ];
    for (keystrokes, action, context) in key_bindings() {
        let predicate = context.map(|context| {
            Rc::new(KeyBindingContextPredicate::parse(context).expect("a valid predicate"))
        });
        bindings.push(
            KeyBinding::load(
                keystrokes,
                action,
                predicate,
                false,
                None,
                cx.keyboard_mapper().as_ref(),
            )
            .expect("a valid binding"),
        );
    }
    cx.bind_keys(bindings);
}

impl Opened {
    pub fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(AnyWindowHandle::from(self.window), cx)
    }

    /// Draws one frame.
    pub fn draw(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
    }

    /// The operations of the last paint.
    pub fn painted(&self, cx: &mut TestAppContext) -> Vec<Op> {
        self.pane.read_with(cx, |pane, _| pane.painted.clone())
    }

    /// Hands the pane the latest frame (`TerminalPane::refresh`), as the
    /// host does on `TerminalEvent::Frame`, and draws it.
    pub fn refresh(&self, cx: &mut TestAppContext) {
        self.pane.update(cx, |pane, cx| pane.refresh(cx));
        self.draw(cx);
    }
}

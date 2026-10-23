//! The system appearance: AppKit's semantic colours resolved under the light
//! or the dark appearance, the reduce-motion setting, and a watch on their
//! changes (「系统外观」 in docs/design/platform.md; prerequisite A of the ui
//! design, ADR 0015). Light versus dark itself is not read here: GPUI gives
//! every window its appearance and its changes.
//!
//! Threads: `system_colors` and `reduce_motion` may be called on any thread
//! (the drawing appearance is per thread; `NSWorkspace` is readable
//! anywhere). `watch` may be called on any thread; its callback runs on the
//! thread that posts the notification, which for AppKit's own posts is the
//! main thread; `Watch` is dropped on the thread that created it. `ui` does
//! all of this on the main thread.

use std::fmt;

#[cfg(test)]
mod tests;

/// The two appearances of macOS. GPUI's `WindowAppearance` maps onto it
/// (`Light`, `VibrantLight` → `Light`; `Dark`, `VibrantDark` → `Dark`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    /// The appearance as a log field: `light` or `dark`.
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
}

/// sRGB components in `0..=1` plus alpha. No GPUI type: `ui` converts it to
/// `Hsla`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Srgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// How many entries `SystemColors::system` has: `systemRedColor` …
/// `systemGrayColor`, in the order of `ui::SystemColor`.
pub const SYSTEM_COLOR_COUNT: usize = 13;

/// The colours `ui::Palette` takes from the system, named as its fields
/// are (without the derived diff backgrounds and the agent colours), all
/// resolved under one appearance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SystemColors {
    /// `labelColor`.
    pub label: Srgba,
    /// `secondaryLabelColor`.
    pub secondary_label: Srgba,
    /// `tertiaryLabelColor`.
    pub tertiary_label: Srgba,
    /// `separatorColor`.
    pub separator: Srgba,
    /// `windowBackgroundColor`.
    pub window_background: Srgba,
    /// `controlBackgroundColor`.
    pub control_background: Srgba,
    /// `underPageBackgroundColor`.
    pub under_page_background: Srgba,
    /// `controlAccentColor`: the accent colour the user chose in System
    /// Settings. `systemBlueColor` when `accent_available` is false.
    pub accent: Srgba,
    /// `selectedContentBackgroundColor`.
    pub selected_content_background: Srgba,
    /// `unemphasizedSelectedContentBackgroundColor`.
    pub unemphasized_selected_content_background: Srgba,
    /// `systemRedColor`, `systemOrangeColor`, `systemYellowColor`,
    /// `systemGreenColor`, `systemMintColor`, `systemTealColor`,
    /// `systemCyanColor`, `systemBlueColor`, `systemIndigoColor`,
    /// `systemPurpleColor`, `systemPinkColor`, `systemBrownColor`,
    /// `systemGrayColor`: the order of `ui::SystemColor`.
    pub system: [Srgba; SYSTEM_COLOR_COUNT],
    /// Whether `accent` was read from `controlAccentColor` (macOS 10.14 and
    /// later have it; `ui::accent_follows_system` reports it).
    pub accent_available: bool,
}

/// The semantic colours of `NSColor` resolved under `appearance`
/// (`NSAppearance.performAsCurrentDrawingAppearance:`, then each colour
/// converted to the sRGB colour space and its components read). In memory
/// only, no IO, no blocking; one call is about 70 Objective-C messages
/// inside one autorelease pool, tens of microseconds. Any thread.
///
/// None on every other platform, and when AppKit cannot resolve one of the
/// colours (a selector missing on this macOS: `systemMintColor` and
/// `systemCyanColor` need macOS 12; a colour that does not convert). `ui`
/// then uses its built-in table and logs the warn.
pub fn system_colors(appearance: Appearance) -> Option<SystemColors> {
    imp::system_colors(appearance)
}

/// `NSWorkspace.shared.accessibilityDisplayShouldReduceMotion`: whether the
/// user turned on "Reduce motion" in Accessibility. In memory only, any
/// thread. None on every other platform.
pub fn reduce_motion() -> Option<bool> {
    imp::reduce_motion()
}

/// What changed, as the `Watch` callback receives it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Change {
    /// `NSSystemColorsDidChangeNotification`: the accent colour, increase
    /// contrast, or another setting the system colours depend on. The
    /// caller reads `system_colors` again for both appearances.
    SystemColors,
    /// `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`: an
    /// accessibility display setting, reduce motion among them. The caller
    /// reads `reduce_motion` again.
    ReduceMotion,
}

impl Change {
    /// The change as a log field: `system_colors` or `reduce_motion`.
    pub fn as_str(self) -> &'static str {
        match self {
            Change::SystemColors => "system_colors",
            Change::ReduceMotion => "reduce_motion",
        }
    }
}

/// Registers for `NSSystemColorsDidChangeNotification` (the default
/// notification centre) and
/// `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` (the
/// workspace's). `on_change` runs synchronously on the thread that posts
/// the notification: AppKit posts both on the main thread, so in the app
/// the callback arrives on the main thread, with no GPUI context (`ui`
/// schedules the update through `AsyncApp`). It gets no notification of
/// any kind until the first change, and none after the returned `Watch` is
/// dropped. A notification posted from inside the callback is not
/// delivered to it again (no re-entry). A panic in the callback is caught
/// at the AppKit boundary: the callback is dropped, later changes are not
/// delivered, and one error record is written (「日志与诊断」).
///
/// On every other platform the `Watch` holds the callback and never calls
/// it. On macOS, when AppKit is not loaded (not the case in the app or
/// the tests) the `Watch` is inert in the same way.
pub fn watch(on_change: Box<dyn FnMut(Change)>) -> Watch {
    Watch(imp::watch(on_change))
}

/// The registration `watch` made. Dropping it removes the observers; no
/// callback starts after the drop returns. Not `Send`: it is dropped on the
/// thread that created it (the main thread in the app).
pub struct Watch(imp::Watch);

impl Watch {
    /// Whether the observers are registered (false on every other platform
    /// and when AppKit is not loaded).
    pub fn is_live(&self) -> bool {
        self.0.is_live()
    }
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("live", &self.is_live())
            .finish()
    }
}

/// HSL lightness of the colour, `(max + min) / 2` of the sRGB components,
/// ignoring alpha. Compiled for tests everywhere: the acceptance of the ui
/// design compares the label's lightness with 0.5.
#[cfg(test)]
fn lightness(color: Srgba) -> f32 {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    (max + min) / 2.0
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
    use std::mem;
    use std::panic::{self, AssertUnwindSafe};
    use std::ptr;
    use std::rc::Rc;

    use lectern_logging::error;

    use super::{Appearance, Change, SYSTEM_COLOR_COUNT, Srgba, SystemColors};

    /// An Objective-C object pointer (`id`).
    type Id = *mut c_void;
    type Sel = *const c_void;
    /// `CGFloat`: a double on both Apple architectures Lectern builds for.
    type CGFloat = f64;
    /// `BOOL`: one byte on both.
    type Bool = u8;

    // The Objective-C runtime, part of libSystem's load set.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        /// Declared without a signature and cast to the receiver's one per
        /// call, as the runtime documents.
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    // AppKit: the names below are its exported string constants. Linked
    // here, not only through GPUI, so that this crate's test binary (which
    // has no GPUI) loads AppKit too.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSAppearanceNameAqua: Id;
        static NSAppearanceNameDarkAqua: Id;
        static NSSystemColorsDidChangeNotification: Id;
        static NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification: Id;
    }

    // The blocks runtime, in libSystem: the class of a block literal on the
    // stack.
    unsafe extern "C" {
        static _NSConcreteStackBlock: [*const c_void; 32];
    }

    /// The fixed part of a block literal's descriptor (the blocks ABI,
    /// `Block_descriptor_1`): with no copy and dispose helpers and no
    /// signature in `flags`, nothing follows these two.
    #[repr(C)]
    struct BlockDescriptor {
        reserved: c_ulong,
        size: c_ulong,
    }

    // SAFETY: two integers, read only.
    unsafe impl Sync for BlockDescriptor {}

    /// A block literal (`Block_literal_1`) with one captured value. Built
    /// on the stack; AppKit copies it when it keeps it (the notification
    /// centre does), and a copy of a block without helpers is a plain
    /// memcpy, which is what a `Copy` payload wants.
    #[repr(C)]
    struct Block<F: Copy, T: Copy> {
        isa: *const c_void,
        flags: c_int,
        reserved: c_int,
        invoke: F,
        descriptor: *const BlockDescriptor,
        payload: T,
    }

    impl<F: Copy, T: Copy> Block<F, T> {
        /// A stack block with no copy or dispose helpers and no signature.
        /// `descriptor.size` must be `size_of::<Self>()`.
        fn new(invoke: F, descriptor: &'static BlockDescriptor, payload: T) -> Self {
            Block {
                isa: ptr::addr_of!(_NSConcreteStackBlock).cast::<c_void>(),
                flags: 0,
                reserved: 0,
                invoke,
                descriptor,
                payload,
            }
        }

        /// The block as the `id` an Objective-C method takes.
        fn as_id(&self) -> Id {
            ptr::from_ref(self).cast_mut().cast::<c_void>()
        }
    }

    fn sel(name: &CStr) -> Sel {
        // SAFETY: a NUL-terminated C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn class(name: &CStr) -> Option<Id> {
        // SAFETY: a NUL-terminated C string; null when the class is not
        // loaded.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        (!class.is_null()).then_some(class)
    }

    /// `[receiver selector]` returning an object (or nil).
    ///
    /// # Safety
    ///
    /// `receiver` is a live object or a class that responds to `selector`
    /// with that signature.
    unsafe fn send0(receiver: Id, selector: Sel) -> Id {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector)
        }
    }

    /// `[receiver selector:arg]` returning an object (or nil).
    ///
    /// # Safety
    ///
    /// As `send0`, with one object argument.
    unsafe fn send1(receiver: Id, selector: Sel, arg: Id) -> Id {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg)
        }
    }

    /// `[receiver selector:arg]` returning `BOOL`.
    ///
    /// # Safety
    ///
    /// As `send1`, for a method that returns `BOOL`.
    unsafe fn send1_bool(receiver: Id, selector: Sel, arg: *const c_void) -> bool {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, *const c_void) -> Bool =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector, arg) != 0
        }
    }

    /// `[receiver selector]` returning `BOOL`.
    ///
    /// # Safety
    ///
    /// As `send0`, for a method that returns `BOOL`.
    unsafe fn send0_bool(receiver: Id, selector: Sel) -> bool {
        // SAFETY: the caller's contract; the cast is to the method's
        // signature.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Bool =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector) != 0
        }
    }

    /// `[receiver respondsToSelector:selector]`; for a class receiver this
    /// asks about its class methods.
    ///
    /// # Safety
    ///
    /// `receiver` is a live object or a class.
    unsafe fn responds(receiver: Id, selector: Sel) -> bool {
        // SAFETY: every object responds to `respondsToSelector:`.
        unsafe { send1_bool(receiver, sel(c"respondsToSelector:"), selector) }
    }

    /// The autorelease pool around a call that returns autoreleased
    /// objects (AppKit's colour conversions do): popped when dropped.
    struct Pool(*mut c_void);

    impl Pool {
        fn push() -> Pool {
            // SAFETY: no arguments; pairs with the pop in `drop`.
            Pool(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: the token `push` returned, popped once.
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }

    /// The `NSColor` class methods behind `SystemColors::system`, in its
    /// order.
    const SYSTEM_SELECTORS: [&CStr; SYSTEM_COLOR_COUNT] = [
        c"systemRedColor",
        c"systemOrangeColor",
        c"systemYellowColor",
        c"systemGreenColor",
        c"systemMintColor",
        c"systemTealColor",
        c"systemCyanColor",
        c"systemBlueColor",
        c"systemIndigoColor",
        c"systemPurpleColor",
        c"systemPinkColor",
        c"systemBrownColor",
        c"systemGrayColor",
    ];

    /// What the appearance block writes to. It stays None when the block
    /// does not run at all.
    #[derive(Clone, Copy)]
    struct ResolveSlot {
        out: *mut Option<SystemColors>,
    }

    /// The block's `invoke`: AppKit passes the block itself, untyped.
    type ResolveInvoke = unsafe extern "C" fn(*mut c_void);

    static RESOLVE_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block<ResolveInvoke, ResolveSlot>>() as c_ulong,
    };

    /// The body of the appearance block: reads every colour under the
    /// appearance AppKit made current for this block.
    unsafe extern "C" fn resolve_invoke(block: *mut c_void) {
        // SAFETY: AppKit calls the block with its own pointer, which is
        // the `Block<ResolveInvoke, ResolveSlot>` `system_colors` built and
        // is valid for the call; `out` points at the caller's slot, which
        // outlives the synchronous `performAsCurrentDrawingAppearance:`.
        unsafe {
            let block = block.cast::<Block<ResolveInvoke, ResolveSlot>>();
            let slot = (*block).payload;
            *slot.out = read_colors();
        }
    }

    /// One colour under the current drawing appearance: the class method,
    /// converted to sRGB, its components read. None when the method is
    /// missing on this macOS or the colour does not convert.
    fn read_color(ns_color: Id, srgb: Id, selector: &CStr) -> Option<Srgba> {
        let selector = sel(selector);
        // SAFETY: `ns_color` is the loaded `NSColor` class; the class
        // method, when it exists, takes no argument and returns a colour.
        let color = unsafe {
            if !responds(ns_color, selector) {
                return None;
            }
            send0(ns_color, selector)
        };
        if color.is_null() {
            return None;
        }
        // SAFETY: `color` is an `NSColor`; `colorUsingColorSpace:` takes a
        // colour space and returns a colour or nil.
        let converted = unsafe { send1(color, sel(c"colorUsingColorSpace:"), srgb) };
        if converted.is_null() {
            return None;
        }
        let (mut r, mut g, mut b, mut a): (CGFloat, CGFloat, CGFloat, CGFloat) =
            (0.0, 0.0, 0.0, 0.0);
        // SAFETY: `converted` is in the sRGB colour space (an RGB model),
        // so `getRed:green:blue:alpha:` does not raise; the four pointers
        // are valid for writes of one `CGFloat` each; the method returns
        // nothing.
        unsafe {
            let send: unsafe extern "C" fn(
                Id,
                Sel,
                *mut CGFloat,
                *mut CGFloat,
                *mut CGFloat,
                *mut CGFloat,
            ) = mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(
                converted,
                sel(c"getRed:green:blue:alpha:"),
                &mut r,
                &mut g,
                &mut b,
                &mut a,
            );
        }
        Some(Srgba {
            r: r as f32,
            g: g as f32,
            b: b as f32,
            a: a as f32,
        })
    }

    /// Every colour under the current drawing appearance.
    fn read_colors() -> Option<SystemColors> {
        let ns_color = class(c"NSColor")?;
        let color_space = class(c"NSColorSpace")?;
        // SAFETY: `NSColorSpace` is loaded; `sRGBColorSpace` takes no
        // argument and returns the shared colour space.
        let srgb = unsafe { send0(color_space, sel(c"sRGBColorSpace")) };
        if srgb.is_null() {
            return None;
        }
        let read = |selector: &CStr| read_color(ns_color, srgb, selector);
        let mut system = [Srgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }; SYSTEM_COLOR_COUNT];
        for (slot, selector) in system.iter_mut().zip(SYSTEM_SELECTORS) {
            *slot = read(selector)?;
        }
        let (accent, accent_available) = match read(c"controlAccentColor") {
            Some(accent) => (accent, true),
            None => (read(c"systemBlueColor")?, false),
        };
        Some(SystemColors {
            label: read(c"labelColor")?,
            secondary_label: read(c"secondaryLabelColor")?,
            tertiary_label: read(c"tertiaryLabelColor")?,
            separator: read(c"separatorColor")?,
            window_background: read(c"windowBackgroundColor")?,
            control_background: read(c"controlBackgroundColor")?,
            under_page_background: read(c"underPageBackgroundColor")?,
            accent,
            selected_content_background: read(c"selectedContentBackgroundColor")?,
            unemphasized_selected_content_background: read(
                c"unemphasizedSelectedContentBackgroundColor",
            )?,
            system,
            accent_available,
        })
    }

    pub(super) fn system_colors(appearance: Appearance) -> Option<SystemColors> {
        let _pool = Pool::push();
        let ns_appearance = class(c"NSAppearance")?;
        // SAFETY: AppKit is loaded (the class exists), so its exported
        // name constants are initialised; `appearanceNamed:` takes a name
        // and returns an appearance or nil.
        let named = unsafe {
            let name = match appearance {
                Appearance::Light => NSAppearanceNameAqua,
                Appearance::Dark => NSAppearanceNameDarkAqua,
            };
            send1(ns_appearance, sel(c"appearanceNamed:"), name)
        };
        if named.is_null() {
            return None;
        }
        let perform = sel(c"performAsCurrentDrawingAppearance:");
        // SAFETY: `named` is a live `NSAppearance`.
        if !unsafe { responds(named, perform) } {
            return None;
        }
        let mut out: Option<SystemColors> = None;
        let block = Block::new(
            resolve_invoke as ResolveInvoke,
            &RESOLVE_DESCRIPTOR,
            ResolveSlot { out: &mut out },
        );
        // SAFETY: `named` responds to the selector (checked); it takes a
        // `void (^)(void)` block, which `block` is laid out as, runs it
        // synchronously on this thread and returns nothing. `out` lives
        // until after the call.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(named, perform, block.as_id());
        }
        out
    }

    pub(super) fn reduce_motion() -> Option<bool> {
        let workspace = class(c"NSWorkspace")?;
        // SAFETY: `NSWorkspace` is loaded; `sharedWorkspace` returns the
        // shared instance, whose
        // `accessibilityDisplayShouldReduceMotion` returns `BOOL`.
        unsafe {
            let shared = send0(workspace, sel(c"sharedWorkspace"));
            if shared.is_null() {
                return None;
            }
            Some(send0_bool(
                shared,
                sel(c"accessibilityDisplayShouldReduceMotion"),
            ))
        }
    }

    type Callback = Box<dyn FnMut(Change)>;

    /// The callback, shared by the two notification blocks through a raw
    /// `Rc` pointer (one thread: the posting thread is the thread the
    /// `Watch` lives on, see the module doc). `RefCell`: the callback is
    /// `FnMut` and a second delivery while it runs (a post from inside it)
    /// is skipped.
    struct Inner {
        callback: RefCell<Option<Callback>>,
    }

    impl Inner {
        fn deliver(&self, change: Change) {
            let Ok(mut slot) = self.callback.try_borrow_mut() else {
                return;
            };
            let Some(callback) = slot.as_mut() else {
                return;
            };
            if panic::catch_unwind(AssertUnwindSafe(|| callback(change))).is_err() {
                *slot = None;
                error!(
                    "the appearance change callback panicked; later changes are not delivered";
                    change = change.as_str(),
                );
            }
        }
    }

    /// What a notification block captures: which change it reports, and
    /// the shared callback.
    #[derive(Clone, Copy)]
    struct NotifySlot {
        change: Change,
        inner: *const Inner,
    }

    /// The block's `invoke`: the centre passes the block itself, untyped,
    /// and the notification.
    type NotifyInvoke = unsafe extern "C" fn(*mut c_void, Id);

    static NOTIFY_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: mem::size_of::<Block<NotifyInvoke, NotifySlot>>() as c_ulong,
    };

    /// The body of a notification block.
    unsafe extern "C" fn notify_invoke(block: *mut c_void, _notification: Id) {
        // SAFETY: the centre calls the block (its copy of the
        // `Block<NotifyInvoke, NotifySlot>` `Observer::add` built) with its
        // own pointer, valid for the call. `inner` is the `Rc` the `Watch`
        // holds: the observer is removed before the `Watch` drops it, so
        // the pointer is live for every delivery that starts before the
        // removal (a delivery in flight on another thread while the
        // `Watch` drops is the known limitation; AppKit posts on the main
        // thread, where the `Watch` lives). The count is raised for the
        // call so that a callback that drops the `Watch` keeps `inner`.
        unsafe {
            let block = block.cast::<Block<NotifyInvoke, NotifySlot>>();
            let slot = (*block).payload;
            Rc::increment_strong_count(slot.inner);
            let inner = Rc::from_raw(slot.inner);
            inner.deliver(slot.change);
        }
    }

    /// One registration: the centre it was made on and the observer token
    /// the centre returned, retained.
    struct Observer {
        center: Id,
        token: Id,
    }

    impl Observer {
        /// `[center addObserverForName:name object:nil queue:nil
        /// usingBlock:…]`: the block runs on the posting thread.
        fn add(center: Id, name: Id, change: Change, inner: &Rc<Inner>) -> Option<Observer> {
            let block = Block::new(
                notify_invoke as NotifyInvoke,
                &NOTIFY_DESCRIPTOR,
                NotifySlot {
                    change,
                    inner: Rc::as_ptr(inner),
                },
            );
            // SAFETY: `center` is a live notification centre and `name` an
            // `NSString`; the method takes name, object, queue and a
            // `void (^)(NSNotification *)` block (which `block` is laid out
            // as; the centre copies it) and returns an autoreleased
            // observer, retained here so that it can be removed later.
            let token = unsafe {
                let send: unsafe extern "C" fn(Id, Sel, Id, Id, Id, Id) -> Id =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let token = send(
                    center,
                    sel(c"addObserverForName:object:queue:usingBlock:"),
                    name,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    block.as_id(),
                );
                if token.is_null() {
                    return None;
                }
                send0(token, sel(c"retain"))
            };
            Some(Observer { center, token })
        }
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            // SAFETY: `center` and `token` are the pair `add` made; the
            // centre is a process-wide singleton, so it is alive. After
            // `removeObserver:` the centre no longer calls the block;
            // `release` balances the `retain` in `add`.
            unsafe {
                send1(self.center, sel(c"removeObserver:"), self.token);
                send0(self.token, sel(c"release"));
            }
        }
    }

    /// The live registration: dropped in declaration order, so both
    /// observers are removed before `inner` is released.
    struct Live {
        #[allow(dead_code, reason = "held for its drop")]
        colors: Observer,
        #[allow(dead_code, reason = "held for its drop")]
        display: Observer,
        #[allow(dead_code, reason = "held for its drop")]
        inner: Rc<Inner>,
    }

    pub(super) struct Watch(Option<Live>);

    impl Watch {
        pub(super) fn is_live(&self) -> bool {
            self.0.is_some()
        }
    }

    pub(super) fn watch(on_change: Callback) -> Watch {
        Watch(register(on_change))
    }

    fn register(on_change: Callback) -> Option<Live> {
        let _pool = Pool::push();
        let center_class = class(c"NSNotificationCenter")?;
        let workspace_class = class(c"NSWorkspace")?;
        // SAFETY: both classes are loaded; `defaultCenter`,
        // `sharedWorkspace` and `notificationCenter` take no argument and
        // return the singletons. AppKit is loaded (`NSWorkspace` exists),
        // so its exported name constants are initialised.
        let (default_center, workspace_center, colors_name, display_name) = unsafe {
            let default_center = send0(center_class, sel(c"defaultCenter"));
            let workspace = send0(workspace_class, sel(c"sharedWorkspace"));
            if workspace.is_null() {
                return None;
            }
            let workspace_center = send0(workspace, sel(c"notificationCenter"));
            (
                default_center,
                workspace_center,
                NSSystemColorsDidChangeNotification,
                NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
            )
        };
        if default_center.is_null()
            || workspace_center.is_null()
            || colors_name.is_null()
            || display_name.is_null()
        {
            return None;
        }
        let inner = Rc::new(Inner {
            callback: RefCell::new(Some(on_change)),
        });
        let colors = Observer::add(default_center, colors_name, Change::SystemColors, &inner)?;
        let display = Observer::add(workspace_center, display_name, Change::ReduceMotion, &inner)?;
        Some(Live {
            colors,
            display,
            inner,
        })
    }

    /// Posts `change`'s notification on its centre, as AppKit would, with
    /// no sender: the tests' way to drive a `Watch` without changing the
    /// system settings.
    #[cfg(test)]
    pub(super) fn post(change: Change) {
        let _pool = Pool::push();
        let center_class = class(c"NSNotificationCenter").expect("NSNotificationCenter");
        let workspace_class = class(c"NSWorkspace").expect("NSWorkspace");
        // SAFETY: as in `register`; `postNotificationName:object:` takes a
        // name and a sender (nil) and returns nothing.
        unsafe {
            let (center, name) = match change {
                Change::SystemColors => (
                    send0(center_class, sel(c"defaultCenter")),
                    NSSystemColorsDidChangeNotification,
                ),
                Change::ReduceMotion => (
                    send0(
                        send0(workspace_class, sel(c"sharedWorkspace")),
                        sel(c"notificationCenter"),
                    ),
                    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
                ),
            };
            let send: unsafe extern "C" fn(Id, Sel, Id, Id) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(
                center,
                sel(c"postNotificationName:object:"),
                name,
                ptr::null_mut(),
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Appearance, Change, SystemColors};

    pub(super) fn system_colors(_appearance: Appearance) -> Option<SystemColors> {
        None
    }

    pub(super) fn reduce_motion() -> Option<bool> {
        None
    }

    /// Holds the callback and never calls it.
    pub(super) struct Watch(
        #[allow(dead_code, reason = "held for its drop")] Box<dyn FnMut(Change)>,
    );

    impl Watch {
        pub(super) fn is_live(&self) -> bool {
            false
        }
    }

    pub(super) fn watch(on_change: Box<dyn FnMut(Change)>) -> Watch {
        Watch(on_change)
    }
}

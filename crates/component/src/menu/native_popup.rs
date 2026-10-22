//! Menus in their own platform popup window (an `xdg_popup` on Wayland).
//!
//! A menu drawn inside its parent window can only be glass over that window's
//! own pixels, which no compositor blurs. In a window of its own, with a
//! blurred background, the compositor blurs whatever is behind it, the parent
//! window included.
//!
//! Such a menu is outside its parent's element tree, so nothing reaches the
//! parent by bubbling: actions are dispatched in the parent window explicitly
//! (see `PopupMenu::dispatch_confirm_action`), and the menu bar is told about
//! left/right and clicks elsewhere in its own window itself.

use std::rc::Rc;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render, Size,
    Styled as _, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowId,
    WindowKind, WindowOptions, canvas, div,
    popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions},
    prelude::FluentBuilder as _,
    px, size,
};

use crate::{
    ActiveTheme as _, ElementExt as _,
    actions::{SelectLeft, SelectRight},
    menu::PopupMenu,
};

/// Calls `f` with its parent's bounds at prepaint. Unlike `on_prepaint`, it
/// pins itself to the parent's top left: an absolute child left at its static
/// position lands after its siblings in a block, or inside the padding of a
/// flex row, and reports its parent's size at the wrong place.
pub(crate) fn bounds_probe(
    f: impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(f, |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// Where a native menu window goes, relative to its parent window.
#[derive(Clone, Copy)]
pub(crate) struct NativeMenuPlacement {
    /// In the parent window's coordinates.
    pub anchor_rect: Bounds<Pixels>,
    pub anchor: PopupAnchor,
    pub gravity: PopupGravity,
    pub constraint_adjustment: PopupConstraintAdjustment,
    pub offset: Point<Pixels>,
}

/// Called with `true` for right, `false` for left, when the top-level menu
/// has no submenu to move into or out of.
pub(crate) type SideKeyHandler = Rc<dyn Fn(bool, &mut App)>;

/// The root view of a native menu window: the menu, at its own size, with the
/// window following that size.
///
/// The window opens wider than any menu, transparent, with the menu hidden:
/// laid out in less room than it wants, its labels would wrap and it would
/// never ask for more. The first frame measures it; the window then takes
/// that size and its glass, and shows the menu.
pub(crate) struct NativeMenuHost {
    menu: Entity<PopupMenu>,
    on_side_key: Option<SideKeyHandler>,
    translucent: bool,
    radius: Pixels,
    size: Option<Size<Pixels>>,
}

impl Render for NativeMenuHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = cx.entity();
        let measured = self.size;
        let translucent = self.translucent;
        let radius = self.radius;
        let on_left = self.on_side_key.clone();
        let on_right = self.on_side_key.clone();
        div()
            .size_full()
            // Reached when the menu propagates left/right: it had no submenu
            // to move into or out of. The handler changes windows, so after
            // this window's update.
            .on_action(move |_: &SelectLeft, _, cx| {
                if let Some(handler) = on_left.clone() {
                    cx.defer(move |cx| handler(false, cx));
                }
            })
            .on_action(move |_: &SelectRight, _, cx| {
                if let Some(handler) = on_right.clone() {
                    cx.defer(move |cx| handler(true, cx));
                }
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .when(measured.is_none(), |this| this.opacity(0.))
                    .child(self.menu.clone())
                    .on_prepaint(move |bounds, window, cx| {
                        let wanted = bounds.size.map(|length| length.ceil());
                        if wanted.width <= px(0.) {
                            return;
                        }
                        // A resize before the popup's first configure is
                        // dropped, so this asks again each frame until the
                        // window has the menu's size.
                        let viewport = window.viewport_size();
                        let fits = (viewport.width - wanted.width).abs() < px(1.)
                            && (viewport.height - wanted.height).abs() < px(1.);
                        if !fits {
                            window.defer(cx, move |window, _| window.resize(wanted));
                        }
                        if measured.is_some() {
                            return;
                        }
                        window.defer(cx, move |window, cx| {
                            if translucent {
                                window
                                    .set_background_appearance(WindowBackgroundAppearance::Blurred);
                                window.set_background_blur_corner_radius(radius);
                            }
                            host.update(cx, |host, cx| {
                                host.size = Some(wanted);
                                cx.notify();
                            });
                        });
                    }),
            )
    }
}

/// Open `menu` in a popup window of its own, anchored to `parent`.
///
/// Fails where the platform has no native popups; the caller falls back to
/// drawing the menu inside its window. `on_closed` hears of the window going,
/// whoever closed it.
pub(crate) fn open_native_menu(
    parent: AnyWindowHandle,
    placement: NativeMenuPlacement,
    menu: Entity<PopupMenu>,
    background: Option<Hsla>,
    on_side_key: Option<SideKeyHandler>,
    on_closed: impl FnOnce(WindowId, &mut App) + 'static,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<NativeMenuHost>> {
    let translucent = background.is_some_and(|color| color.a < 1.);
    let radius = cx.theme().radius;
    let estimate = menu.read(cx).estimated_size();
    let options = WindowOptions {
        kind: WindowKind::AnchoredPopup(PopupOptions {
            parent,
            anchor_rect: placement.anchor_rect,
            anchor: placement.anchor,
            gravity: placement.gravity,
            constraint_adjustment: placement.constraint_adjustment,
            offset: placement.offset,
            // Menu-like: keyboard focus, and dismissed by a click in another
            // application.
            grab: true,
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: estimate,
        })),
        titlebar: None,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        menu.read(cx).focus_handle(cx).focus(window, cx);
        let pointer = window.mouse_position();
        menu.update(cx, |menu, _| menu.set_opening_pointer(pointer));
        let host = cx.new(|_| NativeMenuHost {
            menu,
            on_side_key,
            translucent,
            radius,
            size: None,
        });
        let window_id = window.window_handle().window_id();
        cx.observe_release(&host, move |_, cx| on_closed(window_id, cx))
            .detach();
        host
    })
}

/// Close a native menu window; nothing if it is already gone.
pub(crate) fn close_native_menu(handle: WindowHandle<NativeMenuHost>, cx: &mut App) {
    _ = handle.update(cx, |_, window, _| window.remove_window());
}

impl PopupMenu {
    /// Room for the menu's first, measuring frame: the widest a menu gets,
    /// and generously tall.
    pub(crate) fn estimated_size(&self) -> Size<Pixels> {
        let rows = self
            .menu_items
            .iter()
            .filter(|item| !item.is_separator())
            .count();
        let separators = self.menu_items.len() - rows;
        size(
            self.max_width(),
            px(16.) + px(30.) * rows as f32 + px(9.) * separators as f32,
        )
    }
}

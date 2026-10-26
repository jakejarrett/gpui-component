//! Select and Combobox lists in their own platform popup window (an
//! `xdg_popup` on Wayland), behind glass the compositor blurs.
//!
//! As with the native menus (`menu/native_popup.rs`), a list drawn inside its
//! parent window can only be glass over that window's own pixels, which no
//! compositor blurs; in a window of its own it is blurred over whatever is
//! behind it. An app opts in with [`set_dropdown_background`] and a
//! translucent colour. Where the platform has no native popups the list is
//! drawn inside its window as before, opaque.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, FocusHandle, Global, Hsla,
    IntoElement, ParentElement as _, Pixels, Point, Render, Size, Styled as _, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div,
    popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions},
    prelude::FluentBuilder as _,
    px,
};

use crate::{ActiveTheme as _, ElementExt as _};

#[derive(Default)]
struct DropdownSurface {
    background: Option<Hsla>,
    /// A native popup was refused once: the platform has none.
    unsupported: bool,
}

impl Global for DropdownSurface {}

/// The fill every Select and Combobox list takes. A translucent colour also
/// opens the lists in windows of their own, blurred, where the platform can;
/// `None` (the default) keeps the theme's popover fill, in the window.
pub fn set_dropdown_background(background: Option<Hsla>, cx: &mut App) {
    cx.default_global::<DropdownSurface>().background = background;
}

/// Whether lists open in a window of their own.
pub(crate) fn wants_native_dropdowns(cx: &App) -> bool {
    cx.try_global::<DropdownSurface>().is_some_and(|surface| {
        !surface.unsupported && surface.background.is_some_and(|color| color.a < 1.)
    })
}

/// The fill a list takes: glass only in a window of its own.
pub(crate) fn dropdown_background(native: bool, cx: &App) -> Option<Hsla> {
    cx.try_global::<DropdownSurface>()
        .and_then(|surface| surface.background)
        .map(|color| {
            if native {
                color
            } else {
                Hsla { a: 1., ..color }
            }
        })
}

/// What a native list window draws, built again each frame.
pub(crate) type DropdownContent = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// The root view of a native list window: the list at its own size, the
/// window following it. It opens transparent with the list hidden; the
/// first frame measures it, then the window takes that size and its glass.
pub(crate) struct NativeDropdownHost {
    content: DropdownContent,
    translucent: bool,
    radius: Pixels,
    size: Option<Size<Pixels>>,
}

impl Render for NativeDropdownHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = cx.entity();
        let measured = self.size;
        let translucent = self.translucent;
        let radius = self.radius;
        div().size_full().child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .when(measured.is_none(), |this| this.opacity(0.))
                .child((self.content)(window, cx))
                .on_prepaint(move |bounds, window, cx| {
                    let wanted = bounds.size.map(|length| length.ceil());
                    if wanted.width <= px(0.) {
                        return;
                    }
                    // A resize before the popup's first configure is dropped,
                    // so this asks again each frame until the window fits.
                    let viewport = window.viewport_size();
                    let fits = (viewport.width - wanted.width).abs() < px(1.)
                        && (viewport.height - wanted.height).abs() < px(1.);
                    if !fits {
                        window.defer(cx, move |window, _| window.resize(wanted));
                    }
                    if measured.is_some() && measured == Some(wanted) {
                        return;
                    }
                    let first = measured.is_none();
                    window.defer(cx, move |window, cx| {
                        if first && translucent {
                            window.set_background_appearance(WindowBackgroundAppearance::Blurred);
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

/// Open a list in a popup window of its own under `anchor_rect` (its
/// trigger, in `parent`'s coordinates), `width` wide and at most
/// `max_height` tall, focusing `focus` there. Fails where the platform has no
/// native popups, and remembers that, so later lists go straight to the
/// in-window fallback. `on_closed` hears of the window going, whoever closed
/// it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_native_dropdown(
    parent: AnyWindowHandle,
    anchor_rect: Bounds<Pixels>,
    width: Pixels,
    max_height: Pixels,
    focus: FocusHandle,
    content: DropdownContent,
    on_closed: impl FnOnce(&mut App) + 'static,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<NativeDropdownHost>> {
    let translucent = dropdown_background(true, cx).is_some_and(|color| color.a < 1.);
    let radius = cx.theme().radius;
    let options = WindowOptions {
        kind: WindowKind::AnchoredPopup(PopupOptions {
            parent,
            anchor_rect,
            anchor: PopupAnchor::BottomLeft,
            gravity: PopupGravity::BottomRight,
            constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                | PopupConstraintAdjustment::SLIDE_Y
                | PopupConstraintAdjustment::FLIP_Y,
            offset: Point::new(Pixels::ZERO, px(4.)),
            // Like a menu: the keyboard comes with it, and a click in
            // another application dismisses it.
            grab: true,
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Point::default(),
            size: gpui::size(width.max(px(1.)), max_height.max(px(1.))),
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
    let opened = cx.open_window(options, move |window, cx| {
        focus.focus(window, cx);
        let host = cx.new(|_| NativeDropdownHost {
            content,
            translucent,
            radius,
            size: None,
        });
        cx.observe_release(&host, move |_, cx| on_closed(cx))
            .detach();
        host
    });
    if opened.is_err() {
        cx.default_global::<DropdownSurface>().unsupported = true;
    }
    opened
}

/// Close a native list window; nothing if it is already gone.
pub(crate) fn close_native_dropdown(handle: WindowHandle<NativeDropdownHost>, cx: &mut App) {
    _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// Open a Select's or Combobox's list in a window of its own, as
/// [`open_native_dropdown`] does, drawn as the in-window list is. `None`
/// when lists are not opened natively, or the platform refused: the caller
/// draws it in its window.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_native_list<D>(
    list: &gpui::Entity<crate::list::ListState<crate::searchable_list::SearchableListAdapter<D>>>,
    search_placeholder: Option<gpui::SharedString>,
    size: crate::Size,
    menu_width: gpui::Length,
    menu_max_h: gpui::Length,
    scroll_bounce: Option<bool>,
    anchor_rect: Bounds<Pixels>,
    on_closed: impl FnOnce(&mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Option<WindowHandle<NativeDropdownHost>>
where
    D: crate::searchable_list::SearchableListDelegate + 'static,
{
    use crate::{Sizable as _, StyledExt as _, ThemeStyled as _};
    use gpui::{DefiniteLength, Length};

    if !wants_native_dropdowns(cx) {
        return None;
    }
    let rem = window.rem_size();
    let definite = |length: DefiniteLength, whole: Pixels| match length {
        DefiniteLength::Absolute(absolute) => absolute.to_pixels(rem),
        DefiniteLength::Fraction(fraction) => whole * fraction,
    };
    let width = match menu_width {
        Length::Auto => anchor_rect.size.width,
        Length::Definite(length) => definite(length, anchor_rect.size.width),
    };
    let max_height = match menu_max_h {
        Length::Auto => px(480.),
        Length::Definite(length) => definite(length, window.viewport_size().height),
    };
    let focus = list.read(cx).focus_handle.clone();
    let list = list.clone();
    let content: DropdownContent = Rc::new(move |_, cx| {
        crate::v_flex()
            .w(width)
            .popover_style(cx)
            .when_some(dropdown_background(true, cx), |this, background| {
                this.bg(background)
            })
            .child(
                crate::list::List::new(&list)
                    .when_some(search_placeholder.clone(), |this, placeholder| {
                        this.search_placeholder(placeholder)
                    })
                    .with_size(size)
                    .max_h(menu_max_h)
                    .when_some(scroll_bounce, |list, on| list.scroll_bounce(on))
                    .paddings(gpui::Edges::all(crate::sizing::DROPDOWN_LIST_PADDING)),
            )
            .into_any_element()
    });
    open_native_dropdown(
        window.window_handle(),
        anchor_rect,
        width,
        max_height,
        focus,
        content,
        on_closed,
        cx,
    )
    .ok()
}

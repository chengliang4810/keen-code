use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    AnyElement, App, Axis, Bounds, Canvas, Div, ElementId, EmptyView, Entity, EntityId,
    FocusHandle, HoverListenerMode, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, Point, RenderOnce, Role, ScrollHandle, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, canvas, div, point, prelude::*, px, transparent_black,
};
use smallvec::SmallVec;
use web_time::Instant;

use super::ScrollShadow;
use crate::{motion, theme::ActiveTheme};

const IDLE: Duration = Duration::from_millis(900);

/// Sets `scroll`'s offset along `axis` whole so `item`, as painted, sits inside the box, its start first when it is longer; true when it moved.
pub(crate) fn bring_into_view(scroll: &ScrollHandle, item: Bounds<Pixels>, axis: Axis) -> bool {
    let (frame, offset) = (scroll.bounds(), scroll.offset());
    let ((start, end), (first, last)) = match axis {
        Axis::Horizontal => ((item.left(), item.right()), (frame.left(), frame.right())),
        Axis::Vertical => ((item.top(), item.bottom()), (frame.top(), frame.bottom())),
    };
    let shift = if start < first {
        first - start
    } else if end > last {
        (last - end).max(first - start)
    } else {
        Pixels::ZERO
    };
    if shift != Pixels::ZERO {
        scroll.set_offset(match axis {
            Axis::Horizontal => point(offset.x + shift, offset.y),
            Axis::Vertical => point(offset.x, offset.y + shift),
        });
    }
    shift != Pixels::ZERO
}

/// A canvas over a Tab stop in a scroll box: when the stop takes focus, it brings its painted box into view along `axis`, once per focus. A new `id` reveals again.
pub(crate) fn reveal_when_focused(
    id: impl Into<ElementId>,
    scroll: &ScrollHandle,
    focus: &FocusHandle,
    axis: Axis,
    window: &mut Window,
    cx: &mut App,
) -> Canvas<()> {
    let shown = window.use_keyed_state(id, cx, |_, _| false);
    let (scroll, focus) = (scroll.clone(), focus.clone());
    canvas(
        move |bounds, window, cx| {
            let (focused, held) = (focus.is_focused(window), *shown.read(cx));
            if focused && !held {
                shown.update(cx, |shown, _| *shown = true);
                if bring_into_view(&scroll, bounds, axis) {
                    log::info!("scroll: a focused stop came into view");
                    window.request_animation_frame();
                }
            } else if !focused && held {
                shown.update(cx, |shown, _| *shown = false);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Keeps a scroll box's wheel to its own axes; gpui turns a wheel along the other axis onto a box that scrolls one way, while the page scrolls too.
pub fn on_axis<E: Styled>(mut element: E) -> E {
    element.style().restrict_scroll_to_axis = Some(true);
    element
}

struct Activity {
    handle: ScrollHandle,
    last: Point<Pixels>,
    active_at: Option<Instant>,
    hovered: bool,
}

/// Visibility 0..=1: full while active, fading after `IDLE`.
fn presence(activity: &Activity, fade: Duration) -> f32 {
    if activity.hovered {
        return 1.0;
    }
    let Some(at) = activity.active_at else {
        return 0.0;
    };
    let quiet = at.elapsed().saturating_sub(IDLE);
    1.0 - (quiet.as_secs_f32() / fade.as_secs_f32()).min(1.0)
}

/// Thumb start at press, and the pointer's first position.
struct ThumbDrag {
    owner: EntityId,
    start: Pixels,
    anchor: Rc<Cell<Option<Pixels>>>,
}

/// 滚动条的几何尺寸和语义颜色覆盖。
///
/// 未配置时保留 Ely 原有的紧凑 overlay 行为；配置后的轨道会先扣除两端按钮，
/// 再计算 thumb 的有效行程，`thumb_inset` 用来复现来源滚动条的透明端部内缩。
#[derive(Clone, Copy, Debug)]
pub struct ScrollbarVisualStyle {
    /// 轨道总宽度（纵向时为宽度，横向时为高度）。
    pub track_width: Pixels,
    /// thumb 的可见横向宽度（横向滚动时对应可见高度）。
    pub thumb_width: Pixels,
    /// thumb 在横向和两端的透明内缩。
    pub thumb_inset: Pixels,
    /// thumb 的最小布局长度。
    pub min_thumb: Pixels,
    /// 轨道两端按钮的长度。
    pub button_extent: Pixels,
    /// 轨道底色；默认目标为透明。
    pub track_color: Hsla,
    /// thumb 普通、悬停和按下颜色。
    pub thumb_color: Hsla,
    pub thumb_hover_color: Hsla,
    pub thumb_active_color: Hsla,
    /// 按钮悬停和按下时的背景色；普通态沿用透明轨道。
    pub button_hover_color: Hsla,
    pub button_active_color: Hsla,
    /// 按钮箭头普通态和禁用态颜色。
    pub button_icon_color: Hsla,
    pub button_icon_disabled_color: Hsla,
}

impl ScrollbarVisualStyle {
    /// 构造 Settings 专用 Windows 视觉：15px 轨道、9px thumb 和 15px 两端按钮。
    pub fn windows_settings(colors: &crate::theme::Palette) -> Self {
        Self {
            track_width: px(15.0),
            thumb_width: px(9.0),
            thumb_inset: px(3.0),
            min_thumb: px(24.0),
            button_extent: px(15.0),
            track_color: transparent_black(),
            thumb_color: colors.border,
            thumb_hover_color: colors.border_strong,
            thumb_active_color: colors.border_strong,
            button_hover_color: colors.hover,
            button_active_color: colors.active,
            button_icon_color: colors.border,
            button_icon_disabled_color: colors.fg_disabled,
        }
    }
}

/// Overlay thumb for a `ScrollHandle`; drag it to scroll.
#[derive(IntoElement)]
pub struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
    axis: Axis,
    presence: f32,
    visual_style: Option<ScrollbarVisualStyle>,
}

impl Scrollbar {
    pub fn new(id: impl Into<ElementId>, handle: &ScrollHandle, axis: Axis) -> Self {
        Self {
            id: id.into(),
            handle: handle.clone(),
            axis,
            presence: 1.0,
            visual_style: None,
        }
    }

    /// Fades the thumb; 0 hides it.
    pub fn presence(mut self, presence: f32) -> Self {
        self.presence = presence.clamp(0.0, 1.0);
        self
    }

    /// 仅覆盖当前实例的轨道几何、颜色和按钮语义。
    pub fn visual_style(mut self, visual_style: ScrollbarVisualStyle) -> Self {
        self.visual_style = Some(visual_style);
        self
    }
}

impl RenderOnce for Scrollbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let owner = window
            .use_keyed_state(self.id.clone(), cx, |_, _| ())
            .entity_id();
        let theme = cx.theme();
        let rem = window.rem_size();
        let thickness = theme.scrollbar_thickness().to_pixels(rem);
        let min_thumb = theme.scrollbar_min_thumb().to_pixels(rem);
        let (view, max, offset) = (
            self.handle.bounds().size,
            self.handle.max_offset(),
            self.handle.offset(),
        );
        if let Some(style) = self.visual_style {
            let line_step = window.line_height();
            return render_visual_scrollbar(
                self.id.clone(),
                self.handle.clone(),
                self.axis,
                self.presence,
                owner,
                style,
                line_step,
                view,
                max,
                offset,
            );
        }
        let (span, reach, scrolled) = match self.axis {
            Axis::Vertical => (view.height, max.y, -offset.y),
            Axis::Horizontal => (view.width, max.x, -offset.x),
        };
        let length = (span * (span / (span + reach))).max(min_thumb).min(span);
        let travel = span - length;
        if reach <= Pixels::ZERO || travel <= Pixels::ZERO || self.presence <= 0.0 {
            return div().into_any_element();
        }
        let start = travel * (scrolled / reach).clamp(0.0, 1.0);
        let axis = self.axis;
        let handle = self.handle.clone();
        let (idle, busy) = (theme.colors.fg.opacity(0.22), theme.colors.fg.opacity(0.42));
        let thumb = div()
            .id("thumb")
            .absolute()
            .rounded_full()
            .bg(idle)
            .hover(|style| style.bg(busy))
            .on_drag(
                ThumbDrag {
                    owner,
                    start,
                    anchor: Rc::new(Cell::new(None)),
                },
                |_, _, _, cx| cx.new(|_| EmptyView),
            )
            .map(|thumb| match axis {
                Axis::Vertical => thumb.top(start).left_0().w(thickness).h(length),
                Axis::Horizontal => thumb.left(start).top_0().h(thickness).w(length),
            });
        let track = div()
            .id(self.id)
            .absolute()
            .opacity(self.presence)
            .on_drag_move(move |event: &gpui::DragMoveEvent<ThumbDrag>, window, cx| {
                let drag = event.drag(cx);
                if drag.owner != owner {
                    return;
                }
                let local = event.event.position - event.bounds.origin;
                let along = match axis {
                    Axis::Vertical => local.y,
                    Axis::Horizontal => local.x,
                };
                let anchor = match drag.anchor.get() {
                    Some(anchor) => anchor,
                    None => {
                        drag.anchor.set(Some(along));
                        along
                    }
                };
                let ratio = ((drag.start + along - anchor) / travel).clamp(0.0, 1.0);
                let target = -(reach * ratio);
                let current = handle.offset();
                handle.set_offset(match axis {
                    Axis::Vertical => point(current.x, target),
                    Axis::Horizontal => point(target, current.y),
                });
                window.refresh();
            });
        match axis {
            Axis::Vertical => track
                .top_0()
                .bottom_0()
                .right_0()
                .w(thickness * 2.0)
                .pl(thickness * 0.5),
            Axis::Horizontal => track
                .left_0()
                .right_0()
                .bottom_0()
                .h(thickness * 2.0)
                .pt(thickness * 0.5),
        }
        .child(thumb)
        .into_any_element()
    }
}

#[derive(Clone, Copy)]
/// 轨道起点或终点的按钮方向。
enum ScrollbarButtonDirection {
    Start,
    End,
}

fn render_visual_scrollbar(
    id: ElementId,
    handle: ScrollHandle,
    axis: Axis,
    presence: f32,
    owner: EntityId,
    style: ScrollbarVisualStyle,
    line_step: Pixels,
    view: gpui::Size<Pixels>,
    max: Point<Pixels>,
    offset: Point<Pixels>,
) -> AnyElement {
    let (span, reach, scrolled) = match axis {
        Axis::Vertical => (view.height, max.y, -offset.y),
        Axis::Horizontal => (view.width, max.x, -offset.x),
    };
    let button_extent = style.button_extent.max(Pixels::ZERO);
    let thumb_inset = style.thumb_inset.max(Pixels::ZERO);
    let effective_span = (span - button_extent * 2.0).max(Pixels::ZERO);
    let min_thumb = style.min_thumb.max(Pixels::ZERO);
    if span <= Pixels::ZERO
        || reach <= Pixels::ZERO
        || effective_span <= Pixels::ZERO
        || presence <= 0.0
    {
        return div().into_any_element();
    }

    // thumb 按有效轨道占 viewport/content 的比例计算，按钮区域不参与可拖拽行程；
    // 可见填充在 thumb 布局框内保留三像素端部内缩。
    let length = (effective_span * (span / (span + reach)))
        .max(min_thumb)
        .min(effective_span);
    let travel = effective_span - length;
    if travel <= Pixels::ZERO {
        return div().into_any_element();
    }
    let start = button_extent + travel * (scrolled / reach).clamp(0.0, 1.0);
    let cross_inset = style
        .thumb_inset
        .min((style.track_width - style.thumb_width).max(Pixels::ZERO) / 2.0)
        .max(Pixels::ZERO);
    let thumb_width = style
        .thumb_width
        .max(Pixels::ZERO)
        .min(style.track_width.max(Pixels::ZERO));
    let track_width = style.track_width.max(Pixels::ZERO);
    let start_disabled = scrolled <= Pixels::ZERO;
    let end_disabled = scrolled >= reach;
    let thumb_group = format!("scrollbar-thumb-{id}");
    let thumb_axis = axis;
    let thumb = div()
        .id((id.clone(), "thumb"))
        .group(thumb_group.clone())
        .absolute()
        .cursor_pointer()
        .on_drag(
            ThumbDrag {
                owner,
                start,
                anchor: Rc::new(Cell::new(None)),
            },
            |_, _, _, cx| cx.new(|_| EmptyView),
        )
        .map(|thumb| match axis {
            Axis::Vertical => thumb
                .top(start)
                .left(cross_inset)
                .w(thumb_width)
                .h(length)
                .child(
                    div()
                        .id((id.clone(), "thumb-fill"))
                        .absolute()
                        .top(thumb_inset)
                        .bottom(thumb_inset)
                        .left_0()
                        .right_0()
                        .bg(style.thumb_color)
                        .group_hover(thumb_group.clone(), |style_ref| {
                            style_ref.bg(style.thumb_hover_color)
                        })
                        .group_active(thumb_group.clone(), |style_ref| {
                            style_ref.bg(style.thumb_active_color)
                        }),
                ),
            Axis::Horizontal => thumb
                .left(start)
                .top(cross_inset)
                .h(thumb_width)
                .w(length)
                .child(
                    div()
                        .id((id.clone(), "thumb-fill"))
                        .absolute()
                        .left(thumb_inset)
                        .right(thumb_inset)
                        .top_0()
                        .bottom_0()
                        .bg(style.thumb_color)
                        .group_hover(thumb_group.clone(), |style_ref| {
                            style_ref.bg(style.thumb_hover_color)
                        })
                        .group_active(thumb_group, |style_ref| {
                            style_ref.bg(style.thumb_active_color)
                        }),
                ),
        });
    let track_handle = handle.clone();
    let track = div()
        .id(id.clone())
        .absolute()
        .opacity(presence)
        .bg(style.track_color)
        .on_drag_move(move |event: &gpui::DragMoveEvent<ThumbDrag>, window, cx| {
            let drag = event.drag(cx);
            if drag.owner != owner {
                return;
            }
            let local = event.event.position - event.bounds.origin;
            let along = match thumb_axis {
                Axis::Vertical => local.y,
                Axis::Horizontal => local.x,
            };
            let anchor = match drag.anchor.get() {
                Some(anchor) => anchor,
                None => {
                    drag.anchor.set(Some(along));
                    along
                }
            };
            let ratio = ((drag.start + along - anchor - button_extent) / travel).clamp(0.0, 1.0);
            let target = -(reach * ratio);
            let current = track_handle.offset();
            track_handle.set_offset(match thumb_axis {
                Axis::Vertical => point(current.x, target),
                Axis::Horizontal => point(target, current.y),
            });
            window.refresh();
        });
    let track = match axis {
        Axis::Vertical => track.top_0().bottom_0().right_0().w(track_width),
        Axis::Horizontal => track.left_0().right_0().bottom_0().h(track_width),
    };
    let start_button = scrollbar_button(
        (id.clone(), "button-start"),
        axis,
        ScrollbarButtonDirection::Start,
        &handle,
        line_step,
        reach,
        button_extent,
        start_disabled,
        style,
    );
    let end_button = scrollbar_button(
        (id.clone(), "button-end"),
        axis,
        ScrollbarButtonDirection::End,
        &handle,
        line_step,
        reach,
        button_extent,
        end_disabled,
        style,
    );
    track
        .child(start_button)
        .child(end_button)
        .child(thumb)
        .into_any_element()
}

fn scrollbar_button(
    id: impl Into<ElementId>,
    axis: Axis,
    direction: ScrollbarButtonDirection,
    handle: &ScrollHandle,
    step: Pixels,
    reach: Pixels,
    extent: Pixels,
    disabled: bool,
    style: ScrollbarVisualStyle,
) -> impl IntoElement {
    let (label, arrow_start) = match (axis, direction) {
        (Axis::Vertical, ScrollbarButtonDirection::Start) => ("向上滚动", true),
        (Axis::Vertical, ScrollbarButtonDirection::End) => ("向下滚动", false),
        (Axis::Horizontal, ScrollbarButtonDirection::Start) => ("向左滚动", true),
        (Axis::Horizontal, ScrollbarButtonDirection::End) => ("向右滚动", false),
    };
    let delta = match direction {
        ScrollbarButtonDirection::Start => step,
        ScrollbarButtonDirection::End => -step,
    };
    let mut button = div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .absolute()
        .flex()
        .items_center()
        .justify_center()
        .bg(style.track_color)
        .when(!disabled, |button| {
            button
                .hover(|style_ref| style_ref.bg(style.button_hover_color))
                .active(|style_ref| style_ref.bg(style.button_active_color))
        })
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let color = if disabled {
                        style.button_icon_disabled_color
                    } else {
                        style.button_icon_color
                    };
                    paint_scrollbar_arrow(bounds, axis, arrow_start, color, window);
                },
            )
            .size_full(),
        );
    if disabled {
        button = button
            .aria_description("不可用")
            .opacity(0.45)
            .cursor_not_allowed();
    } else {
        let click_handle = handle.clone();
        let key_handle = handle.clone();
        button = button
            .cursor_pointer()
            .tab_index(0)
            .on_click(move |_, window, _| {
                adjust_scroll_offset(&click_handle, axis, delta, reach);
                window.refresh();
            });
        button = button.on_key_down(move |event, window, cx| {
            if event.keystroke.modifiers.modified()
                || !matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                return;
            }
            adjust_scroll_offset(&key_handle, axis, delta, reach);
            cx.stop_propagation();
            window.refresh();
        });
    }
    match (axis, direction) {
        (Axis::Vertical, ScrollbarButtonDirection::Start) => {
            button.top_0().left_0().right_0().h(extent)
        }
        (Axis::Vertical, ScrollbarButtonDirection::End) => {
            button.bottom_0().left_0().right_0().h(extent)
        }
        (Axis::Horizontal, ScrollbarButtonDirection::Start) => {
            button.left_0().top_0().bottom_0().w(extent)
        }
        (Axis::Horizontal, ScrollbarButtonDirection::End) => {
            button.right_0().top_0().bottom_0().w(extent)
        }
    }
}

fn adjust_scroll_offset(handle: &ScrollHandle, axis: Axis, delta: Pixels, reach: Pixels) {
    let current = handle.offset();
    let target = |value: Pixels| value.clamp(-reach, Pixels::ZERO);
    handle.set_offset(match axis {
        Axis::Vertical => point(current.x, target(current.y + delta)),
        Axis::Horizontal => point(target(current.x + delta), current.y),
    });
}

fn paint_scrollbar_arrow(
    bounds: Bounds<Pixels>,
    axis: Axis,
    start: bool,
    color: Hsla,
    window: &mut Window,
) {
    // 原生来源使用实心三角箭头；用 GPUI path 绘制可避免把 chevron 误当成箭头。
    let center = point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    );
    let half = px(4.5);
    let height = px(3.5);
    let mut path = PathBuilder::fill();
    match axis {
        Axis::Vertical if start => {
            path.move_to(point(center.x, center.y - height));
            path.line_to(point(center.x + half, center.y + height));
            path.line_to(point(center.x - half, center.y + height));
        }
        Axis::Vertical => {
            path.move_to(point(center.x - half, center.y - height));
            path.line_to(point(center.x + half, center.y - height));
            path.line_to(point(center.x, center.y + height));
        }
        Axis::Horizontal if start => {
            path.move_to(point(center.x - height, center.y));
            path.line_to(point(center.x + height, center.y - half));
            path.line_to(point(center.x + height, center.y + half));
        }
        Axis::Horizontal => {
            path.move_to(point(center.x + height, center.y));
            path.line_to(point(center.x - height, center.y - half));
            path.line_to(point(center.x - height, center.y + half));
        }
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// Scroll region with overlay scrollbars that show while in use.
#[derive(IntoElement)]
pub struct ScrollArea {
    id: ElementId,
    base: Div,
    shadows: bool,
    body: SmallVec<[AnyElement; 2]>,
}

impl ScrollArea {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            base: div(),
            shadows: false,
            body: SmallVec::new(),
        }
    }

    /// Soft edges where content continues.
    pub fn shadows(mut self) -> Self {
        self.shadows = true;
        self
    }
}

impl Styled for ScrollArea {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for ScrollArea {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.body.extend(elements);
    }
}

fn track_activity(state: &Entity<Activity>, window: &mut Window, cx: &mut App) -> f32 {
    let offset = state.read(cx).handle.offset();
    if state.read(cx).last != offset {
        state.update(cx, |activity, _| {
            activity.last = offset;
            activity.active_at = Some(Instant::now());
        });
    }
    let fade = motion::duration(motion::SLOW, cx);
    let presence = presence(state.read(cx), fade);
    if presence > 0.0 && !state.read(cx).hovered {
        window.request_animation_frame();
    }
    presence
}

impl RenderOnce for ScrollArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| Activity {
            handle: ScrollHandle::new(),
            last: Point::default(),
            active_at: None,
            hovered: false,
        });
        let handle = state.read(cx).handle.clone();
        let presence = track_activity(&state, window, cx);
        self.base
            .id(self.id)
            .relative()
            .overflow_hidden()
            .hover_listener_mode(HoverListenerMode::InputModalityIndependent)
            .on_hover(move |hovered, _, cx| {
                state.update(cx, |activity, cx| {
                    activity.hovered = *hovered;
                    cx.notify();
                })
            })
            .child(
                div()
                    .id("scroll-body")
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&handle)
                    .children(self.body),
            )
            .when(self.shadows, |area| area.child(ScrollShadow::new(&handle)))
            .child(Scrollbar::new("scrollbar-y", &handle, Axis::Vertical).presence(presence))
            .child(Scrollbar::new("scrollbar-x", &handle, Axis::Horizontal).presence(presence))
    }
}

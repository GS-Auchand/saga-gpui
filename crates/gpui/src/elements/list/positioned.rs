use super::*;

/// The fixed edge supplied before an item is laid out.
#[derive(Clone, Copy, Debug)]
pub enum PositionedEdge {
    /// Fixed top edge of the allocated row.
    Top(Pixels),
    /// Fixed bottom edge; the renderer solves allocation before returning.
    Bottom(Pixels),
}

/// Geometry is in window logical pixels. The renderer must not reenter ListState.
#[derive(Clone, Copy, Debug)]
pub struct PositionedContext {
    /// The content viewport after padding.
    pub viewport: Bounds<Pixels>,
    /// The row's left edge in window coordinates.
    pub left: Pixels,
    /// The definite full reading width.
    pub width: Pixels,
    /// The edge held fixed during measurement.
    pub edge: PositionedEdge,
}

/// The element must allocate its final positioned height before prepaint.
pub struct PositionedItem {
    /// The element carrying the final paint and hit-test layout.
    pub element: AnyElement,
    /// The final allocated top in window coordinates.
    pub top: Pixels,
    /// The same row measured without position-dependent exclusion.
    pub baseline: Size<Pixels>,
}

/// Failure leaves the previously committed list measurements unchanged.
#[derive(Debug)]
pub enum PositionedListError {
    /// The state uses an excluded measuring, readiness or anchor mode.
    UnsupportedMode,
    /// A required dimension or coordinate is invalid.
    InvalidGeometry,
    /// The caller's per-frame work limit was exhausted.
    MeasurementBudget,
    /// The measured element does not meet its requested fixed edge.
    PlacementMismatch(usize),
    /// The application renderer could not produce a positioned item.
    Renderer(String),
    /// Child reveal requests did not settle within four transactions.
    RevealDidNotSettle,
}

struct PlacedItem {
    layout: ItemLayout,
    origin: Point<Pixels>,
    baseline: Size<Pixels>,
}

enum AttemptError {
    Retry(ListOffset),
    Failed(PositionedListError),
}

impl From<PositionedListError> for AttemptError {
    fn from(error: PositionedListError) -> Self {
        Self::Failed(error)
    }
}

struct Candidate {
    items: SumTree<ListItem>,
    placed: Vec<PlacedItem>,
    anchor: ListOffset,
    following: bool,
}

/// A successful prepaint transaction. The caller must paint this exact frame.
pub struct PositionedFrame {
    state: ListState,
    bounds: Bounds<Pixels>,
    anchor: ListOffset,
    hitbox: Hitbox,
    placed: Vec<PlacedItem>,
}

fn finite(value: Pixels) -> bool {
    f32::from(value).is_finite()
}

fn replace_item(items: &mut SumTree<ListItem>, index: usize, item: ListItem) {
    let mut cursor = items.cursor::<Count>(());
    let mut next = cursor.slice(&Count(index), Bias::Right);
    next.push(item, ());
    cursor.seek(&Count(index + 1), Bias::Right);
    next.append(cursor.suffix(), ());
    drop(cursor);
    *items = next;
}

pub(super) fn baseline_tree(items: &SumTree<ListItem>) -> SumTree<ListItem> {
    // The summary filter visits only subtrees containing positioned overrides.
    // Unchanged offscreen items remain shared, including unknown measurements.
    let mut cursor = items.filter::<_, Count>((), |summary| summary.has_positioned);
    let mut replacements = Vec::new();
    cursor.next();
    while let Some(item) = cursor.item() {
        if let ListItem::Measured {
            baseline_size: Some(size),
            focus_handle,
            ..
        } = item
        {
            replacements.push((
                cursor.start().0,
                ListItem::Measured {
                    size: *size,
                    baseline_size: None,
                    focus_handle: focus_handle.clone(),
                },
            ));
        }
        cursor.next();
    }
    let mut result = items.clone();
    for (index, item) in replacements {
        replace_item(&mut result, index, item);
    }
    result
}

fn estimate_unknown(items: SumTree<ListItem>, estimate: Pixels) -> SumTree<ListItem> {
    if !items.summary().has_unknown_height {
        return items;
    }
    let fill = |item: &ListItem| match item {
        ListItem::Unmeasured {
            size_hint: None,
            focus_handle,
        } => ListItem::Unmeasured {
            size_hint: Some(size(px(0.), estimate)),
            focus_handle: focus_handle.clone(),
        },
        _ => item.clone(),
    };
    // Initial construction already visits every item; avoid K persistent edits
    // for an entirely unrendered tree. Steady-state appends visit unknown branches.
    if items.summary().rendered_count == 0 {
        return SumTree::from_iter(items.iter().map(fill), ());
    }
    let mut cursor = items.filter::<_, Count>((), |summary| summary.has_unknown_height);
    let mut replacements = Vec::new();
    cursor.next();
    while let Some(item) = cursor.item() {
        replacements.push((cursor.start().0, fill(item)));
        cursor.next();
    }
    drop(cursor);
    let mut result = items;
    for (index, item) in replacements {
        replace_item(&mut result, index, item);
    }
    result
}

fn measure(
    index: usize,
    context: PositionedContext,
    remaining: &mut usize,
    render: &mut impl FnMut(
        usize,
        PositionedContext,
        &mut Window,
        &mut App,
    ) -> Result<PositionedItem, PositionedListError>,
    window: &mut Window,
    cx: &mut App,
) -> Result<PlacedItem, PositionedListError> {
    if *remaining == 0 {
        return Err(PositionedListError::MeasurementBudget);
    }
    let edge = match context.edge {
        PositionedEdge::Top(value) | PositionedEdge::Bottom(value) => value,
    };
    if !finite(edge) {
        return Err(PositionedListError::InvalidGeometry);
    }
    *remaining -= 1;
    let mut item = render(index, context, window, cx)?;
    let item_size = item.element.layout_as_root(
        size(
            AvailableSpace::Definite(context.width),
            AvailableSpace::MinContent,
        ),
        window,
        cx,
    );
    if !finite(item.top)
        || !finite(item_size.height)
        || item_size.height <= px(0.)
        || !finite(item_size.width)
        || !finite(item.baseline.height)
        || item.baseline.height <= px(0.)
        || !finite(item.baseline.width)
    {
        return Err(PositionedListError::InvalidGeometry);
    }
    let mismatch = match context.edge {
        PositionedEdge::Top(top) => (item.top - top).abs(),
        PositionedEdge::Bottom(bottom) => (item.top + item_size.height - bottom).abs(),
    };
    if mismatch > px(0.01) {
        return Err(PositionedListError::PlacementMismatch(index));
    }
    Ok(PlacedItem {
        layout: ItemLayout {
            index,
            element: item.element,
            size: item_size,
        },
        origin: point(context.left, item.top),
        baseline: item.baseline,
    })
}

fn prepare(
    baseline: &SumTree<ListItem>,
    viewport: Bounds<Pixels>,
    alignment: ListAlignment,
    overdraw: Pixels,
    requested: Option<ListOffset>,
    remaining: &mut usize,
    render: &mut impl FnMut(
        usize,
        PositionedContext,
        &mut Window,
        &mut App,
    ) -> Result<PositionedItem, PositionedListError>,
    window: &mut Window,
    cx: &mut App,
) -> Result<Candidate, PositionedListError> {
    let count = baseline.summary().count;
    let context = |edge| PositionedContext {
        viewport,
        left: viewport.left(),
        width: viewport.size.width,
        edge,
    };
    let mut placed = Vec::new();
    let mut restart = false;
    if let Some(anchor) = requested {
        if anchor.item_ix > count || !finite(anchor.offset_in_item) {
            return Err(PositionedListError::InvalidGeometry);
        }
        let mut top = viewport.top() - anchor.offset_in_item;
        let mut reached_end = anchor.item_ix == count;
        for index in anchor.item_ix..count {
            if top >= viewport.bottom() + overdraw {
                break;
            }
            let item = measure(
                index,
                context(PositionedEdge::Top(top)),
                remaining,
                render,
                window,
                cx,
            )?;
            top = item.origin.y + item.layout.size.height;
            placed.push(item);
            reached_end = index + 1 == count;
        }
        restart = reached_end && top < viewport.bottom();
        if restart {
            placed.clear();
        } else {
            let mut bottom = viewport.top() - anchor.offset_in_item;
            let mut leading = Vec::new();
            for index in (0..anchor.item_ix).rev() {
                if bottom <= viewport.top() - overdraw {
                    break;
                }
                let item = measure(
                    index,
                    context(PositionedEdge::Bottom(bottom)),
                    remaining,
                    render,
                    window,
                    cx,
                )?;
                bottom = item.origin.y;
                leading.push(item);
            }
            leading.reverse();
            leading.append(&mut placed);
            placed = leading;
        }
    }
    if requested.is_none() || restart {
        let mut bottom = viewport.bottom();
        for index in (0..count).rev() {
            if bottom <= viewport.top() - overdraw {
                break;
            }
            let item = measure(
                index,
                context(PositionedEdge::Bottom(bottom)),
                remaining,
                render,
                window,
                cx,
            )?;
            bottom = item.origin.y;
            placed.push(item);
        }
        placed.reverse();
    }
    // Short content in a top-aligned viewport must be shaped again at the top:
    // position-dependent widths can change when it moves into an ornament.
    if alignment == ListAlignment::Top
        && placed.first().is_some_and(|item| item.layout.index == 0 && item.origin.y > viewport.top())
    {
        let mut top = viewport.top();
        let mut leading = Vec::new();
        for index in 0..count {
            let item = measure(index, context(PositionedEdge::Top(top)), remaining, render, window, cx)?;
            top = item.origin.y + item.layout.size.height;
            leading.push(item);
            if top > viewport.bottom() { break; }
        }
        if leading.len() == count && top <= viewport.bottom() { placed = leading; }
    }
    let anchor = placed
        .iter()
        .find(|item| item.origin.y + item.layout.size.height > viewport.top())
        .map(|item| ListOffset {
            item_ix: item.layout.index,
            offset_in_item: viewport.top() - item.origin.y,
        })
        .unwrap_or(ListOffset {
            item_ix: count,
            offset_in_item: px(0.),
        });
    let mut items = baseline.clone();
    for item in &placed {
        let mut cursor = baseline.cursor::<Count>(());
        cursor.seek(&Count(item.layout.index), Bias::Right);
        let focus_handle = cursor.item().and_then(ListItem::focus_handle);
        replace_item(
            &mut items,
            item.layout.index,
            ListItem::Measured {
                size: item.layout.size,
                baseline_size: Some(item.baseline),
                focus_handle,
            },
        );
    }
    Ok(Candidate {
        items,
        placed,
        anchor,
        following: requested.is_none(),
    })
}

impl ListState {
    /// Experimental definite-width, synchronous positioned path.
    /// Top alignment affects short content only; overflowing tails stay anchored.
    /// Caller retains normal Element request_layout and calls frame.paint after
    /// success. No state is published on measurement or reveal failure.
    pub fn prepaint_positioned(
        &self,
        bounds: Bounds<Pixels>,
        padding: Edges<Pixels>,
        measurement_budget: usize,
        estimated_item_height: Pixels,
        mut render: impl FnMut(
            usize,
            PositionedContext,
            &mut Window,
            &mut App,
        ) -> Result<PositionedItem, PositionedListError>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<PositionedFrame, PositionedListError> {
        let state = &mut *self.0.borrow_mut();
        if state.item_measurement_ready.is_some()
            || state.measuring_behavior != ListMeasuringBehavior::Visible
            || state
                .scroll_anchor
                .is_some_and(|a| matches!(a.pending, Some(PendingScroll::Proportional(_))))
        {
            return Err(PositionedListError::UnsupportedMode);
        }
        let viewport = Bounds::new(
            bounds.origin + point(padding.left, padding.top),
            size(
                bounds.size.width - padding.along(Axis::Horizontal),
                bounds.size.height - padding.along(Axis::Vertical),
            ),
        );
        if !finite(viewport.left())
            || !finite(viewport.top())
            || !finite(viewport.bottom())
            || !finite(viewport.size.width)
            || viewport.size.width <= px(0.)
            || viewport.size.height <= px(0.)
            || !finite(estimated_item_height)
            || estimated_item_height <= px(0.)
            || !finite(state.overdraw)
            || state.overdraw < px(0.)
            || !finite(viewport.bottom() + state.overdraw)
            || !finite(viewport.top() - state.overdraw)
        {
            return Err(PositionedListError::InvalidGeometry);
        }
        let old_width = state
            .last_layout_bounds
            .zip(state.last_padding)
            .map(|(bounds, padding)| bounds.size.width - padding.along(Axis::Horizontal));
        let baseline = if old_width.is_some_and(|width| width != viewport.size.width) {
            SumTree::from_iter(
                state.items.iter().map(|item| ListItem::Unmeasured {
                    size_hint: item.baseline_hint(),
                    focus_handle: item.focus_handle(),
                }),
                (),
            )
        } else {
            baseline_tree(&state.items)
        };
        let baseline = estimate_unknown(baseline, estimated_item_height);
        let mut requested = if state.follow_state.is_following()
            || state.scroll_anchor.is_some_and(|a| a.pending_end)
            || state.logical_scroll_top().item_ix >= baseline.summary().count
        {
            None
        } else {
            Some(state.logical_scroll_top())
        };
        if let (
            Some(request),
            Some(ScrollAnchor {
                pending: Some(PendingScroll::Absolute(offset)),
                ..
            }),
        ) = (requested.as_mut(), state.scroll_anchor)
        {
            request.offset_in_item = offset;
        }
        let mut remaining = measurement_budget;
        for _ in 0..4 {
            let result = window.transact(|window| {
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                let mut candidate = prepare(
                    &baseline,
                    viewport,
                    state.alignment,
                    state.overdraw,
                    requested,
                    &mut remaining,
                    &mut render,
                    window,
                    cx,
                )?;
                // Offscreen focus participates in dispatch, not contiguous height.
                let mut cursor = candidate
                    .items
                    .filter::<_, Count>((), |s| s.has_focus_handles);
                cursor.next();
                while let Some(item) = cursor.item() {
                    let index = cursor.start().0;
                    if item.contains_focused(window, cx)
                        && !candidate.placed.iter().any(|p| p.layout.index == index)
                    {
                        let extra = measure(
                            index,
                            PositionedContext {
                                viewport,
                                left: viewport.left(),
                                width: viewport.size.width,
                                edge: PositionedEdge::Top(viewport.bottom() + state.overdraw),
                            },
                            &mut remaining,
                            &mut render,
                            window,
                            cx,
                        )?;
                        candidate.placed.push(extra);
                        break;
                    }
                    cursor.next();
                }
                drop(cursor);
                window.take_autoscroll();
                for item in &mut candidate.placed {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        item.layout.element.prepaint_at(item.origin, window, cx);
                    });
                    if let Some(reveal) = window.take_autoscroll()
                        && (reveal.top() < viewport.top()
                            || if reveal.size.height > viewport.size.height {
                                // Full containment is impossible; preserve a visible
                                // leading edge rather than failing every frame.
                                reveal.top() >= viewport.bottom()
                            } else {
                                reveal.bottom() > viewport.bottom()
                            })
                    {
                        let offset = reveal.top() - item.origin.y;
                        return Err(AttemptError::Retry(ListOffset {
                            item_ix: item.layout.index,
                            offset_in_item: offset,
                        }));
                    }
                }
                Ok::<_, AttemptError>((candidate, hitbox))
            });
            match result {
                Err(AttemptError::Retry(anchor)) => {
                    requested = Some(anchor);
                }
                Err(AttemptError::Failed(error)) => return Err(error),
                Ok((candidate, hitbox)) => {
                    state.items = candidate.items;
                    state.scroll_anchor = Some(ScrollAnchor {
                        position: candidate.anchor,
                        pending: None,
                        provisional: false,
                        pending_end: false,
                    });
                    if candidate.following {
                        state.follow_state.start_following();
                    } else {
                        state.follow_state.stop_following();
                    }
                    state.last_layout_bounds = Some(bounds);
                    state.last_padding = Some(padding);
                    state.reset = false;
                    return Ok(PositionedFrame {
                        state: self.clone(),
                        bounds,
                        anchor: candidate.anchor,
                        hitbox,
                        placed: candidate.placed,
                    });
                }
            }
        }
        Err(PositionedListError::RevealDidNotSettle)
    }
}

impl PositionedFrame {
    /// Paint the committed elements and register the ordinary list scroll handler.
    pub fn paint(&mut self, window: &mut Window, cx: &mut App) {
        let current_view = window.current_view();
        let state = self.state.clone();
        let bounds = self.bounds;
        let anchor = self.anchor;
        let hitbox = self.hitbox.id;
        let mut accumulated = ScrollDelta::default();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                accumulated = accumulated.coalesce(event.delta);
                state.0.borrow_mut().scroll(
                    &anchor,
                    bounds.size.height,
                    accumulated.pixel_delta(px(20.)),
                    current_view,
                    window,
                    cx,
                );
            }
        });
        window.with_content_mask(
            Some(ContentMask {
                bounds: self.bounds,
            }),
            |window| {
                for item in &mut self.placed {
                    item.layout.element.paint(window, cx);
                }
            },
        );
    }
}


#[cfg(test)]
mod alignment_tests {
    use super::*;
    use crate::{self as gpui, IntoElement, Styled, TestAppContext, canvas, div};
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    fn positioned_short_top_flow_preserves_long_follow_tail(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        for (count, alignment) in [(2, ListAlignment::Top), (20, ListAlignment::Top), (2, ListAlignment::Bottom)] {
            let state = ListState::new(count, alignment, px(0.));
            state.set_follow_mode(FollowMode::Tail);
            let output = Rc::new(RefCell::new(Vec::new()));
            let recorded = output.clone();
            let painted = state.clone();
            cx.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), move |_, _| {
                canvas(move |bounds, window, cx| {
                    let frame = painted.prepaint_positioned(bounds, Edges::default(), 100, px(20.), |_, context, _, _| {
                        let top = match context.edge { PositionedEdge::Top(top) => top, PositionedEdge::Bottom(bottom) => bottom - px(20.) };
                        Ok(PositionedItem { element: div().w_full().h(px(20.)).into_any_element(), top, baseline: size(context.width, px(20.)) })
                    }, window, cx).expect("positioned frame");
                    *recorded.borrow_mut() = frame.placed.iter().map(|item| (item.layout.index, item.origin.y, item.layout.size.height)).collect();
                    frame
                }, |_, mut frame, window, cx| frame.paint(window, cx)).size_full()
            });
            assert!(state.is_following_tail());
            let rows = output.borrow();
            if count == 2 { assert_eq!(rows[0].1, if alignment == ListAlignment::Top {px(0.)} else {px(60.)}); }
            else { let last = rows.last().expect("tail row"); assert_eq!(last.0, 19); assert_eq!(last.1 + last.2, px(100.)); }
        }
    }
}

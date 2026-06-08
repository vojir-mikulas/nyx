//! The visible view-order and entry selection.

use super::*;

impl AppState {
    /// Recompute [`view_order`](Self::view_order) from the current listing, filter
    /// and sort. This is the one O(n log n) pass; it runs only on a data change,
    /// never per frame, and reuses each row's precomputed `name_lower` so name
    /// filtering/sorting allocates nothing.
    pub(super) fn rebuild_view_order(&mut self) {
        let now = SystemTime::now();
        let query = &self.filter_query;
        let mut order: Vec<usize> = self
            .listing
            .iter()
            .enumerate()
            .filter(|(_, row)| query.matches(&row.entry, &row.name_lower, now))
            .map(|(ix, _)| ix)
            .collect();

        let (key, asc) = self.sort;
        let listing = &self.listing;
        order.sort_by(|&a, &b| {
            let (a, b) = (&listing[a], &listing[b]);
            // Directories always sort before files.
            let dir_order = b.entry.is_dir().cmp(&a.entry.is_dir());
            if dir_order != std::cmp::Ordering::Equal {
                return dir_order;
            }
            let ord = match key {
                SortKey::Name => a.name_lower.cmp(&b.name_lower),
                SortKey::Size => a.entry.size.cmp(&b.entry.size),
                SortKey::Modified => a.entry.modified.cmp(&b.entry.modified),
                SortKey::Kind => a.type_label.cmp(&b.type_label),
            };
            if asc {
                ord
            } else {
                ord.reverse()
            }
        });
        self.view_order = Rc::new(order);
    }

    /// The indices into [`listing`](Self::listing) in visible order, shareable
    /// with the browser's `'static` row closures.
    pub fn view_order(&self) -> Rc<Vec<usize>> {
        self.view_order.clone()
    }

    /// Apply a row click: plain click replaces, cmd/ctrl-click toggles. Either
    /// way the clicked row becomes the anchor a later shift-click extends from.
    pub fn select(&mut self, name: SharedString, additive: bool) {
        if additive {
            if !self.selected.remove(&name) {
                self.selected.insert(name.clone());
            }
        } else {
            self.selected.clear();
            self.selected.insert(name.clone());
        }
        self.select_anchor = Some(name);
    }

    /// Apply a shift-click: select the inclusive range from the anchor row to the
    /// clicked row in the current visible order. With no (visible) anchor it
    /// behaves like a plain click. The anchor is left where it was so successive
    /// shift-clicks re-extend from the same origin.
    pub fn select_range(&mut self, name: SharedString) {
        let names = self.visible_names();
        let clicked = names.iter().position(|n| *n == name);
        let anchor = self
            .select_anchor
            .as_ref()
            .and_then(|a| names.iter().position(|n| n == a));
        match (clicked, anchor) {
            (Some(click), Some(anchor)) => {
                let (lo, hi) = (click.min(anchor), click.max(anchor));
                self.selected = names[lo..=hi].iter().cloned().collect();
            }
            // No anchor (or it scrolled out of the listing): fall back to a plain
            // select, seeding the anchor for the next shift-click.
            _ => self.select(name, false),
        }
    }

    /// Count of selected entries.
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Count of entries in the current listing.
    pub fn item_count(&self) -> usize {
        self.listing.len()
    }

    /// The visible (filtered + sorted) entry names, in display order.
    pub(super) fn visible_names(&self) -> Vec<SharedString> {
        self.view_order
            .iter()
            .map(|&ix| SharedString::from(self.listing[ix].entry.name.clone()))
            .collect()
    }

    /// Move the single-row selection by `delta` rows (keyboard up/down). With no
    /// selection, down picks the first row and up the last.
    pub fn move_selection(&mut self, delta: i32) {
        let names = self.visible_names();
        if names.is_empty() {
            return;
        }
        let next = match names.iter().position(|n| self.selected.contains(n)) {
            Some(cur) => (cur as i32 + delta).clamp(0, names.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => names.len() - 1,
        };
        self.selected.clear();
        self.selected.insert(names[next].clone());
    }

    /// Select the first (`last == false`) or last row (Home / End).
    pub fn select_edge(&mut self, last: bool) {
        let names = self.visible_names();
        let Some(target) = (if last { names.last() } else { names.first() }) else {
            return;
        };
        let target = target.clone();
        self.selected.clear();
        self.selected.insert(target);
    }

    /// Select every visible row (`cmd-a` in the file table).
    pub fn select_all_visible(&mut self) {
        self.selected = self.visible_names().into_iter().collect();
    }

    /// The file table's scroll handle, to bind the list to it.
    pub fn file_scroll(&self) -> &UniformListScrollHandle {
        &self.file_scroll
    }

    /// The file list's painted viewport as `(top, height, scroll_offset_y)` in
    /// window coordinates, or `None` before its first paint. `scroll_offset_y` is
    /// `≤ 0` and grows more negative as the list scrolls down. Rows are uniform
    /// height, so this is all the geometry the rubber-band needs.
    fn list_geometry(&self) -> Option<(Pixels, Pixels, Pixels)> {
        let state = self.file_scroll.0.borrow();
        let bounds = state.base_handle.bounds();
        if bounds.size.height <= px(0.) {
            return None;
        }
        Some((
            bounds.origin.y,
            bounds.size.height,
            state.base_handle.offset().y,
        ))
    }

    /// The row index a window-coord `y` falls on (may be off either end), given
    /// the list viewport top and scroll offset.
    fn row_at_y(&self, y: Pixels, top: Pixels, offset_y: Pixels) -> f32 {
        let row_h = self.density.row_height();
        (f32::from(y - top - offset_y) / row_h).floor()
    }

    /// Begin a rubber-band at a left-press. Starts only in the list's empty area
    /// below the last row - never on the header above the rows, nor on a row
    /// itself (those keep their own click/drag, so a file grab is never hijacked).
    /// Clears the selection, so a press on empty space also deselects. Returns
    /// whether a rubber-band was started.
    ///
    /// A 16ms poll loop then drives the whole gesture from the live cursor
    /// position - including while the cursor is dragged outside the window (macOS
    /// keeps reporting it to the window that captured the press), so the box and
    /// selection keep tracking it. The loop ends when the gesture does (via
    /// [`marquee_end`](Self::marquee_end) on release) or a newer one supersedes it.
    pub fn marquee_start(
        &mut self,
        origin: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((top, height, offset_y)) = self.list_geometry() else {
            return false;
        };
        if origin.y < top || origin.y > top + height {
            return false;
        }
        let count = self.view_order.len();
        if count > 0 {
            let ix = self.row_at_y(origin.y, top, offset_y);
            if (0.0..count as f32).contains(&ix) {
                return false; // the press landed on a row
            }
        }
        // Pin the anchor in content space so auto-scroll doesn't slide it onto a
        // different row.
        let anchor_y = origin.y - top - offset_y;
        self.marquee_gen = self.marquee_gen.wrapping_add(1);
        self.selected.clear();
        self.select_anchor = None;
        self.marquee = Some(Marquee {
            origin,
            anchor_y,
            current: origin,
            active: false,
        });
        let generation = self.marquee_gen;
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            let cont = this
                .update_in(cx, |this, window, cx| {
                    this.marquee_drive(window.mouse_position(), generation, cx)
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
        cx.notify();
        true
    }

    /// The rubber-band rectangle in window coordinates, for the table to paint;
    /// `None` unless a rubber-band is active. The vertical anchor is reprojected
    /// through the live scroll offset so the box tracks the list as it scrolls,
    /// and every edge is clamped to the file list's viewport so the box never
    /// spills over the sidebar, the toolbar, or the column header.
    pub fn marquee_rect(&self) -> Option<Bounds<Pixels>> {
        let marquee = self.marquee.as_ref().filter(|m| m.active)?;
        let (bounds, offset_y) = {
            let state = self.file_scroll.0.borrow();
            let bounds = state.base_handle.bounds();
            if bounds.size.height <= px(0.) {
                return None;
            }
            (bounds, state.base_handle.offset().y)
        };
        let (left, right) = (bounds.origin.x, bounds.origin.x + bounds.size.width);
        let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
        let anchor_win_y = marquee.anchor_y + top + offset_y;
        let x0 = marquee.origin.x.min(marquee.current.x).clamp(left, right);
        let x1 = marquee.origin.x.max(marquee.current.x).clamp(left, right);
        let y0 = anchor_win_y.min(marquee.current.y).clamp(top, bottom);
        let y1 = anchor_win_y.max(marquee.current.y).clamp(top, bottom);
        Some(Bounds::from_corners(point(x0, y0), point(x1, y1)))
    }

    /// End the rubber-band gesture (left-release). A no-op if none is active.
    /// The poll loop notices the cleared marquee and stops on its next tick.
    pub fn marquee_end(&mut self, cx: &mut Context<Self>) {
        if self.marquee.take().is_some() {
            cx.notify();
        }
    }

    /// One poll-loop tick: track the cursor to `pos`, activate past the drag
    /// threshold, auto-scroll when it hugs a list edge, and reselect. Returns
    /// whether the loop should keep running.
    fn marquee_drive(
        &mut self,
        pos: Point<Pixels>,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        // Stop a stale loop a newer gesture (or a release) has superseded.
        if self.marquee_gen != generation {
            return false;
        }
        let (active, moved) = {
            let Some(marquee) = self.marquee.as_mut() else {
                return false;
            };
            let moved = marquee.current != pos;
            marquee.current = pos;
            // Below the start threshold a press still reads as a plain click; don't
            // draw the box or touch selection yet.
            if !marquee.active {
                let threshold = px(4.);
                if (pos.x - marquee.origin.x).abs() > threshold
                    || (pos.y - marquee.origin.y).abs() > threshold
                {
                    marquee.active = true;
                }
            }
            (marquee.active, moved)
        };
        if !active {
            return true;
        }
        let velocity = self.marquee_scroll_velocity(pos);
        if velocity != 0.0 {
            self.apply_scroll(velocity);
        } else if !moved {
            return true; // nothing changed this tick
        }
        self.marquee_select();
        cx.notify();
        true
    }

    /// Replace the selection with every row the current rubber-band rect spans.
    /// Purely geometric (row index = pixel ÷ row height), so it covers rows that
    /// scrolled out of view, not just the painted ones.
    fn marquee_select(&mut self) {
        let Some(marquee) = self.marquee.as_ref() else {
            return;
        };
        let (anchor_y, current) = (marquee.anchor_y, marquee.current);
        let Some((top, _height, offset_y)) = self.list_geometry() else {
            return;
        };
        let count = self.view_order.len();
        if count == 0 {
            self.selected.clear();
            return;
        }
        let row_h = self.density.row_height();
        // Anchor is already in content space; the pointer is reprojected into it.
        let anchor_row = (f32::from(anchor_y) / row_h).floor();
        let current_row = self.row_at_y(current.y, top, offset_y);
        let lo = anchor_row.min(current_row);
        let hi = anchor_row.max(current_row);
        // The rect can sit entirely above the first row or below the last.
        if hi < 0.0 || lo >= count as f32 {
            self.selected.clear();
            return;
        }
        let lo = lo.max(0.0) as usize;
        let hi = (hi as usize).min(count - 1);
        let order = self.view_order.clone();
        let listing = self.listing.clone();
        self.selected = order[lo..=hi]
            .iter()
            .map(|&i| SharedString::from(listing[i].entry.name.clone()))
            .collect();
    }

    /// Auto-scroll speed for a pointer at `pos`: proportional to how deep it is in
    /// a top/bottom edge zone, `0` when clear of both. Positive scrolls down
    /// (content up); negative scrolls up.
    fn marquee_scroll_velocity(&self, pos: Point<Pixels>) -> f32 {
        let Some((top, height, _)) = self.list_geometry() else {
            return 0.0;
        };
        let margin = px(24.);
        let max_step = 20.0_f32;
        if pos.y > top + height - margin {
            let depth = f32::from(pos.y - (top + height - margin));
            (depth / f32::from(margin)).clamp(0.0, 1.0) * max_step
        } else if pos.y < top + margin {
            let depth = f32::from(top + margin - pos.y);
            -(depth / f32::from(margin)).clamp(0.0, 1.0) * max_step
        } else {
            0.0
        }
    }

    /// Shift the list's scroll offset by `velocity` pixels, clamped to its range.
    fn apply_scroll(&self, velocity: f32) {
        let state = self.file_scroll.0.borrow();
        let off = state.base_handle.offset();
        let max = f32::from(state.base_handle.max_offset().y);
        let new_y = (f32::from(off.y) - velocity).clamp(-max, 0.0);
        state.base_handle.set_offset(point(off.x, px(new_y)));
    }
}

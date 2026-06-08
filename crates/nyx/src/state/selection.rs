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

    /// The current rubber-band rectangle, for the table to paint.
    pub fn marquee(&self) -> Option<&Marquee> {
        self.marquee.as_ref()
    }

    /// Whether a window-coord point lies over a visible file row.
    fn point_over_row(&self, p: Point<Pixels>) -> bool {
        self.row_bounds.borrow().iter().any(|(_, b)| b.contains(&p))
    }

    /// Begin a rubber-band at a left-press. Starts only on empty space (a press
    /// over a row is left to its click/drag handlers, so a file grab is never
    /// hijacked) and clears the selection, so a press on empty space deselects.
    /// Returns whether a rubber-band was started.
    pub fn marquee_start(&mut self, origin: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if self.point_over_row(origin) {
            return false;
        }
        self.selected.clear();
        self.select_anchor = None;
        self.marquee = Some(Marquee {
            origin,
            current: origin,
            active: false,
        });
        cx.notify();
        true
    }

    /// Grow the active rubber-band to `current` and reselect every row its rect
    /// crosses. A no-op without an active rubber-band.
    pub fn marquee_update(&mut self, current: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(marquee) = self.marquee.as_mut() else {
            return;
        };
        marquee.current = current;
        // Below the start threshold a press still reads as a plain click; don't
        // draw the box or touch selection yet.
        let threshold = px(4.);
        let moved = (current.x - marquee.origin.x).abs() > threshold
            || (current.y - marquee.origin.y).abs() > threshold;
        if !marquee.active && !moved {
            return;
        }
        marquee.active = true;
        let origin = marquee.origin;

        let top_left = point(origin.x.min(current.x), origin.y.min(current.y));
        let bottom_right = point(origin.x.max(current.x), origin.y.max(current.y));
        let rect = Bounds::from_corners(top_left, bottom_right);
        self.selected = self
            .row_bounds
            .borrow()
            .iter()
            .filter(|(_, b)| rect.intersects(b))
            .map(|(name, _)| name.clone())
            .collect();
        cx.notify();
    }

    /// End the rubber-band gesture (left-release). A no-op if none is active.
    pub fn marquee_end(&mut self, cx: &mut Context<Self>) {
        if self.marquee.take().is_some() {
            cx.notify();
        }
    }
}

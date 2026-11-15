//! Grid layout and two-dimensional keyboard navigation, independent of any UI toolkit.
//!
//! Sections start on a new row. `cols` is decided by the UI from the available width (or forced
//! by the view); the model turns visible-item indices into rows and moves the selection.

/// One laid-out line of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRow {
    /// Index in the list of *visible sections* (not the view's section index).
    pub section: usize,
    /// First visible item of this row, and how many items it holds (≤ cols).
    pub start: usize,
    pub len: usize,
    /// First row of its section (a header is drawn above it).
    pub first_in_section: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridLayout {
    pub cols: usize,
    pub rows: Vec<GridRow>,
}

impl GridLayout {
    /// `section_sizes[i]` = number of visible items in the i-th visible section.
    pub fn new(section_sizes: &[usize], cols: usize) -> Self {
        let cols = cols.max(1);
        let mut rows = Vec::new();
        let mut start = 0;
        for (s, &n) in section_sizes.iter().enumerate() {
            let mut left = n;
            let mut first = true;
            while left > 0 {
                let len = left.min(cols);
                rows.push(GridRow { section: s, start, len, first_in_section: first });
                start += len;
                left -= len;
                first = false;
            }
        }
        Self { cols, rows }
    }

    pub fn item_count(&self) -> usize {
        self.rows.iter().map(|r| r.len).sum()
    }

    /// (row, column) of a visible item.
    pub fn position(&self, item: usize) -> Option<(usize, usize)> {
        let r = self.rows.iter().position(|r| item >= r.start && item < r.start + r.len)?;
        Some((r, item - self.rows[r].start))
    }

    pub fn left(&self, item: usize) -> usize {
        item.saturating_sub(1)
    }

    pub fn right(&self, item: usize) -> usize {
        (item + 1).min(self.item_count().saturating_sub(1))
    }

    /// Same column one row up/down; a shorter row clamps to its last cell. Stays put at the edges.
    pub fn vertical(&self, item: usize, delta: isize) -> usize {
        let Some((r, c)) = self.position(item) else { return item };
        let target = r as isize + delta;
        if target < 0 || target >= self.rows.len() as isize {
            return item;
        }
        let row = &self.rows[target as usize];
        row.start + c.min(row.len - 1)
    }

    pub fn up(&self, item: usize) -> usize {
        self.vertical(item, -1)
    }

    pub fn down(&self, item: usize) -> usize {
        self.vertical(item, 1)
    }

    pub fn home(&self) -> usize {
        0
    }

    pub fn end(&self) -> usize {
        self.item_count().saturating_sub(1)
    }
}

/// Columns for a grid `width` px wide with `cell` px cells and `gap` between them.
pub fn columns_for(width: f32, cell: f32, gap: f32, forced: Option<u32>) -> usize {
    if let Some(c) = forced {
        return c.max(1) as usize;
    }
    (((width + gap) / (cell + gap)).floor() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_start_on_new_rows() {
        let g = GridLayout::new(&[5, 2, 3], 3);
        let shape: Vec<_> = g.rows.iter().map(|r| (r.section, r.start, r.len, r.first_in_section)).collect();
        assert_eq!(shape, [(0, 0, 3, true), (0, 3, 2, false), (1, 5, 2, true), (2, 7, 3, true)]);
        assert_eq!(g.item_count(), 10);
        assert_eq!(g.position(4), Some((1, 1)));
        assert_eq!(g.position(10), None);
    }

    #[test]
    fn arrows_move_in_two_dimensions_and_clamp() {
        let g = GridLayout::new(&[5, 2], 3);
        // 0 1 2 / 3 4 / 5 6
        assert_eq!((g.right(0), g.right(6), g.left(0), g.left(3)), (1, 6, 0, 2));
        assert_eq!(g.down(1), 4);
        assert_eq!(g.down(2), 4, "column 2 does not exist in the short row: clamp to its last cell");
        assert_eq!(g.down(4), 6, "crosses into the next section");
        assert_eq!(g.up(6), 4);
        assert_eq!(g.up(1), 1, "stays on the first row");
        assert_eq!(g.down(6), 6, "stays on the last row");
        assert_eq!((g.home(), g.end()), (0, 6));
    }

    #[test]
    fn empty_and_single_column_grids() {
        let g = GridLayout::new(&[], 4);
        assert_eq!((g.item_count(), g.end(), g.right(0)), (0, 0, 0));
        let one = GridLayout::new(&[3], 0); // 0 columns is treated as 1
        assert_eq!(one.cols, 1);
        assert_eq!((one.down(0), one.down(1), one.up(2)), (1, 2, 1));
    }

    #[test]
    fn column_count_follows_the_width() {
        assert_eq!(columns_for(700., 112., 8., None), 5);
        assert_eq!(columns_for(50., 112., 8., None), 1);
        assert_eq!(columns_for(700., 112., 8., Some(3)), 3);
        assert_eq!(columns_for(700., 112., 8., Some(0)), 1);
    }
}

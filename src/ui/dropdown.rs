use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DropdownSpec {
    pub anchor: Rect,
    pub item_count: usize,
    pub selected: usize,
    pub has_filter: bool,
    pub max_rows: usize,
    pub min_width: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DropdownLayout {
    pub rect: Rect,
    pub first_visible: usize,
    pub visible_rows: usize,
    pub filter_rect: Option<Rect>,
    pub list_rect: Rect,
}

/// Lay out a popup below its anchor without consulting or changing UI state.
pub(crate) fn layout_dropdown(spec: &DropdownSpec, area: Rect) -> Option<DropdownLayout> {
    if spec.item_count == 0 || spec.max_rows == 0 || area.width == 0 || area.height == 0 {
        return None;
    }

    let top = spec.anchor.bottom();
    if top < area.y || top >= area.bottom() {
        return None;
    }

    let filter_rows = usize::from(spec.has_filter);
    let available_rows = usize::from(area.bottom().saturating_sub(top));
    let visible_rows = spec
        .item_count
        .min(spec.max_rows)
        .min(available_rows.saturating_sub(filter_rows));
    if visible_rows == 0 {
        return None;
    }

    let width = spec.min_width.max(1).min(area.width);
    let x = spec
        .anchor
        .x
        .max(area.x)
        .min(area.right().saturating_sub(width));
    let height = u16::try_from(visible_rows.saturating_add(filter_rows)).unwrap_or(u16::MAX);
    let rect = Rect::new(x, top, width, height);
    let filter_rect = spec
        .has_filter
        .then_some(Rect::new(rect.x, rect.y, rect.width, 1));
    let list_y = rect.y.saturating_add(u16::from(spec.has_filter));
    let list_rect = Rect::new(
        rect.x,
        list_y,
        rect.width,
        u16::try_from(visible_rows).unwrap_or(u16::MAX),
    );

    let selected = spec.selected.min(spec.item_count.saturating_sub(1));
    let max_first = spec.item_count.saturating_sub(visible_rows);
    let first_visible = selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(max_first);

    Some(DropdownLayout {
        rect,
        first_visible,
        visible_rows,
        filter_rect,
        list_rect,
    })
}

// The Phase 0 widget publishes filtering before a filtered picker consumes it.
#[allow(dead_code)]
pub(crate) fn filter_items<'a>(items: &'a [String], query: &str) -> Vec<(usize, &'a str)> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let mut item_chars = item.chars().flat_map(char::to_lowercase);
            query
                .iter()
                .all(|query_char| {
                    item_chars
                        .by_ref()
                        .any(|item_char| item_char == *query_char)
                })
                .then_some((index, item.as_str()))
        })
        .collect()
}

pub(crate) fn hit_test(layout: &DropdownLayout, x: u16, y: u16) -> Option<usize> {
    let list = layout.list_rect;
    if list.width == 0
        || list.height == 0
        || x < list.x
        || x >= list.right()
        || y < list.y
        || y >= list.bottom()
    {
        return None;
    }
    Some(layout.first_visible + usize::from(y - list.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(anchor: Rect, item_count: usize, selected: usize) -> DropdownSpec {
        DropdownSpec {
            anchor,
            item_count,
            selected,
            has_filter: false,
            max_rows: 8,
            min_width: 12,
        }
    }

    #[test]
    fn fits_below() {
        let anchor = Rect::new(4, 2, 7, 2);
        let layout = layout_dropdown(&spec(anchor, 3, 0), Rect::new(0, 0, 30, 12))
            .expect("three rows fit below the anchor");

        assert_eq!(layout.rect, Rect::new(4, anchor.bottom(), 12, 3));
        assert_eq!(layout.first_visible, 0);
        assert_eq!(layout.visible_rows, 3);
        assert_eq!(layout.filter_rect, None);
        assert_eq!(layout.list_rect, layout.rect);
    }

    #[test]
    fn clamp_and_scroll_keeps_selected_visible() {
        let anchor = Rect::new(2, 5, 8, 1);
        let area = Rect::new(0, 0, 30, 9);
        let layout =
            layout_dropdown(&spec(anchor, 10, 8), area).expect("space below holds a clamped list");

        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.rect.height, 3);
        assert_eq!(layout.visible_rows, 3);
        assert_eq!(layout.first_visible, 6);
        assert!(layout.first_visible <= 8);
        assert!(8 < layout.first_visible + layout.visible_rows);
    }

    #[test]
    fn never_above() {
        let anchor = Rect::new(2, 7, 8, 1);
        let layout = layout_dropdown(&spec(anchor, 10, 9), Rect::new(0, 0, 30, 10))
            .expect("two rows fit below");

        assert_eq!(layout.rect.y, anchor.bottom());
        assert!(layout.rect.y >= anchor.bottom());
        assert!(layout.rect.bottom() <= 10);
    }

    #[test]
    fn last_row_anchor_returns_none() {
        let anchor = Rect::new(2, 9, 8, 1);
        assert_eq!(
            layout_dropdown(&spec(anchor, 2, 0), Rect::new(0, 0, 30, 10)),
            None
        );
    }

    #[test]
    fn filter_narrows_and_is_case_insensitive() {
        let items = vec![
            "Claude Code".to_string(),
            "Codex".to_string(),
            "Gemini CLI".to_string(),
            "code runner".to_string(),
        ];

        assert_eq!(
            filter_items(&items, "CdE"),
            vec![(0, "Claude Code"), (1, "Codex"), (3, "code runner")]
        );
        assert_eq!(filter_items(&items, "MIC"), vec![(2, "Gemini CLI")]);
    }

    #[test]
    fn hit_test_without_filter_line_applies_scroll_offset() {
        let layout = layout_dropdown(&spec(Rect::new(4, 4, 8, 1), 10, 8), Rect::new(0, 0, 30, 8))
            .expect("three item rows fit");

        assert_eq!(hit_test(&layout, 4, 5), Some(6));
        assert_eq!(hit_test(&layout, 15, 7), Some(8));
        assert_eq!(hit_test(&layout, 16, 7), None);
        assert_eq!(hit_test(&layout, 4, 4), None);
    }

    #[test]
    fn hit_test_ignores_filter_line() {
        let mut filtered = spec(Rect::new(4, 3, 8, 1), 10, 7);
        filtered.has_filter = true;
        let layout = layout_dropdown(&filtered, Rect::new(0, 0, 30, 8))
            .expect("one filter row and three item rows fit");

        assert_eq!(layout.filter_rect, Some(Rect::new(4, 4, 12, 1)));
        assert_eq!(hit_test(&layout, 4, 4), None);
        assert_eq!(hit_test(&layout, 4, 5), Some(5));
        assert_eq!(hit_test(&layout, 4, 7), Some(7));
    }
}

//! Convert window-relative avoid areas to surface-relative edge insets.

pub(crate) fn edge_insets(
    surface_start: i32,
    surface_len: i32,
    avoid_start: i32,
    avoid_len: i32,
) -> (u32, u32) {
    if surface_len <= 0 || avoid_len <= 0 {
        return (0, 0);
    }
    let start = i64::from(surface_start);
    let len = i64::from(surface_len);
    let end = start + len;
    let avoid_start = i64::from(avoid_start);
    let avoid_end = avoid_start + i64::from(avoid_len);
    let near = if avoid_start <= start {
        (avoid_end - start).clamp(0, len)
    } else {
        0
    };
    let far = if avoid_end >= end {
        (end - avoid_start).clamp(0, len)
    } else {
        0
    };
    (near as u32, far as u32)
}

#[cfg(test)]
mod tests {
    use super::edge_insets;

    #[test]
    fn arkui_already_avoids_system_bars() {
        assert_eq!(edge_insets(136, 2526, 0, 136), (0, 0));
        assert_eq!(edge_insets(136, 2526, 2662, 98), (0, 0));
    }

    #[test]
    fn edge_to_edge_and_partial_overlap() {
        assert_eq!(edge_insets(0, 2760, 0, 136), (136, 0));
        assert_eq!(edge_insets(0, 2760, 2662, 98), (0, 98));
        assert_eq!(edge_insets(100, 2660, 0, 136), (36, 0));
        assert_eq!(edge_insets(0, 2700, 2662, 98), (0, 38));
    }

    #[test]
    fn empty_and_interior_areas_do_not_create_edge_insets() {
        assert_eq!(edge_insets(0, 100, 50, 0), (0, 0));
        assert_eq!(edge_insets(0, 100, 20, 30), (0, 0));
        assert_eq!(edge_insets(0, 0, 0, 100), (0, 0));
    }
}

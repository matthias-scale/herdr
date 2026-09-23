//! Shared terminal bar glyph selection for usage charts and quota meters.

const BAR_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub(super) fn bar_glyph(value: f64, maximum: f64) -> char {
    if maximum <= 0.0 {
        return BAR_GLYPHS[0];
    }
    let index = ((value / maximum) * 7.0).round().clamp(0.0, 7.0) as usize;
    BAR_GLYPHS[index]
}

pub(super) fn percent_meter(percent: u8, width: usize) -> String {
    let filled = f64::from(percent.min(100)) * width as f64 / 100.0;
    (0..width)
        .map(|column| {
            let fill = (filled - column as f64).clamp(0.0, 1.0);
            if fill == 0.0 {
                '·'
            } else if fill == 1.0 {
                BAR_GLYPHS[7]
            } else {
                bar_glyph(fill, 1.0)
            }
        })
        .collect()
}

#[cfg(test)]
pub(super) fn is_bar_glyph(glyph: char) -> bool {
    BAR_GLYPHS.contains(&glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_glyph_scale_matches_the_existing_eight_steps() {
        assert_eq!(bar_glyph(0.0, 0.0), '▁');
        assert_eq!(bar_glyph(0.0, 8.0), '▁');
        assert_eq!(bar_glyph(4.0, 8.0), '▅');
        assert_eq!(bar_glyph(8.0, 8.0), '█');
    }

    #[test]
    fn percentage_meter_keeps_partial_and_empty_cells_visible() {
        assert_eq!(percent_meter(0, 4), "····");
        assert_eq!(percent_meter(50, 4), "██··");
        assert_eq!(percent_meter(81, 8), "██████▄·");
        assert_eq!(percent_meter(100, 4), "████");
    }
}

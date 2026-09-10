//! Pure star-field generator for the sidebar's idle animation.
//!
//! Stars accelerate out of a centre point and stretch into streaks as they go,
//! so the widget reads as forward motion. The field is a pure function of
//! `(width, height, step)`: no clock, no state, no allocation. The caller owns
//! the tick, which keeps this testable without a terminal and keeps the render
//! path allocation-free.

/// Frames in one loop. `step` is taken modulo this, so the animation repeats
/// exactly and callers can hand it a free-running counter.
pub(crate) const FRAMES: u32 = 30;

/// Upper bound on the grid this module will fill. The sidebar is clamped well
/// below both, and anything larger is simply cropped, so the buffer can live on
/// the stack instead of the heap.
pub(super) const MAX_COLS: usize = 64;
pub(super) const MAX_ROWS: usize = 8;

/// How bright a cell is. The caller maps this onto theme colours; the generator
/// never names a colour itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Level {
    /// Far tail of a streak, or the resting field.
    Faint,
    /// Near tail, and the still centre marker.
    Dim,
    /// Star head, once it has picked up speed.
    Bright,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cell {
    pub ch: char,
    pub level: Level,
}

const EMPTY: Cell = Cell {
    ch: ' ',
    level: Level::Faint,
};

/// A rendered frame. Rows outside `height` and columns outside `width` are left
/// blank; `rows()` hands back only the live region.
pub(super) struct Field {
    grid: [[Cell; MAX_COLS]; MAX_ROWS],
    width: usize,
    height: usize,
}

impl Field {
    pub(super) fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        self.grid[..self.height]
            .iter()
            .map(|row| &row[..self.width])
    }

    fn put(&mut self, x: f32, y: f32, ch: char, level: Level) {
        // Reject before casting: a negative or non-finite coordinate must not
        // wrap into a valid index.
        if !(x.is_finite() && y.is_finite()) || x < -0.5 || y < -0.5 {
            return;
        }
        let (col, row) = ((x + 0.5) as usize, (y + 0.5) as usize);
        if col < self.width && row < self.height {
            self.grid[row][col] = Cell { ch, level };
        }
    }
}

/// Deterministic hash in `0.0..1.0`. Used only to scatter the stars' starting
/// phases, so any stable scramble does; this one keeps the field reproducible
/// across platforms without pulling in a RNG dependency.
fn scatter(n: u32) -> f32 {
    let v = ((n as f32) * 12.9898 + 78.233).sin() * 43758.545;
    v - v.floor()
}

/// The golden angle, which spreads successive stars around the circle without
/// them ever lining up into spokes.
const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// Stars per cell of grid area. Tuned on the 28x6 sidebar slot (~26 stars);
/// wider sidebars get proportionally more so density stays constant.
const DENSITY: f32 = 0.155;

/// Cap on star count, so a very wide sidebar cannot turn this into real work.
const MAX_STARS: u32 = 64;

/// Floor on star count. The panel is a small square, so density alone would
/// leave its field too sparse to read as motion.
const MIN_STARS: u32 = 14;

/// How far past the edge a star travels before wrapping. Above 1.0 so stars
/// leave the frame instead of piling up on the border.
const REACH: f32 = 1.15;

/// Gap between successive samples of a streak, in normalised radius.
const STREAK_GAP: f32 = 0.055;

/// Longest streak, in samples, drawn behind the fastest star.
const MAX_STREAK: u32 = 5;

pub(super) fn field(width: u16, height: u16, step: u32) -> Field {
    // Either dimension at zero means there is nothing to draw, so collapse
    // both and hand back an empty field rather than a stack of blank rows.
    let (width, height) = if width == 0 || height == 0 {
        (0, 0)
    } else {
        (
            (width as usize).min(MAX_COLS),
            (height as usize).min(MAX_ROWS),
        )
    };
    let mut f = Field {
        grid: [[EMPTY; MAX_COLS]; MAX_ROWS],
        width,
        height,
    };
    if width == 0 {
        return f;
    }

    let cx = (width as f32 - 1.0) / 2.0;
    let cy = (height as f32 - 1.0) / 2.0;
    let rx = width as f32 * 0.5 * REACH;
    let ry = height as f32 * 0.5 * REACH;
    let phase = (step % FRAMES) as f32 / FRAMES as f32;
    let stars = (((width * height) as f32 * DENSITY) as u32).clamp(MIN_STARS, MAX_STARS);

    // Still marker first, so a star that reaches the centre paints over it.
    f.put(cx, cy, '+', Level::Dim);

    for k in 0..stars {
        let angle = k as f32 * GOLDEN_ANGLE;
        let (sin_a, cos_a) = angle.sin_cos();
        // Quadratic radius: slow near the centre, snapping away at the edge.
        let t = (phase + scatter(k)).fract();
        let head = t * t;
        let streak = 1 + (t * MAX_STREAK as f32) as u32;

        for s in 0..streak {
            let r = head - s as f32 * STREAK_GAP;
            if r < 0.04 {
                continue;
            }
            let level = match (s, t) {
                (0, t) if t > 0.40 => Level::Bright,
                (0, _) | (1, _) => Level::Dim,
                _ => Level::Faint,
            };
            let ch = if s == 0 && t > 0.55 { '*' } else { '·' };
            f.put(cx + r * rx * cos_a, cy + r * ry * sin_a, ch, level);
        }
    }

    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(width: u16, height: u16, step: u32) -> Vec<String> {
        field(width, height, step)
            .rows()
            .map(|row| row.iter().map(|c| c.ch).collect())
            .collect()
    }

    fn lit(width: u16, height: u16, step: u32) -> usize {
        field(width, height, step)
            .rows()
            .map(|row| row.iter().filter(|c| c.ch != ' ').count())
            .sum()
    }

    #[test]
    fn frame_matches_requested_geometry() {
        let rows = ascii(28, 6, 0);
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().all(|r| r.chars().count() == 28));
    }

    #[test]
    fn animation_loops_exactly() {
        for step in 0..FRAMES {
            assert_eq!(
                ascii(28, 6, step),
                ascii(28, 6, step + FRAMES),
                "step {step} must repeat one full loop later"
            );
        }
    }

    #[test]
    fn successive_steps_differ() {
        // A frame counter that produced a still image would burn redraws for
        // nothing, so prove the field actually moves.
        let moved = (0..FRAMES)
            .filter(|s| ascii(28, 6, *s) != ascii(28, 6, s + 1))
            .count();
        assert_eq!(moved, FRAMES as usize);
    }

    #[test]
    fn field_is_populated_but_not_solid() {
        for step in 0..FRAMES {
            let n = lit(28, 6, step);
            assert!(n > 8, "step {step} drew only {n} cells");
            assert!(
                n < 28 * 6 / 2,
                "step {step} drew {n} cells, too dense to read"
            );
        }
    }

    #[test]
    fn degenerate_geometry_is_safe() {
        assert_eq!(ascii(0, 6, 3).len(), 0, "zero width yields no rows at all");
        assert_eq!(ascii(28, 0, 3).len(), 0);
        // A 1x1 field is all centre; whatever lands there, it must be one
        // row of one cell and must not panic.
        let tiny = ascii(1, 1, 7);
        assert_eq!(tiny.len(), 1);
        assert_eq!(tiny[0].chars().count(), 1);
    }

    #[test]
    fn oversized_geometry_is_cropped_not_panicking() {
        let rows = ascii(MAX_COLS as u16 + 40, MAX_ROWS as u16 + 40, 11);
        assert_eq!(rows.len(), MAX_ROWS);
        assert!(rows.iter().all(|r| r.chars().count() == MAX_COLS));
    }

    #[test]
    fn narrow_sidebar_still_animates() {
        // 20 columns is below every default sidebar width, and the slot must
        // still hold a readable field there.
        for step in 0..FRAMES {
            assert!(lit(20, 4, step) > 4);
        }
    }
}

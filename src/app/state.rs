use crate::config::{
    Keybinds, NewTerminalCwdConfig, SoundConfig, TabBarPositionConfig, ToastConfig, ToastDelivery,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::detect::AgentState;
use crate::layout::{PaneId, PaneInfo, SplitBorder};
use crate::selection::Selection;

pub(crate) type InstalledPluginRegistry =
    std::collections::HashMap<String, crate::api::schema::InstalledPluginInfo>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPaneRecord {
    pub plugin_id: String,
    pub entrypoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneGraphicsLayer {
    pub format: crate::api::schema::PaneGraphicsFormat,
    pub image_width: u32,
    pub image_height: u32,
    pub data: Vec<u8>,
    pub data_fingerprint: u64,
    pub render: crate::api::schema::PaneGraphicsPlacementParams,
}

impl PaneGraphicsLayer {
    pub(crate) fn new(
        format: crate::api::schema::PaneGraphicsFormat,
        image_width: u32,
        image_height: u32,
        data: Vec<u8>,
        render: crate::api::schema::PaneGraphicsPlacementParams,
    ) -> Self {
        let data_fingerprint = pane_graphics_data_fingerprint(&data);
        Self {
            format,
            image_width,
            image_height,
            data,
            data_fingerprint,
            render,
        }
    }
}

fn pane_graphics_data_fingerprint(data: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PopupPaneState {
    pub pane_id: PaneId,
    pub terminal_id: crate::terminal::TerminalId,
    pub width: Option<crate::popup_size::PopupSize>,
    pub height: Option<crate::popup_size::PopupSize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockEditorSession {
    pub pane_id: PaneId,
    pub terminal_id: crate::terminal::TerminalId,
}

// ---------------------------------------------------------------------------
// Selection autoscroll types
// ---------------------------------------------------------------------------

/// Direction of automatic scrolling during text selection drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionAutoscrollDirection {
    Up,
    Down,
}

/// State for automatic scrolling during text selection drag.
///
/// When the cursor hovers in the 1-row hot zone at the top or bottom edge
/// of a pane (or outside the pane), this struct captures the direction and
/// last known mouse position so a recurring 30ms tick can continue scrolling
/// and extending the selection even when the mouse is not moving.
#[derive(Clone, Debug)]
pub(crate) struct SelectionAutoscroll {
    pub direction: SelectionAutoscrollDirection,
    pub last_mouse_screen_col: u16,
    pub last_mouse_screen_row: u16,
    pub inner_rect: Rect,
}

#[derive(Clone)]
pub(crate) struct RightClickPassthroughGesture {
    pub pane_info: PaneInfo,
    pub modifiers: KeyModifiers,
}
use crate::terminal_theme::{HostAppearance, TerminalTheme};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Theme palette — all UI colors in one place, ready for theming
// ---------------------------------------------------------------------------

/// Group-header status colours (F12-3): the glyph tint for a Linear ticket
/// state, a pull request state, or a Missive conversation state.
///
/// Every entry is an override. `None` follows the theme tone named on the
/// accessor, so a theme that retints `yellow` retints "in progress" with it
/// and only a user who wants a different status vocabulary has to say so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkStatusColors {
    /// Backlog, todo, draft, closed, cancelled, unassigned.
    pub neutral: Option<Color>,
    /// In progress.
    pub active: Option<Color>,
    /// In review.
    pub review: Option<Color>,
    /// Done, closed conversation.
    pub done: Option<Color>,
    /// Triage.
    pub triage: Option<Color>,
    /// Merged pull request.
    pub merged: Option<Color>,
    /// Open pull request, open conversation.
    pub open: Option<Color>,
}

impl WorkStatusColors {
    /// Every entry unset: the built-in themes tint status glyphs from their
    /// own tokens rather than carrying a second copy of them.
    pub const DEFAULT: Self = Self {
        neutral: None,
        active: None,
        review: None,
        done: None,
        triage: None,
        merged: None,
        open: None,
    };
}

/// All colors used by the UI. Derived from a base accent color for now,
/// but structured so a full theme system can replace it later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // all fields defined for theming — some used later
pub struct Palette {
    /// Primary accent (highlight, active borders).
    pub accent: Color,
    /// Background for the tab bar, floating panels, overlays, and modals.
    pub panel_bg: Color,
    /// Desktop sidebar background. `None` follows `panel_bg`; an explicit
    /// `Some(Color::Reset)` hands the sidebar back to the terminal.
    pub sidebar_bg: Option<Color>,
    /// Subtle surface background for selected/focused items.
    pub surface0: Color,
    /// Slightly lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text (secondary info, numbers).
    pub overlay0: Color,
    /// Slightly brighter overlay text.
    pub overlay1: Color,
    /// Main text color — soft white.
    pub text: Color,
    /// Subdued text (workspace numbers, dim labels).
    pub subtext0: Color,
    /// Branch name / special label color.
    pub mauve: Color,
    /// Done / idle states.
    pub green: Color,
    /// Working / running states.
    pub yellow: Color,
    /// Needs attention / blocked states.
    pub red: Color,
    /// Unseen / done notification accent.
    pub blue: Color,
    /// Notification accent / unseen markers.
    pub teal: Color,
    /// Interrupted / warning states.
    pub peach: Color,
    /// Group-header status glyph colours. Unset entries follow the theme.
    pub work_status: WorkStatusColors,
}

/// Resolve a ratatui color to concrete channels for legibility decisions.
///
/// `Rgb` is exact. Named and indexed colors resolve through the xterm default
/// palette: the real terminal may have retinted them, but herdr has to choose
/// a readable foreground with the information it has, and the xterm defaults
/// are a far better estimate than treating the color as unknowable. `Reset`
/// stays `None` — it means "whatever the terminal uses", which is genuinely
/// not knowable here.
pub fn color_rgb(color: Color) -> Option<crate::terminal_theme::RgbColor> {
    use crate::terminal_theme::RgbColor;
    let rgb = |r: u8, g: u8, b: u8| Some(RgbColor { r, g, b });
    match color {
        Color::Rgb(r, g, b) => rgb(r, g, b),
        Color::Reset => None,
        Color::Black => rgb(0, 0, 0),
        Color::Red => rgb(128, 0, 0),
        Color::Green => rgb(0, 128, 0),
        Color::Yellow => rgb(128, 128, 0),
        Color::Blue => rgb(0, 0, 128),
        Color::Magenta => rgb(128, 0, 128),
        Color::Cyan => rgb(0, 128, 128),
        Color::Gray => rgb(192, 192, 192),
        Color::DarkGray => rgb(128, 128, 128),
        Color::LightRed => rgb(255, 0, 0),
        Color::LightGreen => rgb(0, 255, 0),
        Color::LightYellow => rgb(255, 255, 0),
        Color::LightBlue => rgb(0, 0, 255),
        Color::LightMagenta => rgb(255, 0, 255),
        Color::LightCyan => rgb(0, 255, 255),
        Color::White => rgb(255, 255, 255),
        Color::Indexed(index) => match index {
            0..=15 => color_rgb(match index {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                _ => Color::White,
            }),
            16..=231 => {
                // 6x6x6 color cube, xterm's non-linear level steps.
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                let index = index - 16;
                rgb(level(index / 36), level((index % 36) / 6), level(index % 6))
            }
            _ => {
                let value = 8 + (index - 232) * 10;
                rgb(value, value, value)
            }
        },
    }
}

impl Palette {
    /// Catppuccin Mocha — the default.
    pub fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),
            sidebar_bg: None,
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),
            surface_dim: Color::Rgb(30, 30, 46),
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Catppuccin Latte — the light Catppuccin flavor.
    pub fn catppuccin_latte() -> Self {
        Self {
            accent: Color::Rgb(30, 102, 245),
            panel_bg: Color::Rgb(239, 241, 245),
            sidebar_bg: None,
            surface0: Color::Rgb(204, 208, 218),
            surface1: Color::Rgb(188, 192, 204),
            surface_dim: Color::Rgb(230, 233, 239),
            overlay0: Color::Rgb(156, 160, 176),
            overlay1: Color::Rgb(140, 143, 161),
            text: Color::Rgb(76, 79, 105),
            subtext0: Color::Rgb(108, 111, 133),
            mauve: Color::Rgb(136, 57, 239),
            green: Color::Rgb(64, 160, 43),
            yellow: Color::Rgb(223, 142, 29),
            red: Color::Rgb(210, 15, 57),
            blue: Color::Rgb(30, 102, 245),
            teal: Color::Rgb(23, 146, 153),
            peach: Color::Rgb(254, 100, 11),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Terminal 16-color theme.
    pub fn terminal() -> Self {
        Self {
            accent: Color::Blue,
            panel_bg: Color::Reset,
            sidebar_bg: None,
            surface0: Color::Reset,
            surface1: Color::DarkGray,
            surface_dim: Color::DarkGray,
            overlay0: Color::Gray,
            overlay1: Color::White,
            text: Color::Reset,
            subtext0: Color::Gray,
            mauve: Color::Gray,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::LightRed,
            blue: Color::Blue,
            teal: Color::Cyan,
            peach: Color::Yellow,
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Tokyo Night — blue-purple aesthetic.
    pub fn tokyo_night() -> Self {
        Self {
            accent: Color::Rgb(122, 162, 247), // blue
            panel_bg: Color::Rgb(26, 27, 38),
            sidebar_bg: None,
            surface0: Color::Rgb(36, 40, 59),
            surface1: Color::Rgb(65, 72, 104),
            surface_dim: Color::Rgb(26, 27, 38),
            overlay0: Color::Rgb(86, 95, 137),
            overlay1: Color::Rgb(105, 113, 150),
            text: Color::Rgb(192, 202, 245),
            subtext0: Color::Rgb(169, 177, 214),
            mauve: Color::Rgb(187, 154, 247),
            green: Color::Rgb(158, 206, 106),
            yellow: Color::Rgb(224, 175, 104),
            red: Color::Rgb(247, 118, 142),
            blue: Color::Rgb(122, 162, 247),
            teal: Color::Rgb(125, 207, 255),
            peach: Color::Rgb(255, 158, 100),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Tokyo Night Day — the light Tokyo Night style.
    pub fn tokyo_night_day() -> Self {
        Self {
            accent: Color::Rgb(46, 125, 233),
            panel_bg: Color::Rgb(225, 226, 231),
            sidebar_bg: None,
            surface0: Color::Rgb(196, 200, 218),
            surface1: Color::Rgb(168, 174, 203),
            surface_dim: Color::Rgb(210, 211, 218),
            overlay0: Color::Rgb(137, 144, 179),
            overlay1: Color::Rgb(104, 112, 154),
            text: Color::Rgb(55, 96, 191),
            subtext0: Color::Rgb(97, 114, 176),
            mauve: Color::Rgb(120, 71, 189),
            green: Color::Rgb(88, 117, 57),
            yellow: Color::Rgb(140, 108, 62),
            red: Color::Rgb(245, 42, 101),
            blue: Color::Rgb(46, 125, 233),
            teal: Color::Rgb(17, 140, 116),
            peach: Color::Rgb(177, 92, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Dracula — purple/pink/green.
    pub fn dracula() -> Self {
        Self {
            accent: Color::Rgb(189, 147, 249), // purple
            panel_bg: Color::Rgb(40, 42, 54),
            sidebar_bg: None,
            surface0: Color::Rgb(68, 71, 90),
            surface1: Color::Rgb(98, 114, 164),
            surface_dim: Color::Rgb(40, 42, 54),
            overlay0: Color::Rgb(98, 114, 164),
            overlay1: Color::Rgb(130, 140, 180),
            text: Color::Rgb(248, 248, 242),
            subtext0: Color::Rgb(210, 210, 220),
            mauve: Color::Rgb(255, 121, 198), // pink
            green: Color::Rgb(80, 250, 123),
            yellow: Color::Rgb(241, 250, 140),
            red: Color::Rgb(255, 85, 85),
            blue: Color::Rgb(139, 233, 253), // cyan-ish
            teal: Color::Rgb(139, 233, 253),
            peach: Color::Rgb(255, 184, 108),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Nord — frosty blue palette.
    pub fn nord() -> Self {
        Self {
            accent: Color::Rgb(136, 192, 208), // frost
            panel_bg: Color::Rgb(46, 52, 64),
            sidebar_bg: None,
            surface0: Color::Rgb(59, 66, 82),
            surface1: Color::Rgb(67, 76, 94),
            surface_dim: Color::Rgb(46, 52, 64),
            // Nord's own nord3 (#4c566a) is a comment color: on nord0 it lands
            // at 1.69:1, dim enough to lose the sidebar's secondary text. Use
            // the brighter muted tone the Nord UI ports settle on instead.
            overlay0: Color::Rgb(123, 136, 161),
            overlay1: Color::Rgb(143, 156, 178),
            text: Color::Rgb(236, 239, 244),
            subtext0: Color::Rgb(216, 222, 233),
            mauve: Color::Rgb(180, 142, 173),
            green: Color::Rgb(163, 190, 140),
            yellow: Color::Rgb(235, 203, 139),
            red: Color::Rgb(191, 97, 106),
            blue: Color::Rgb(129, 161, 193),
            teal: Color::Rgb(143, 188, 187),
            peach: Color::Rgb(208, 135, 112),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Gruvbox Dark — warm retro palette.
    pub fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(215, 153, 33), // yellow
            panel_bg: Color::Rgb(40, 40, 40),
            sidebar_bg: None,
            surface0: Color::Rgb(60, 56, 54),
            surface1: Color::Rgb(80, 73, 69),
            surface_dim: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(168, 153, 132),
            text: Color::Rgb(235, 219, 178),
            subtext0: Color::Rgb(213, 196, 161),
            mauve: Color::Rgb(211, 134, 155),
            green: Color::Rgb(184, 187, 38),
            yellow: Color::Rgb(250, 189, 47),
            red: Color::Rgb(251, 73, 52),
            blue: Color::Rgb(131, 165, 152),
            teal: Color::Rgb(142, 192, 124),
            peach: Color::Rgb(254, 128, 25),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Gruvbox Light — the light retro palette.
    pub fn gruvbox_light() -> Self {
        Self {
            accent: Color::Rgb(7, 102, 120),
            panel_bg: Color::Rgb(251, 241, 199),
            sidebar_bg: None,
            surface0: Color::Rgb(235, 219, 178),
            surface1: Color::Rgb(213, 196, 161),
            surface_dim: Color::Rgb(242, 229, 188),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(124, 111, 100),
            text: Color::Rgb(60, 56, 54),
            subtext0: Color::Rgb(80, 73, 69),
            mauve: Color::Rgb(143, 63, 113),
            green: Color::Rgb(121, 116, 14),
            yellow: Color::Rgb(181, 118, 20),
            red: Color::Rgb(157, 0, 6),
            blue: Color::Rgb(7, 102, 120),
            teal: Color::Rgb(66, 123, 88),
            peach: Color::Rgb(175, 58, 3),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// One Dark — Atom's classic dark theme.
    pub fn one_dark() -> Self {
        Self {
            accent: Color::Rgb(97, 175, 239), // blue
            panel_bg: Color::Rgb(40, 44, 52),
            sidebar_bg: None,
            surface0: Color::Rgb(44, 49, 58),
            surface1: Color::Rgb(62, 68, 81),
            surface_dim: Color::Rgb(40, 44, 52),
            overlay0: Color::Rgb(92, 99, 112),
            overlay1: Color::Rgb(115, 122, 135),
            text: Color::Rgb(171, 178, 191),
            subtext0: Color::Rgb(150, 156, 168),
            mauve: Color::Rgb(198, 120, 221),
            green: Color::Rgb(152, 195, 121),
            yellow: Color::Rgb(229, 192, 123),
            red: Color::Rgb(224, 108, 117),
            blue: Color::Rgb(97, 175, 239),
            teal: Color::Rgb(86, 182, 194),
            peach: Color::Rgb(209, 154, 102),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// One Light — Atom's classic light theme.
    pub fn one_light() -> Self {
        Self {
            accent: Color::Rgb(64, 120, 242),
            panel_bg: Color::Rgb(250, 250, 250),
            sidebar_bg: None,
            surface0: Color::Rgb(240, 240, 241),
            surface1: Color::Rgb(229, 229, 230),
            surface_dim: Color::Rgb(245, 245, 246),
            overlay0: Color::Rgb(160, 161, 167),
            overlay1: Color::Rgb(104, 107, 119),
            text: Color::Rgb(56, 58, 66),
            subtext0: Color::Rgb(104, 107, 119),
            mauve: Color::Rgb(166, 38, 164),
            green: Color::Rgb(80, 161, 79),
            yellow: Color::Rgb(193, 132, 1),
            red: Color::Rgb(228, 86, 73),
            blue: Color::Rgb(64, 120, 242),
            teal: Color::Rgb(1, 132, 188),
            peach: Color::Rgb(152, 104, 1),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// GitHub Dark High Contrast — Primer's accessible dark palette.
    pub fn github_dark_high_contrast() -> Self {
        Self {
            accent: Color::Rgb(113, 183, 255),
            panel_bg: Color::Rgb(10, 12, 16),
            sidebar_bg: None,
            surface0: Color::Rgb(39, 43, 51),
            surface1: Color::Rgb(82, 89, 100),
            surface_dim: Color::Rgb(1, 4, 9),
            overlay0: Color::Rgb(122, 130, 142),
            overlay1: Color::Rgb(158, 167, 179),
            text: Color::Rgb(240, 243, 246),
            subtext0: Color::Rgb(189, 196, 204),
            mauve: Color::Rgb(203, 158, 255),
            green: Color::Rgb(38, 205, 77),
            yellow: Color::Rgb(240, 183, 47),
            red: Color::Rgb(255, 148, 146),
            blue: Color::Rgb(113, 183, 255),
            teal: Color::Rgb(57, 197, 207),
            peach: Color::Rgb(255, 183, 87),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// GitHub Light High Contrast — Primer's accessible light palette.
    pub fn github_light_high_contrast() -> Self {
        Self {
            accent: Color::Rgb(3, 73, 180),
            panel_bg: Color::Rgb(255, 255, 255),
            sidebar_bg: None,
            surface0: Color::Rgb(231, 236, 240),
            surface1: Color::Rgb(172, 182, 192),
            surface_dim: Color::Rgb(231, 236, 240),
            overlay0: Color::Rgb(136, 146, 157),
            overlay1: Color::Rgb(102, 112, 123),
            text: Color::Rgb(14, 17, 22),
            subtext0: Color::Rgb(75, 83, 93),
            mauve: Color::Rgb(98, 44, 188),
            green: Color::Rgb(5, 93, 32),
            yellow: Color::Rgb(116, 69, 0),
            red: Color::Rgb(160, 17, 31),
            blue: Color::Rgb(3, 73, 180),
            teal: Color::Rgb(27, 124, 131),
            peach: Color::Rgb(112, 44, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Solarized Dark — Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210), // blue
            panel_bg: Color::Rgb(0, 43, 54),
            sidebar_bg: None,
            surface0: Color::Rgb(7, 54, 66),
            surface1: Color::Rgb(88, 110, 117),
            surface_dim: Color::Rgb(0, 43, 54),
            overlay0: Color::Rgb(88, 110, 117),
            overlay1: Color::Rgb(101, 123, 131),
            text: Color::Rgb(147, 161, 161),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Solarized Light — Ethan Schoonover's light variant.
    pub fn solarized_light() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210),
            panel_bg: Color::Rgb(253, 246, 227),
            sidebar_bg: None,
            surface0: Color::Rgb(238, 232, 213),
            surface1: Color::Rgb(147, 161, 161),
            surface_dim: Color::Rgb(238, 232, 213),
            overlay0: Color::Rgb(147, 161, 161),
            overlay1: Color::Rgb(88, 110, 117),
            text: Color::Rgb(101, 123, 131),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Kanagawa — inspired by Katsushika Hokusai.
    pub fn kanagawa() -> Self {
        Self {
            accent: Color::Rgb(126, 156, 216), // blue
            panel_bg: Color::Rgb(31, 31, 40),
            sidebar_bg: None,
            surface0: Color::Rgb(42, 42, 55),
            surface1: Color::Rgb(54, 54, 70),
            surface_dim: Color::Rgb(31, 31, 40),
            overlay0: Color::Rgb(114, 113, 105),
            overlay1: Color::Rgb(135, 134, 125),
            text: Color::Rgb(220, 215, 186),
            subtext0: Color::Rgb(200, 195, 170),
            mauve: Color::Rgb(149, 127, 184),
            green: Color::Rgb(118, 148, 106),
            yellow: Color::Rgb(192, 163, 110),
            red: Color::Rgb(195, 64, 67),
            blue: Color::Rgb(126, 156, 216),
            teal: Color::Rgb(127, 180, 202),
            peach: Color::Rgb(255, 160, 102),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Kanagawa Lotus — the light Kanagawa variant.
    pub fn kanagawa_lotus() -> Self {
        Self {
            accent: Color::Rgb(77, 105, 155),
            panel_bg: Color::Rgb(242, 236, 188),
            sidebar_bg: None,
            surface0: Color::Rgb(220, 213, 172),
            surface1: Color::Rgb(201, 203, 209),
            surface_dim: Color::Rgb(213, 206, 163),
            overlay0: Color::Rgb(160, 156, 172),
            overlay1: Color::Rgb(138, 137, 128),
            text: Color::Rgb(84, 84, 100),
            subtext0: Color::Rgb(67, 67, 108),
            mauve: Color::Rgb(98, 76, 131),
            green: Color::Rgb(111, 137, 78),
            yellow: Color::Rgb(119, 113, 63),
            red: Color::Rgb(200, 64, 83),
            blue: Color::Rgb(77, 105, 155),
            teal: Color::Rgb(78, 140, 162),
            peach: Color::Rgb(204, 109, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Rosé Pine — muted, elegant.
    pub fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(196, 167, 231), // iris
            panel_bg: Color::Rgb(25, 23, 36),
            sidebar_bg: None,
            surface0: Color::Rgb(31, 29, 46),
            surface1: Color::Rgb(38, 35, 58),
            surface_dim: Color::Rgb(38, 35, 58),
            overlay0: Color::Rgb(110, 106, 134),
            overlay1: Color::Rgb(144, 140, 170),
            text: Color::Rgb(224, 222, 244),
            subtext0: Color::Rgb(200, 197, 220),
            mauve: Color::Rgb(196, 167, 231),  // iris
            green: Color::Rgb(49, 116, 143),   // pine
            yellow: Color::Rgb(246, 193, 119), // gold
            red: Color::Rgb(235, 111, 146),    // love
            blue: Color::Rgb(49, 116, 143),    // pine
            teal: Color::Rgb(156, 207, 216),   // foam
            peach: Color::Rgb(234, 154, 151),  // rose
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Rosé Pine Dawn — the light Rosé Pine variant.
    pub fn rose_pine_dawn() -> Self {
        Self {
            accent: Color::Rgb(144, 122, 169),
            panel_bg: Color::Rgb(250, 244, 237),
            sidebar_bg: None,
            surface0: Color::Rgb(242, 233, 225),
            surface1: Color::Rgb(255, 250, 243),
            surface_dim: Color::Rgb(242, 233, 225),
            overlay0: Color::Rgb(152, 147, 165),
            overlay1: Color::Rgb(121, 117, 147),
            text: Color::Rgb(70, 66, 97),
            subtext0: Color::Rgb(121, 117, 147),
            mauve: Color::Rgb(144, 122, 169),
            green: Color::Rgb(40, 105, 131),
            yellow: Color::Rgb(234, 157, 52),
            red: Color::Rgb(180, 99, 122),
            blue: Color::Rgb(40, 105, 131),
            teal: Color::Rgb(86, 148, 159),
            peach: Color::Rgb(215, 130, 126),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Vesper — minimal high-contrast monochrome with peach and mint accents.
    pub fn vesper() -> Self {
        Self {
            accent: Color::Rgb(255, 199, 153),
            panel_bg: Color::Rgb(26, 26, 26),
            sidebar_bg: None,
            surface0: Color::Rgb(35, 35, 35),
            surface1: Color::Rgb(40, 40, 40),
            surface_dim: Color::Rgb(16, 16, 16),
            overlay0: Color::Rgb(92, 92, 92),
            overlay1: Color::Rgb(126, 126, 126),
            text: Color::Rgb(255, 255, 255),
            subtext0: Color::Rgb(160, 160, 160),
            mauve: Color::Rgb(255, 209, 168),
            green: Color::Rgb(153, 255, 228),
            yellow: Color::Rgb(255, 199, 153),
            red: Color::Rgb(255, 128, 128),
            blue: Color::Rgb(176, 176, 176),
            teal: Color::Rgb(102, 221, 204),
            peach: Color::Rgb(255, 199, 153),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Background the desktop sidebar paints under its rows.
    ///
    /// An unset `sidebar_bg` follows the palette's own panel background rather
    /// than the terminal's. Borrowing the terminal background is what makes a
    /// mismatched palette unreadable: theme foregrounds land on a background
    /// chosen by something else. The `terminal` palette keeps `panel_bg` at
    /// `Reset`, so it still inherits, and so does an explicit
    /// `theme.custom.sidebar_bg = "reset"`.
    pub fn sidebar_background(&self) -> Color {
        self.sidebar_bg.unwrap_or(self.panel_bg)
    }

    /// Appearance this palette was built for, inferred from `panel_bg`.
    ///
    /// `None` when the palette leaves its panel background to the terminal
    /// (the `terminal` theme), where no appearance can be claimed.
    pub fn appearance(&self) -> Option<crate::terminal_theme::HostAppearance> {
        Some(color_rgb(self.panel_bg)?.inferred_appearance())
    }

    /// Resolve a theme by name. Returns None for unknown names.
    /// Status glyph colour for backlog, todo, draft, closed, cancelled and
    /// unassigned work. Defaults to the muted chrome tone (gray).
    pub fn work_status_neutral(&self) -> Color {
        self.work_status.neutral.unwrap_or(self.overlay0)
    }

    /// Status glyph colour for work in progress. Defaults to yellow.
    pub fn work_status_active(&self) -> Color {
        self.work_status.active.unwrap_or(self.yellow)
    }

    /// Status glyph colour for work in review. Defaults to green.
    pub fn work_status_review(&self) -> Color {
        self.work_status.review.unwrap_or(self.green)
    }

    /// Status glyph colour for finished work. Defaults to blue.
    pub fn work_status_done(&self) -> Color {
        self.work_status.done.unwrap_or(self.blue)
    }

    /// Status glyph colour for triage. Defaults to orange.
    pub fn work_status_triage(&self) -> Color {
        self.work_status.triage.unwrap_or(self.peach)
    }

    /// Status glyph colour for a merged pull request. Defaults to purple.
    pub fn work_status_merged(&self) -> Color {
        self.work_status.merged.unwrap_or(self.mauve)
    }

    /// Status glyph colour for an open pull request or conversation.
    /// Defaults to green.
    pub fn work_status_open(&self) -> Color {
        self.work_status.open.unwrap_or(self.green)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().replace([' ', '_'], "-").as_str() {
            "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin()),
            "catppuccin-latte" | "latte" | "light" => Some(Self::catppuccin_latte()),
            "terminal" => Some(Self::terminal()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some(Self::tokyo_night_day()),
            "dracula" => Some(Self::dracula()),
            "nord" => Some(Self::nord()),
            "gruvbox" | "gruvbox-dark" => Some(Self::gruvbox()),
            "gruvbox-light" => Some(Self::gruvbox_light()),
            "one-dark" | "onedark" => Some(Self::one_dark()),
            "one-light" | "onelight" => Some(Self::one_light()),
            "github-dark-high-contrast" | "github-dark-hc" => {
                Some(Self::github_dark_high_contrast())
            }
            "github-light-high-contrast" | "github-light-hc" => {
                Some(Self::github_light_high_contrast())
            }
            "solarized" | "solarized-dark" => Some(Self::solarized()),
            "solarized-light" => Some(Self::solarized_light()),
            "kanagawa" => Some(Self::kanagawa()),
            "kanagawa-lotus" | "lotus" => Some(Self::kanagawa_lotus()),
            "rose-pine" | "rosepine" => Some(Self::rose_pine()),
            "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some(Self::rose_pine_dawn()),
            "vesper" => Some(Self::vesper()),
            _ => None,
        }
    }

    /// Apply custom color overrides on top of this palette.
    pub fn with_overrides(mut self, custom: &crate::config::CustomThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.sidebar_bg {
            // An explicit `reset` is a choice, not an absence: it hands the
            // sidebar back to the terminal background.
            self.sidebar_bg = Some(parse_color(c));
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        if let Some(c) = &custom.work_status_neutral {
            self.work_status.neutral = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_active {
            self.work_status.active = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_review {
            self.work_status.review = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_done {
            self.work_status.done = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_triage {
            self.work_status.triage = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_merged {
            self.work_status.merged = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_open {
            self.work_status.open = Some(parse_color(c));
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCardArea {
    pub ws_idx: usize,
    pub rect: Rect,
    pub indented: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCardArea {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: PaneId,
    pub rect: Rect,
    /// Index into the sidebar rows this frame. A blocked pane owns two rows --
    /// one in its group, one in the tree -- so the ids alone cannot say which
    /// of the two a card came from.
    pub row_idx: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabCardArea {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: PaneId,
    pub depth: u16,
    pub rect: Rect,
}

/// Per-view narrowing for work-item projections. This remains TUI-only state:
/// provider observations are shared runtime facts, while each attached client
/// chooses its own filters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct SidebarWorkFilter {
    /// Legacy field names preserve existing 2b presentation files.
    pub(crate) team: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) linear_statuses: std::collections::BTreeSet<LinearStatusFilter>,
    pub(crate) github: GithubSidebarFilter,
    pub(crate) missive: MissiveSidebarFilter,
}

impl SidebarWorkFilter {
    pub(crate) fn linear_label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.team.as_deref().unwrap_or("all teams"),
            assignee_filter_label(self.assignee.as_deref()),
            if self.linear_statuses == default_linear_statuses() {
                "active".into()
            } else {
                format!("{} statuses", self.linear_statuses.len())
            }
        )
    }

    pub(crate) fn github_label(&self) -> String {
        format!(
            "{} · drafts {} · {}",
            assignee_filter_label(self.github.assignee.as_deref()),
            if self.github.show_drafts {
                "shown"
            } else {
                "hidden"
            },
            self.github.state.label(),
        )
    }

    pub(crate) fn missive_label(&self) -> String {
        format!(
            "{} · closed {}",
            assignee_filter_label(self.missive.assignee.as_deref()),
            if self.missive.show_closed {
                "shown"
            } else {
                "hidden"
            },
        )
    }

    pub(crate) fn matches_linear(
        &self,
        ticket: &crate::work_index::WorkTicket,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        if let Some(team) = self.team.as_deref() {
            let actual_team = ticket.identifier.split_once('-').map(|(team, _)| team);
            if actual_team != Some(team) {
                return false;
            }
        }
        if !assignee_filter_matches(
            self.assignee.as_deref(),
            ticket.assignee.as_deref(),
            session.linear.viewer.as_deref(),
        ) {
            return false;
        }
        ticket
            .state
            .as_deref()
            .and_then(LinearStatusFilter::from_name)
            .is_none_or(|status| self.linear_statuses.contains(&status))
    }

    pub(crate) fn matches_github(
        &self,
        item: &crate::work_index::WorkItem,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        if !item.source.github {
            return true;
        }
        if item.draft && !self.github.show_drafts {
            return false;
        }
        if item
            .pr_state
            .as_deref()
            .is_some_and(|state| !state.eq_ignore_ascii_case(self.github.state.label()))
        {
            return false;
        }
        let selected = self.github.assignee.as_deref();
        if selected.is_none() {
            return true;
        }
        let resolved = if selected == Some("me") {
            let Some(viewer) = session.github.viewer.as_deref() else {
                return true;
            };
            viewer
        } else {
            selected.unwrap_or_default()
        };
        item.assignees
            .iter()
            .any(|assignee| assignee.eq_ignore_ascii_case(resolved))
    }

    pub(crate) fn matches_missive_conversation(
        &self,
        conversation: Option<&crate::work_index::MissiveConversation>,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        let Some(conversation) = conversation else {
            return true;
        };
        if conversation.closed && !self.missive.show_closed {
            return false;
        }
        let Some(selected) = self.missive.assignee.as_deref() else {
            return true;
        };
        let resolved = if selected == "me" {
            let Some(viewer) = session.missive.viewer.as_deref() else {
                return true;
            };
            viewer
        } else {
            selected
        };
        conversation
            .assignees
            .iter()
            .any(|user| user.name.eq_ignore_ascii_case(resolved))
    }
}

impl Default for SidebarWorkFilter {
    fn default() -> Self {
        Self {
            team: Some("SCA".into()),
            assignee: Some("me".into()),
            linear_statuses: default_linear_statuses(),
            github: GithubSidebarFilter::default(),
            missive: MissiveSidebarFilter::default(),
        }
    }
}

fn assignee_filter_label(value: Option<&str>) -> &str {
    value.unwrap_or("all")
}

fn assignee_filter_matches(
    selected: Option<&str>,
    actual: Option<&str>,
    viewer: Option<&str>,
) -> bool {
    let Some(selected) = selected else {
        return true;
    };
    let selected = if selected == "me" {
        let Some(viewer) = viewer else {
            return true;
        };
        viewer
    } else {
        selected
    };
    actual.is_some_and(|actual| actual.eq_ignore_ascii_case(selected))
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinearStatusFilter {
    Draft,
    Backlog,
    Ready,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
    Duplicate,
    Triage,
}

impl LinearStatusFilter {
    pub(crate) const ALL: [Self; 10] = [
        Self::Draft,
        Self::Backlog,
        Self::Ready,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
        Self::Canceled,
        Self::Duplicate,
        Self::Triage,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Backlog => "Backlog",
            Self::Ready => "Ready",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
            Self::Canceled => "Canceled",
            Self::Duplicate => "Duplicate",
            Self::Triage => "Triage",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.label().eq_ignore_ascii_case(value))
    }
}

fn default_linear_statuses() -> std::collections::BTreeSet<LinearStatusFilter> {
    LinearStatusFilter::ALL
        .into_iter()
        .filter(|status| {
            !matches!(
                status,
                LinearStatusFilter::Canceled | LinearStatusFilter::Duplicate
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct GithubSidebarFilter {
    pub(crate) assignee: Option<String>,
    pub(crate) show_drafts: bool,
    pub(crate) state: GithubStateFilter,
}

impl Default for GithubSidebarFilter {
    fn default() -> Self {
        Self {
            assignee: Some("me".into()),
            show_drafts: false,
            state: GithubStateFilter::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GithubStateFilter {
    #[default]
    Open,
    Merged,
    Closed,
}

impl GithubStateFilter {
    pub(crate) const ALL: [Self; 3] = [Self::Open, Self::Merged, Self::Closed];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct MissiveSidebarFilter {
    pub(crate) assignee: Option<String>,
    pub(crate) show_closed: bool,
}

impl Default for MissiveSidebarFilter {
    fn default() -> Self {
        Self {
            assignee: Some("me".into()),
            show_closed: false,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarGroupMode {
    #[default]
    Repo,
    RepoPr,
    RepoWorktree,
    LinearTeam,
    Missive,
}

impl SidebarGroupMode {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::Repo,
        Self::RepoPr,
        Self::RepoWorktree,
        Self::LinearTeam,
        Self::Missive,
    ];

    pub(crate) const VIEWS: [Self; 4] = [Self::Repo, Self::LinearTeam, Self::RepoPr, Self::Missive];

    pub(crate) fn view_label(self) -> &'static str {
        match self {
            Self::Repo | Self::RepoWorktree => "Repo",
            Self::LinearTeam => "Linear",
            Self::RepoPr => "GitHub",
            Self::Missive => "Missive",
        }
    }

    pub(crate) fn next(self) -> Self {
        let index = self.view_index();
        Self::VIEWS[(index + 1) % Self::VIEWS.len()]
    }

    pub(crate) fn view_index(self) -> usize {
        Self::VIEWS
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or(0)
    }

    pub(crate) fn collapse_namespace(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::RepoPr => "repo_pr",
            Self::RepoWorktree => "repo_worktree",
            Self::LinearTeam => "linear_team",
            Self::Missive => "missive",
        }
    }
}

/// Attach-local sidebar state. The headless server swaps one instance into
/// `AppState` while routing input or rendering for that client; the monolithic
/// app keeps its own instance directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarPresentationState {
    pub(crate) expanded_workspace_ids: std::collections::HashSet<String>,
    pub(crate) known_workspace_ids: std::collections::HashSet<String>,
    /// Workspace this attach last scrolled into view. Focus changes routed
    /// through the JSON API/CLI run outside the per-client presentation swap,
    /// so each attach compares this against the active workspace when it
    /// renders and scrolls itself instead of relying on the mutating path.
    pub(crate) revealed_workspace_id: Option<String>,
    pub(crate) workspace_scroll: usize,
    pub(crate) mobile_switcher_scroll: usize,
    /// Global projection revision last reconciled into this attach.
    pub(crate) projection_revision: u64,
    pub(crate) group_mode: SidebarGroupMode,
    pub(crate) group_menu_open: bool,
    pub(crate) group_menu_selected: usize,
    pub(crate) work_filter: SidebarWorkFilter,
    pub(crate) filter_menu_open: bool,
    pub(crate) filter_menu_selected: usize,
    pub(crate) selected_work_group: Option<String>,
    pub(crate) unassigned_expanded_views: std::collections::HashSet<SidebarGroupMode>,
    pub(crate) selected_settled: Option<PaneFocusTarget>,
    pub(crate) settled_menu_target: Option<PaneFocusTarget>,
    pub(crate) settled_menu_selected: usize,
}

/// Attach-local dock presentation. The headless server swaps one instance into
/// `AppState` while routing input or rendering that client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockPresentationState {
    pub(crate) width: u16,
    pub(crate) collapsed: bool,
    /// An explicit surface pick wins until the next sidebar/work view change.
    pub(crate) surface_override: bool,
    /// Active surface. `None` while the dock is a chooser with nothing open.
    pub(crate) tab: Option<DockSurface>,
    pub(crate) open_surfaces: Vec<DockSurface>,
    pub(crate) maximized: bool,
    pub(crate) surface_menu: Option<DockSurfaceMenu>,
    pub(crate) chooser_focused: bool,
    pub(crate) scroll: u16,
    pub(crate) editor_focused: bool,
    pub(crate) diff_focused: bool,
    pub(crate) diff_ignore_whitespace: bool,
    pub(crate) diff_selected: usize,
    pub(crate) diff_collapsed: std::collections::HashSet<String>,
    pub(crate) diff_request: Option<super::diff::DiffRefreshRequest>,
    pub(crate) diff_active_key: Option<DiffCacheKey>,
    pub(crate) files_focused: bool,
    pub(crate) files_selection: Option<std::path::PathBuf>,
    pub(crate) files_filter: String,
    pub(crate) files_collapsed: std::collections::HashSet<std::path::PathBuf>,
    /// Selection inside the home tab. Stored as a work-item key, never an
    /// index, so it survives snapshot refreshes and list reordering.
    pub(crate) home_selection: Option<WorkItemKey>,
    pub(crate) home_ticket_selection: Option<WorkItemKey>,
    pub(crate) home_poll_selection: Option<WorkItemKey>,
    pub(crate) home_section: DockHomeSection,
    pub(crate) home_detail_tab: DockHomeDetailTab,
    pub(crate) home_focused: bool,
    /// Focus target last considered for automatic PR-tab selection. Keeping
    /// this attach-local lets an explicit selection survive while pane focus
    /// remains unchanged.
    pub(crate) home_followed_pane: Option<PaneFocusTarget>,
}

impl Default for DockPresentationState {
    fn default() -> Self {
        Self {
            width: crate::ui::DOCK_DEFAULT_WIDTH,
            collapsed: true,
            surface_override: false,
            tab: Some(DockSurface::Home),
            open_surfaces: DockSurface::DEFAULT_OPEN.to_vec(),
            maximized: false,
            surface_menu: None,
            chooser_focused: false,
            scroll: 0,
            editor_focused: false,
            diff_focused: false,
            diff_ignore_whitespace: false,
            diff_selected: 0,
            diff_collapsed: std::collections::HashSet::new(),
            diff_request: None,
            diff_active_key: None,
            files_focused: false,
            files_selection: None,
            files_filter: String::new(),
            files_collapsed: std::collections::HashSet::new(),
            home_selection: None,
            home_ticket_selection: None,
            home_poll_selection: None,
            home_section: DockHomeSection::Prs,
            home_detail_tab: DockHomeDetailTab::Overview,
            home_focused: false,
            home_followed_pane: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCreateState {
    pub source_workspace_id: String,
    pub source_checkout_path: std::path::PathBuf,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub branch: String,
    pub checkout_path: std::path::PathBuf,
    pub error: Option<String>,
    pub creating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemoveState {
    pub workspace_id: String,
    pub repo_root: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub error: Option<String>,
    pub removing: bool,
    pub force_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenEntry {
    pub path: std::path::PathBuf,
    pub branch: Option<String>,
    pub is_linked_worktree: bool,
    pub already_open_ws_idx: Option<usize>,
}

impl WorktreeOpenEntry {
    pub(crate) fn display_name(&self) -> String {
        self.branch.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| self.path.display().to_string())
        })
    }

    pub(crate) fn status_label(&self) -> &'static str {
        if self.already_open_ws_idx.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.display_name(),
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
            self.path.display(),
            self.status_label()
        )
        .to_lowercase()
    }

    fn matches_query(&self, query: &str) -> bool {
        text_matches_query(query, &self.search_text())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenState {
    pub source_workspace_id: String,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub entries: Vec<WorktreeOpenEntry>,
    pub selected: usize,
    pub query: String,
    pub search_focused: bool,
    pub error: Option<String>,
}

impl WorktreeOpenState {
    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        let query = self.query.trim();
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                (query.is_empty() || entry.matches_query(query)).then_some(idx)
            })
            .collect()
    }

    pub(crate) fn selected_entry_index(&self) -> Option<usize> {
        let indices = self.filtered_indices();
        if indices.contains(&self.selected) {
            Some(self.selected)
        } else {
            indices.first().copied()
        }
    }

    pub(crate) fn normalize_selection(&mut self) {
        if let Some(selected) = self.selected_entry_index() {
            self.selected = selected;
        }
    }

    pub(crate) fn select_previous_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[pos.saturating_sub(1)];
    }

    pub(crate) fn select_next_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[(pos + 1).min(indices.len().saturating_sub(1))];
    }
}

pub(crate) fn text_matches_query(query: &str, text: &str) -> bool {
    let haystack = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|needle| haystack.contains(needle))
}

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewLayout {
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSurface {
    Home,
    Terminal,
    Files,
    Diff,
    Pr,
    Linear,
    Missive,
    Agents,
    Editor,
    Shortcuts,
    Context,
    Scratchpad,
}

impl DockSurface {
    /// Every surface the chooser can open, in menu order.
    pub const ALL: [Self; 12] = [
        Self::Terminal,
        Self::Files,
        Self::Diff,
        Self::Pr,
        Self::Linear,
        Self::Missive,
        Self::Agents,
        Self::Home,
        Self::Editor,
        Self::Shortcuts,
        Self::Context,
        Self::Scratchpad,
    ];

    /// The card grid of the empty dock, each with its single-key shortcut.
    pub const CARDS: [Self; 6] = [
        Self::Terminal,
        Self::Files,
        Self::Diff,
        Self::Pr,
        Self::Linear,
        Self::Agents,
    ];

    /// Surfaces a dock opens with. Keeping the pre-chooser tab strip as the
    /// default set makes the rename behaviour-neutral: the same five tabs are
    /// there, in the same order, until the user closes one.
    pub const DEFAULT_OPEN: [Self; 5] = [
        Self::Home,
        Self::Editor,
        Self::Shortcuts,
        Self::Context,
        Self::Scratchpad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Editor => "edit",
            Self::Shortcuts => "keys",
            Self::Context => "ctx",
            Self::Scratchpad => "note",
            Self::Terminal => "term",
            Self::Files => "files",
            Self::Diff => "diff",
            Self::Pr => "pr",
            Self::Linear => "linear",
            Self::Missive => "missive",
            Self::Agents => "agents",
        }
    }

    /// Title used by the chooser card grid and the `+` menu.
    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Editor => "Editor",
            Self::Shortcuts => "Shortcuts",
            Self::Context => "Context",
            Self::Scratchpad => "Scratchpad",
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::Pr => "PR",
            Self::Linear => "Linear",
            Self::Missive => "Missive",
            Self::Agents => "Agents",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Home => "work index",
            Self::Editor => "edit files",
            Self::Shortcuts => "keybinds",
            Self::Context => "pane facts",
            Self::Scratchpad => "notes",
            Self::Terminal => "shell here",
            Self::Files => "browse",
            Self::Diff => "vs base",
            Self::Pr => "this branch",
            Self::Linear => "ticket",
            Self::Missive => "conversation",
            Self::Agents => "subagents",
        }
    }

    /// Single-key shortcut of the empty-dock card grid.
    pub fn shortcut(self) -> Option<char> {
        match self {
            Self::Terminal => Some('T'),
            Self::Files => Some('F'),
            Self::Diff => Some('D'),
            Self::Pr => Some('P'),
            Self::Linear => Some('L'),
            Self::Agents => Some('A'),
            Self::Missive => None,
            _ => None,
        }
    }

    pub fn from_shortcut(key: char) -> Option<Self> {
        let key = key.to_ascii_uppercase();
        Self::CARDS
            .into_iter()
            .find(|surface| surface.shortcut() == Some(key))
    }

    /// Placeholder body until the surface gets its implementation slice.
    pub fn placeholder(self) -> Option<String> {
        matches!(self, Self::Terminal | Self::Files | Self::Agents)
            .then(|| format!("{}: coming in a later slice", self.title()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DockSurfaceMenu {
    pub(crate) selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DiffCacheKey {
    pub(crate) root: std::path::PathBuf,
    pub(crate) base: String,
    pub(crate) ignore_whitespace: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffFileSummary {
    pub(crate) path: String,
    pub(crate) display_path: String,
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
    pub(crate) binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffFileContent {
    pub(crate) committed: Vec<super::diff::DiffLine>,
    pub(crate) uncommitted: Vec<super::diff::DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffCacheEntry {
    pub(crate) branch: String,
    pub(crate) files: Vec<DiffFileSummary>,
    pub(crate) contents: std::collections::HashMap<String, DiffFileContent>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockFileRowHitArea {
    pub(crate) path: std::path::PathBuf,
    pub(crate) kind: crate::files::FileTreeRowKind,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeHitTarget {
    QueueRow(usize),
    NewTask,
    Reply,
    Detach,
    Prompt,
    Agent,
    Model,
    Effort,
    Access,
    Context,
    Directory,
    Workspace,
    Ref,
    Target,
    PickerOption(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HomeHitArea {
    pub(crate) target: HomeHitTarget,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UsageHitTarget {
    Cost,
    Tokens,
    Hours24,
    Days7,
    Days30,
    Days90,
    Model,
    Day,
    Rescan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageHitArea {
    pub(crate) target: UsageHitTarget,
    pub(crate) rect: Rect,
}

/// Which pane-toggle button in the tab row was pressed. TUI-only presentation
/// state: it is resolved into the existing split/close runtime calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneToggleDirection {
    Below,
    Right,
}

impl PaneToggleDirection {
    pub(crate) fn nav(self) -> crate::layout::NavDirection {
        match self {
            PaneToggleDirection::Below => crate::layout::NavDirection::Down,
            PaneToggleDirection::Right => crate::layout::NavDirection::Right,
        }
    }

    pub(crate) fn split(self) -> crate::api::schema::SplitDirection {
        match self {
            PaneToggleDirection::Below => crate::api::schema::SplitDirection::Down,
            PaneToggleDirection::Right => crate::api::schema::SplitDirection::Right,
        }
    }
}

pub struct ViewState {
    pub layout: ViewLayout,
    /// Full-width top status row (tmux-parity). Empty on mobile / tiny heights.
    pub status_bar_rect: Rect,
    pub sidebar_rect: Rect,
    /// Sidebar-footer entry for the full-screen pull-request view.
    pub(crate) sidebar_footer_work_hit_area: Rect,
    /// Sidebar-footer entry for the client-local historical usage view.
    pub(crate) sidebar_footer_usage_hit_area: Rect,
    /// Header and breakdown controls inside the historical usage view.
    pub(crate) usage_hit_areas: Vec<UsageHitArea>,
    /// Sidebar-footer entry for the full-screen Linear ticket view.
    pub(crate) sidebar_footer_ticket_hit_area: Rect,
    /// Sidebar-footer entry for the full-screen Missive conversation view.
    pub(crate) sidebar_footer_missive_hit_area: Rect,
    pub workspace_card_areas: Vec<WorkspaceCardArea>,
    pub agent_card_areas: Vec<AgentCardArea>,
    pub(crate) visible_agent_activity_instants: Vec<Instant>,
    pub tab_bar_rect: Rect,
    pub tab_hit_areas: Vec<Rect>,
    pub tab_scroll_left_hit_area: Rect,
    pub tab_scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub repo_editor_button_hit_area: Rect,
    pub add_action_button_hit_area: Rect,
    pub user_action_hit_areas: Vec<(usize, Rect)>,
    pub add_action_close_hit_area: Rect,
    pub add_action_field_hit_areas: Vec<(AddActionField, Rect)>,
    pub add_action_cancel_hit_area: Rect,
    pub add_action_save_hit_area: Rect,
    pub(crate) add_project_layout: crate::ui::add_project::AddProjectLayout,
    pub git_menu_button_hit_area: Rect,
    pub git_menu_popup_rect: Rect,
    pub git_menu_first_visible: usize,
    pub git_menu_row_hit_areas: Vec<Rect>,
    pub pane_toggle_below_hit_area: Rect,
    pub pane_toggle_right_hit_area: Rect,
    pub terminal_area: Rect,
    pub info_panel_rect: Rect,
    pub info_panel_link_rows: Vec<InfoPanelLinkRow>,
    pub mobile_header_rect: Rect,
    pub mobile_menu_hit_area: Rect,
    pub toast_hit_area: Rect,
    /// `(queue index, row rect)` for each row the home view is showing.
    pub home_row_hit_areas: Vec<(usize, Rect)>,
    /// All clickable home controls, including composer fields and open pickers.
    pub(crate) home_hit_areas: Vec<HomeHitArea>,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
    pub dock_rect: Rect,
    pub dock_handle_rect: Rect,
    pub dock_divider_rect: Rect,
    pub dock_tab_bar_rect: Rect,
    pub dock_tab_hit_areas: Vec<Rect>,
    /// Close glyph of the active tab; empty when no surface is open.
    pub dock_tab_close_rect: Rect,
    /// The `+` that opens the surface chooser.
    pub dock_plus_rect: Rect,
    /// The `⤢` that maximises the dock.
    pub dock_maximize_rect: Rect,
    /// One rect per `DockSurface::CARDS` entry of the empty-dock grid.
    pub dock_surface_card_hit_areas: Vec<Rect>,
    /// Geometry of the open `+` menu.
    pub(crate) dock_surface_menu_layout: Option<crate::ui::dropdown::DropdownLayout>,
    pub dock_home_section_hit_areas: Vec<Rect>,
    pub dock_home_tab_hit_areas: Vec<Rect>,
    pub(crate) dock_home_tab_keys: Vec<WorkItemKey>,
    pub dock_home_detail_tab_hit_areas: Vec<Rect>,
    pub(crate) dock_file_row_hit_areas: Vec<DockFileRowHitArea>,
    pub dock_body_rect: Rect,
    pub scratchpad_link_rows: Vec<ScratchpadLinkRow>,
    /// Left-aligned status-bar buttons, computed once per frame so the rendered
    /// label and the clickable rect can never disagree.
    pub status_buttons: Vec<StatusButton>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusButtonAction {
    Home,
    Work,
    BlockedFilter,
    Dock,
    /// Expand or collapse the usage detail in the status row.
    StatusDetail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusButton {
    pub rect: Rect,
    pub label: String,
    pub action: StatusButtonAction,
    /// Drawn with the accent instead of the dim overlay: the inbox has work in
    /// it, or the surface this button opens is already showing.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InfoPanelLinkRow {
    pub rect: Rect,
    pub copy_value: String,
}

/// A scratchpad link row opens its URL; the work-context rows copy instead, so
/// the two cannot share a type without making the click semantics ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScratchpadLinkRow {
    pub rect: Rect,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkLinkPickerAction {
    Open,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkLinkPickerState {
    pub candidates: Vec<crate::work_context::WorkLinkCandidate>,
    pub action: WorkLinkPickerAction,
    pub return_mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Onboarding,
    ReleaseNotes,
    ProductAnnouncement,
    Navigate,
    Prefix,
    Copy,
    Terminal,
    RenameWorkspace,
    RenameTab,
    RenamePane,
    NewLinkedWorktree,
    OpenExistingWorktree,
    ConfirmRemoveWorktree,
    Resize,
    ConfirmClose,
    ContextMenu,
    GitMenu,
    AddAction,
    Settings,
    GlobalMenu,
    KeybindHelp,
    Navigator,
    WorkLinkPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddActionField {
    Name,
    Key,
    Command,
    RunOnWorktreeCreate,
    OpenInBottomPane,
}

impl AddActionField {
    pub(crate) const ALL: [Self; 5] = [
        Self::Name,
        Self::Key,
        Self::Command,
        Self::RunOnWorktreeCreate,
        Self::OpenInBottomPane,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddActionState {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) command: String,
    pub(crate) run_on_worktree_create: bool,
    pub(crate) open_in_bottom_pane: bool,
    pub(crate) field: AddActionField,
    pub(crate) error: Option<String>,
}

impl Default for AddActionState {
    fn default() -> Self {
        Self {
            name: String::new(),
            key: String::new(),
            command: String::new(),
            run_on_worktree_create: false,
            open_in_bottom_pane: true,
            field: AddActionField::Name,
            error: None,
        }
    }
}

impl Mode {
    pub(crate) fn mouse_motion_changes_view(self) -> bool {
        matches!(
            self,
            Self::GlobalMenu | Self::ContextMenu | Self::GitMenu | Self::Navigator
        )
    }

    /// Whether keys in this mode are commands/navigation (an ASCII input source is wanted) rather
    /// than free text. This is an explicit **allowlist** of the prefix command/navigation realm:
    /// any mode NOT listed defaults to leaving the user's IME alone (the safe default), so adding a
    /// new text-entry or overlay mode can never silently force ASCII. Used by
    /// `sync_prefix_input_source` (gated by `switch_ascii_input_source_in_prefix`) so multi-level
    /// prefix commands keep ASCII until they return to the terminal.
    ///
    /// Known limitation: the search boxes in `Navigator` and `KeybindHelp` are also held on ASCII,
    /// since this `Mode`-level predicate can't see `search_focused` (non-ASCII filtering there
    /// would need a runtime check).
    pub(crate) fn wants_ascii_input(self) -> bool {
        matches!(
            self,
            Mode::Prefix
                | Mode::Navigate
                | Mode::Navigator
                | Mode::Copy
                | Mode::Resize
                | Mode::ConfirmClose
                | Mode::ConfirmRemoveWorktree
                | Mode::ContextMenu
                | Mode::GitMenu
                | Mode::GlobalMenu
                | Mode::KeybindHelp
                | Mode::WorkLinkPicker
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitAction {
    Pull,
    Commit,
    Push,
    CreatePr,
}

impl GitAction {
    pub(crate) const ALL: [Self; 4] = [Self::Pull, Self::Commit, Self::Push, Self::CreatePr];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pull => "⇣ Pull",
            Self::Commit => "⊙ Commit",
            Self::Push => "⇡ Push",
            Self::CreatePr => "⑂ Create PR",
        }
    }

    pub(crate) fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Pull => &["git", "pull", "--rebase"],
            Self::Commit => &["git", "commit"],
            Self::Push => &["git", "push"],
            Self::CreatePr => &["gh", "pr", "create", "--fill"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavigatorTarget {
    Workspace {
        ws_idx: usize,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavigatorRow {
    pub target: NavigatorTarget,
    pub depth: u8,
    pub label: String,
    pub meta: String,
    pub status: AgentState,
    pub seen: bool,
    pub stale: bool,
    pub is_current: bool,
    pub is_workspace: bool,
    pub is_tab: bool,
    pub expanded: bool,
    pub search_text: String,
    /// Whether this row itself matched the active query/state filter, as
    /// opposed to being included as ancestor context or cascaded subtree of a
    /// matching workspace or tab. Always true when no filter is active.
    pub matched: bool,
}

/// One rendered line in the navigator body. Spacer lines separate workspace
/// groups visually and are not selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorDisplayLine {
    Spacer,
    Row(usize),
}

pub(crate) fn navigator_display_lines(rows: &[NavigatorRow]) -> Vec<NavigatorDisplayLine> {
    let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));
    for (idx, row) in rows.iter().enumerate() {
        if row.is_workspace && !lines.is_empty() {
            lines.push(NavigatorDisplayLine::Spacer);
        }
        lines.push(NavigatorDisplayLine::Row(idx));
    }
    lines
}

pub(crate) fn navigator_display_index_of_row(
    lines: &[NavigatorDisplayLine],
    row_idx: usize,
) -> Option<usize> {
    lines
        .iter()
        .position(|line| *line == NavigatorDisplayLine::Row(row_idx))
}

pub(crate) fn navigator_first_row_at_or_after(
    lines: &[NavigatorDisplayLine],
    line_idx: usize,
) -> Option<usize> {
    lines.get(line_idx..)?.iter().find_map(|line| match line {
        NavigatorDisplayLine::Row(idx) => Some(*idx),
        NavigatorDisplayLine::Spacer => None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorStateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NavigatorState {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub search_focused: bool,
    pub state_filter: Option<NavigatorStateFilter>,
    pub expanded_workspaces: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeState {
    pub pane_id: PaneId,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub entry_offset_from_bottom: usize,
    pub selection: Option<CopyModeSelection>,
    pub search: CopyModeSearchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSelection {
    Character,
    Linewise { anchor_row: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeSearchPrompt {
    pub direction: CopyModeSearchDirection,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CopyModeSearchState {
    pub prompt: Option<CopyModeSearchPrompt>,
    pub query: String,
    pub direction: Option<CopyModeSearchDirection>,
    pub matches: Vec<crate::pane::TerminalTextMatch>,
    pub current: Option<usize>,
    pub geometry: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentPanelSort {
    #[default]
    Spaces,
    Priority,
}

// ---------------------------------------------------------------------------
// Settings UI state
// ---------------------------------------------------------------------------

/// Which section of the settings panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Theme,
    Indicators,
    Sound,
    Toast,
    PaneLabels,
    Keybindings,
    Providers,
    Integrations,
    SourceControl,
    Archive,
    About,
}

impl SettingsSection {
    pub const ALL: &[Self] = &[
        Self::General,
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::Toast,
        Self::PaneLabels,
        Self::Keybindings,
        Self::Providers,
        Self::Integrations,
        Self::SourceControl,
        Self::Archive,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Theme => "theme",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::Toast => "toasts",
            Self::PaneLabels => "pane labels",
            Self::Keybindings => "keybindings",
            Self::Providers => "providers",
            Self::Integrations => "integrations",
            Self::SourceControl => "source control",
            Self::Archive => "archive",
            Self::About => "about",
        }
    }

    /// The glyph in front of the nav row, mirroring the design's icon column.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::General => "⚙",
            Self::Theme => "◐",
            Self::Indicators => "●",
            Self::Sound => "♪",
            Self::Toast => "▣",
            Self::PaneLabels => "▭",
            Self::Keybindings => "⌨",
            Self::Providers => "⚛",
            Self::Integrations => "⊞",
            Self::SourceControl => "⑂",
            Self::Archive => "▤",
            Self::About => "ⓘ",
        }
    }

    /// Position in the unfiltered nav column.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0)
    }
}

/// The sections a search query leaves in the nav column, in nav order.
///
/// An empty query is not a filter, it is the whole list. A query that matches
/// nothing returns nothing, so the column shows the operator that their query
/// is the reason the list is empty rather than silently ignoring it.
pub fn settings_sections_matching(query: &str) -> Vec<SettingsSection> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return SettingsSection::ALL.to_vec();
    }
    SettingsSection::ALL
        .iter()
        .copied()
        .filter(|section| section.label().contains(&query))
        .collect()
}

/// All built-in theme names in display order.
pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "github-dark-high-contrast",
    "github-light-high-contrast",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListState {
    pub highlighted: usize,
}

impl MenuListState {
    pub fn new(highlighted: usize) -> Self {
        Self { highlighted }
    }

    pub fn move_prev(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.highlighted = (self.highlighted + 1).min(item_count - 1);
        }
    }

    pub fn hover(&mut self, idx: Option<usize>) {
        if let Some(idx) = idx {
            self.highlighted = idx;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionListState {
    pub selected: usize,
}

impl SelectionListState {
    pub fn new(selected: usize) -> Self {
        Self { selected }
    }

    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selected = (self.selected + 1).min(item_count - 1);
        }
    }

    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
    }
}

#[derive(Debug, Clone)]
pub struct ThemeRuntimeConfig {
    pub manual_name: String,
    pub dark_name: String,
    pub light_name: String,
    pub auto_switch: bool,
    pub custom: Option<crate::config::CustomThemeColors>,
    pub legacy_accent: Option<String>,
}

pub struct SettingsState {
    /// Which section tab is active.
    pub section: SettingsSection,
    /// Selected item index within the current section.
    pub list: SelectionListState,
    /// The palette before opening settings (for cancel/restore).
    pub original_palette: Option<Palette>,
    /// The theme name before opening settings.
    pub original_theme: Option<String>,
    /// The archive row's delete has been pressed once and waits for the
    /// confirming second press. Only armed while `ui.confirm_close` is on.
    pub archive_delete_armed: bool,
    /// The section-nav filter query. TUI-only: nothing outside the settings
    /// screen reads it and it is never persisted.
    pub search: String,
    /// Typed characters go to the filter line rather than the section list.
    pub search_active: bool,
    /// The keybindings row whose chord is being captured, if any. TUI-only:
    /// capture is a settings-screen interaction, not a session fact.
    pub keybind_capture: Option<KeybindCapture>,
}

/// A keybindings row waiting for the operator to press the chord they want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindCapture {
    /// Index into `settings_keybinding_rows`.
    pub row: usize,
    /// The refusal shown inline under the row, from the same validator the
    /// add-action modal uses.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceDropTarget {
    Before(usize),
    End,
}

pub(crate) enum DragTarget {
    WorkspaceReorder {
        source_ws_idx: usize,
        drop_target: Option<WorkspaceDropTarget>,
    },
    TabReorder {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: Option<usize>,
    },
    WorkspaceListScrollbar {
        grab_row_offset: u16,
    },
    PaneSplit {
        path: Vec<bool>,
        direction: Direction,
        area: Rect,
        grab_offset: u16,
    },
    PaneScrollbar {
        pane_id: crate::layout::PaneId,
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    KeybindHelpScrollbar {
        grab_row_offset: u16,
    },
    SidebarDivider,
    DockDivider,
}

/// Active mouse drag on a split border or sidebar divider.
pub(crate) struct DragState {
    pub target: DragTarget,
}

pub(crate) struct WorkspacePressState {
    pub ws_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct TabPressState {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuKind {
    Workspace {
        ws_idx: usize,
    },
    GitWorkspace {
        ws_idx: usize,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        collapsed: bool,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
        source_pane_id: Option<PaneId>,
        has_manual_label: bool,
    },
}

/// Right-click context menu state.
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub x: u16,
    pub y: u16,
    pub list: MenuListState,
}

impl ContextMenuState {
    pub fn items(&self) -> &'static [&'static str] {
        match self.kind {
            ContextMenuKind::Workspace { .. } => &["Rename", "Close"],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => &["Rename", "Close", "New worktree", "Open worktree..."],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: true,
                ..
            } => &["Rename", "Close", "Delete worktree checkout..."],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: true,
                ..
            } => &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Expand",
            ],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
                ..
            } => &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Collapse",
            ],
            ContextMenuKind::Tab { .. } => &["New tab", "Rename", "Close"],
            ContextMenuKind::Pane {
                has_manual_label: true,
                source_pane_id: Some(_),
                ..
            } => &[
                "Rename pane",
                "Clear pane name",
                "Swap with focused pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                source_pane_id: Some(_),
                ..
            } => &[
                "Rename pane",
                "Swap with focused pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: true,
                source_pane_id: None,
                ..
            } => &[
                "Rename pane",
                "Clear pane name",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
            ContextMenuKind::Pane {
                has_manual_label: false,
                source_pane_id: None,
                ..
            } => &[
                "Rename pane",
                "Split right",
                "Split down",
                "Zoom",
                "Close pane",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    pub kind: ToastKind,
    pub title: String,
    pub context: String,
    pub position: Option<crate::config::ToastHerdrPosition>,
    pub target: Option<ToastTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAgentNotification {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub state: AgentState,
    pub deadline: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNotificationDelivery {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub toast: Option<ToastNotification>,
    pub client_notification: Option<ToastNotification>,
    pub sound: Option<crate::sound::Sound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFeedback {
    pub message: String,
}

pub struct ReleaseNotesState {
    pub version: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct ProductAnnouncementState {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

#[derive(Default)]
pub struct KeybindHelpState {
    pub scroll: u16,
    pub query: String,
    pub search_focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidthSource {
    ConfigDefault,
    Persisted,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneFocusTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneSettlementChange {
    pub(crate) workspace_id: String,
    pub(crate) pane_id: PaneId,
    pub(crate) settled_at: Option<u64>,
}

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
pub struct AppState {
    /// Clock snapshot captured during `compute_view`; renderers consume it
    /// without reading the clock or mutating shared runtime state.
    pub(crate) view_observed_at: Instant,
    /// Server-owned observation facts loaded from the fleet receipt and loop
    /// registry files. The UI detail projection reads these values without
    /// touching the filesystem during render.
    pub(crate) loop_run_history: crate::loop_runs::RunHistory,
    pub(crate) loop_registry: crate::loop_runs::LoopRegistry,
    pub(crate) loop_run_history_detail: Option<LoopRunHistoryDetail>,
    pub(crate) symphony_snapshot: crate::symphony::Snapshot,
    pub(crate) symphony_detail: Option<SymphonyDetail>,
    /// Open work projection view. `Some` means the view owns the screen and the
    /// keyboard, like the Symphony and loop-history details above it.
    pub(crate) work_view: Option<WorkViewState>,
    /// Client-local historical usage view and its scan result.
    pub(crate) usage_view: Option<UsageViewState>,
    /// Cached local usage loaded before the first background rescan.
    pub(crate) usage_snapshot: Option<crate::provider_usage::UsageSnapshot>,
    /// Local list-price table used only by the usage presentation.
    pub(crate) usage_pricing: crate::config::UsageConfig,
    /// Set by client-local input and drained into the background scan job.
    pub(crate) request_usage_scan: bool,
    /// Open inbox cursor. `Some` means the inbox overlay owns the screen and the
    /// keyboard, exactly like the Symphony and loop-history details above it.
    pub(crate) inbox: Option<crate::app::inbox::InboxState>,
    /// Open home view. `Some` means home owns the screen, the same way `inbox`
    /// does; the two are mutually exclusive because each wants the whole frame.
    pub(crate) home: Option<crate::app::home::HomeState>,
    /// Client-local provider choices retained when Home closes.
    pub(crate) home_agent_choices: Vec<crate::app::home::HomeAgentChoice>,
    /// Provider choices resolved outside `HomeState`, ready for the next Home open.
    pub(crate) home_catalog: crate::app::home_catalog::HomeCatalog,
    /// Ref snapshots are TUI-only picker data, keyed by the repository's common root.
    pub(crate) home_ref_cache:
        std::collections::HashMap<std::path::PathBuf, crate::app::home_refs::HomeRefCacheEntry>,
    /// Opening the ref picker asks the runtime layer to refresh this repository.
    pub(crate) request_home_ref_refresh: Option<std::path::PathBuf>,
    /// A settings section that needs the tool probes was entered.
    /// Drained by `App::start_requested_tool_probes`, which owns the
    /// thread the pure state layer cannot spawn.
    pub(crate) request_tool_probes: bool,
    /// Unsent text typed by a human in each pane. Home replies must not touch a
    /// pane while this draft exists because the terminal owns that edit buffer.
    pub(crate) pending_human_drafts: std::collections::HashMap<PaneId, String>,
    /// Server-owned native metric snapshot consumed by pure rendering.
    pub(crate) status_metrics: Option<crate::platform::status_metrics::StatusMetricsSnapshot>,
    pub(crate) status_git_cwd: Option<std::path::PathBuf>,
    pub(crate) status_git_branch: Option<String>,
    pub(crate) status_git_ahead_behind: Option<(usize, usize)>,
    /// Runtime-resolved cwd of the focused pane, projected from the same
    /// source the Git refresh uses so rendering never re-derives a weaker one.
    pub(crate) status_focused_cwd: Option<std::path::PathBuf>,
    pub(crate) status_focus_projection_initialized: bool,
    /// Git root observed for a pane cwd, keyed by that cwd, or `None` when the
    /// cwd is not inside a repository. Filled by the background Git work
    /// context refresh so pure rendering can answer "is this pane in a repo?"
    /// without touching the filesystem.
    pub(crate) git_root_for_cwd:
        std::collections::HashMap<std::path::PathBuf, Option<std::path::PathBuf>>,
    /// Vim-family command resolved outside the pure render path.
    pub(crate) repo_editor_argv: Option<Vec<String>>,
    /// Pane whose most recent mouse event was forwarded to its terminal.
    pub(crate) forwarded_pane_input: Option<PaneId>,
    /// Whether the full-width top status row is enabled by configuration.
    pub(crate) status_bar_enabled: bool,
    /// Wall clock at the last background refresh, so reset countdowns are
    /// computed from a server-owned instant instead of inside rendering.
    pub(crate) status_now_unix: Option<i64>,
    /// Account-level provider quota, refreshed off the render thread.
    pub(crate) provider_usage: crate::provider_usage::ProviderUsageSnapshot,
    /// Reachability of the wider internet, as folded from periodic probes.
    pub(crate) connectivity: crate::connectivity::Connectivity,
    /// Expanded status bar: percentages and reset times instead of bars alone.
    pub(crate) status_bar_expanded: bool,
    /// Whether the disk segment was visible last frame, which is what gives the
    /// show/hide threshold its hysteresis.
    pub(crate) status_disk_visible: bool,
    pub(crate) full_lifecycle_hook_authority_timeout: std::time::Duration,
    pub(crate) hide_done_after: std::time::Duration,
    pub(crate) reap_done_after: std::time::Duration,
    pub(crate) reap_done_panes: bool,
    pub(crate) settle_after: std::time::Duration,
    pub terminals:
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    /// Terminal ids whose size is currently owned by a direct attach client.
    pub direct_attach_resize_locks: std::collections::HashSet<crate::terminal::TerminalId>,
    pub(crate) pane_id_aliases: std::collections::HashMap<u32, PaneId>,
    pub(crate) public_pane_id_aliases: std::collections::HashMap<String, PaneId>,
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub(crate) previous_pane_focus: Option<PaneFocusTarget>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    /// In monolithic --no-session mode, detach exits the app because there is no server to detach from.
    pub detach_exits: bool,
    /// Set when the current client should detach from the persistent session.
    /// The server's event loop checks this and handles client detach.
    pub detach_requested: bool,
    pub request_new_workspace: bool,
    pub request_new_tab: bool,
    /// Set by a click on a tab-row pane toggle, drained by the runtime loop
    /// into the same split/close calls the keybindings use.
    pub(crate) request_pane_toggle: Option<PaneToggleDirection>,
    /// The top-bar repository editor button was activated.
    pub(crate) request_open_repo_editor: bool,
    /// Git action chosen from the tab-row menu, drained by the runtime loop.
    pub(crate) request_git_action: Option<GitAction>,
    pub(crate) request_user_action: Option<usize>,
    pub(crate) request_save_add_action: bool,
    pub(crate) request_pr_land: Option<PrLandConfirmation>,
    /// A click landed on a tab's pin glyph. Drained by the app loop, which is
    /// the layer that owns the API client the mutation has to travel through.
    pub request_pin_toggle: Option<(usize, usize)>,
    pub request_new_linked_worktree: Option<usize>,
    pub request_open_existing_worktree: Option<usize>,
    pub request_new_workspace_cwd: Option<std::path::PathBuf>,
    pub request_remove_linked_worktree: Option<usize>,
    pub request_submit_worktree_create: bool,
    pub request_submit_worktree_open: bool,
    pub request_submit_worktree_remove: bool,
    pub request_reload_config: bool,
    /// Set when the headless server should ask attached clients to reload
    /// their client-local sound config from disk.
    pub request_client_config_reload: bool,
    /// Width to persist in the attached client's local presentation state.
    pub(crate) dock_width_persistence_request: Option<u16>,
    pub(crate) sidebar_group_mode_persistence_request: Option<SidebarGroupMode>,
    pub(crate) sidebar_work_filter_persistence_request: Option<SidebarWorkFilter>,
    /// Set when UI interaction requested a clipboard write that must be
    /// handled by the outer App/event loop instead of directly from AppState.
    pub request_clipboard_write: Option<Vec<u8>>,
    pub creating_new_tab: bool,
    pub requested_new_tab_name: Option<String>,
    pub pending_workspace_create_cwd: Option<std::path::PathBuf>,
    pub rename_pane_target: Option<PaneId>,
    pub worktree_create: Option<WorktreeCreateState>,
    pub worktree_open: Option<WorktreeOpenState>,
    pub worktree_remove: Option<WorktreeRemoveState>,
    pub worktree_directory: std::path::PathBuf,
    pub collapsed_space_keys: std::collections::HashSet<String>,
    pub prio_panel_collapsed: bool,
    /// Sidebar group headers the user folded away, keyed by title. Separate from
    /// `collapsed_space_keys`, which folds one space inside the tree; this folds
    /// a whole group, the tree included.
    pub collapsed_sidebar_groups: std::collections::HashSet<String>,
    pub(crate) sidebar_group_mode: SidebarGroupMode,
    pub(crate) sidebar_group_menu_open: bool,
    pub(crate) sidebar_group_menu_selected: usize,
    pub(crate) sidebar_work_filter: SidebarWorkFilter,
    pub(crate) sidebar_filter_menu_open: bool,
    pub(crate) sidebar_filter_menu_selected: usize,
    /// Dim work-item header the operator selected with the mouse. Enter on it
    /// starts a thread for that ticket or conversation.
    pub(crate) sidebar_selected_work_group: Option<String>,
    /// Views whose Unassigned section has expanded past its ten newest rows.
    /// Attach-local TUI state; provider objects remain shared work-index facts.
    pub(crate) sidebar_unassigned_expanded_views: std::collections::HashSet<SidebarGroupMode>,
    pub(crate) sidebar_selected_settled: Option<PaneFocusTarget>,
    pub(crate) sidebar_settled_menu_target: Option<PaneFocusTarget>,
    pub(crate) sidebar_settled_menu_selected: usize,
    /// The settled menu's delete row has been pressed once and is waiting for
    /// the confirming second press. Only armed while `ui.confirm_close` is on.
    pub(crate) sidebar_settled_menu_delete_armed: bool,
    pub(crate) pending_pane_settlement_changes: Vec<PaneSettlementChange>,
    pub request_complete_onboarding: bool,
    pub name_input: String,
    pub name_input_replace_on_type: bool,
    /// Label the rename-tab modal was prefilled with, so an unedited Enter stays a
    /// no-op even when the live derived label changes while the modal is open.
    pub rename_tab_prefill: Option<String>,
    pub release_notes: Option<ReleaseNotesState>,
    pub product_announcement: Option<ProductAnnouncementState>,
    pub keybind_help: KeybindHelpState,
    pub navigator: NavigatorState,
    pub work_link_picker: Option<WorkLinkPickerState>,
    pub(crate) add_action: Option<AddActionState>,
    pub copy_mode: Option<CopyModeState>,
    pub(crate) sidebar_presentation: SidebarPresentationState,
    /// Monotonic client-only revision for changes that replace the sidebar row
    /// projection. Each attach resets its own offsets when it next reconciles.
    pub(crate) sidebar_projection_revision: u64,
    pub(crate) workspace_picker_forces_spaces_tree: bool,
    pub workspace_scroll: usize,
    pub tab_scroll: usize,
    pub tab_scroll_follow_active: bool,
    pub mobile_switcher_scroll: usize,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    pub(crate) workspace_press: Option<WorkspacePressState>,
    pub(crate) tab_press: Option<TabPressState>,
    pub selection: Option<Selection>,
    pub selection_autoscroll: Option<SelectionAutoscroll>,
    pub context_menu: Option<ContextMenuState>,
    pub(crate) git_menu: MenuListState,
    // Notifications
    pub update_available: Option<String>,
    pub update_install_command: String,
    pub latest_release_notes_available: bool,
    pub update_dismissed: bool,
    pub config_diagnostic: Option<String>,
    pub toast: Option<ToastNotification>,
    pub pending_agent_notifications: std::collections::HashMap<PaneId, PendingAgentNotification>,
    pub copy_feedback: Option<CopyFeedback>,
    /// Last reported focus state for the outer terminal hosting herdr.
    /// None means unsupported or not yet reported, which preserves active-pane suppression.
    pub outer_terminal_focus: Option<bool>,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    pub default_sidebar_width: u16,
    pub sidebar_width: u16,
    pub sidebar_min_width: u16,
    pub sidebar_max_width: u16,
    pub dock_width: u16,
    pub dock_collapsed: bool,
    /// Set by an explicit dock pick and cleared by `follow_view`.
    /// Attach-local TUI state; it never enters server state or the JSON API.
    pub(crate) dock_surface_override: bool,
    /// Active dock surface, `None` when nothing is open and the dock shows the
    /// surface chooser. TUI presentation state; never leaves the client.
    pub dock_tab: Option<DockSurface>,
    /// Surfaces open as tabs, in strip order. TUI presentation state.
    pub dock_open_surfaces: Vec<DockSurface>,
    /// Dock takes the whole main area. TUI presentation state.
    pub dock_maximized: bool,
    /// Open `+` chooser dropdown. TUI presentation state.
    pub(crate) dock_surface_menu: Option<DockSurfaceMenu>,
    /// Keyboard focus sits on the chooser card grid. TUI presentation state.
    pub(crate) dock_chooser_focused: bool,
    pub dock_scroll: u16,
    pub(crate) dock_editor_focused: bool,
    /// Diff interaction state is attach-local TUI state. The whitespace choice
    /// survives surface switches for the lifetime of the client session.
    pub(crate) dock_diff_focused: bool,
    /// Compact PR surface interaction state. TUI presentation state: the
    /// pull request itself is a shared work-index fact, the open menu and the
    /// staged confirmation are not.
    pub(crate) dock_pr_focused: bool,
    pub(crate) dock_pr_checkout_menu: Option<PrCheckoutChoice>,
    pub(crate) dock_pr_pending_land: Option<PrLandConfirmation>,
    /// `git diff -w` for the Diff surface. Initialised from
    /// `ui.hide_whitespace_in_diff` and written by both the dock's own
    /// whitespace toggle and the General settings row, so the two never drift.
    pub(crate) dock_diff_ignore_whitespace: bool,
    pub(crate) dock_diff_selected: usize,
    pub(crate) dock_diff_collapsed: std::collections::HashSet<String>,
    pub(crate) dock_diff_request: Option<super::diff::DiffRefreshRequest>,
    pub(crate) dock_diff_active_key: Option<DiffCacheKey>,
    pub(crate) dock_diff_cache: std::collections::HashMap<DiffCacheKey, DiffCacheEntry>,
    pub(crate) dock_diff_resolved_requests:
        std::collections::HashMap<super::diff::DiffRefreshRequest, DiffCacheKey>,
    pub(crate) dock_files_focused: bool,
    pub(crate) dock_files_selection: Option<std::path::PathBuf>,
    pub(crate) dock_files_filter: String,
    pub(crate) dock_files_collapsed: std::collections::HashSet<std::path::PathBuf>,
    /// Cached client-side file snapshots, keyed by repository root.
    pub(crate) dock_file_cache:
        std::collections::HashMap<std::path::PathBuf, crate::files::FileTreeSnapshot>,
    pub(crate) dock_files_root: Option<std::path::PathBuf>,
    pub(crate) dock_files_cwd: Option<std::path::PathBuf>,
    pub(crate) dock_files_roots_by_cwd:
        std::collections::HashMap<std::path::PathBuf, std::path::PathBuf>,
    pub(crate) files_icons: crate::config::FilesIconConfig,
    /// Selection inside the dock home tab, swapped per client through
    /// `DockPresentationState`. A key, never an index.
    pub(crate) dock_home_selection: Option<WorkItemKey>,
    pub(crate) dock_home_ticket_selection: Option<WorkItemKey>,
    pub(crate) dock_home_poll_selection: Option<WorkItemKey>,
    /// A comment being typed against the selected work item.
    /// True when the focused pane resolves to no pull request and no ticket.
    /// Without this the selection falls back to the first row, so switching to
    /// an unrelated tab kept showing the previous item as though it were the
    /// current one.
    pub(crate) dock_home_focus_unbound: bool,
    pub(crate) dock_comment_draft: Option<String>,
    /// A write staged by a button but not yet confirmed. Nothing leaves herdr
    /// until the user presses the confirm key with this set.
    pub(crate) dock_pending_write: Option<crate::work_index::WorkItemWrite>,
    /// The outcome of the last write, shown until the next one is staged.
    pub(crate) dock_write_notice: Option<String>,
    pub(crate) dock_home_section: DockHomeSection,
    pub(crate) dock_home_detail_tab: DockHomeDetailTab,
    pub(crate) dock_home_focused: bool,
    /// Focus target last considered for automatic dock-home selection, swapped
    /// per client through `DockPresentationState`.
    pub(crate) dock_home_followed_pane: Option<PaneFocusTarget>,
    /// Server-global work index snapshot. Set on every applied
    /// `WorkIndexRefreshed`; the dock home enriches rows from it.
    pub(crate) work_index_snapshot: Option<crate::work_index::Snapshot>,
    /// Provider viewers and assignee directories resolved by the work-index
    /// runtime once for this app session.
    pub(crate) work_index_session: crate::work_index::WorkIndexSession,
    /// Server-global on-demand detail facts, keyed by stable work identity.
    /// Client-local selection decides which entry is rendered, but never owns
    /// or duplicates the fetched data.
    pub(crate) work_item_detail_cache: crate::work_index::WorkItemDetailCache,
    /// Detail keys in the one bounded batch currently running. This is runtime
    /// observation state, not a client-local loading flag inferred by render.
    pub(crate) work_item_detail_loading: std::collections::HashSet<WorkItemKey>,
    /// Whether the work index is configured on. Mirrored so the dock home can
    /// distinguish "off" from "on but not observed yet" instead of rendering
    /// one indistinguishable `unknown` for both.
    pub(crate) work_index_enabled: bool,
    /// Client-local approval label used by the PR landing gate.
    pub(crate) land_approval_label: String,
    /// `source_control.branch_prefix`: the prefix Herdr puts in front of a
    /// branch name derived from a ticket.
    pub(crate) branch_prefix: String,
    /// `source_control.commit_message_model`: exported to the Commit action so
    /// a hook can draft the message. Empty leaves `git commit` unchanged.
    pub(crate) commit_message_model: String,
    pub(crate) work_index_linear_team_configured: bool,
    pub(crate) dock_editor_sessions: std::collections::HashMap<PaneId, DockEditorSession>,
    pub(crate) dock_editor_errors: std::collections::HashMap<PaneId, String>,
    pub(crate) dock_editor_requested_paths: std::collections::HashMap<PaneId, std::path::PathBuf>,
    pub(crate) scratchpad: crate::scratchpad::ScratchpadDoc,
    pub mobile_width_threshold: u16,
    pub sidebar_width_source: SidebarWidthSource,
    pub sidebar_width_auto: bool,
    pub sidebar_collapsed: bool,
    /// Whether the sidebar is showing only rows that require human attention.
    pub blocked_filter: bool,
    /// Whether the desktop focused-pane work-context panel is expanded.
    pub info_panel_expanded: bool,
    pub sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig,
    /// Ratio of sidebar height allocated to the workspaces section.
    pub sidebar_section_split: f32,
    pub agent_panel_scroll: usize,
    pub agent_panel_sort: AgentPanelSort,
    /// Transient session-wide compatibility projection for indexed Agent focus.
    pub status_indicators: crate::config::StatusIndicatorStyle,
    /// Transient session-wide projection override for the built-in Agents view.
    pub agent_view_override: Option<crate::api::schema::AgentViewSetParams>,
    pub sidebar_agents: crate::config::AgentsSidebarConfig,
    pub sidebar_spaces: crate::config::SpacesSidebarConfig,
    pub next_agent_state_change_seq: u64,
    /// Capture mouse input for Herdr's own mouse UI. When false, Herdr only
    /// captures mouse while the focused pane app requests mouse reporting.
    pub mouse_capture: bool,
    pub copy_on_select: bool,
    pub right_click_passthrough_modifiers: Option<KeyModifiers>,
    pub right_click_passthrough: Option<RightClickPassthroughGesture>,
    pub redraw_on_focus_gained: bool,
    pub mouse_scroll_lines: usize,
    pub confirm_close: bool,
    /// Group workspaces that check out the same repository under one project
    /// header even when the checkout roots differ (`ui.combine_repos_across_hosts`).
    pub combine_repos_across_hosts: bool,
    /// Workspace preselected for a new Home thread (`ui.new_thread_workspace`).
    pub new_thread_workspace: crate::config::NewThreadWorkspaceConfig,
    /// Directory the add-project picker starts in (`ui.add_project_start_dir`).
    /// Empty keeps the last used directory.
    pub add_project_start_dir: String,
    /// Settle a pane when its linked work finishes (`session.auto_settle_finished`).
    pub auto_settle_finished: bool,
    /// Settle a pane after `settle_after` of inactivity (`session.auto_settle_inactive`).
    pub auto_settle_inactive: bool,
    pub prompt_new_tab_name: bool,
    pub prompt_new_workspace_name: bool,
    pub pane_borders: bool,
    pub pane_scrollbars: bool,
    pub pane_gaps: bool,
    pub show_agent_labels_on_pane_borders: bool,
    pub hide_tab_bar_when_single_tab: bool,
    pub show_subscription_usage: bool,
    pub tab_bar_position: TabBarPositionConfig,
    pub pane_history_persistence: bool,
    /// Expose the focused pane's cursor anchor to the outer terminal even when
    /// the pane requested `?25l`. See `[experimental] reveal_hidden_cursor_for_cjk_ime`.
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    /// Restrict cursor reveal to focused panes whose detected agent matches
    /// one of these. When false, apply to any focused pane.
    pub cjk_ime_agent_filter_configured: bool,
    pub cjk_ime_agents: Vec<crate::detect::Agent>,
    /// DECSCUSR shape parameter (1–6) for the IME anchor cursor.
    pub cjk_ime_cursor_shape: u8,
    /// While prefix mode is active, switch the macOS host input source to an
    /// ASCII-capable layout so prefix commands register as ASCII even when a
    /// CJK IME is active. macOS only; a no-op elsewhere. See
    /// `[experimental] switch_ascii_input_source_in_prefix`.
    pub switch_ascii_input_source_in_prefix: bool,
    pub kitty_graphics_enabled: bool,
    pub default_shell: String,
    pub shell_mode: crate::config::ShellModeConfig,
    pub new_terminal_cwd: NewTerminalCwdConfig,
    pub pane_scrollback_limit_bytes: usize,
    #[allow(dead_code)] // kept for backward compat; palette.accent is the source of truth
    pub accent: Color,
    pub sound: SoundConfig,
    pub local_sound_playback: bool,
    pub toast_config: ToastConfig,
    pub keybinds: Keybinds,
    /// UI color palette — all sidebar/UI colors centralized for theming.
    pub palette: Palette,
    /// Currently applied theme name (for settings UI).
    pub theme_name: String,
    /// Runtime theme configuration used to resolve manual and auto-switch palettes.
    pub theme_runtime: ThemeRuntimeConfig,
    /// Last known foreground host terminal appearance.
    pub host_terminal_appearance: Option<HostAppearance>,
    /// True when the foreground host explicitly reported appearance via Mode 2031.
    pub host_terminal_appearance_explicit: bool,
    /// Set when the active palette contradicts the appearance the host terminal
    /// reports, e.g. a light theme pinned in front of a dark terminal. Surfaced
    /// as a config-reload diagnostic so the mismatch is named, not guessed at.
    pub theme_appearance_mismatch: Option<String>,
    /// Settings panel state.
    pub settings: SettingsState,
    /// Session cache of the settings Providers/Integrations probes. TUI-only:
    /// nothing outside the settings screen reads it.
    pub tool_probes: crate::app::probes::ToolProbeState,
    /// Cached integration recommendations for onboarding/settings UI.
    pub integration_recommendations: Vec<crate::integration::IntegrationRecommendation>,
    /// Cached detection manifest source/version summaries for runtime/API status.
    pub agent_manifest_summaries: Vec<crate::detect::manifest::AgentManifestSummary>,
    /// Cached remote detection manifest update diagnostics for runtime/API status.
    pub agent_manifest_update_status: crate::detect::manifest_update::ManifestUpdateStatus,
    /// Result messages from the latest integration install action.
    pub integration_install_messages: Vec<String>,
    /// Installed or linked plugins known to this running Herdr instance.
    pub(crate) installed_plugins: InstalledPluginRegistry,
    /// Pane ids opened through the plugin pane API.
    pub(crate) plugin_panes: std::collections::HashMap<PaneId, PluginPaneRecord>,
    /// Runtime image layers owned by API clients and composited over panes.
    pub(crate) pane_graphics_layers: std::collections::HashMap<PaneId, PaneGraphicsLayer>,
    /// Active streaming graphics owner token by pane id.
    pub(crate) pane_graphics_streams: std::collections::HashMap<PaneId, String>,
    /// Monotonic marker for accepted pane graphics mutations.
    pub(crate) pane_graphics_revision: u64,
    /// Session-modal terminal popup. This is intentionally outside workspace layouts.
    pub(crate) popup_pane: Option<PopupPaneState>,
    /// Recent plugin action/event command executions.
    pub(crate) plugin_command_logs: Vec<crate::api::schema::PluginCommandLogInfo>,
    pub(crate) next_plugin_command_log_id: u64,
    pub(crate) plugin_commands_in_flight: usize,
    /// Highlight state for the bottom-right global launcher menu.
    pub global_menu: MenuListState,
    /// Resolved host terminal default colors for theming embedded panes.
    pub host_terminal_theme: TerminalTheme,
    /// Last known foreground host terminal cell size in pixels.
    pub(crate) host_cell_size: crate::kitty_graphics::HostCellSize,
    /// Set when a persisted session snapshot would change.
    pub session_dirty: bool,
    /// Monotonic revision used to avoid clearing mutations made during a save.
    pub(crate) session_dirty_revision: u64,
    /// Terminal runtimes that should be shut down by the app/runtime layer
    /// after state has detached their terminal metadata.
    pub(crate) terminal_runtime_shutdowns: Vec<crate::terminal::TerminalId>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoopRunHistoryDetail {
    pub(crate) loop_id: String,
    pub(crate) history: crate::loop_runs::RunHistory,
    pub(crate) observed_at: std::time::SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymphonyDetail {
    pub(crate) snapshot: crate::symphony::Snapshot,
    pub(crate) selected: usize,
    pub(crate) observed_at: std::time::SystemTime,
}

impl SymphonyDetail {
    pub(crate) fn replace_snapshot(&mut self, snapshot: crate::symphony::Snapshot) {
        let selected_identity = self
            .snapshot
            .workflows
            .get(self.selected)
            .map(|workflow| (workflow.workflow_id.as_str(), workflow.run_id.as_str()));
        let selected = selected_identity
            .and_then(|(workflow_id, run_id)| {
                snapshot.workflows.iter().position(|workflow| {
                    workflow.workflow_id == workflow_id && workflow.run_id == run_id
                })
            })
            .unwrap_or_else(|| {
                self.selected
                    .min(snapshot.workflows.len().saturating_sub(1))
            });
        self.snapshot = snapshot;
        self.selected = selected;
        self.observed_at = std::time::SystemTime::now();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkProjection {
    PullRequests,
    Tickets,
    Missive,
    Agents,
    ReviewQueue,
}

impl WorkProjection {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PullRequests => "PRs",
            Self::Tickets => "tickets",
            Self::Missive => "Missive",
            Self::Agents => "agents",
            Self::ReviewQueue => "review queue",
        }
    }

    pub(crate) fn rotate_left(self) -> Self {
        match self {
            Self::PullRequests => Self::ReviewQueue,
            Self::Tickets => Self::PullRequests,
            Self::Missive => Self::Tickets,
            Self::Agents => Self::Missive,
            Self::ReviewQueue => Self::Agents,
        }
    }

    pub(crate) fn rotate_right(self) -> Self {
        match self {
            Self::PullRequests => Self::Tickets,
            Self::Tickets => Self::Missive,
            Self::Missive => Self::Agents,
            Self::Agents => Self::ReviewQueue,
            Self::ReviewQueue => Self::PullRequests,
        }
    }
}

/// Stable identity of a projected work row. Selection is stored as this key,
/// not an index, so it survives snapshot refreshes and projection rotations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkItemKey {
    pub(crate) repo: String,
    pub(crate) pr_number: Option<u64>,
    pub(crate) pr_url: Option<String>,
    pub(crate) ticket_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DockHomeSection {
    #[default]
    Prs,
    Tickets,
    /// Closing-block gates across every pane: the questions waiting on a
    /// human answer, gathered where the rest of the work lives.
    XPolls,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DockHomeDetailTab {
    #[default]
    Overview,
    Comments,
    Actions,
    Files,
    Commits,
    Ticket,
}

impl DockHomeDetailTab {
    pub(crate) const ALL: [Self; 6] = [
        Self::Overview,
        Self::Comments,
        Self::Actions,
        Self::Files,
        Self::Commits,
        Self::Ticket,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Comments => "comments",
            Self::Actions => "actions",
            Self::Files => "files",
            Self::Commits => "commits",
            Self::Ticket => "ticket",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkViewState {
    pub(crate) projection: WorkProjection,
    /// `None` means "no explicit selection yet" and resolves to the first row.
    pub(crate) selected: Option<WorkItemKey>,
    pub(crate) repo_filter: Option<String>,
    pub(crate) enabled: bool,
    /// `None` means the enabled index has not been collected yet.
    pub(crate) snapshot: Option<crate::work_index::Snapshot>,
    pub(crate) hint: Option<String>,
    pub(crate) search: String,
    pub(crate) search_focused: bool,
    pub(crate) sort: crate::ui::work_list_detail::PrSort,
    pub(crate) ticket_sort: crate::ui::work_list_detail::TicketSort,
    pub(crate) open_only: bool,
    pub(crate) ticket_open_only: bool,
    pub(crate) detail_tab: PrDetailTab,
    pub(crate) checkout_menu: Option<PrCheckoutChoice>,
    pub(crate) pending_land: Option<PrLandConfirmation>,
    pub(crate) ticket_start_menu: Option<PrCheckoutChoice>,
    pub(crate) ticket_transition_menu: Option<TicketTransitionChoice>,
    pub(crate) ticket_more_menu: Option<TicketMoreChoice>,
    pub(crate) missive_start_menu: Option<PrCheckoutChoice>,
    pub(crate) selected_missive: Option<String>,
    pub(crate) missive_detail_scroll: u16,
    pub(crate) ticket_comment_draft: Option<String>,
    pub(crate) pending_write: Option<crate::work_index::WorkItemWrite>,
    pub(crate) refreshing: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageMetric {
    #[default]
    Cost,
    Tokens,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageRange {
    Hours24,
    Days7,
    #[default]
    Days30,
    Days90,
}

impl UsageRange {
    pub(crate) fn seconds(self) -> i64 {
        match self {
            Self::Hours24 => 24 * 60 * 60,
            Self::Days7 => 7 * 24 * 60 * 60,
            Self::Days30 => 30 * 24 * 60 * 60,
            Self::Days90 => 90 * 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageBreakdown {
    #[default]
    Model,
    Day,
}

/// Client-local controls and scan data for the full-screen usage view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageViewState {
    pub(crate) metric: UsageMetric,
    pub(crate) range: UsageRange,
    pub(crate) breakdown: UsageBreakdown,
    pub(crate) snapshot: Option<crate::provider_usage::UsageSnapshot>,
    pub(crate) scanning: bool,
}

impl UsageViewState {
    pub(crate) fn new(snapshot: Option<crate::provider_usage::UsageSnapshot>) -> Self {
        Self {
            metric: UsageMetric::default(),
            range: UsageRange::default(),
            breakdown: UsageBreakdown::default(),
            scanning: snapshot.is_none(),
            snapshot,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TicketTransitionChoice {
    #[default]
    Todo,
    InProgress,
    InReview,
    Done,
}

impl TicketTransitionChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Todo, Self::InProgress, Self::InReview, Self::Done];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
        }
    }

    pub(crate) fn move_by(self, delta: i8) -> Self {
        let index = Self::ALL
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0) as i8;
        Self::ALL[(index + delta).clamp(0, 3) as usize]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TicketMoreChoice {
    #[default]
    Open,
    CopyIdentifier,
    Comment,
}

impl TicketMoreChoice {
    pub(crate) const ALL: [Self; 3] = [Self::Open, Self::CopyIdentifier, Self::Comment];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "Open in browser",
            Self::CopyIdentifier => "Copy identifier",
            Self::Comment => "Comment",
        }
    }

    pub(crate) fn move_by(self, delta: i8) -> Self {
        let index = Self::ALL
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0) as i8;
        Self::ALL[(index + delta).clamp(0, 2) as usize]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PrDetailTab {
    #[default]
    Summary,
    Timeline,
    Code,
}

impl PrDetailTab {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Summary => Self::Timeline,
            Self::Timeline => Self::Code,
            Self::Code => Self::Summary,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PrCheckoutChoice {
    #[default]
    CurrentCheckout,
    NewWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrLandConfirmation {
    pub(crate) repo: String,
    pub(crate) number: u64,
    pub(crate) head_sha: String,
    pub(crate) approval_signal: String,
}

impl WorkViewState {
    pub(crate) fn new(enabled: bool, snapshot: Option<crate::work_index::Snapshot>) -> Self {
        Self {
            projection: WorkProjection::PullRequests,
            selected: None,
            repo_filter: None,
            enabled,
            snapshot,
            hint: None,
            search: String::new(),
            search_focused: false,
            sort: crate::ui::work_list_detail::PrSort::Updated,
            ticket_sort: crate::ui::work_list_detail::TicketSort::Updated,
            open_only: true,
            ticket_open_only: false,
            detail_tab: PrDetailTab::Summary,
            checkout_menu: None,
            pending_land: None,
            ticket_start_menu: None,
            ticket_transition_menu: None,
            ticket_more_menu: None,
            missive_start_menu: None,
            selected_missive: None,
            missive_detail_scroll: 0,
            ticket_comment_draft: None,
            pending_write: None,
            refreshing: false,
        }
    }
}

impl AppState {
    pub(crate) fn swap_symphony_detail(&mut self, other: &mut Option<SymphonyDetail>) {
        std::mem::swap(&mut self.symphony_detail, other);
    }

    pub(crate) fn toggle_symphony(&mut self) {
        if self.symphony_detail.is_some() {
            self.symphony_detail = None;
        } else {
            self.symphony_detail = Some(SymphonyDetail {
                snapshot: self.symphony_snapshot.clone(),
                selected: 0,
                observed_at: std::time::SystemTime::now(),
            });
        }
    }

    pub(crate) fn clear_symphony(&mut self) {
        self.symphony_detail = None;
    }

    pub(crate) fn swap_work_view(&mut self, other: &mut Option<WorkViewState>) {
        std::mem::swap(&mut self.work_view, other);
    }

    pub(crate) fn clear_work_view(&mut self) {
        self.work_view = None;
    }

    pub(crate) fn swap_usage_view(&mut self, other: &mut Option<UsageViewState>) {
        std::mem::swap(&mut self.usage_view, other);
    }

    pub(crate) fn toggle_usage_view(&mut self) {
        if self.usage_view.is_some() {
            self.usage_view = None;
            return;
        }
        self.work_view = None;
        self.usage_view = Some(UsageViewState::new(self.usage_snapshot.clone()));
        self.request_usage_scan = true;
        self.follow_view(SidebarGroupMode::Repo);
    }

    pub(crate) fn clear_usage_view(&mut self) {
        self.usage_view = None;
    }

    pub(crate) fn swap_loop_run_history_detail(
        &mut self,
        other: &mut Option<LoopRunHistoryDetail>,
    ) {
        std::mem::swap(&mut self.loop_run_history_detail, other);
    }

    pub(crate) fn show_loop_run_history(
        &mut self,
        loop_id: String,
        history: crate::loop_runs::RunHistory,
        observed_at: std::time::SystemTime,
    ) {
        self.loop_run_history_detail = Some(LoopRunHistoryDetail {
            loop_id,
            history,
            observed_at,
        });
    }

    pub(crate) fn clear_loop_run_history(&mut self) {
        self.loop_run_history_detail = None;
    }

    /// Reveal the scratchpad without spawning an editor: the dock opens if it was
    /// collapsed and selects the Note tab. Reading the note is the common case;
    /// editing it is the deliberate one.
    pub(crate) fn show_scratchpad_tab(&mut self) {
        self.dock_collapsed = false;
        self.open_dock_surface(DockSurface::Scratchpad);
        self.dock_editor_focused = false;
        self.dock_diff_focused = false;
        self.dock_files_focused = false;
    }

    pub(crate) fn toggle_loop_run_history(&mut self) {
        if self.loop_run_history_detail.is_some() {
            self.clear_loop_run_history();
            return;
        }
        self.show_loop_run_history(
            crate::loop_runs::ALL_LOOPS_ID.to_string(),
            crate::loop_runs::RunHistory {
                runs: crate::loop_runs::runs_for_loop(&self.loop_run_history, None),
                skipped_lines: self.loop_run_history.skipped_lines,
            },
            std::time::SystemTime::now(),
        );
    }
}

impl AppState {
    pub(crate) fn set_dock_width(&mut self, width: u16) {
        if self.dock_width != width {
            self.dock_width = width;
            self.dock_width_persistence_request = Some(width);
        }
    }

    pub(crate) fn take_dock_width_persistence_request(&mut self) -> Option<u16> {
        self.dock_width_persistence_request.take()
    }

    pub(crate) fn set_sidebar_group_mode(&mut self, mode: SidebarGroupMode) {
        if self.sidebar_group_mode == mode {
            self.sidebar_group_menu_open = false;
            return;
        }
        self.sidebar_group_mode = mode;
        self.follow_view(mode);
        self.sidebar_group_menu_selected = mode.view_index();
        self.sidebar_group_menu_open = false;
        self.sidebar_group_mode_persistence_request = Some(mode);
        self.sidebar_selected_work_group = None;
        self.sidebar_selected_settled = None;
        self.sidebar_settled_menu_target = None;
        self.sidebar_settled_menu_delete_armed = false;
        self.sidebar_filter_menu_open = false;
        self.workspace_scroll = 0;
        self.mark_sidebar_projection_changed();
    }

    pub(crate) fn cycle_sidebar_group_mode(&mut self) {
        self.set_sidebar_group_mode(self.sidebar_group_mode.next());
    }

    pub(crate) fn take_sidebar_group_mode_persistence_request(
        &mut self,
    ) -> Option<SidebarGroupMode> {
        self.sidebar_group_mode_persistence_request.take()
    }

    pub(crate) fn set_sidebar_work_filter(&mut self, filter: SidebarWorkFilter) {
        self.sidebar_filter_menu_open = false;
        if self.sidebar_work_filter == filter {
            return;
        }
        self.sidebar_work_filter = filter.clone();
        self.sidebar_work_filter_persistence_request = Some(filter);
        self.sidebar_selected_work_group = None;
        self.workspace_scroll = 0;
        self.mark_sidebar_projection_changed();
    }

    pub(crate) fn take_sidebar_work_filter_persistence_request(
        &mut self,
    ) -> Option<SidebarWorkFilter> {
        self.sidebar_work_filter_persistence_request.take()
    }

    pub(crate) fn swap_sidebar_presentation(&mut self, other: &mut SidebarPresentationState) {
        std::mem::swap(
            &mut self.sidebar_presentation.expanded_workspace_ids,
            &mut other.expanded_workspace_ids,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.known_workspace_ids,
            &mut other.known_workspace_ids,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.revealed_workspace_id,
            &mut other.revealed_workspace_id,
        );
        std::mem::swap(&mut self.workspace_scroll, &mut other.workspace_scroll);
        std::mem::swap(
            &mut self.mobile_switcher_scroll,
            &mut other.mobile_switcher_scroll,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.projection_revision,
            &mut other.projection_revision,
        );
        std::mem::swap(&mut self.sidebar_group_mode, &mut other.group_mode);
        std::mem::swap(
            &mut self.sidebar_group_menu_open,
            &mut other.group_menu_open,
        );
        std::mem::swap(
            &mut self.sidebar_group_menu_selected,
            &mut other.group_menu_selected,
        );
        std::mem::swap(&mut self.sidebar_work_filter, &mut other.work_filter);
        std::mem::swap(
            &mut self.sidebar_filter_menu_open,
            &mut other.filter_menu_open,
        );
        std::mem::swap(
            &mut self.sidebar_filter_menu_selected,
            &mut other.filter_menu_selected,
        );
        std::mem::swap(
            &mut self.sidebar_selected_work_group,
            &mut other.selected_work_group,
        );
        std::mem::swap(
            &mut self.sidebar_unassigned_expanded_views,
            &mut other.unassigned_expanded_views,
        );
        std::mem::swap(
            &mut self.sidebar_selected_settled,
            &mut other.selected_settled,
        );
        std::mem::swap(
            &mut self.sidebar_settled_menu_target,
            &mut other.settled_menu_target,
        );
        std::mem::swap(
            &mut self.sidebar_settled_menu_selected,
            &mut other.settled_menu_selected,
        );
    }

    /// Open `surface` as a tab and make it active. Already-open surfaces are
    /// only reactivated, so the strip order never shuffles under the user.
    pub(crate) fn open_dock_surface(&mut self, surface: DockSurface) {
        self.dock_surface_override = true;
        self.select_dock_surface(surface);
    }

    fn select_dock_surface(&mut self, surface: DockSurface) {
        if !self.dock_open_surfaces.contains(&surface) {
            self.dock_open_surfaces.push(surface);
        }
        self.dock_tab = Some(surface);
        self.dock_surface_menu = None;
        self.dock_chooser_focused = false;
        if surface == DockSurface::Editor {
            self.retry_dock_editor();
        }
    }

    /// Keep the dock on the compact companion for a sidebar or full-screen
    /// work view. A later explicit surface pick remains visible until another
    /// view change calls this function.
    pub(crate) fn follow_view(&mut self, view: SidebarGroupMode) {
        self.dock_collapsed = false;
        match view {
            SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree => {}
            SidebarGroupMode::LinearTeam => self.select_dock_surface(DockSurface::Linear),
            SidebarGroupMode::RepoPr => self.select_dock_surface(DockSurface::Pr),
            SidebarGroupMode::Missive => self.select_dock_surface(DockSurface::Missive),
        }
        self.dock_surface_override = false;
    }

    /// Close `surface`. The active surface moves to the neighbour that took its
    /// place, or to `None` when the dock is left empty and becomes a chooser.
    pub(crate) fn close_dock_surface(&mut self, surface: DockSurface) {
        let Some(index) = self
            .dock_open_surfaces
            .iter()
            .position(|open| *open == surface)
        else {
            return;
        };
        self.dock_open_surfaces.remove(index);
        if self.dock_tab != Some(surface) {
            return;
        }
        self.dock_tab = self
            .dock_open_surfaces
            .get(index)
            .or_else(|| self.dock_open_surfaces.get(index.saturating_sub(1)))
            .copied();
        self.dock_scroll = 0;
        if self.dock_tab.is_none() {
            self.dock_editor_focused = false;
            self.dock_home_focused = false;
            self.dock_diff_focused = false;
            self.dock_files_focused = false;
            self.dock_chooser_focused = true;
        }
    }

    pub(crate) fn toggle_dock_maximized(&mut self) {
        self.dock_maximized = !self.dock_maximized;
    }

    /// Adjacent open surface, wrapping. `None` when nothing is open.
    pub(crate) fn adjacent_dock_surface(&self, forward: bool) -> Option<DockSurface> {
        let open = &self.dock_open_surfaces;
        if open.is_empty() {
            return None;
        }
        let current = self
            .dock_tab
            .and_then(|tab| open.iter().position(|surface| *surface == tab))
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % open.len()
        } else {
            (current + open.len() - 1) % open.len()
        };
        open.get(next).copied()
    }

    pub(crate) fn swap_dock_presentation(&mut self, other: &mut DockPresentationState) {
        std::mem::swap(&mut self.dock_width, &mut other.width);
        std::mem::swap(&mut self.dock_collapsed, &mut other.collapsed);
        std::mem::swap(&mut self.dock_surface_override, &mut other.surface_override);
        std::mem::swap(&mut self.dock_tab, &mut other.tab);
        std::mem::swap(&mut self.dock_open_surfaces, &mut other.open_surfaces);
        std::mem::swap(&mut self.dock_maximized, &mut other.maximized);
        std::mem::swap(&mut self.dock_surface_menu, &mut other.surface_menu);
        std::mem::swap(&mut self.dock_chooser_focused, &mut other.chooser_focused);
        std::mem::swap(&mut self.dock_scroll, &mut other.scroll);
        std::mem::swap(&mut self.dock_editor_focused, &mut other.editor_focused);
        std::mem::swap(&mut self.dock_diff_focused, &mut other.diff_focused);
        std::mem::swap(
            &mut self.dock_diff_ignore_whitespace,
            &mut other.diff_ignore_whitespace,
        );
        std::mem::swap(&mut self.dock_diff_selected, &mut other.diff_selected);
        std::mem::swap(&mut self.dock_diff_collapsed, &mut other.diff_collapsed);
        std::mem::swap(&mut self.dock_diff_request, &mut other.diff_request);
        std::mem::swap(&mut self.dock_diff_active_key, &mut other.diff_active_key);
        std::mem::swap(&mut self.dock_files_focused, &mut other.files_focused);
        std::mem::swap(&mut self.dock_files_selection, &mut other.files_selection);
        std::mem::swap(&mut self.dock_files_filter, &mut other.files_filter);
        std::mem::swap(&mut self.dock_files_collapsed, &mut other.files_collapsed);
        std::mem::swap(&mut self.dock_home_selection, &mut other.home_selection);
        std::mem::swap(
            &mut self.dock_home_ticket_selection,
            &mut other.home_ticket_selection,
        );
        std::mem::swap(
            &mut self.dock_home_poll_selection,
            &mut other.home_poll_selection,
        );
        std::mem::swap(&mut self.dock_home_section, &mut other.home_section);
        std::mem::swap(&mut self.dock_home_detail_tab, &mut other.home_detail_tab);
        std::mem::swap(&mut self.dock_home_focused, &mut other.home_focused);
        std::mem::swap(
            &mut self.dock_home_followed_pane,
            &mut other.home_followed_pane,
        );
    }

    pub(crate) fn reconcile_sidebar_presentation(&mut self) {
        if self.sidebar_presentation.projection_revision != self.sidebar_projection_revision {
            self.workspace_scroll = 0;
            self.mobile_switcher_scroll = 0;
            self.sidebar_presentation.projection_revision = self.sidebar_projection_revision;
        }
        let current_ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let first_attach = self.sidebar_presentation.known_workspace_ids.is_empty();
        let newly_added = current_ids
            .difference(&self.sidebar_presentation.known_workspace_ids)
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        self.sidebar_presentation
            .expanded_workspace_ids
            .retain(|workspace_id| current_ids.contains(workspace_id));
        self.sidebar_presentation.revealed_workspace_id = self
            .sidebar_presentation
            .revealed_workspace_id
            .take()
            .filter(|workspace_id| current_ids.contains(workspace_id));
        // A client attaches to the complete session projection, not just the
        // currently active workspace. Restricting this to the active space
        // makes inactive, completed and agentless tabs disappear from both
        // the expanded sidebar and its compact presentation. Disclosure stays
        // client-local after this initial (or newly-created workspace) setup.
        if first_attach {
            self.sidebar_presentation
                .expanded_workspace_ids
                .extend(current_ids.iter().cloned());
        } else {
            self.sidebar_presentation
                .expanded_workspace_ids
                .extend(newly_added);
        }
        self.sidebar_presentation.known_workspace_ids = current_ids;
    }

    pub(crate) fn mark_sidebar_projection_changed(&mut self) {
        self.sidebar_projection_revision = self.sidebar_projection_revision.wrapping_add(1);
        self.workspace_scroll = 0;
        self.mobile_switcher_scroll = 0;
    }

    pub(crate) fn sidebar_shows_spaces_tree(&self) -> bool {
        // The final sidebar is always a stable ownership projection. Priority
        // remains available for indexed Agent focus, but may not reorder rows.
        true
    }

    pub(crate) fn begin_workspace_picker_presentation(&mut self) {
        self.mobile_switcher_scroll = 0;
        self.workspace_picker_forces_spaces_tree = true;
    }

    pub(crate) fn end_workspace_picker_presentation(&mut self) {
        self.workspace_picker_forces_spaces_tree = false;
    }

    pub(crate) fn workspace_agents_expanded(&self, ws_idx: usize) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|workspace| {
            (self.sidebar_presentation.known_workspace_ids.is_empty()
                && self.active == Some(ws_idx))
                || self
                    .sidebar_presentation
                    .expanded_workspace_ids
                    .contains(&workspace.id)
        })
    }

    pub(crate) fn next_agent_activity_age_change(&self, now: Instant) -> Option<Instant> {
        self.view
            .visible_agent_activity_instants
            .iter()
            .filter_map(|observed_at| {
                crate::activity_age::next_coarse_change_at(Some(*observed_at), now)
            })
            .min()
    }

    pub(crate) fn toggle_workspace_agent_disclosure(&mut self, ws_idx: usize) -> bool {
        let Some(workspace_id) = self
            .workspaces
            .get(ws_idx)
            .map(|workspace| workspace.id.clone())
        else {
            return false;
        };
        if crate::ui::sidebar_thread_entries(self)
            .iter()
            .all(|entry| entry.ws_idx != ws_idx)
        {
            return false;
        }
        if !self
            .sidebar_presentation
            .expanded_workspace_ids
            .remove(&workspace_id)
        {
            self.sidebar_presentation
                .expanded_workspace_ids
                .insert(workspace_id);
        }
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    pub(crate) fn toggle_prio_panel(&mut self) -> bool {
        self.prio_panel_collapsed = !self.prio_panel_collapsed;
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    pub(crate) fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
        self.session_dirty_revision = self.session_dirty_revision.wrapping_add(1);
    }

    pub(crate) fn remove_alias_shadowed_by_new_pane(&mut self, pane_id: PaneId) {
        self.pane_id_aliases.remove(&pane_id.raw());
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound.enabled
    }

    pub fn toast_delivery(&self) -> ToastDelivery {
        self.toast_config.delivery
    }

    pub fn agent_border_labels_enabled(&self) -> bool {
        self.show_agent_labels_on_pane_borders
    }

    pub(crate) fn pane_exposes_host_cursor(
        &self,
        _ws_idx: usize,
        _pane_id: crate::layout::PaneId,
    ) -> bool {
        true
    }

    pub(crate) fn integration_updates_available(&self) -> bool {
        self.integration_recommendations
            .iter()
            .any(|item| item.state == crate::integration::IntegrationStatusKind::Outdated)
    }

    pub(crate) fn refresh_agent_manifest_summaries(&mut self) {
        self.agent_manifest_summaries = crate::detect::manifest::manifest_summaries();
    }

    pub(crate) fn global_menu_attention_badge_visible(&self) -> bool {
        self.update_available.is_some() || self.integration_updates_available()
    }

    pub(crate) fn global_menu_item_has_badge(&self, item: &str) -> bool {
        (item == "update ready" && self.update_available.is_some())
            || (item == "settings" && self.integration_updates_available())
    }

    pub(crate) fn settings_section_has_badge(&self, section: SettingsSection) -> bool {
        section == SettingsSection::Integrations && self.integration_updates_available()
    }

    pub(crate) fn focused_pane_requests_mouse_capture_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.mode == Mode::Terminal
            && self
                .active
                .and_then(|idx| self.focused_runtime_in_workspace(terminal_runtimes, idx))
                .and_then(crate::terminal::TerminalRuntime::input_state)
                .is_some_and(crate::pane::InputState::mouse_reporting_enabled)
    }

    pub(crate) fn should_capture_host_mouse_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.mouse_capture
            || self.popup_pane.is_some()
            || self.focused_pane_requests_mouse_capture_from(terminal_runtimes)
    }

    pub fn is_prefix_key(&self, key: &crate::input::TerminalKey) -> bool {
        crate::config::terminal_key_matches_combo(key, (self.prefix_code, self.prefix_mods))
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (24, 80)
        }
    }

    /// Returns true when the given (workspace, tab, pane) refers to the
    /// currently focused pane in the active workspace's active tab.
    pub(crate) fn runtime_for_pane_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        #[cfg(test)]
        if let Some(runtime) = self.workspaces.get(ws_idx)?.test_runtimes.get(&pane_id) {
            return Some(runtime);
        }
        #[cfg(test)]
        if let Some(runtime) = self
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find_map(|tab| tab.runtimes.get(&pane_id))
        {
            return Some(runtime);
        }
        let terminal_id = self.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        terminal_runtimes.get(terminal_id)
    }

    #[cfg(test)]
    pub(crate) fn runtime_for_pane<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        self.workspaces.iter().find_map(|ws| {
            #[cfg(test)]
            if let Some(runtime) = ws.test_runtimes.get(&pane_id) {
                return Some(runtime);
            }
            #[cfg(test)]
            if let Some(runtime) = ws.tabs.iter().find_map(|tab| tab.runtimes.get(&pane_id)) {
                return Some(runtime);
            }
            let terminal_id = ws.terminal_id(pane_id)?;
            terminal_runtimes.get(terminal_id)
        })
    }

    pub(crate) fn focused_runtime_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
    }

    pub fn is_active_pane(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if ws_idx != active_ws_idx {
            return false;
        }
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        if tab_idx != ws.active_tab_index() {
            return false;
        }
        ws.active_tab().map(|tab| tab.layout.focused()) == Some(pane_id)
    }
}

#[cfg(test)]
pub fn key_matches(
    key: &crossterm::event::KeyEvent,
    expected_code: KeyCode,
    expected_mods: KeyModifiers,
) -> bool {
    crate::config::terminal_key_matches_combo(
        &crate::input::TerminalKey::from(*key),
        (expected_code, expected_mods),
    )
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl AppState {
    /// Create an AppState for testing — no channels, no PTYs.
    pub fn test_new() -> Self {
        Self {
            view_observed_at: std::time::Instant::now(),
            loop_run_history: crate::loop_runs::RunHistory::default(),
            loop_registry: crate::loop_runs::LoopRegistry::default(),
            loop_run_history_detail: None,
            symphony_snapshot: crate::symphony::Snapshot::default(),
            symphony_detail: None,
            work_view: None,
            usage_view: None,
            usage_snapshot: None,
            usage_pricing: crate::config::UsageConfig::default(),
            request_usage_scan: false,
            inbox: None,
            home: None,
            home_agent_choices: Vec::new(),
            home_catalog: crate::app::home_catalog::HomeCatalog::fallback(),
            home_ref_cache: std::collections::HashMap::new(),
            request_home_ref_refresh: None,
            request_tool_probes: false,
            pending_human_drafts: std::collections::HashMap::new(),
            status_metrics: Some(crate::platform::status_metrics::StatusMetricsSnapshot {
                metrics: crate::platform::status_metrics::status_metrics_fixture(),
                sampled_at: std::time::Instant::now(),
            }),
            status_git_cwd: None,
            status_git_branch: None,
            status_git_ahead_behind: None,
            status_focused_cwd: None,
            status_focus_projection_initialized: false,
            git_root_for_cwd: std::collections::HashMap::new(),
            repo_editor_argv: None,
            forwarded_pane_input: None,
            status_bar_enabled: true,
            status_now_unix: None,
            provider_usage: crate::provider_usage::ProviderUsageSnapshot::default(),
            connectivity: crate::connectivity::Connectivity::default(),
            status_bar_expanded: false,
            status_disk_visible: false,
            full_lifecycle_hook_authority_timeout: std::time::Duration::from_secs(
                crate::config::Config::default()
                    .agent_detection
                    .full_lifecycle_hook_authority_timeout_seconds,
            ),
            hide_done_after: std::time::Duration::from_secs(30 * 60),
            reap_done_after: std::time::Duration::from_secs(4 * 60 * 60),
            reap_done_panes: true,
            settle_after: std::time::Duration::from_secs(3 * 24 * 60 * 60),
            terminals: std::collections::HashMap::new(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            pane_id_aliases: std::collections::HashMap::new(),
            public_pane_id_aliases: std::collections::HashMap::new(),
            workspaces: Vec::new(),
            active: None,
            previous_pane_focus: None,
            selected: 0,
            mode: Mode::Navigate,
            should_quit: false,
            detach_exits: false,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_pane_toggle: None,
            request_open_repo_editor: false,
            request_git_action: None,
            request_user_action: None,
            request_save_add_action: false,
            request_pr_land: None,
            request_pin_toggle: None,
            request_new_linked_worktree: None,
            request_open_existing_worktree: None,
            request_new_workspace_cwd: None,
            request_remove_linked_worktree: None,
            request_submit_worktree_create: false,
            request_submit_worktree_open: false,
            request_submit_worktree_remove: false,
            request_reload_config: false,
            request_client_config_reload: false,
            dock_width_persistence_request: None,
            sidebar_group_mode_persistence_request: None,
            sidebar_work_filter_persistence_request: None,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            pending_workspace_create_cwd: None,
            rename_pane_target: None,
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            worktree_directory: std::path::PathBuf::from("/tmp/herdr-worktrees"),
            collapsed_space_keys: std::collections::HashSet::new(),
            collapsed_sidebar_groups: std::iter::once("repo:Recently done".to_string()).collect(),
            sidebar_group_mode: SidebarGroupMode::Repo,
            sidebar_group_menu_open: false,
            sidebar_group_menu_selected: 0,
            sidebar_work_filter: SidebarWorkFilter::default(),
            sidebar_filter_menu_open: false,
            sidebar_filter_menu_selected: 0,
            sidebar_selected_work_group: None,
            sidebar_unassigned_expanded_views: std::collections::HashSet::new(),
            sidebar_selected_settled: None,
            sidebar_settled_menu_target: None,
            sidebar_settled_menu_selected: 0,
            sidebar_settled_menu_delete_armed: false,
            pending_pane_settlement_changes: Vec::new(),
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            rename_tab_prefill: None,
            release_notes: None,
            product_announcement: None,
            keybind_help: KeybindHelpState::default(),
            navigator: NavigatorState::default(),
            work_link_picker: None,
            add_action: None,
            copy_mode: None,
            sidebar_presentation: SidebarPresentationState::default(),
            sidebar_projection_revision: 0,
            workspace_picker_forces_spaces_tree: false,
            workspace_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: ViewState {
                layout: ViewLayout::Desktop,
                status_bar_rect: Rect::default(),
                sidebar_rect: Rect::default(),
                sidebar_footer_work_hit_area: Rect::default(),
                sidebar_footer_usage_hit_area: Rect::default(),
                usage_hit_areas: Vec::new(),
                sidebar_footer_ticket_hit_area: Rect::default(),
                sidebar_footer_missive_hit_area: Rect::default(),
                workspace_card_areas: Vec::new(),
                agent_card_areas: Vec::new(),
                visible_agent_activity_instants: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                repo_editor_button_hit_area: Rect::default(),
                add_action_button_hit_area: Rect::default(),
                user_action_hit_areas: Vec::new(),
                add_action_close_hit_area: Rect::default(),
                add_action_field_hit_areas: Vec::new(),
                add_action_cancel_hit_area: Rect::default(),
                add_action_save_hit_area: Rect::default(),
                add_project_layout: crate::ui::add_project::AddProjectLayout::default(),
                git_menu_button_hit_area: Rect::default(),
                git_menu_popup_rect: Rect::default(),
                git_menu_first_visible: 0,
                git_menu_row_hit_areas: Vec::new(),
                pane_toggle_below_hit_area: Rect::default(),
                pane_toggle_right_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                info_panel_rect: Rect::default(),
                info_panel_link_rows: Vec::new(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                home_row_hit_areas: Vec::new(),
                home_hit_areas: Vec::new(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
                dock_rect: Rect::default(),
                dock_handle_rect: Rect::default(),
                dock_divider_rect: Rect::default(),
                dock_tab_bar_rect: Rect::default(),
                dock_tab_hit_areas: Vec::new(),
                dock_tab_close_rect: Rect::default(),
                dock_plus_rect: Rect::default(),
                dock_maximize_rect: Rect::default(),
                dock_surface_card_hit_areas: Vec::new(),
                dock_surface_menu_layout: None,
                dock_home_section_hit_areas: Vec::new(),
                dock_home_tab_hit_areas: Vec::new(),
                dock_home_tab_keys: Vec::new(),
                dock_home_detail_tab_hit_areas: Vec::new(),
                dock_file_row_hit_areas: Vec::new(),
                dock_body_rect: Rect::default(),
                scratchpad_link_rows: Vec::new(),
                status_buttons: Vec::new(),
            },
            drag: None,
            workspace_press: None,
            tab_press: None,
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            git_menu: MenuListState::new(0),
            update_available: None,
            update_install_command: "herdr update".into(),
            latest_release_notes_available: false,
            update_dismissed: false,
            config_diagnostic: None,
            toast: None,
            pending_agent_notifications: std::collections::HashMap::new(),
            copy_feedback: None,
            outer_terminal_focus: None,
            prefix_code: KeyCode::Char('b'),
            prefix_mods: KeyModifiers::CONTROL,
            default_sidebar_width: 26,
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 36,
            dock_width: crate::ui::DOCK_DEFAULT_WIDTH,
            dock_collapsed: true,
            dock_surface_override: false,
            dock_tab: Some(DockSurface::Home),
            dock_open_surfaces: DockSurface::DEFAULT_OPEN.to_vec(),
            dock_maximized: false,
            dock_surface_menu: None,
            dock_chooser_focused: false,
            dock_scroll: 0,
            dock_editor_focused: false,
            dock_diff_focused: false,
            dock_pr_focused: false,
            dock_pr_checkout_menu: None,
            dock_pr_pending_land: None,
            dock_diff_ignore_whitespace: false,
            dock_diff_selected: 0,
            dock_diff_collapsed: std::collections::HashSet::new(),
            dock_diff_request: None,
            dock_diff_active_key: None,
            dock_diff_cache: std::collections::HashMap::new(),
            dock_diff_resolved_requests: std::collections::HashMap::new(),
            dock_files_focused: false,
            dock_files_selection: None,
            dock_files_filter: String::new(),
            dock_files_collapsed: std::collections::HashSet::new(),
            dock_file_cache: std::collections::HashMap::new(),
            dock_files_root: None,
            dock_files_cwd: None,
            dock_files_roots_by_cwd: std::collections::HashMap::new(),
            files_icons: crate::config::FilesIconConfig::Badges,
            dock_home_selection: None,
            dock_home_ticket_selection: None,
            dock_home_poll_selection: None,
            dock_home_focus_unbound: false,
            dock_comment_draft: None,
            dock_pending_write: None,
            dock_write_notice: None,
            dock_home_section: DockHomeSection::Prs,
            dock_home_detail_tab: DockHomeDetailTab::Overview,
            dock_home_focused: false,
            dock_home_followed_pane: None,
            work_index_snapshot: None,
            work_index_session: crate::work_index::WorkIndexSession::default(),
            work_item_detail_cache: crate::work_index::WorkItemDetailCache::default(),
            work_item_detail_loading: std::collections::HashSet::new(),
            work_index_enabled: false,
            land_approval_label: crate::config::DEFAULT_LAND_APPROVAL_LABEL.into(),
            branch_prefix: crate::config::DEFAULT_BRANCH_PREFIX.into(),
            commit_message_model: String::new(),
            work_index_linear_team_configured: false,
            dock_editor_sessions: std::collections::HashMap::new(),
            dock_editor_errors: std::collections::HashMap::new(),
            dock_editor_requested_paths: std::collections::HashMap::new(),
            scratchpad: crate::scratchpad::ScratchpadDoc::default(),
            info_panel_expanded: false,
            mobile_width_threshold: crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD,
            sidebar_width_source: SidebarWidthSource::ConfigDefault,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            blocked_filter: false,
            sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig::Compact,
            sidebar_section_split: 0.5,
            prio_panel_collapsed: false,
            agent_panel_scroll: 0,
            agent_panel_sort: AgentPanelSort::Spaces,
            status_indicators: crate::config::StatusIndicatorStyle::Dots,
            agent_view_override: None,
            sidebar_agents: crate::config::AgentsSidebarConfig::default(),
            sidebar_spaces: crate::config::SpacesSidebarConfig::default(),
            next_agent_state_change_seq: 0,
            mouse_capture: true,
            copy_on_select: true,
            right_click_passthrough_modifiers: None,
            right_click_passthrough: None,
            redraw_on_focus_gained: true,
            mouse_scroll_lines: crate::config::DEFAULT_MOUSE_SCROLL_LINES,
            confirm_close: true,
            combine_repos_across_hosts: false,
            new_thread_workspace: crate::config::NewThreadWorkspaceConfig::CurrentCheckout,
            add_project_start_dir: String::new(),
            auto_settle_finished: true,
            auto_settle_inactive: true,
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: true,
            pane_scrollbars: true,
            pane_gaps: false,
            show_agent_labels_on_pane_borders: false,
            hide_tab_bar_when_single_tab: false,
            show_subscription_usage: true,
            // Deliberately not the shipped default (`Hidden`): the UI tests that
            // exercise tab-row geometry need a tab row to measure.
            tab_bar_position: TabBarPositionConfig::Top,
            pane_history_persistence: false,
            reveal_hidden_cursor_for_cjk_ime: false,
            cjk_ime_agent_filter_configured: false,
            cjk_ime_agents: Vec::new(),
            cjk_ime_cursor_shape: 2, // steady_block
            switch_ascii_input_source_in_prefix: false,
            kitty_graphics_enabled: false,
            default_shell: String::new(),
            shell_mode: crate::config::ShellModeConfig::Auto,
            new_terminal_cwd: NewTerminalCwdConfig::Follow,
            pane_scrollback_limit_bytes: crate::config::DEFAULT_SCROLLBACK_LIMIT_BYTES,
            accent: Color::Cyan,
            sound: SoundConfig {
                enabled: false,
                ..SoundConfig::default()
            },
            local_sound_playback: false,
            toast_config: ToastConfig::default(),
            keybinds: Keybinds::default(),
            palette: Palette::catppuccin(),
            theme_name: "catppuccin".to_string(),
            theme_runtime: ThemeRuntimeConfig {
                manual_name: "catppuccin".to_string(),
                dark_name: "catppuccin".to_string(),
                light_name: "catppuccin-latte".to_string(),
                auto_switch: false,
                custom: None,
                legacy_accent: None,
            },
            host_terminal_appearance: None,
            host_terminal_appearance_explicit: false,
            theme_appearance_mismatch: None,
            tool_probes: crate::app::probes::ToolProbeState::Idle,
            settings: SettingsState {
                section: SettingsSection::Theme,
                list: SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
                archive_delete_armed: false,
                search: String::new(),
                search_active: false,
                keybind_capture: None,
            },
            integration_recommendations: Vec::new(),
            agent_manifest_summaries: Vec::new(),
            agent_manifest_update_status:
                crate::detect::manifest_update::ManifestUpdateStatus::default(),
            integration_install_messages: Vec::new(),
            installed_plugins: std::collections::HashMap::new(),
            plugin_panes: std::collections::HashMap::new(),
            pane_graphics_layers: std::collections::HashMap::new(),
            pane_graphics_streams: std::collections::HashMap::new(),
            pane_graphics_revision: 0,
            popup_pane: None,
            plugin_command_logs: Vec::new(),
            next_plugin_command_log_id: 1,
            plugin_commands_in_flight: 0,
            global_menu: MenuListState::new(0),
            host_terminal_theme: TerminalTheme::default(),
            host_cell_size: crate::kitty_graphics::HostCellSize::default(),
            session_dirty: false,
            session_dirty_revision: 0,
            terminal_runtime_shutdowns: Vec::new(),
        }
    }

    /// Populate missing `TerminalState` entries for every pane so tests that
    /// read or write terminal metadata don't need to manually create them.
    pub fn ensure_test_terminals(&mut self) {
        use crate::terminal::TerminalState;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    if !self.terminals.contains_key(&pane.attached_terminal_id) {
                        let cwd = ws.identity_cwd.clone();
                        self.terminals.insert(
                            pane.attached_terminal_id.clone(),
                            TerminalState::new(pane.attached_terminal_id.clone(), cwd),
                        );
                    }
                }
            }
        }
    }

    pub fn test_with_adversarial_identity_state() -> Self {
        let mut state = Self::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_adversarial_identity_state()];
        state.active = Some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        state
    }

    pub fn assert_invariants_for_test(&self) {
        if self.workspaces.is_empty() {
            assert!(
                self.active.is_none(),
                "empty app state must not have active workspace {:?}",
                self.active
            );
            assert_eq!(
                self.selected, 0,
                "empty app state should keep selected workspace at 0"
            );
            assert!(
                self.pane_id_aliases.is_empty(),
                "empty app state must not keep raw pane aliases"
            );
            assert!(
                self.public_pane_id_aliases.is_empty(),
                "empty app state must not keep public pane aliases"
            );
            assert!(
                self.previous_pane_focus.is_none(),
                "empty app state must not keep previous pane focus"
            );
            assert!(
                self.plugin_panes.is_empty(),
                "empty app state must not keep plugin pane records"
            );
            assert!(
                self.pending_agent_notifications.is_empty(),
                "empty app state must not keep pending agent notifications"
            );
            assert!(
                self.copy_mode.is_none(),
                "empty app state must not keep copy mode"
            );
            assert!(
                self.rename_pane_target.is_none(),
                "empty app state must not keep rename pane target"
            );
            assert!(
                self.selection.is_none(),
                "empty app state must not keep text selection"
            );
            assert!(
                self.selection_autoscroll.is_none(),
                "empty app state must not keep selection autoscroll"
            );
            if let Some(toast) = &self.toast {
                assert!(
                    toast.target.is_none(),
                    "empty app state must not keep pane-targeted toast"
                );
            }
            assert!(
                self.right_click_passthrough.is_none(),
                "empty app state must not keep right-click passthrough gesture"
            );
            assert!(
                self.drag.is_none(),
                "empty app state must not keep drag state"
            );
            assert!(
                self.workspace_press.is_none(),
                "empty app state must not keep workspace press state"
            );
            assert!(
                self.tab_press.is_none(),
                "empty app state must not keep tab press state"
            );
            assert!(
                self.context_menu.is_none(),
                "empty app state must not keep context menu"
            );
            return;
        }

        assert!(
            self.selected < self.workspaces.len(),
            "selected workspace {} out of bounds for {} workspaces",
            self.selected,
            self.workspaces.len()
        );
        let active = self
            .active
            .expect("non-empty app state must have active workspace");
        assert!(
            active < self.workspaces.len(),
            "active workspace {} out of bounds for {} workspaces",
            active,
            self.workspaces.len()
        );

        let mut workspace_ids = std::collections::HashSet::new();
        let mut workspace_id_to_idx = std::collections::HashMap::new();
        let mut pane_ids = std::collections::HashSet::new();
        let mut attached_terminal_ids = std::collections::HashSet::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            assert!(
                workspace_ids.insert(ws.id.clone()),
                "duplicate workspace id {} at workspace index {}",
                ws.id,
                ws_idx
            );
            workspace_id_to_idx.insert(ws.id.clone(), ws_idx);
            ws.assert_invariants_for_test();

            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    assert!(
                        pane_ids.insert(*pane_id),
                        "pane {:?} appears in more than one workspace",
                        pane_id
                    );
                    assert!(
                        attached_terminal_ids.insert(pane.attached_terminal_id.clone()),
                        "terminal {} is attached to more than one app pane",
                        pane.attached_terminal_id
                    );
                    assert!(
                        self.terminals.contains_key(&pane.attached_terminal_id),
                        "pane {:?} is attached to missing terminal {}",
                        pane_id,
                        pane.attached_terminal_id
                    );
                }
            }
        }

        let assert_live_pane = |pane_id: PaneId, context: &str| {
            assert!(
                pane_ids.contains(&pane_id),
                "{context} references missing pane {:?}",
                pane_id
            );
        };
        let assert_workspace_pane = |workspace_id: &str, pane_id: PaneId, context: &str| {
            let ws_idx = workspace_id_to_idx
                .get(workspace_id)
                .copied()
                .unwrap_or_else(|| panic!("{context} references missing workspace {workspace_id}"));
            assert!(
                self.workspaces[ws_idx].pane_state(pane_id).is_some(),
                "{context} references pane {:?} outside workspace {}",
                pane_id,
                workspace_id
            );
        };
        let assert_workspace_index = |ws_idx: usize, context: &str| {
            assert!(
                ws_idx < self.workspaces.len(),
                "{context} references workspace index {} out of bounds for {} workspaces",
                ws_idx,
                self.workspaces.len()
            );
        };
        let assert_tab_index = |ws_idx: usize, tab_idx: usize, context: &str| {
            assert_workspace_index(ws_idx, context);
            assert!(
                tab_idx < self.workspaces[ws_idx].tabs.len(),
                "{context} references tab index {} out of bounds for workspace {} with {} tabs",
                tab_idx,
                ws_idx,
                self.workspaces[ws_idx].tabs.len()
            );
        };

        for (&raw, &pane_id) in &self.pane_id_aliases {
            assert_live_pane(pane_id, &format!("raw pane alias {raw}"));
        }
        for (public_id, &pane_id) in &self.public_pane_id_aliases {
            assert_live_pane(pane_id, &format!("public pane alias {public_id}"));
        }
        if let Some(focus) = &self.previous_pane_focus {
            assert_workspace_pane(&focus.workspace_id, focus.pane_id, "previous pane focus");
        }
        if let Some(toast) = &self.toast {
            if let Some(target) = &toast.target {
                assert_workspace_pane(&target.workspace_id, target.pane_id, "toast target");
            }
        }
        for (&pane_id, notification) in &self.pending_agent_notifications {
            assert_eq!(
                pane_id, notification.pane_id,
                "pending agent notification map key must match payload pane id"
            );
            assert_workspace_pane(
                &notification.workspace_id,
                notification.pane_id,
                "pending agent notification",
            );
        }
        if let Some(popup) = &self.popup_pane {
            assert!(
                self.terminals.contains_key(&popup.terminal_id),
                "popup {:?} references missing terminal {}",
                popup.pane_id,
                popup.terminal_id
            );
            assert!(
                !attached_terminal_ids.contains(&popup.terminal_id),
                "popup terminal {} must not be attached to a tiled pane",
                popup.terminal_id
            );
        }
        for &pane_id in self.plugin_panes.keys() {
            assert_live_pane(pane_id, "plugin pane record");
        }
        if let Some(copy_mode) = &self.copy_mode {
            assert_live_pane(copy_mode.pane_id, "copy mode");
        }
        if let Some(pane_id) = self.rename_pane_target {
            assert_live_pane(pane_id, "rename pane target");
        }
        if let Some(selection) = &self.selection {
            assert_live_pane(selection.pane_id, "text selection");
        } else {
            assert!(
                self.selection_autoscroll.is_none(),
                "selection autoscroll must not remain without an active text selection"
            );
        }
        if let Some(gesture) = &self.right_click_passthrough {
            assert_live_pane(gesture.pane_info.id, "right-click passthrough gesture");
        }
        if let Some(drag) = &self.drag {
            match &drag.target {
                DragTarget::WorkspaceReorder {
                    source_ws_idx,
                    drop_target,
                } => {
                    assert_workspace_index(*source_ws_idx, "workspace drag source");
                    if let Some(WorkspaceDropTarget::Before(ws_idx)) = drop_target {
                        assert_workspace_index(*ws_idx, "workspace drag target");
                    }
                }
                DragTarget::TabReorder {
                    ws_idx,
                    source_tab_idx,
                    insert_idx,
                } => {
                    assert_tab_index(*ws_idx, *source_tab_idx, "tab drag source");
                    if let Some(insert_idx) = insert_idx {
                        assert!(
                            *insert_idx <= self.workspaces[*ws_idx].tabs.len(),
                            "tab drag insert index {} out of bounds for workspace {} with {} tabs",
                            insert_idx,
                            ws_idx,
                            self.workspaces[*ws_idx].tabs.len()
                        );
                    }
                }
                DragTarget::PaneScrollbar { pane_id, .. } => {
                    assert_live_pane(*pane_id, "pane scrollbar drag")
                }
                _ => {}
            }
        }
        if let Some(press) = &self.workspace_press {
            assert_workspace_index(press.ws_idx, "workspace press");
        }
        if let Some(press) = &self.tab_press {
            assert_tab_index(press.ws_idx, press.tab_idx, "tab press");
        }
        if let Some(menu) = &self.context_menu {
            match menu.kind {
                ContextMenuKind::Workspace { ws_idx }
                | ContextMenuKind::GitWorkspace { ws_idx, .. } => {
                    assert_workspace_index(ws_idx, "context menu workspace")
                }
                ContextMenuKind::Tab { ws_idx, tab_idx } => {
                    assert_tab_index(ws_idx, tab_idx, "context menu tab")
                }
                ContextMenuKind::Pane {
                    ws_idx,
                    tab_idx,
                    pane_id,
                    source_pane_id,
                    ..
                } => {
                    assert_tab_index(ws_idx, tab_idx, "context menu pane tab");
                    assert!(
                        self.workspaces[ws_idx].tabs[tab_idx]
                            .panes
                            .contains_key(&pane_id),
                        "context menu pane references pane {:?} outside workspace {} tab {}",
                        pane_id,
                        ws_idx,
                        tab_idx
                    );
                    if let Some(source_pane_id) = source_pane_id {
                        assert_live_pane(source_pane_id, "context menu source pane");
                    }
                }
            }
        }
    }

    pub fn insert_test_runtime(
        &mut self,
        pane_id: crate::layout::PaneId,
        runtime: crate::terminal::TerminalRuntime,
    ) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.terminal_id(pane_id).is_some())
        {
            ws.insert_test_runtime(pane_id, runtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn work_projection_rotation_includes_missive_in_both_directions() {
        assert_eq!(
            WorkProjection::Tickets.rotate_right(),
            WorkProjection::Missive
        );
        assert_eq!(
            WorkProjection::Missive.rotate_right(),
            WorkProjection::Agents
        );
        assert_eq!(
            WorkProjection::Agents.rotate_left(),
            WorkProjection::Missive
        );
        assert_eq!(
            WorkProjection::Missive.rotate_left(),
            WorkProjection::Tickets
        );
        assert_eq!(
            WorkProjection::ReviewQueue.rotate_left(),
            WorkProjection::Agents
        );
    }

    #[test]
    fn symphony_refresh_preserves_selection_by_workflow_identity() {
        let workflow = |workflow_id: &str, run_id: &str| crate::symphony::Workflow {
            workflow_id: workflow_id.to_string(),
            run_id: run_id.to_string(),
            name: workflow_id.to_string(),
            phase: "running".to_string(),
            wait: None,
            started_at: None,
            ticket: None,
            repo: None,
            pr: None,
            receipts: None,
        };
        let mut detail = SymphonyDetail {
            snapshot: crate::symphony::Snapshot {
                workflows: vec![workflow("first", "run-1"), workflow("selected", "run-2")],
                unavailable: None,
            },
            selected: 1,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        };

        detail.replace_snapshot(crate::symphony::Snapshot {
            workflows: vec![workflow("selected", "run-2"), workflow("first", "run-1")],
            unavailable: None,
        });

        assert_eq!(detail.selected, 0);
        assert_eq!(
            detail.snapshot.workflows[detail.selected].workflow_id,
            "selected"
        );
        assert_eq!(detail.snapshot.workflows[detail.selected].run_id, "run-2");

        detail.selected = 1;
        detail.replace_snapshot(crate::symphony::Snapshot {
            workflows: vec![workflow("replacement", "run-3")],
            unavailable: None,
        });
        assert_eq!(detail.selected, 0);
    }

    #[test]
    fn agent_terminal_keeps_final_child_cursor_exposed() {
        let mut state = AppState::test_new();
        let ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        state.terminals.insert(
            ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
            crate::terminal::TerminalState::new(
                ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
                std::path::PathBuf::from("/tmp"),
            ),
        );
        state
            .terminals
            .get_mut(&ws.tabs[0].panes[&pane_id].attached_terminal_id)
            .expect("terminal state")
            .launch_argv = Some(vec!["codex".to_string()]);
        state.workspaces = vec![ws];

        assert!(state.pane_exposes_host_cursor(0, pane_id));
    }

    #[test]
    fn adversarial_identity_state_satisfies_app_invariants_after_mutation() {
        let mut state = AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();

        let ws = &mut state.workspaces[0];
        let active_public = ws.tabs[ws.active_tab].number;
        assert_ne!(ws.active_tab + 1, active_public);
        let new_pane = ws.test_split(ratatui::layout::Direction::Horizontal);
        assert!(ws.public_pane_number(new_pane).is_some());
        state.ensure_test_terminals();

        state.assert_invariants_for_test();
    }

    fn navigator_row_for_display(is_workspace: bool) -> NavigatorRow {
        NavigatorRow {
            target: NavigatorTarget::Workspace { ws_idx: 0 },
            depth: if is_workspace { 0 } else { 1 },
            label: String::new(),
            meta: String::new(),
            status: crate::detect::AgentState::Idle,
            seen: true,
            stale: false,
            is_current: false,
            is_workspace,
            is_tab: false,
            expanded: true,
            search_text: String::new(),
            matched: true,
        }
    }

    #[test]
    fn navigator_display_lines_separate_workspace_groups() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![
                NavigatorDisplayLine::Row(0),
                NavigatorDisplayLine::Row(1),
                NavigatorDisplayLine::Spacer,
                NavigatorDisplayLine::Row(2),
                NavigatorDisplayLine::Row(3),
            ]
        );
    }

    #[test]
    fn navigator_display_lines_have_no_leading_spacer() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![NavigatorDisplayLine::Row(0), NavigatorDisplayLine::Row(1)]
        );
        assert!(navigator_display_lines(&[]).is_empty());
    }

    #[test]
    fn navigator_display_index_maps_row_to_line() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        assert_eq!(navigator_display_index_of_row(&lines, 2), Some(3));
        assert_eq!(navigator_display_index_of_row(&lines, 9), None);
    }

    #[test]
    fn navigator_first_row_skips_spacer_lines() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        // Line 2 is the spacer before the second workspace.
        assert_eq!(navigator_first_row_at_or_after(&lines, 2), Some(2));
        assert_eq!(navigator_first_row_at_or_after(&lines, 4), None);
    }

    #[test]
    fn built_in_theme_names_resolve() {
        for name in THEME_NAMES {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn built_in_themes_leave_sidebar_background_unset() {
        for name in THEME_NAMES {
            let palette = Palette::from_name(name).unwrap();
            assert_eq!(
                palette.sidebar_bg, None,
                "built-in theme changed the sidebar background: {name}"
            );
        }
    }

    #[test]
    fn custom_sidebar_background_overrides_the_default() {
        let custom = crate::config::CustomThemeColors {
            sidebar_bg: Some("#181825".to_string()),
            ..Default::default()
        };

        assert_eq!(
            Palette::catppuccin().with_overrides(&custom).sidebar_bg,
            Some(Color::Rgb(24, 24, 37))
        );
    }

    /// `reset` is documented as a value for this token, and it means something
    /// an unset field cannot: hand the sidebar back to the terminal on a themed
    /// palette. Unset follows `panel_bg` instead, so the two cannot share a
    /// representation.
    #[test]
    fn an_explicit_reset_sidebar_background_still_inherits_the_terminal() {
        let custom = crate::config::CustomThemeColors {
            sidebar_bg: Some("reset".to_string()),
            ..Default::default()
        };
        let palette = Palette::catppuccin().with_overrides(&custom);

        assert_eq!(palette.sidebar_bg, Some(Color::Reset));
        assert_eq!(palette.sidebar_background(), Color::Reset);
        assert_eq!(
            Palette::catppuccin().sidebar_background(),
            Color::Rgb(24, 24, 37)
        );
    }

    /// Named and indexed colors are legal everywhere a hex color is, so a
    /// mismatch must still be classified when a palette carries them.
    #[test]
    fn appearance_classifies_named_and_indexed_panel_backgrounds() {
        use crate::terminal_theme::HostAppearance;

        let mut light = Palette::catppuccin();
        light.panel_bg = Color::White;
        assert_eq!(light.appearance(), Some(HostAppearance::Light));

        let mut dark = Palette::catppuccin_latte();
        dark.panel_bg = Color::Indexed(16);
        assert_eq!(dark.appearance(), Some(HostAppearance::Dark));

        let mut inherited = Palette::catppuccin();
        inherited.panel_bg = Color::Reset;
        assert_eq!(inherited.appearance(), None);
    }

    #[test]
    fn light_theme_aliases_resolve() {
        for name in ["light", "latte", "tokyo-day", "onelight", "lotus", "dawn"] {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn key_matches_requires_exact_modifiers() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        assert!(!key_matches(
            &KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
    }

    #[test]
    fn key_matches_letters_case_insensitively() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            KeyCode::Char('b'),
            KeyModifiers::SHIFT,
        ));
    }

    #[test]
    fn linked_worktree_context_menu_keeps_safe_close_and_explicit_remove() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: true,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "Delete worktree checkout..."]
        );
    }

    #[test]
    fn git_workspace_context_menu_keeps_remove_for_managed_worktrees_only() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "New worktree", "Open worktree..."]
        );
    }

    #[test]
    fn parent_worktree_context_menu_uses_repo_actions() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
            },
            x: 0,
            y: 0,
            list: MenuListState::new(0),
        };

        assert_eq!(
            menu.items(),
            &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Collapse"
            ]
        );
    }
}

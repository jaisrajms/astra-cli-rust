//! Stable layout rectangles + width policy (Plan §5.3). Ratatui-free on purpose: the same `Layout`
//! is held in app state (for mouse hit-testing) and converted to ratatui `Rect`s at render time.

/// The sidebar width in columns, and the terminal width at which it docks.
pub const SIDEBAR_WIDTH: u16 = 42;
pub const SIDEBAR_THRESHOLD: u16 = 120;

/// Fixed row counts of the vertical stack (top to bottom), excluding the flexible transcript and
/// the caller-supplied prompt rows.
const TITLE_ROWS: u16 = 3;
const TOOL_STATUS_ROWS: u16 = 1;
const AGENT_TABS_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;

/// A ratatui-free rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    /// Whether the cell (x, y) is inside this rectangle (exclusive of x+width/y+height).
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x
            && x < self.x.saturating_add(self.width)
            && y >= self.y
            && y < self.y.saturating_add(self.height)
    }
}

/// Which surface a terminal cell belongs to (for mouse hit-testing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Transcript,
    Sidebar,
    Prompt,
    None,
}

/// The computed layout for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub sidebar: Option<Rect>,
    pub transcript: Rect,
    pub prompt: Rect,
    pub footer: Rect,
}

impl Layout {
    /// Compute the layout for a terminal of `width` x `height`, given the prompt row count.
    pub fn compute(width: u16, height: u16, prompt_rows: u16) -> Layout {
        // A right-hand sidebar when the terminal is wide enough; otherwise the main column spans
        // the full width and there is no sidebar.
        let main_width = if width >= SIDEBAR_THRESHOLD {
            width.saturating_sub(SIDEBAR_WIDTH)
        } else {
            width
        };
        let sidebar = if width >= SIDEBAR_THRESHOLD {
            Some(Rect {
                x: main_width,
                y: 0,
                width: SIDEBAR_WIDTH,
                height,
            })
        } else {
            None
        };

        // Every fixed row other than the transcript: title + tool status + agent tabs + prompt +
        // footer. The transcript is the flexible remainder and shrinks first on a tiny terminal.
        let fixed_rows = TITLE_ROWS
            .saturating_add(TOOL_STATUS_ROWS)
            .saturating_add(AGENT_TABS_ROWS)
            .saturating_add(prompt_rows)
            .saturating_add(FOOTER_ROWS);
        let transcript_height = height.saturating_sub(fixed_rows);

        let transcript = Rect {
            x: 0,
            y: TITLE_ROWS,
            width: main_width,
            height: transcript_height,
        };

        let prompt_y =
            transcript_height.saturating_add(TITLE_ROWS + TOOL_STATUS_ROWS + AGENT_TABS_ROWS);
        let prompt = Rect {
            x: 0,
            y: prompt_y,
            width: main_width,
            height: prompt_rows,
        };
        let footer = Rect {
            x: 0,
            y: prompt_y.saturating_add(prompt_rows),
            width: main_width,
            height: FOOTER_ROWS,
        };

        Layout {
            sidebar,
            transcript,
            prompt,
            footer,
        }
    }

    /// Which surface contains the cell (x, y): sidebar first, then transcript, then prompt.
    pub fn hit_test(&self, x: u16, y: u16) -> Surface {
        if let Some(sidebar) = self.sidebar {
            if sidebar.contains(x, y) {
                return Surface::Sidebar;
            }
        }
        if self.transcript.contains(x, y) {
            Surface::Transcript
        } else if self.prompt.contains(x, y) {
            Surface::Prompt
        } else {
            Surface::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_layout_reserves_sidebar_on_right() {
        let layout = Layout::compute(120, 40, 3);
        let sidebar = layout.sidebar.expect("width 120 should dock the sidebar");
        assert_eq!(sidebar.x, 120 - 42);
        assert_eq!(sidebar.y, 0);
        assert_eq!(sidebar.width, 42);
        assert_eq!(sidebar.height, 40);
        assert_eq!(layout.transcript.width, 78);
        assert_eq!(layout.prompt.width, 78);
    }

    #[test]
    fn narrow_layout_has_no_sidebar() {
        let layout = Layout::compute(119, 40, 3);
        assert_eq!(layout.sidebar, None);
        assert_eq!(layout.transcript.width, 119);
    }

    #[test]
    fn prompt_rows_shrink_transcript_by_exactly_the_extra_rows() {
        let base = Layout::compute(80, 40, 3);
        let taller = Layout::compute(80, 40, 8);
        // prompt grew by 5 rows, so the transcript shrinks by exactly 5.
        assert_eq!(
            taller.transcript.height,
            base.transcript.height.saturating_sub(5)
        );
        assert_eq!(taller.transcript.height, 40 - 3 - 1 - 1 - 8 - 1);
        assert_eq!(base.prompt.height, 3);
        assert_eq!(taller.prompt.height, 8);
        // the prompt starts right after title + transcript + tool status + agent tabs.
        assert_eq!(base.prompt.y, 3 + base.transcript.height + 1 + 1);
        assert_eq!(base.footer.y, base.prompt.y + 3);
        assert_eq!(base.footer.height, 1);
    }

    #[test]
    fn hit_test_maps_cells_to_surfaces() {
        // width 120, height 40, prompt_rows 3: sidebar x 78..120, transcript y 3..34, prompt y 36..39.
        let layout = Layout::compute(120, 40, 3);
        assert_eq!(layout.hit_test(100, 10), Surface::Sidebar);
        assert_eq!(layout.hit_test(10, 10), Surface::Transcript);
        assert_eq!(layout.hit_test(10, 37), Surface::Prompt);
        // title (y 0..3), tool status, agent tabs, and footer (y 39) map to nothing.
        assert_eq!(layout.hit_test(10, 0), Surface::None);
        assert_eq!(layout.hit_test(10, 39), Surface::None);
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let layout = Layout::compute(20, 3, 12);
        assert_eq!(layout.sidebar, None);
        // fixed rows (3 + 1 + 1 + 12 + 1 = 18) exceed height 3, so the transcript collapses to 0.
        assert_eq!(layout.transcript.height, 0);
        assert_eq!(layout.prompt.height, 12);
        assert_eq!(layout.footer.height, 1);
        assert_eq!(layout.footer.y, layout.prompt.y + 12);
        // hit-testing a collapsed transcript never panics and never matches.
        assert_eq!(layout.hit_test(0, 0), Surface::None);
    }

    #[test]
    fn zero_terminal_does_not_panic() {
        let layout = Layout::compute(0, 0, 0);
        assert_eq!(layout.sidebar, None);
        assert_eq!(
            layout.transcript,
            Rect {
                x: 0,
                y: 3,
                width: 0,
                height: 0
            }
        );
        assert_eq!(layout.footer.height, 1);
        assert_eq!(layout.hit_test(0, 0), Surface::None);
    }

    #[test]
    fn rect_contains_is_exclusive() {
        let r = Rect {
            x: 2,
            y: 3,
            width: 3,
            height: 2,
        };
        assert!(r.contains(2, 3));
        assert!(r.contains(4, 4));
        assert!(!r.contains(5, 3)); // x + width is exclusive
        assert!(!r.contains(2, 5)); // y + height is exclusive
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn empty_rect_contains_nothing() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        assert!(!r.contains(0, 0));
    }
}

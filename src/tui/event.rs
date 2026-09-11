//! Focus-aware terminal-event routing (Plan §5.2).
//!
//! This module owns the **pure** half of input handling: [`route`] maps one crossterm [`Event`] plus
//! the current [`App`] state onto a side-effect-free [`UiCommand`]. The `mod.rs` event loop applies
//! the command (mutating `App` and sending daemon messages), so the routing is unit-testable without
//! a terminal, a daemon stream, or async.
//!
//! Popup precedence (§4.2) is encoded by match-arm order: palette, then mention, then a pending
//! permission/question/diff-review, then the prompt, then global keys.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::app::{App, Focus, Prompt, RowItem};
use super::layout::Surface;

/// A local UI command produced by routing one terminal event. `mod.rs` applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Noop,
    /// Scroll the transcript by `delta` rows (negative = up).
    ScrollTranscript(i32),
    /// Scroll the sidebar by `delta` rows.
    ScrollSidebar(i32),
    /// Jump to the top/bottom of the transcript.
    TranscriptStart,
    TranscriptEnd,
    /// Move focus to the given surface.
    Focus(Focus),
    /// Toggle the expansion of a tool/reasoning block.
    ToggleExpand(RowItem),
    /// Toggle the sidebar visibility (force-dock on narrow, or hide on wide).
    ToggleSidebar,
    // Prompt editing.
    Insert(char),
    Backspace,
    DeleteForward,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    // Prompt history recall.
    RecallOlder,
    RecallNewer,
    // Agent / theme.
    NextAgent,
    PrevAgent,
    NextTheme,
    // Command palette.
    PaletteToggle,
    PaletteClose,
    PaletteUp,
    PaletteDown,
    PaletteChoose,
    // @-mention popup.
    MentionOpen,
    MentionSelect,
    MentionUp,
    MentionDown,
    MentionDismiss,
    MentionBackspace,
    MentionChar(char),
    // Submit / resolve.
    Submit,
    ResolvePermission(bool),
    ResolveQuestion {
        answer: String,
    },
    ResolveDiffReviewAccept,
    ResolveDiffReviewReject,
    EditDiffReview,
    // Global.
    Quit,
    /// The terminal was resized (clamp scroll offsets).
    Resize,
}

/// Route one terminal event to a [`UiCommand`].
pub fn route(event: &Event, app: &App) -> UiCommand {
    match event {
        Event::Key(key) => route_key(key, app),
        Event::Mouse(mouse) => route_mouse(mouse, app),
        Event::Resize(_, _) => UiCommand::Resize,
        _ => UiCommand::Noop,
    }
}

fn route_key(key: &KeyEvent, app: &App) -> UiCommand {
    if key.kind == KeyEventKind::Release {
        return UiCommand::Noop;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => UiCommand::Quit,
        // Command palette (leader key Ctrl-X).
        KeyCode::Char('x') if ctrl => UiCommand::PaletteToggle,
        // Sidebar toggle (Ctrl-B).
        KeyCode::Char('b') if ctrl => UiCommand::ToggleSidebar,
        KeyCode::Esc if app.palette.is_some() => UiCommand::PaletteClose,
        KeyCode::Enter if app.palette.is_some() => UiCommand::PaletteChoose,
        KeyCode::Up if app.palette.is_some() => UiCommand::PaletteUp,
        KeyCode::Down if app.palette.is_some() => UiCommand::PaletteDown,
        // @-mention popup.
        KeyCode::Esc if app.mention.is_some() => UiCommand::MentionDismiss,
        KeyCode::Enter if app.mention.is_some() => UiCommand::MentionSelect,
        KeyCode::Tab if app.mention.is_some() => UiCommand::MentionSelect,
        KeyCode::Up if app.mention.is_some() => UiCommand::MentionUp,
        KeyCode::Down if app.mention.is_some() => UiCommand::MentionDown,
        KeyCode::Backspace if app.mention.is_some() => UiCommand::MentionBackspace,
        KeyCode::Char(c) if app.mention.is_some() => UiCommand::MentionChar(c),
        // Transcript focus: scroll keys route to the transcript; Esc returns focus to the prompt.
        KeyCode::Up if app.ui.focus == Focus::Transcript => UiCommand::ScrollTranscript(-1),
        KeyCode::Down if app.ui.focus == Focus::Transcript => UiCommand::ScrollTranscript(1),
        KeyCode::PageUp => UiCommand::ScrollTranscript(-page_size(app)),
        KeyCode::PageDown => UiCommand::ScrollTranscript(page_size(app)),
        KeyCode::Home if app.ui.focus == Focus::Transcript => UiCommand::TranscriptStart,
        KeyCode::End if app.ui.focus == Focus::Transcript => UiCommand::TranscriptEnd,
        KeyCode::Esc if matches!(app.ui.focus, Focus::Transcript | Focus::Sidebar) => {
            UiCommand::Focus(Focus::Prompt)
        }
        // A pending interaction consumes Enter/Esc before the composer can see them.
        KeyCode::Esc => match &app.pending {
            Some(Prompt::Permission { .. }) => UiCommand::ResolvePermission(false),
            Some(Prompt::Question { .. }) => UiCommand::ResolveQuestion {
                answer: String::new(),
            },
            Some(Prompt::DiffReview { .. }) => UiCommand::ResolveDiffReviewReject,
            None => UiCommand::Quit,
        },
        KeyCode::Enter => match &app.pending {
            Some(Prompt::Permission { .. }) => UiCommand::ResolvePermission(true),
            Some(Prompt::Question { .. }) => UiCommand::ResolveQuestion {
                answer: app.input.trim().to_string(),
            },
            Some(Prompt::DiffReview { .. }) => UiCommand::ResolveDiffReviewAccept,
            None => UiCommand::Submit,
        },
        KeyCode::Char('y') if matches!(app.pending, Some(Prompt::Permission { .. })) => {
            UiCommand::ResolvePermission(true)
        }
        KeyCode::Char('n') if matches!(app.pending, Some(Prompt::Permission { .. })) => {
            UiCommand::ResolvePermission(false)
        }
        KeyCode::Char('e') if matches!(app.pending, Some(Prompt::DiffReview { .. })) => {
            UiCommand::EditDiffReview
        }
        // Match the reference CLI: `q` quits only when the prompt is empty.
        KeyCode::Char('q') if app.input.is_empty() && app.pending.is_none() => UiCommand::Quit,
        KeyCode::Tab if app.pending.is_none() => UiCommand::NextAgent,
        KeyCode::BackTab if app.pending.is_none() => UiCommand::PrevAgent,
        KeyCode::Up if app.pending.is_none() => UiCommand::RecallOlder,
        KeyCode::Down if app.pending.is_none() => UiCommand::RecallNewer,
        KeyCode::F(2) if app.pending.is_none() => UiCommand::NextTheme,
        KeyCode::Left => UiCommand::CursorLeft,
        KeyCode::Right => UiCommand::CursorRight,
        KeyCode::Home => UiCommand::CursorHome,
        KeyCode::End => UiCommand::CursorEnd,
        KeyCode::Backspace => UiCommand::Backspace,
        KeyCode::Delete => UiCommand::DeleteForward,
        KeyCode::Char('/') if app.input.is_empty() && app.pending.is_none() => {
            UiCommand::PaletteToggle
        }
        KeyCode::Char('@')
            if app.pending.is_none() && app.palette.is_none() && app.mention.is_none() =>
        {
            UiCommand::MentionOpen
        }
        KeyCode::Char(c) => UiCommand::Insert(c),
        _ => UiCommand::Noop,
    }
}

fn route_mouse(mouse: &MouseEvent, app: &App) -> UiCommand {
    let surface = app
        .ui
        .layout
        .map(|l| l.hit_test(mouse.column, mouse.row))
        .unwrap_or(Surface::None);
    match mouse.kind {
        // Wheel scrolls the surface under the cursor (over a popup/prompt it is consumed, not
        // routed to the transcript).
        MouseEventKind::ScrollUp => match surface {
            Surface::Sidebar => UiCommand::ScrollSidebar(-3),
            Surface::Transcript => UiCommand::ScrollTranscript(-3),
            _ => UiCommand::Noop,
        },
        MouseEventKind::ScrollDown => match surface {
            Surface::Sidebar => UiCommand::ScrollSidebar(3),
            Surface::Transcript => UiCommand::ScrollTranscript(3),
            _ => UiCommand::Noop,
        },
        // Left click: over a tool/reasoning row it toggles expansion; otherwise it moves focus.
        MouseEventKind::Down(MouseButton::Left) => {
            if surface == Surface::Transcript {
                if let Some(layout) = app.ui.layout {
                    // The transcript inner area starts one row below the block border.
                    let k = mouse.row.saturating_sub(layout.transcript.y + 1);
                    if let Some(Some(item)) = app.ui.visible_item_ids.get(k as usize) {
                        return UiCommand::ToggleExpand(item.clone());
                    }
                }
                UiCommand::Focus(Focus::Transcript)
            } else {
                match surface {
                    Surface::Sidebar => UiCommand::Focus(Focus::Sidebar),
                    Surface::Prompt => UiCommand::Focus(Focus::Prompt),
                    _ => UiCommand::Noop,
                }
            }
        }
        _ => UiCommand::Noop,
    }
}

/// One page of transcript scroll: the measured viewport height (at least one row).
fn page_size(app: &App) -> i32 {
    (app.ui.transcript.viewport.max(1)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::Prompt;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn app_with_pending(pending: Prompt) -> App {
        let mut app = App::new(vec!["build".into()]);
        app.pending = Some(pending);
        app
    }

    fn permission() -> Prompt {
        Prompt::Permission {
            id: "p".into(),
            tool: "Bash".into(),
            summary: "run".into(),
        }
    }

    fn question() -> Prompt {
        Prompt::Question {
            id: "q".into(),
            question: "Which?".into(),
            options: vec!["A".into(), "B".into()],
        }
    }

    fn diff_review() -> Prompt {
        Prompt::DiffReview {
            id: "d".into(),
            tool_name: "Write".into(),
            file_path: "x".into(),
            before: String::new(),
            after: "hi".into(),
            unified_diff: "+hi\n".into(),
        }
    }

    #[test]
    fn popup_precedence_consumes_enter_and_esc() {
        // Palette open: Enter/Esc/Up/Down route to the palette, not submit/quit.
        let mut app = App::new(vec![]);
        app.palette = Some(0);
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &app),
            UiCommand::PaletteChoose
        );
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &app),
            UiCommand::PaletteClose
        );

        // Mention open: Enter selects, Esc dismisses, a char narrows the mention.
        app.palette = None;
        app.mention = Some(crate::tui::app::Mention {
            trigger: 0,
            query: String::new(),
            items: vec![],
            selection: 0,
        });
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &app),
            UiCommand::MentionSelect
        );
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &app),
            UiCommand::MentionDismiss
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('a'), KeyModifiers::NONE), &app),
            UiCommand::MentionChar('a')
        );
    }

    #[test]
    fn pending_interaction_resolves_before_submit() {
        let perm = app_with_pending(permission());
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &perm),
            UiCommand::ResolvePermission(true)
        );
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &perm),
            UiCommand::ResolvePermission(false)
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &perm),
            UiCommand::ResolvePermission(true)
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &perm),
            UiCommand::ResolvePermission(false)
        );

        let mut q = app_with_pending(question());
        q.push_char('B');
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &q),
            UiCommand::ResolveQuestion { answer: "B".into() }
        );
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &q),
            UiCommand::ResolveQuestion {
                answer: String::new()
            }
        );

        let d = app_with_pending(diff_review());
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &d),
            UiCommand::ResolveDiffReviewAccept
        );
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &d),
            UiCommand::ResolveDiffReviewReject
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('e'), KeyModifiers::NONE), &d),
            UiCommand::EditDiffReview
        );
    }

    #[test]
    fn prompt_and_global_keys() {
        let app = App::new(vec!["build".into()]);
        assert_eq!(
            route_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), &app),
            UiCommand::Quit
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('q'), KeyModifiers::NONE), &app),
            UiCommand::Quit
        );
        assert_eq!(
            route_key(&key(KeyCode::Char('h'), KeyModifiers::NONE), &app),
            UiCommand::Insert('h')
        );
        assert_eq!(
            route_key(&key(KeyCode::Enter, KeyModifiers::NONE), &app),
            UiCommand::Submit
        );
        assert_eq!(
            route_key(&key(KeyCode::Tab, KeyModifiers::NONE), &app),
            UiCommand::NextAgent
        );
        assert_eq!(
            route_key(&key(KeyCode::Up, KeyModifiers::NONE), &app),
            UiCommand::RecallOlder
        );

        // `q` with non-empty input types, does not quit.
        let mut typed = App::new(vec!["build".into()]);
        typed.push_char('q');
        assert_eq!(
            route_key(&key(KeyCode::Char('q'), KeyModifiers::NONE), &typed),
            UiCommand::Insert('q')
        );
    }

    #[test]
    fn mouse_wheel_routes_by_cursor_position() {
        use crate::tui::layout::Layout;

        let mut app = App::new(vec![]);
        app.ui.layout = Some(Layout::compute(120, 40, 3));

        let at = |column: u16, row: u16, kind| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };

        // Over the transcript (x=10, y=10).
        assert_eq!(
            route_mouse(&at(10, 10, MouseEventKind::ScrollUp), &app),
            UiCommand::ScrollTranscript(-3)
        );
        assert_eq!(
            route_mouse(&at(10, 10, MouseEventKind::ScrollDown), &app),
            UiCommand::ScrollTranscript(3)
        );
        // Over the sidebar (x=100, y=10) the wheel scrolls the sidebar instead.
        assert_eq!(
            route_mouse(&at(100, 10, MouseEventKind::ScrollUp), &app),
            UiCommand::ScrollSidebar(-3)
        );
        assert_eq!(
            route_mouse(&at(100, 10, MouseEventKind::ScrollDown), &app),
            UiCommand::ScrollSidebar(3)
        );
        // Over the prompt (x=10, y=37) the wheel is consumed, not routed.
        assert_eq!(
            route_mouse(&at(10, 37, MouseEventKind::ScrollUp), &app),
            UiCommand::Noop
        );

        // Left click focuses the surface under the cursor.
        assert_eq!(
            route_mouse(&at(10, 10, MouseEventKind::Down(MouseButton::Left)), &app),
            UiCommand::Focus(Focus::Transcript)
        );
        assert_eq!(
            route_mouse(&at(100, 10, MouseEventKind::Down(MouseButton::Left)), &app),
            UiCommand::Focus(Focus::Sidebar)
        );
        assert_eq!(
            route_mouse(&at(10, 37, MouseEventKind::Down(MouseButton::Left)), &app),
            UiCommand::Focus(Focus::Prompt)
        );
    }

    #[test]
    fn transcript_focus_scroll_keys_route_to_scroll() {
        use crate::tui::layout::Layout;

        let mut app = App::new(vec![]);
        app.ui.layout = Some(Layout::compute(120, 40, 3));
        app.ui.transcript.viewport = 20;
        app.ui.focus = Focus::Transcript;

        assert_eq!(
            route_key(&key(KeyCode::Up, KeyModifiers::NONE), &app),
            UiCommand::ScrollTranscript(-1)
        );
        assert_eq!(
            route_key(&key(KeyCode::Down, KeyModifiers::NONE), &app),
            UiCommand::ScrollTranscript(1)
        );
        assert_eq!(
            route_key(&key(KeyCode::PageDown, KeyModifiers::NONE), &app),
            UiCommand::ScrollTranscript(20)
        );
        assert_eq!(
            route_key(&key(KeyCode::Home, KeyModifiers::NONE), &app),
            UiCommand::TranscriptStart
        );
        assert_eq!(
            route_key(&key(KeyCode::End, KeyModifiers::NONE), &app),
            UiCommand::TranscriptEnd
        );
        // Esc returns focus to the prompt (instead of quitting).
        assert_eq!(
            route_key(&key(KeyCode::Esc, KeyModifiers::NONE), &app),
            UiCommand::Focus(Focus::Prompt)
        );

        // In prompt focus the same keys retain their prompt meaning.
        app.ui.focus = Focus::Prompt;
        assert_eq!(
            route_key(&key(KeyCode::Up, KeyModifiers::NONE), &app),
            UiCommand::RecallOlder
        );
        assert_eq!(
            route_key(&key(KeyCode::Home, KeyModifiers::NONE), &app),
            UiCommand::CursorHome
        );
    }

    #[test]
    fn resize_routes_to_resize() {
        let app = App::new(vec![]);
        assert_eq!(route(&Event::Resize(80, 24), &app), UiCommand::Resize);
    }
}

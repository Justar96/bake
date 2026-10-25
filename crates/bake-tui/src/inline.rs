//! Output ownership for the normal-screen preview. Ratatui's inline resize
//! clears the whole screen on width shrink, so this writer uses fixed frames
//! and never calls its resize operation. Presentation stays in the view crate.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicU16, Ordering};

use bake_tui_view::frame::FrameStyle;
use bake_tui_view::render::{inline_height, render_inline};
use bake_tui_view::state::State;
use bake_tui_view::transcript::{self, Look};
use crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Attribute, Print, PrintStyledContent, ResetColor, SetAttribute};
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::{CrosstermBackend, IntoCrossterm};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::{Terminal, TerminalOptions, Viewport};

#[derive(Default)]
pub(crate) struct Inline {
    published: usize,
    /// The transcript's [`State::transcript_epoch`] `published` counts in.
    /// A replaced transcript is written after what scrollback already
    /// holds, from its first row; nothing already written is erased.
    epoch: u64,
    held: u16,
    /// Whether the last frame held a dialog, an approval request, or an
    /// open tray, whose closing shrinks the held region
    /// ([`Inline::reanchor`]).
    panel: bool,
    initialized: bool,
}

pub(crate) fn fixed(area: Rect) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )
}

impl Inline {
    pub(crate) fn draw(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        state: &mut State,
    ) -> io::Result<u16> {
        let (width, height) = crossterm::terminal::size()?;
        if width == 0 || height == 0 {
            return Ok(self.held);
        }
        if !self.initialized {
            // Move pre-launch shell rows into scrollback before owning any
            // screen cells. Neither screen nor scrollback is erased.
            execute!(
                terminal.backend_mut(),
                Print("\r\n".repeat(usize::from(height)))
            )?;
            self.initialized = true;
        }
        let old = self.held.min(height);
        if old > 0 {
            // The terminal relocates its saved cursor with normal-screen
            // reflow. This boundary follows the owned UI even when a resize
            // burst finishes at the original dimensions.
            execute!(
                terminal.backend_mut(),
                RestorePosition,
                Clear(ClearType::FromCursorDown)
            )?;
        }
        let replaced = self.epoch != state.transcript_epoch;
        if replaced {
            self.epoch = state.transcript_epoch;
            self.published = 0;
        }
        let look = Look {
            tones: state.tones,
            classic: state.frame == FrameStyle::Classic,
            lit: true,
        };
        // The scrollbar's reserved column matches the existing transcript
        // component; presenting against the whole sequence keeps separators.
        let columns = usize::from(width.saturating_sub(u16::from(width > 20)));
        let wanted = inline_height(state, width, height).min(height);
        // An open tray grows the held region as a dialog does, so its
        // closing redraws the transcript into the rows it frees.
        let panel = state.dialog.is_some()
            || state.approval().is_some()
            || state.focus != bake_tui_view::state::Focus::Composer;
        let closed = std::mem::replace(&mut self.panel, panel) && !panel;
        let mut old = old;
        if closed && wanted < old {
            if replaced {
                // The rows on screen belong to the session just replaced,
                // which the state no longer holds: they scroll into
                // scrollback whole, and the new session's rows are
                // published against the controls.
                crate::terminal::HELD_ROWS.store(0, Ordering::SeqCst);
                execute!(
                    terminal.backend_mut(),
                    MoveTo(0, height - 1),
                    Print("\r\n".repeat(usize::from(height))),
                    MoveTo(0, height - wanted),
                    SavePosition
                )?;
            } else {
                self.reanchor(terminal, state, width, height - wanted, columns, look)?;
            }
            old = wanted;
        }
        let held = wanted.max(old);
        let settled = state.settled_rows();
        let committing = self.published < settled;
        // Cleared UI cells are blank. During publishing, transcript output
        // is never owned by restoration as a transient held region.
        crate::terminal::HELD_ROWS.store(if committing { 0 } else { old }, Ordering::SeqCst);
        publish(
            &mut self.published,
            &crate::terminal::HELD_ROWS,
            settled,
            |index| {
                for line in transcript::present(&state.transcript.rows, index, columns, look) {
                    write_line(terminal.backend_mut(), &line, width)?;
                }
                Ok(())
            },
        )?;
        if committing || old == 0 {
            execute!(terminal.backend_mut(), SavePosition)?;
        } else {
            execute!(terminal.backend_mut(), RestorePosition)?;
        }
        // Reserving from the saved boundary scrolls only the rows needed to
        // fit the held region; its ordinary redraw leaves scrollback still.
        let reserve = held.saturating_sub(1);
        reserve_owned(&crate::terminal::HELD_ROWS, 1, reserve, || {
            execute!(terminal.backend_mut(), Print("\r\n"))
        })?;
        // The save slot is the output owner's boundary, independent of the
        // visible editing caret; cleanup restores this same boundary.
        execute!(
            terminal.backend_mut(),
            MoveTo(0, height - held),
            SavePosition
        )?;
        self.held = held;
        *terminal = fixed(Rect::new(0, height - held, width, held))?;
        crate::terminal::HELD_ROWS.store(held, Ordering::SeqCst);
        terminal.draw(|frame| render_inline(state, frame, self.published, height))?;
        Ok(held)
    }
}

impl Inline {
    /// The TypeScript `useRepaint` for a closing sheet: the held region
    /// grew for a dialog or an approval, which scrolled the transcript
    /// above it up; the terminal cannot pull those rows back down, so
    /// shrinking the region alone would leave its freed rows as a blank gap
    /// over the controls. Instead the visible rows above the new region,
    /// `top` of them, are written again with the published transcript's
    /// last lines, blank above a short one, so the newest row sits against
    /// the controls. Only the visible screen is rewritten: rows already in
    /// scrollback, including the shell's, stay as they are. The boundary
    /// is saved at the new region's top.
    fn reanchor(
        &self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        state: &State,
        width: u16,
        top: u16,
        columns: usize,
        look: Look,
    ) -> io::Result<()> {
        // Transcript cells only, while they are written.
        crate::terminal::HELD_ROWS.store(0, Ordering::SeqCst);
        let room = usize::from(top);
        let published = self.published.min(state.transcript.rows.len());
        let mut tail: Vec<Line<'static>> = Vec::new();
        for index in (0..published).rev() {
            if tail.len() >= room {
                break;
            }
            let mut lines = transcript::present(&state.transcript.rows, index, columns, look);
            lines.extend(tail);
            tail = lines;
        }
        let skip = tail.len().saturating_sub(room);
        let tail = &tail[skip..];
        let blank = room - tail.len();
        let out = terminal.backend_mut();
        execute!(out, MoveTo(0, 0))?;
        for _ in 0..blank {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                ResetColor,
                Clear(ClearType::CurrentLine),
                Print("\r\n")
            )?;
        }
        for line in tail {
            write_line(out, line, width)?;
        }
        execute!(out, MoveTo(0, top), SavePosition)
    }
}

/// Writes a width-bounded presentation line at the current cursor, advancing
/// by CRLF. Ratatui owns grapheme clipping and style composition; this writer
/// skips wide-cell fillers and uses its Crossterm style conversion unchanged.
fn write_line(out: &mut impl Write, line: &Line<'_>, width: u16) -> io::Result<()> {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    buffer.set_line(0, 0, line, width);
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        ResetColor,
        Clear(ClearType::CurrentLine)
    )?;
    let used = line.width().min(usize::from(width));
    let mut column = 0;
    while column < used {
        let cell = &buffer[(column as u16, 0)];
        let style: crossterm::style::ContentStyle = cell.style().into_crossterm();
        queue!(out, PrintStyledContent(style.apply(cell.symbol())))?;
        column += bake_tui_view::editor::cell_width(cell.symbol()).max(1);
    }
    execute!(
        out,
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n")
    )
}

/// A failed row is never marked as published. The caller exits on an output
/// failure rather than retrying a partly written row on the same terminal.
fn publish(
    published: &mut usize,
    owned: &AtomicU16,
    settled: usize,
    mut write: impl FnMut(usize) -> io::Result<()>,
) -> io::Result<()> {
    if *published < settled {
        owned.store(0, Ordering::SeqCst);
    }
    for index in *published..settled {
        write(index)?;
        *published = index + 1;
    }
    Ok(())
}

/// Claims only blank rows whose reservation finished. A partial write can
/// leave additional blank cells, but never puts transient UI in those cells.
fn reserve_owned(
    owned: &AtomicU16,
    mut reserved: u16,
    count: u16,
    mut newline: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    owned.store(reserved, Ordering::SeqCst);
    for _ in 0..count {
        newline()?;
        reserved += 1;
        owned.store(reserved, Ordering::SeqCst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line writer emits only SGR and line erase commands besides text.
    fn text_only(bytes: &[u8]) -> String {
        let ansi = std::str::from_utf8(bytes).unwrap();
        let mut chars = ansi.chars();
        let mut text = String::new();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                assert_eq!(chars.next(), Some('['));
                let command = chars.find(|ch| ch.is_ascii_alphabetic()).unwrap();
                assert!(
                    matches!(command, 'm' | 'K'),
                    "unexpected positioning command {command}"
                );
            } else {
                text.push(ch);
            }
        }
        text
    }

    #[test]
    fn committed_batches_keep_the_transcripts_own_separator() {
        let rows = [
            transcript::Row::Answer("previous".into()),
            transcript::Row::User("next".into()),
        ];
        let look = Look {
            tones: bake_tui_view::activity::Tones::None,
            classic: true,
            lit: true,
        };
        let mut output = Vec::new();
        for index in 0..rows.len() {
            for line in transcript::present(&rows, index, 79, look) {
                write_line(&mut output, &line, 80).unwrap();
            }
        }
        assert_eq!(
            text_only(&output),
            format!("  previous\r\n\r\n{}\r\n> next\r\n", "-".repeat(79))
        );
    }

    #[test]
    fn relative_lines_preserve_styles_and_graphemes_without_wide_fillers() {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::Span;
        let style = Style::new()
            .fg(Color::Rgb(1, 2, 3))
            .bg(Color::Rgb(4, 5, 6))
            .add_modifier(Modifier::BOLD | Modifier::DIM | Modifier::REVERSED);
        let line = Line::from(vec![
            Span::styled("界🙂e\u{301}", style),
            Span::raw(" plain"),
        ]);
        let mut output = Vec::new();
        write_line(&mut output, &line, 80).unwrap();
        assert_eq!(text_only(&output), "界🙂e\u{301} plain\r\n");
        let ansi = String::from_utf8(output).unwrap();
        for rendition in ["[1m", "[2m", "[7m"] {
            assert!(ansi.contains(rendition), "missing {rendition}: {ansi:?}");
        }
        // Crossterm applies NO_COLOR process-wide. Respect that policy
        // without changing global state shared by concurrent tests.
        let foreground =
            crossterm::style::SetForegroundColor(crossterm::style::Color::Rgb { r: 1, g: 2, b: 3 })
                .to_string();
        let background =
            crossterm::style::SetBackgroundColor(crossterm::style::Color::Rgb { r: 4, g: 5, b: 6 })
                .to_string();
        assert!(ansi.contains(&foreground));
        assert!(ansi.contains(&background));
        let mut clipped = Vec::new();
        write_line(&mut clipped, &line, 4).unwrap();
        assert_eq!(text_only(&clipped), "界🙂\r\n");
    }

    #[test]
    fn output_failure_keeps_the_failed_row_unpublished() {
        let mut published = 2;
        let mut written = Vec::new();
        let owned = AtomicU16::new(8);
        let error = publish(&mut published, &owned, 5, |index| {
            assert_eq!(owned.load(Ordering::SeqCst), 0);
            if index == 3 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
            }
            written.push(index);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(published, 3);
        assert_eq!(written, [2]);
        assert_eq!(owned.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn partial_reservation_owns_only_successful_blank_rows() {
        let owned = AtomicU16::new(0);
        let mut calls = 0;
        let error = reserve_owned(&owned, 1, 4, || {
            calls += 1;
            if calls == 2 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(owned.load(Ordering::SeqCst), 2);
        assert_eq!(calls, 2);
    }

    #[test]
    fn reservation_fits_the_largest_terminal_height() {
        let owned = AtomicU16::new(0);
        reserve_owned(&owned, 1, u16::MAX - 1, || Ok(())).unwrap();
        assert_eq!(owned.load(Ordering::SeqCst), u16::MAX);
    }

    #[test]
    fn redraws_and_navigation_do_not_republish_settled_rows() {
        let mut published = 0;
        let mut written = Vec::new();
        for settled in [2, 2, 2, 5, 5] {
            publish(&mut published, &AtomicU16::new(0), settled, |index| {
                written.push(index);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(written, [0, 1, 2, 3, 4]);
    }
}

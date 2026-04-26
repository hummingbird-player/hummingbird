// adapted from https://github.com/zed-industries/zed/blob/main/crates/gpui/examples/input.rs

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollHandle, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    actions, div, fill, hsla, point, prelude::*, px, relative, size,
};
use unicode_segmentation::*;

use crate::ui::{global_actions::PlayPause, theme::Theme};

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Next,
        Previous,
        Accept
    ]
);

fn next_word_boundary(content: &str, offset: usize) -> usize {
    for (start, segment) in content.split_word_bound_indices() {
        if segment.chars().all(|c| c.is_whitespace()) {
            continue;
        }
        let end = start + segment.len();
        if end > offset {
            return end;
        }
    }
    content.len()
}

fn previous_word_boundary(content: &str, offset: usize) -> usize {
    let mut last_start = 0;
    for (start, segment) in content.split_word_bound_indices() {
        if segment.chars().all(|c| c.is_whitespace()) {
            continue;
        }
        if start >= offset {
            return last_start;
        }
        last_start = start;
    }
    last_start
}

fn word_range_for_offset(content: &str, offset: usize) -> Range<usize> {
    if content.is_empty() {
        return 0..0;
    }

    let mut last_before: Option<Range<usize>> = None;

    for (start, segment) in content.split_word_bound_indices() {
        if segment.chars().all(|c| c.is_whitespace()) {
            continue;
        }
        let end = start + segment.len();
        if start <= offset && offset < end {
            return start..end;
        }
        if end <= offset {
            last_before = Some(start..end);
        } else {
            return last_before.unwrap_or(start..end);
        }
    }

    last_before.unwrap_or(0..0)
}

#[derive(Copy, Clone, Default, PartialEq)]
enum SelectionDragMode {
    #[default]
    Char,
    Word,
    All,
}

#[derive(Copy, Clone)]
pub enum EnrichedInputAction {
    Next,
    Previous,
    Accept,
}

type EnrichedInputHandler = Box<dyn Fn(EnrichedInputAction, &mut Window, &mut App)>;

pub struct TextInput {
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    pub content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    selection_drag_mode: SelectionDragMode,
    word_drag_anchor: usize,
    word_drag_anchor_range: Range<usize>,
    enriched_input_handler: Option<EnrichedInputHandler>,
}

impl EventEmitter<String> for TextInput {}

impl TextInput {
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let target = previous_word_boundary(&self.content, self.cursor_offset());
        self.select_to(target, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let target = next_word_boundary(&self.content, self.cursor_offset());
        self.select_to(target, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.index_for_mouse_position(event.position);
        self.is_selecting = true;

        match event.click_count {
            2 => {
                let word_range = self.word_range_for_offset(offset);
                if event.modifiers.shift {
                    let anchor = if self.selection_reversed {
                        self.selected_range.end
                    } else {
                        self.selected_range.start
                    };
                    let cursor_target = if word_range.end <= anchor {
                        word_range.start
                    } else if word_range.start >= anchor || offset >= anchor {
                        word_range.end
                    } else {
                        word_range.start
                    };
                    self.select_to(cursor_target, cx);
                    self.word_drag_anchor = anchor;
                    self.word_drag_anchor_range = self.word_range_for_offset(anchor);
                } else {
                    self.move_to(word_range.start, cx);
                    self.select_to(word_range.end, cx);
                    self.word_drag_anchor = offset;
                    self.word_drag_anchor_range = word_range;
                }
                self.selection_drag_mode = SelectionDragMode::Word;
            }
            3 => {
                self.move_to(0, cx);
                self.select_to(self.content.len(), cx);
                self.selection_drag_mode = SelectionDragMode::All;
            }
            _ => {
                self.selection_drag_mode = SelectionDragMode::Char;
                if event.modifiers.shift {
                    self.select_to(offset, cx);
                } else {
                    self.move_to(offset, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.selection_drag_mode = SelectionDragMode::Char;
        self.word_drag_anchor_range = 0..0;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }

        let offset = self.index_for_mouse_position(event.position);

        match self.selection_drag_mode {
            SelectionDragMode::Char => self.select_to(offset, cx),
            SelectionDragMode::Word => {
                let current_word = self.word_range_for_offset(offset);
                let new_range = if offset >= self.word_drag_anchor {
                    self.word_drag_anchor_range.start..current_word.end
                } else {
                    current_word.start..self.word_drag_anchor_range.end
                };
                let new_reversed = offset < self.word_drag_anchor;

                if self.selected_range != new_range || self.selection_reversed != new_reversed {
                    self.selected_range = new_range;
                    self.selection_reversed = new_reversed;
                    cx.notify();
                }
            }
            SelectionDragMode::All => {}
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\n", " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn word_range_for_offset(&self, offset: usize) -> Range<usize> {
        word_range_for_offset(&self.content, offset)
    }

    pub fn reset(&mut self) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_bounds = None;
        self.is_selecting = false;
        self.selection_drag_mode = SelectionDragMode::Char;
        self.word_drag_anchor = 0;
        self.word_drag_anchor_range = 0..0;
    }

    fn space(&mut self, _: &PlayPause, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, " ", window, cx)
    }

    pub fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handler) = self.enriched_input_handler.as_mut() else {
            return;
        };
        handler(EnrichedInputAction::Next, window, cx);
    }

    pub fn previous(&mut self, _: &Previous, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handler) = self.enriched_input_handler.as_mut() else {
            return;
        };
        handler(EnrichedInputAction::Previous, window, cx);
    }

    pub fn accept(&mut self, _: &Accept, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handler) = self.enriched_input_handler.as_mut() else {
            return;
        };
        handler(EnrichedInputAction::Accept, window, cx);
    }

    pub fn set_value(&mut self, cx: &mut Context<Self>, value: SharedString) {
        self.content = value;
        self.move_to(self.content.len(), cx);
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();

        cx.emit(self.content.to_string());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;

        assert_eq!(last_layout.text, self.content);
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), hsla(0., 0., 0., 0.2))
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let theme = cx.global::<Theme>();

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(1.), bounds.bottom() - bounds.top()),
                    ),
                    theme.caret_color,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    theme.text_highlight_background,
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(
            bounds.origin,
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl TextInput {
    pub fn new(
        cx: &mut App,
        focus_handle: FocusHandle,
        content: Option<SharedString>,
        placeholder: Option<SharedString>,
        enriched_input_handler: Option<EnrichedInputHandler>,
    ) -> Entity<TextInput> {
        cx.new(|_| TextInput {
            focus_handle,
            content: content.unwrap_or_else(|| "".into()),
            placeholder: placeholder.unwrap_or_else(|| "".into()),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            selection_drag_mode: SelectionDragMode::Char,
            word_drag_anchor: 0,
            word_drag_anchor_range: 0..0,
            scroll_handle: ScrollHandle::new(),
            enriched_input_handler,
        })
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::space))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::accept))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(
                div()
                    .id(("textinput", cx.entity_id()))
                    .overflow_x_scroll()
                    .track_scroll(&self.scroll_handle)
                    .w_full()
                    .pr(px(2.0))
                    .pb(px(2.0))
                    .child(TextElement {
                        input: cx.entity().clone(),
                    }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_range_empty_content() {
        assert_eq!(word_range_for_offset("", 0), 0..0);
    }

    #[test]
    fn word_range_single_word_at_start() {
        assert_eq!(word_range_for_offset("hello", 0), 0..5);
    }

    #[test]
    fn word_range_single_word_at_end() {
        assert_eq!(word_range_for_offset("hello", 4), 0..5);
    }

    #[test]
    fn word_range_single_word_at_boundary() {
        assert_eq!(word_range_for_offset("hello", 5), 0..5);
    }

    #[test]
    fn word_range_two_words_inside_first() {
        assert_eq!(word_range_for_offset("hello world", 2), 0..5);
    }

    #[test]
    fn word_range_two_words_in_space_prefers_preceding() {
        assert_eq!(word_range_for_offset("hello world", 5), 0..5);
    }

    #[test]
    fn word_range_two_words_inside_second() {
        assert_eq!(word_range_for_offset("hello world", 8), 6..11);
    }

    #[test]
    fn word_range_two_words_at_second_start() {
        assert_eq!(word_range_for_offset("hello world", 6), 6..11);
    }

    #[test]
    fn word_range_multiple_spaces_prefers_preceding() {
        assert_eq!(word_range_for_offset("hello   world", 7), 0..5);
    }

    #[test]
    fn word_range_offset_before_first_word() {
        assert_eq!(word_range_for_offset("  hello", 0), 2..7);
    }

    #[test]
    fn word_range_only_whitespace() {
        assert_eq!(word_range_for_offset("   ", 1), 0..0);
    }

    #[test]
    fn word_range_only_punctuation() {
        // Each "!" is a separate segment per UAX#29 word boundaries
        assert_eq!(word_range_for_offset("!!!", 1), 1..2);
    }

    #[test]
    fn word_range_punctuation_between_words() {
        assert_eq!(word_range_for_offset("hello, world", 5), 5..6);
    }

    #[test]
    fn word_range_punctuation_before_word() {
        assert_eq!(word_range_for_offset("hello, world", 6), 5..6);
    }

    #[test]
    fn word_range_unicode_emoji() {
        let content = "hello 🌍 world";
        let emoji_start = "hello ".len();
        let emoji_end = emoji_start + "🌍".len();
        let world_start = emoji_end + " ".len();
        let world_end = world_start + "world".len();

        assert_eq!(word_range_for_offset(content, 0), 0..5);
        assert_eq!(
            word_range_for_offset(content, emoji_start),
            emoji_start..emoji_end
        );
        assert_eq!(
            word_range_for_offset(content, world_start),
            world_start..world_end
        );
    }

    #[test]
    fn next_word_boundary_empty() {
        assert_eq!(next_word_boundary("", 0), 0);
    }

    #[test]
    fn next_word_boundary_at_start() {
        assert_eq!(next_word_boundary("hello", 0), 5);
    }

    #[test]
    fn next_word_boundary_mid_word() {
        assert_eq!(next_word_boundary("hello", 2), 5);
    }

    #[test]
    fn next_word_boundary_at_word_end() {
        assert_eq!(next_word_boundary("hello", 5), 5);
    }

    #[test]
    fn next_word_boundary_between_words() {
        assert_eq!(next_word_boundary("hello  world", 5), 12);
    }

    #[test]
    fn next_word_boundary_past_last() {
        assert_eq!(next_word_boundary("hello", 5), 5);
    }

    #[test]
    fn next_word_boundary_unicode() {
        let content = "hello 🌍 world";
        let emoji_start = "hello ".len();
        let emoji_end = emoji_start + "🌍".len();
        let world_end = emoji_end + " world".len();

        assert_eq!(next_word_boundary(content, 0), 5);
        assert_eq!(next_word_boundary(content, emoji_start), emoji_end);
        assert_eq!(next_word_boundary(content, emoji_end), world_end);
    }

    #[test]
    fn previous_word_boundary_empty() {
        assert_eq!(previous_word_boundary("", 0), 0);
    }

    #[test]
    fn previous_word_boundary_at_start() {
        assert_eq!(previous_word_boundary("hello", 0), 0);
    }

    #[test]
    fn previous_word_boundary_mid_word() {
        assert_eq!(previous_word_boundary("hello", 3), 0);
    }

    #[test]
    fn previous_word_boundary_at_word_end() {
        assert_eq!(previous_word_boundary("hello", 5), 0);
    }

    #[test]
    fn previous_word_boundary_between_words() {
        assert_eq!(previous_word_boundary("hello  world", 7), 0);
    }

    #[test]
    fn previous_word_boundary_past_last() {
        assert_eq!(previous_word_boundary("hello", 5), 0);
    }

    #[test]
    fn previous_word_boundary_unicode() {
        let content = "hello 🌍 world";
        let emoji_start = "hello ".len();
        let emoji_end = emoji_start + "🌍".len();
        let world_start = emoji_end + " ".len();

        assert_eq!(previous_word_boundary(content, 5), 0);
        assert_eq!(previous_word_boundary(content, emoji_end), emoji_start);
        assert_eq!(previous_word_boundary(content, world_start), emoji_start);
    }
}

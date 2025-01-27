use crate::compositor::{Component, Context};
use arc_swap::ArcSwap;
use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans, Text},
};

use std::{cmp::Ordering, collections::HashSet, sync::Arc};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use helix_core::{
    syntax::{self, HighlightEvent, InjectionLanguageMarker, Syntax},
    test::print,
    Rope, RopeSlice,
};
use helix_view::{
    graphics::{Margin, Rect, Style},
    theme::Modifier,
    Theme,
};

fn styled_multiline_text<'a>(text: &str, style: Style) -> Text<'a> {
    let spans: Vec<_> = text
        .lines()
        .map(|line| Span::styled(line.to_string(), style))
        .map(Spans::from)
        .collect();
    Text::from(spans)
}

pub fn highlighted_code_block<'a>(
    text: &str,
    language: &str,
    theme: Option<&Theme>,
    config_loader: Arc<ArcSwap<syntax::Loader>>,
    additional_highlight_spans: Option<Vec<(usize, std::ops::Range<usize>)>>,
) -> Text<'a> {
    let mut spans = Vec::new();
    let mut lines = Vec::new();

    let get_theme = |key: &str| -> Style { theme.map(|t| t.get(key)).unwrap_or_default() };
    let text_style = get_theme(Markdown::TEXT_STYLE);
    let code_style = get_theme(Markdown::BLOCK_STYLE);

    let theme = match theme {
        Some(t) => t,
        None => return styled_multiline_text(text, code_style),
    };

    let ropeslice = RopeSlice::from(text);
    let syntax = config_loader
        .load()
        .language_configuration_for_injection_string(&InjectionLanguageMarker::Name(
            language.into(),
        ))
        .and_then(|config| config.highlight_config(theme.scopes()))
        .and_then(|config| Syntax::new(ropeslice, config, Arc::clone(&config_loader)));

    let syntax = match syntax {
        Some(s) => s,
        None => return styled_multiline_text(text, code_style),
    };

    let highlight_iter = syntax
        .highlight_iter(ropeslice, None, None)
        .map(|e| e.unwrap());
    let highlight_iter: Box<dyn Iterator<Item = HighlightEvent>> =
        if let Some(spans) = additional_highlight_spans {
            Box::new(helix_core::syntax::merge(highlight_iter, spans))
        } else {
            Box::new(highlight_iter)
        };

    if language == "helix" {
        let (text, selections) = print(text);

        let style_cursor = get_theme("ui.cursor");
        let style_cursor_primary = get_theme("ui.cursor.primary");
        let style_selection = get_theme("ui.selection");
        let style_selection_primary = get_theme("ui.selection.primary");
        let style_text = get_theme("ui.text");

        let mut selection_positions = HashSet::new();
        let mut cursors_positions = HashSet::new();
        let primary_idx = selections.primary_index();

        for range in selections.iter() {
            selection_positions.extend(range.from()..range.to());
            cursors_positions.insert(if range.head > range.anchor {
                range.head.saturating_sub(1)
            } else {
                range.head
            });
        }

        let mut chars = text.chars().enumerate().peekable();

        while let Some((idx, ch)) = chars.next() {
            if ch == '\r' && chars.peek().is_some_and(|(_, ch)| *ch == '\n') {
                // We're on a line break. We already have the
                // code to handle newlines in place, so we can just
                // handle the newline on the next iteration
                continue;
            }

            let is_cursor = cursors_positions.contains(&idx);
            let is_selection = selection_positions.contains(&idx);

            let style = if is_cursor {
                if idx == primary_idx {
                    style_cursor_primary
                } else {
                    style_cursor
                }
            } else if is_selection {
                if idx == primary_idx {
                    style_selection_primary
                } else {
                    style_selection
                }
            } else {
                style_text
            };

            if ch == '\n' {
                lines.push(Spans::from(spans));
                spans = vec![];
            } else {
                spans.push(Span::styled(ch.to_string(), style));
            }
        }
    } else {
        let mut highlights = Vec::new();
        for event in highlight_iter {
            match event {
                HighlightEvent::HighlightStart(span) => {
                    highlights.push(span);
                }
                HighlightEvent::HighlightEnd => {
                    highlights.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let style = highlights
                        .iter()
                        .fold(text_style, |acc, span| acc.patch(theme.highlight(span.0)));

                    let mut slice = &text[start..end];
                    // TODO: do we need to handle all unicode line endings
                    // here, or is just '\n' okay?
                    while let Some(end) = slice.find('\n') {
                        // emit span up to newline
                        let text = &slice[..end];
                        let text = text.replace('\t', "    "); // replace tabs
                        let span = Span::styled(text, style);
                        spans.push(span);

                        // truncate slice to after newline
                        slice = &slice[end + 1..];

                        // make a new line
                        let spans = std::mem::take(&mut spans);
                        lines.push(Spans::from(spans));
                    }

                    // if there's anything left, emit it too
                    if !slice.is_empty() {
                        let span = Span::styled(slice.replace('\t', "    "), style);
                        spans.push(span);
                    }
                }
            }
        }
    }

    if !spans.is_empty() {
        let spans = std::mem::take(&mut spans);
        lines.push(Spans::from(spans));
    }

    Text::from(lines)
}

pub struct Markdown {
    contents: String,

    config_loader: Arc<ArcSwap<syntax::Loader>>,
}

// TODO: pre-render and self reference via Pin
// better yet, just use Tendril + subtendril for references

impl Markdown {
    const TEXT_STYLE: &'static str = "ui.text";
    const BLOCK_STYLE: &'static str = "markup.raw.inline";
    const HEADING_STYLES: [&'static str; 6] = [
        "markup.heading.1",
        "markup.heading.2",
        "markup.heading.3",
        "markup.heading.4",
        "markup.heading.5",
        "markup.heading.6",
    ];
    const INDENT: &'static str = "  ";

    pub fn new(contents: String, config_loader: Arc<ArcSwap<syntax::Loader>>) -> Self {
        Self {
            contents,
            config_loader,
        }
    }

    pub fn parse(&self, theme: Option<&Theme>) -> tui::text::Text<'_> {
        fn push_line<'a>(spans: &mut Vec<Span<'a>>, lines: &mut Vec<Spans<'a>>) {
            let spans = std::mem::take(spans);
            if !spans.is_empty() {
                lines.push(Spans::from(spans));
            }
        }

        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(&self.contents, options);

        // TODO: if possible, render links as terminal hyperlinks: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
        let mut tags = Vec::new();
        let mut spans = Vec::new();
        let mut lines = Vec::new();
        let mut list_stack = Vec::new();

        let get_indent = |level: usize| {
            if level < 1 {
                String::new()
            } else {
                Self::INDENT.repeat(level - 1)
            }
        };

        let get_theme = |key: &str| -> Style { theme.map(|t| t.get(key)).unwrap_or_default() };
        let text_style = get_theme(Self::TEXT_STYLE);
        let code_style = get_theme(Self::BLOCK_STYLE);
        let heading_styles: Vec<Style> = Self::HEADING_STYLES
            .iter()
            .map(|key| get_theme(key))
            .collect();

        // Transform text in `<code>` blocks into `Event::Code`
        let mut in_code = false;
        let parser = parser.filter_map(|event| match event {
            Event::Html(tag)
                if tag.starts_with("<code") && matches!(tag.chars().nth(5), Some(' ' | '>')) =>
            {
                in_code = true;
                None
            }
            Event::Html(tag) if *tag == *"</code>" => {
                in_code = false;
                None
            }
            Event::Text(text) if in_code => Some(Event::Code(text)),
            _ => Some(event),
        });

        for event in parser {
            match event {
                Event::Start(Tag::List(list)) => {
                    // if the list stack is not empty this is a sub list, in that
                    // case we need to push the current line before proceeding
                    if !list_stack.is_empty() {
                        push_line(&mut spans, &mut lines);
                    }

                    list_stack.push(list);
                }
                Event::End(TagEnd::List(_)) => {
                    list_stack.pop();

                    // whenever top-level list closes, empty line
                    if list_stack.is_empty() {
                        lines.push(Spans::default());
                    }
                }
                Event::Start(Tag::Item) => {
                    if list_stack.is_empty() {
                        log::warn!("markdown parsing error, list item without list");
                    }

                    tags.push(Tag::Item);

                    // get the appropriate bullet for the current list
                    let bullet = list_stack
                        .last()
                        .unwrap_or(&None) // use the '- ' bullet in case the list stack would be empty
                        .map_or(String::from("- "), |number| format!("{}. ", number));

                    // increment the current list number if there is one
                    if let Some(v) = list_stack.last_mut().unwrap_or(&mut None).as_mut() {
                        *v += 1;
                    }

                    let prefix = get_indent(list_stack.len()) + bullet.as_str();
                    spans.push(Span::from(prefix));
                }
                Event::Start(tag) => {
                    tags.push(tag);
                    if spans.is_empty() && !list_stack.is_empty() {
                        // TODO: could push indent + 2 or 3 spaces to align with
                        // the rest of the list.
                        spans.push(Span::from(get_indent(list_stack.len())));
                    }
                }
                Event::End(tag) => {
                    tags.pop();
                    match tag {
                        TagEnd::Heading(_)
                        | TagEnd::Paragraph
                        | TagEnd::CodeBlock
                        | TagEnd::Item => {
                            push_line(&mut spans, &mut lines);
                        }
                        _ => (),
                    }

                    // whenever heading, code block or paragraph closes, empty line
                    match tag {
                        TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::CodeBlock => {
                            lines.push(Spans::default());
                        }
                        _ => (),
                    }
                }
                Event::Text(text) => {
                    if let Some(Tag::CodeBlock(kind)) = tags.last() {
                        let language = match kind {
                            CodeBlockKind::Fenced(language) => language,
                            CodeBlockKind::Indented => "",
                        };
                        let tui_text = highlighted_code_block(
                            &text,
                            language,
                            theme,
                            Arc::clone(&self.config_loader),
                            None,
                        );
                        lines.extend(tui_text.lines.into_iter());
                    } else {
                        let style = match tags.last() {
                            Some(Tag::Heading { level, .. }) => match level {
                                HeadingLevel::H1 => heading_styles[0],
                                HeadingLevel::H2 => heading_styles[1],
                                HeadingLevel::H3 => heading_styles[2],
                                HeadingLevel::H4 => heading_styles[3],
                                HeadingLevel::H5 => heading_styles[4],
                                HeadingLevel::H6 => heading_styles[5],
                            },
                            Some(Tag::Emphasis) => text_style.add_modifier(Modifier::ITALIC),
                            Some(Tag::Strong) => text_style.add_modifier(Modifier::BOLD),
                            Some(Tag::Strikethrough) => {
                                text_style.add_modifier(Modifier::CROSSED_OUT)
                            }
                            _ => text_style,
                        };
                        spans.push(Span::styled(text, style));
                    }
                }
                Event::Code(text) | Event::Html(text) => {
                    spans.push(Span::styled(text, code_style));
                }
                Event::SoftBreak | Event::HardBreak => {
                    push_line(&mut spans, &mut lines);
                    if !list_stack.is_empty() {
                        // TODO: could push indent + 2 or 3 spaces to align with
                        // the rest of the list.
                        spans.push(Span::from(get_indent(list_stack.len())));
                    }
                }
                Event::Rule => {
                    lines.push(Spans::from(Span::styled("---", code_style)));
                    lines.push(Spans::default());
                }
                // TaskListMarker(bool) true if checked
                _ => {
                    log::warn!("unhandled markdown event {:?}", event);
                }
            }
            // build up a vec of Paragraph tui widgets
        }

        if !spans.is_empty() {
            lines.push(Spans::from(spans));
        }

        // if last line is empty, remove it
        if let Some(line) = lines.last() {
            if line.0.is_empty() {
                lines.pop();
            }
        }

        Text::from(lines)
    }
}

impl Component for Markdown {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        use tui::widgets::{Paragraph, Widget, Wrap};

        let text = self.parse(Some(&cx.editor.theme));

        let par = Paragraph::new(&text)
            .wrap(Wrap { trim: false })
            .scroll((cx.scroll.unwrap_or_default() as u16, 0));

        let margin = Margin::all(1);
        par.render(area.inner(margin), surface);
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        let padding = 2;
        let contents = self.parse(None);

        // TODO: account for tab width
        let max_text_width = (viewport.0.saturating_sub(padding)).min(120);
        let (width, height) = crate::ui::text::required_size(&contents, max_text_width);

        Some((width + padding, height + padding))
    }
}

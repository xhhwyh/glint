use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    Renderer::new(width.max(1) as usize).render(text)
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    row: Vec<Span<'static>>,
    row_width: usize,
    style: Style,
    styles: Vec<Style>,
    list_stack: Vec<ListState>,
    code_block: bool,
    quote_depth: usize,
    table_row: Vec<String>,
}

struct ListState {
    next: Option<u64>,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            row: Vec::new(),
            row_width: 0,
            style: Style::default(),
            styles: Vec::new(),
            list_stack: Vec::new(),
            code_block: false,
            quote_depth: 0,
            table_row: Vec::new(),
        }
    }

    fn render(mut self, text: &str) -> Vec<Line<'static>> {
        let parser = Parser::new_ext(text, Options::all());
        for event in parser {
            self.handle(event);
        }
        self.finish_line();
        self.lines
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => self.push_styled(&text, code_style()),
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text),
            Event::SoftBreak | Event::HardBreak => self.finish_line(),
            Event::Rule => {
                self.finish_line();
                self.push_styled(
                    &"─".repeat(self.width),
                    Style::default().fg(Color::DarkGray),
                );
                self.finish_line();
            }
            Event::TaskListMarker(done) => self.push_text(if done { "☑ " } else { "☐ " }),
            Event::FootnoteReference(text) => self.push_text(&format!("[{text}]")),
            Event::InlineMath(text) => self.push_styled(&format!("${text}$"), code_style()),
            Event::DisplayMath(text) => {
                self.finish_line();
                self.push_styled(&format!("$$ {text} $$"), code_style());
                self.finish_line();
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.finish_line();
                self.push_style(heading_style(level));
                self.push_text(match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    HeadingLevel::H3 => "### ",
                    HeadingLevel::H4 => "#### ",
                    HeadingLevel::H5 => "##### ",
                    HeadingLevel::H6 => "###### ",
                });
            }
            Tag::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth += 1;
                self.push_styled(&"│ ".repeat(self.quote_depth), quote_style());
            }
            Tag::CodeBlock(kind) => {
                self.finish_line();
                self.code_block = true;
                self.push_style(code_style());
                if let CodeBlockKind::Fenced(language) = kind
                    && !language.is_empty()
                {
                    self.push_text(&format!("{language}\n"));
                }
            }
            Tag::List(start) => self.list_stack.push(ListState { next: start }),
            Tag::Item => {
                self.finish_line();
                let depth = self.list_stack.len().saturating_sub(1);
                self.push_text(&"  ".repeat(depth));
                if let Some(list) = self.list_stack.last_mut() {
                    let marker = match list.next {
                        Some(number) => {
                            list.next = Some(number + 1);
                            format!("{number}. ")
                        }
                        None => "- ".to_owned(),
                    };
                    self.push_text(&marker);
                }
            }
            Tag::Emphasis => self.push_modifier(Modifier::ITALIC),
            Tag::Strong => self.push_modifier(Modifier::BOLD),
            Tag::Strikethrough => self.push_modifier(Modifier::CROSSED_OUT),
            Tag::Link { .. } => self.push_style(link_style()),
            Tag::Image {
                dest_url, title, ..
            } => {
                self.push_styled("[image", Style::default().fg(Color::Magenta));
                if !title.is_empty() {
                    self.push_styled(&format!(": {title}"), Style::default().fg(Color::Magenta));
                }
                self.push_styled(
                    &format!("]({dest_url})"),
                    Style::default().fg(Color::Magenta),
                );
            }
            Tag::Table(_) | Tag::TableHead | Tag::TableRow => {
                self.finish_line();
                self.table_row.clear();
            }
            Tag::TableCell => self.table_row.push(String::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_line(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_line();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.finish_line();
            }
            TagEnd::CodeBlock => {
                self.pop_style();
                self.code_block = false;
                self.finish_line();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.finish_line();
            }
            TagEnd::Item => self.finish_line(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style();
            }
            TagEnd::Image => {}
            TagEnd::TableCell => {}
            TagEnd::TableHead | TagEnd::TableRow => self.flush_table_row(),
            TagEnd::Table => self.finish_line(),
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        self.push_styled(text, self.style);
    }

    fn push_styled(&mut self, text: &str, style: Style) {
        if let Some(cell) = self.table_row.last_mut() {
            cell.push_str(text);
            return;
        }

        for char in text.chars() {
            if char == '\n' {
                self.finish_line();
                continue;
            }

            let char_width = char.width().unwrap_or(0);
            if !self.code_block && self.row_width + char_width > self.width && !self.row.is_empty()
            {
                self.finish_line();
                if self.quote_depth > 0 {
                    self.push_styled(&"│ ".repeat(self.quote_depth), quote_style());
                }
            }
            self.row.push(Span::styled(char.to_string(), style));
            self.row_width += char_width;
        }
    }

    fn finish_line(&mut self) {
        if self.row.is_empty() && self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            return;
        }
        self.lines.push(Line::from(std::mem::take(&mut self.row)));
        self.row_width = 0;
    }

    fn flush_table_row(&mut self) {
        if self.table_row.is_empty() {
            return;
        }
        self.finish_line();
        self.push_text(&self.table_row.join(" │ "));
        self.finish_line();
        self.table_row.clear();
    }

    fn push_modifier(&mut self, modifier: Modifier) {
        self.push_style(self.style.add_modifier(modifier));
    }

    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        if let Some(style) = self.styles.pop() {
            self.style = style;
        }
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let color = match level {
        HeadingLevel::H1 | HeadingLevel::H2 => Color::Cyan,
        _ => Color::Blue,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn code_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn quote_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn link_style() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::UNDERLINED)
}

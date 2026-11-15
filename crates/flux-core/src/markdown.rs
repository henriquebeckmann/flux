//! Markdown → a small block/inline tree the UI can draw without a markdown engine of its own.
//! Raw HTML is dropped, images become their alt text (extensions may not load arbitrary URLs
//! through markdown), links keep their URL and the UI only opens http(s)/mailto.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, PartialEq)]
pub enum Span {
    Text(String),
    Code(String),
    Strong(Vec<Span>),
    Emphasis(Vec<Span>),
    Strike(Vec<Span>),
    Link { text: Vec<Span>, url: String },
    LineBreak,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    Code { lang: Option<String>, text: String },
    List { ordered: bool, start: u64, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Rule,
    Table { header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
}

impl Span {
    /// Plain text of a span list (for tooltips, tests, accessibility).
    pub fn plain(spans: &[Span]) -> String {
        let mut out = String::new();
        for s in spans {
            match s {
                Span::Text(t) | Span::Code(t) => out.push_str(t),
                Span::Strong(v) | Span::Emphasis(v) | Span::Strike(v) => out.push_str(&Span::plain(v)),
                Span::Link { text, .. } => out.push_str(&Span::plain(text)),
                Span::LineBreak => out.push('\n'),
            }
        }
        out
    }
}

enum Node {
    /// Document, block quote or list item.
    Blocks { kind: Container, blocks: Vec<Block> },
    /// Paragraph, heading, emphasis, link, table cell… `implicit` = opened by loose text in a tight list item.
    Inline { kind: Inline, spans: Vec<Span>, implicit: bool },
    List { ordered: bool, start: u64, items: Vec<Vec<Block>> },
    Table { header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>>, row: Vec<Vec<Span>> },
}

#[derive(Clone, Copy, PartialEq)]
enum Container {
    Doc,
    Quote,
    Item,
}

enum Inline {
    Paragraph,
    Heading(u8),
    Strong,
    Emphasis,
    Strike,
    Link(String),
    Image,
    Cell,
}

pub fn parse(src: &str) -> Vec<Block> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut b = Builder { stack: vec![Node::Blocks { kind: Container::Doc, blocks: vec![] }], code: None };
    for ev in Parser::new_ext(src, opts) {
        b.event(ev);
    }
    b.finish()
}

struct Builder {
    stack: Vec<Node>,
    code: Option<(Option<String>, String)>,
}

impl Builder {
    fn add_block(&mut self, block: Block) {
        self.close_implicit();
        for node in self.stack.iter_mut().rev() {
            if let Node::Blocks { blocks, .. } = node {
                blocks.push(block);
                return;
            }
        }
    }

    /// Inline content directly inside a container (tight list items) gets an implicit paragraph.
    fn add_span(&mut self, s: Span) {
        if let Some(Node::Blocks { .. }) = self.stack.last() {
            self.stack.push(Node::Inline { kind: Inline::Paragraph, spans: vec![], implicit: true });
        }
        if let Some(Node::Inline { spans, .. }) = self.stack.last_mut() {
            spans.push(s);
        }
    }

    fn close_implicit(&mut self) {
        if matches!(self.stack.last(), Some(Node::Inline { implicit: true, .. })) {
            if let Some(Node::Inline { spans, .. }) = self.stack.pop() {
                self.add_block(Block::Paragraph(spans));
            }
        }
    }

    fn pop_spans(&mut self) -> Vec<Span> {
        match self.stack.pop() {
            Some(Node::Inline { spans, .. }) => spans,
            other => {
                if let Some(n) = other {
                    self.stack.push(n);
                }
                vec![]
            }
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(t) => match &mut self.code {
                Some((_, buf)) => buf.push_str(&t),
                None => self.add_span(Span::Text(t.to_string())),
            },
            Event::Code(t) => self.add_span(Span::Code(t.to_string())),
            Event::SoftBreak => self.add_span(Span::Text(" ".into())),
            Event::HardBreak => self.add_span(Span::LineBreak),
            Event::Rule => self.add_block(Block::Rule),
            Event::TaskListMarker(done) => self.add_span(Span::Text(if done { "☑ " } else { "☐ " }.into())),
            _ => {} // raw HTML and footnotes are dropped
        }
    }

    fn inline(&mut self, kind: Inline) {
        self.stack.push(Node::Inline { kind, spans: vec![], implicit: false });
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.close_implicit();
                self.inline(Inline::Paragraph);
            }
            Tag::Heading { level, .. } => {
                self.close_implicit();
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.inline(Inline::Heading(n));
            }
            Tag::BlockQuote(_) => {
                self.close_implicit();
                self.stack.push(Node::Blocks { kind: Container::Quote, blocks: vec![] });
            }
            Tag::CodeBlock(kind) => {
                self.close_implicit();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.split_whitespace().next().unwrap_or("").to_string()),
                    _ => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.close_implicit();
                self.stack.push(Node::List { ordered: start.is_some(), start: start.unwrap_or(1), items: vec![] });
            }
            Tag::Item => self.stack.push(Node::Blocks { kind: Container::Item, blocks: vec![] }),
            Tag::Emphasis => self.inline(Inline::Emphasis),
            Tag::Strong => self.inline(Inline::Strong),
            Tag::Strikethrough => self.inline(Inline::Strike),
            Tag::Link { dest_url, .. } => {
                self.ensure_inline_parent();
                self.inline(Inline::Link(dest_url.to_string()));
            }
            Tag::Image { .. } => {
                self.ensure_inline_parent();
                self.inline(Inline::Image);
            }
            Tag::Table(_) => {
                self.close_implicit();
                self.stack.push(Node::Table { header: vec![], rows: vec![], row: vec![] });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(Node::Table { row, .. }) = self.stack.last_mut() {
                    row.clear();
                }
            }
            Tag::TableCell => self.inline(Inline::Cell),
            _ => {}
        }
    }

    fn ensure_inline_parent(&mut self) {
        if matches!(self.stack.last(), Some(Node::Blocks { .. })) {
            self.stack.push(Node::Inline { kind: Inline::Paragraph, spans: vec![], implicit: true });
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::CodeBlock => {
                if let Some((lang, text)) = self.code.take() {
                    self.add_block(Block::Code { lang, text: text.trim_end_matches('\n').to_string() });
                }
            }
            TagEnd::Paragraph => {
                let spans = self.pop_spans();
                self.add_block(Block::Paragraph(spans));
            }
            TagEnd::Heading(_) => {
                let level = match self.stack.last() {
                    Some(Node::Inline { kind: Inline::Heading(l), .. }) => *l,
                    _ => 1,
                };
                let spans = self.pop_spans();
                self.add_block(Block::Heading { level, spans });
            }
            TagEnd::Emphasis => self.wrap(Span::Emphasis),
            TagEnd::Strong => self.wrap(Span::Strong),
            TagEnd::Strikethrough => self.wrap(Span::Strike),
            TagEnd::Link => {
                let url = match self.stack.last() {
                    Some(Node::Inline { kind: Inline::Link(u), .. }) => u.clone(),
                    _ => String::new(),
                };
                let text = self.pop_spans();
                self.add_span(Span::Link { text, url });
            }
            TagEnd::Image => {
                for s in self.pop_spans() {
                    self.add_span(s); // alt text
                }
            }
            TagEnd::TableCell => {
                let cell = self.pop_spans();
                if let Some(Node::Table { row, .. }) = self.stack.last_mut() {
                    row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(Node::Table { header, row, .. }) = self.stack.last_mut() {
                    *header = std::mem::take(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Node::Table { rows, row, .. }) = self.stack.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Table => {
                if let Some(Node::Table { header, rows, .. }) = self.stack.pop() {
                    self.add_block(Block::Table { header, rows });
                }
            }
            TagEnd::BlockQuote(_) => {
                self.close_implicit();
                if let Some(Node::Blocks { blocks, .. }) = self.stack.pop() {
                    self.add_block(Block::Quote(blocks));
                }
            }
            TagEnd::Item => {
                self.close_implicit();
                if let Some(Node::Blocks { blocks, .. }) = self.stack.pop() {
                    if let Some(Node::List { items, .. }) = self.stack.last_mut() {
                        items.push(blocks);
                    }
                }
            }
            TagEnd::List(_) => {
                if let Some(Node::List { ordered, start, items }) = self.stack.pop() {
                    self.add_block(Block::List { ordered, start, items });
                }
            }
            _ => {}
        }
    }

    fn wrap(&mut self, f: fn(Vec<Span>) -> Span) {
        let inner = self.pop_spans();
        self.add_span(f(inner));
    }

    fn finish(mut self) -> Vec<Block> {
        self.close_implicit();
        match self.stack.into_iter().next() {
            Some(Node::Blocks { blocks, .. }) => blocks,
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Span {
        Span::Text(s.into())
    }

    #[test]
    fn headings_paragraphs_and_inline_styles() {
        let b = parse("# Title\n\nHello **bold** and *it* and `code` and ~~gone~~ [link](https://x.org).\n");
        assert_eq!(b[0], Block::Heading { level: 1, spans: vec![text("Title")] });
        let Block::Paragraph(p) = &b[1] else { panic!("{b:?}") };
        assert_eq!(p[1], Span::Strong(vec![text("bold")]));
        assert_eq!(p[3], Span::Emphasis(vec![text("it")]));
        assert_eq!(p[5], Span::Code("code".into()));
        assert_eq!(p[7], Span::Strike(vec![text("gone")]));
        assert_eq!(p[9], Span::Link { text: vec![text("link")], url: "https://x.org".into() });
        assert_eq!(Span::plain(p), "Hello bold and it and code and gone link.");
    }

    #[test]
    fn code_blocks_keep_text_and_language() {
        let b = parse("```lua\nlocal x = 1\nprint(x)\n```\n\n    indented\n");
        assert_eq!(b[0], Block::Code { lang: Some("lua".into()), text: "local x = 1\nprint(x)".into() });
        assert_eq!(b[1], Block::Code { lang: None, text: "indented".into() });
    }

    #[test]
    fn tight_and_loose_lists_nested_and_ordered() {
        let b = parse("- one\n- two\n  - nested\n\n3. a\n4. b\n");
        let Block::List { ordered: false, items, .. } = &b[0] else { panic!("{b:?}") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], vec![Block::Paragraph(vec![text("one")])]);
        assert_eq!(items[1][0], Block::Paragraph(vec![text("two")]));
        assert!(matches!(&items[1][1], Block::List { ordered: false, items, .. } if items.len() == 1));
        assert!(matches!(&b[1], Block::List { ordered: true, start: 3, items, .. } if items.len() == 2));
        let loose = parse("- a\n\n- b\n");
        assert!(matches!(&loose[0], Block::List { items, .. } if items.iter().all(|i| matches!(i[0], Block::Paragraph(_)))));
    }

    #[test]
    fn quotes_rules_tables_and_task_lists() {
        let b = parse("> quoted *text*\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n");
        assert!(matches!(&b[0], Block::Quote(inner) if matches!(&inner[0], Block::Paragraph(_))));
        assert_eq!(b[1], Block::Rule);
        let Block::Table { header, rows } = &b[2] else { panic!("{b:?}") };
        assert_eq!((Span::plain(&header[0]), Span::plain(&header[1])), ("a".into(), "b".into()));
        assert_eq!(rows.len(), 1);
        assert_eq!(Span::plain(&rows[0][1]), "2");
        let Block::List { items, .. } = &b[3] else { panic!() };
        let Block::Paragraph(p) = &items[0][0] else { panic!() };
        assert_eq!(Span::plain(p), "☑ done");
    }

    #[test]
    fn html_is_dropped_images_become_alt_text_and_empty_input_is_empty() {
        let b = parse("<script>alert(1)</script>\n\ntext ![alt text](http://evil/x.png) end\n");
        let all = format!("{b:?}");
        assert!(!all.contains("alert") && !all.contains("evil"), "{all}");
        let Block::Paragraph(p) = b.last().unwrap() else { panic!("{b:?}") };
        assert_eq!(Span::plain(p), "text alt text end");
        assert!(parse("").is_empty());
    }

    #[test]
    fn hard_breaks_and_soft_breaks() {
        let Block::Paragraph(p) = &parse("a  \nb\nc")[0] else { panic!() };
        assert_eq!(Span::plain(p), "a\nb c");
    }
}

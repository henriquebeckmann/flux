//! Draws the block tree produced by `flux_core::markdown`. Links open through the core (which
//! only lets web and mail links out).

use std::{ops::Range, rc::Rc};

use flux_core::markdown::{Block, Span};
use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement, StrikethroughStyle, StyledText, UnderlineStyle, Window, div,
    prelude::*, px,
};

use super::theme;

pub type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Flattened inline content: the text, its style ranges and its link ranges.
pub struct Inline {
    pub text: String,
    pub styles: Vec<(Range<usize>, HighlightStyle)>,
    pub links: Vec<(Range<usize>, String)>,
}

pub fn flatten(spans: &[Span]) -> Inline {
    #[derive(Clone, Copy, Default)]
    struct Ctx {
        bold: bool,
        italic: bool,
        strike: bool,
        code: bool,
        link: bool,
    }
    fn walk(spans: &[Span], ctx: Ctx, out: &mut Inline) {
        for s in spans {
            match s {
                Span::Text(t) => push(t, ctx, out),
                Span::Code(t) => push(t, Ctx { code: true, ..ctx }, out),
                Span::Strong(v) => walk(v, Ctx { bold: true, ..ctx }, out),
                Span::Emphasis(v) => walk(v, Ctx { italic: true, ..ctx }, out),
                Span::Strike(v) => walk(v, Ctx { strike: true, ..ctx }, out),
                Span::LineBreak => out.text.push('\n'),
                Span::Link { text, url } => {
                    let start = out.text.len();
                    walk(text, Ctx { link: true, ..ctx }, out);
                    out.links.push((start..out.text.len(), url.clone()));
                }
            }
        }
    }
    fn push(t: &str, ctx: Ctx, out: &mut Inline) {
        if t.is_empty() {
            return;
        }
        let start = out.text.len();
        out.text.push_str(t);
        let mut style = HighlightStyle::default();
        if ctx.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if ctx.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if ctx.strike {
            style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
        }
        if ctx.code {
            style.background_color = Some(theme::chip().into());
            style.color = Some(theme::text().into());
        }
        if ctx.link {
            style.color = Some(theme::accent().into());
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
        }
        if style != HighlightStyle::default() {
            out.styles.push((start..out.text.len(), style));
        }
    }
    let mut out = Inline { text: String::new(), styles: vec![], links: vec![] };
    walk(spans, Ctx::default(), &mut out);
    out
}

fn inline_element(spans: &[Span], id: usize, on_link: &LinkHandler) -> AnyElement {
    let inline = flatten(spans);
    let styled = StyledText::new(inline.text).with_highlights(inline.styles);
    if inline.links.is_empty() {
        return styled.into_any_element();
    }
    let (ranges, urls): (Vec<_>, Vec<_>) = inline.links.into_iter().unzip();
    let on_link = on_link.clone();
    InteractiveText::new(("md-text", id), styled)
        .on_click(ranges, move |i, window, cx| {
            if let Some(u) = urls.get(i) {
                on_link(u, window, cx);
            }
        })
        .into_any_element()
}

pub fn render(blocks: &[Block], on_link: &LinkHandler) -> AnyElement {
    let mut n = 0;
    div().flex().flex_col().gap_3().children(blocks.iter().map(|b| block(b, &mut n, on_link))).into_any_element()
}

fn block(b: &Block, n: &mut usize, on_link: &LinkHandler) -> AnyElement {
    *n += 1;
    let id = *n;
    match b {
        Block::Heading { level, spans } => {
            let size = match level {
                1 => 22.,
                2 => 18.,
                3 => 16.,
                _ => 14.,
            };
            div().text_size(px(size)).font_weight(FontWeight::BOLD).text_color(theme::text()).pt_1().child(inline_element(spans, id, on_link)).into_any_element()
        }
        Block::Paragraph(spans) => div().text_size(px(14.)).line_height(px(21.)).text_color(theme::text()).child(inline_element(spans, id, on_link)).into_any_element(),
        Block::Code { text, .. } => div()
            .w_full()
            .p_3()
            .rounded_md()
            .bg(theme::chip())
            .font_family("monospace")
            .text_size(px(12.5))
            .line_height(px(18.))
            .text_color(theme::text())
            .child(text.clone())
            .into_any_element(),
        Block::Quote(inner) => div()
            .pl_3()
            .border_l_2()
            .border_color(theme::border())
            .text_color(theme::text_dim())
            .flex()
            .flex_col()
            .gap_2()
            .children(inner.iter().map(|x| block(x, n, on_link)))
            .into_any_element(),
        Block::Rule => div().h(px(1.)).w_full().bg(theme::divider()).into_any_element(),
        Block::List { ordered, start, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(i, item)| {
                let marker = if *ordered { format!("{}.", *start + i as u64) } else { "•".to_string() };
                div()
                    .flex()
                    .gap_2()
                    .text_size(px(14.))
                    .child(div().w(px(22.)).flex_none().text_color(theme::text_dim()).child(marker))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap_1().children(item.iter().map(|x| block(x, n, on_link))))
            }))
            .into_any_element(),
        Block::Table { header, rows } => {
            let cell = |spans: &Vec<Span>, bold: bool, id: usize, on_link: &LinkHandler| {
                let mut d = div().flex_1().min_w_0().px_2().py_1().text_size(px(13.)).text_color(theme::text());
                if bold {
                    d = d.font_weight(FontWeight::BOLD);
                }
                d.child(inline_element(spans, id, on_link))
            };
            let mut cid = id * 1000;
            let mut rows_el: Vec<AnyElement> = Vec::new();
            rows_el.push(
                div()
                    .flex()
                    .bg(theme::chip())
                    .children(header.iter().map(|c| {
                        cid += 1;
                        cell(c, true, cid, on_link)
                    }))
                    .into_any_element(),
            );
            for r in rows {
                rows_el.push(
                    div()
                        .flex()
                        .border_t_1()
                        .border_color(theme::divider())
                        .children(r.iter().map(|c| {
                            cid += 1;
                            cell(c, false, cid, on_link)
                        }))
                        .into_any_element(),
                );
            }
            div().w_full().border_1().border_color(theme::border()).rounded_md().overflow_hidden().flex().flex_col().children(rows_el).into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::markdown::parse;

    fn spans(src: &str) -> Vec<Span> {
        match parse(src).remove(0) {
            Block::Paragraph(s) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn styles_cover_exactly_the_styled_text() {
        let i = flatten(&spans("plain **bold** and `code` and *it*"));
        assert_eq!(i.text, "plain bold and code and it");
        let slices: Vec<&str> = i.styles.iter().map(|(r, _)| &i.text[r.clone()]).collect();
        assert_eq!(slices, ["bold", "code", "it"]);
        assert!(i.links.is_empty());
    }

    #[test]
    fn links_report_their_ranges_and_urls_and_nested_styles_compose() {
        let i = flatten(&spans("go [**here** now](https://x.org/a) ok"));
        assert_eq!(i.text, "go here now ok");
        assert_eq!(i.links.len(), 1);
        assert_eq!((&i.text[i.links[0].0.clone()], i.links[0].1.as_str()), ("here now", "https://x.org/a"));
        assert!(i.styles.len() >= 2, "link style over the whole text plus bold over a part");
    }

    #[test]
    fn unicode_ranges_stay_on_char_boundaries() {
        let i = flatten(&spans("café **ñandú** [λ](https://x.org)"));
        for (r, _) in &i.styles {
            assert!(i.text.is_char_boundary(r.start) && i.text.is_char_boundary(r.end));
        }
        assert_eq!(&i.text[i.links[0].0.clone()], "λ");
    }
}

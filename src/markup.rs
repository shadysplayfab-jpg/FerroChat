//! Discord message markup. Content is parsed once per message revision; the view
//! only resolves ids (users, roles, channels) to current names.

use crate::discord::model::CustomEmoji;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Index of the enclosing spoiler within the message.
    pub spoiler: Option<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text(String),
    Code(String),
    /// Always `http://` or `https://`.
    Link(String),
    User(String),
    Role(String),
    Channel(String),
    Everyone(&'static str),
    Emoji { id: String, name: String, animated: bool },
    /// `<t:unix:format>`; format is one of `tTdDfFR`.
    Timestamp { unix: i64, format: char },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inline {
    pub kind: Kind,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineKind {
    Plain,
    Quote,
    Heading(u8),
    Subtext,
    Bullet,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Line { kind: LineKind, inlines: Vec<Inline> },
    Code(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Body {
    pub blocks: Vec<Block>,
    /// Only emoji: rendered large.
    pub jumbo: bool,
}

impl Body {
    /// Custom emoji used: (id, animated).
    pub fn emoji_ids(&self) -> impl Iterator<Item = (&str, bool)> {
        self.blocks.iter().flat_map(|block| match block {
            Block::Line { inlines, .. } => inlines.as_slice(),
            Block::Code(_) => &[],
        })
        .filter_map(|inline| match &inline.kind {
            Kind::Emoji { id, animated, .. } => Some((id.as_str(), *animated)),
            _ => None,
        })
    }
}

/// Discord renders at most this many emoji large.
const JUMBO_LIMIT: usize = 30;

pub fn parse(content: &str) -> Body {
    let mut parser = InlineParser { spoilers: 0 };
    let mut blocks = Vec::new();
    let mut block_quote = false;
    let parts: Vec<&str> = content.split("```").collect();
    for (index, part) in parts.iter().enumerate() {
        let is_code = index % 2 == 1;
        if is_code && index + 1 < parts.len() {
            if let Some(code) = code_block(part) {
                blocks.push(Block::Code(code));
            }
            continue;
        }
        // An unclosed fence is literal text.
        let restored;
        let text = if is_code {
            restored = format!("```{part}");
            restored.as_str()
        } else {
            part
        };
        let text = text.strip_prefix('\n').unwrap_or(text);
        let text = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            continue;
        }
        for line in text.split('\n') {
            let (kind, line) = classify(line, &mut block_quote);
            let mut inlines = Vec::new();
            parser.parse(line, Style::default(), &mut inlines);
            blocks.push(Block::Line { kind, inlines });
        }
    }
    let jumbo = is_jumbo(&blocks);
    Body { blocks, jumbo }
}

fn code_block(part: &str) -> Option<String> {
    // "```rust\ncode```": a first line without spaces is a language tag.
    let code = match part.split_once('\n') {
        Some((first, rest))
            if !first.is_empty()
                && first.chars().all(|c| c.is_ascii_alphanumeric() || "+-#_".contains(c)) =>
        {
            rest
        }
        _ => part.strip_prefix('\n').unwrap_or(part),
    };
    let code = code.strip_suffix('\n').unwrap_or(code);
    (!code.trim().is_empty()).then(|| code.to_owned())
}

fn classify<'a>(line: &'a str, block_quote: &mut bool) -> (LineKind, &'a str) {
    if *block_quote {
        return (LineKind::Quote, line);
    }
    if let Some(rest) = line.strip_prefix(">>> ") {
        *block_quote = true;
        return (LineKind::Quote, rest);
    }
    let prefixes: [(&str, LineKind); 7] = [
        ("> ", LineKind::Quote),
        ("# ", LineKind::Heading(1)),
        ("## ", LineKind::Heading(2)),
        ("### ", LineKind::Heading(3)),
        ("-# ", LineKind::Subtext),
        ("- ", LineKind::Bullet),
        ("* ", LineKind::Bullet),
    ];
    for (prefix, kind) in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (kind, rest);
        }
    }
    (LineKind::Plain, line)
}

struct InlineParser {
    spoilers: u16,
}

const DELIMITERS: [&str; 6] = ["||", "**", "__", "~~", "*", "_"];

impl InlineParser {
    fn parse(&mut self, s: &str, style: Style, out: &mut Vec<Inline>) {
        let mut text = String::new();
        let mut i = 0;
        while i < s.len() {
            let rest = &s[i..];
            if let Some(escaped) = rest.strip_prefix('\\').and_then(|r| r.chars().next()) {
                if escaped.is_ascii_punctuation() {
                    text.push(escaped);
                    i += 1 + escaped.len_utf8();
                    continue;
                }
            }
            let token = special(rest)
                .or_else(|| code_span(rest).map(|(code, len)| (Kind::Code(code.to_owned()), len)))
                .or_else(|| link(s, i))
                .or_else(|| {
                    ["@everyone", "@here"]
                        .into_iter()
                        .find(|m| rest.starts_with(m))
                        .map(|m| (Kind::Everyone(m), m.len()))
                });
            if let Some((kind, len)) = token {
                flush(&mut text, style, out);
                out.push(Inline { kind, style });
                i += len;
                continue;
            }
            if let Some((delimiter, close)) = delimited(s, i) {
                flush(&mut text, style, out);
                let inner = &rest[delimiter.len()..delimiter.len() + close];
                let inner_style = self.apply(delimiter, style);
                self.parse(inner, inner_style, out);
                i += 2 * delimiter.len() + close;
                continue;
            }
            let c = rest.chars().next().expect("i < s.len()");
            text.push(c);
            i += c.len_utf8();
        }
        flush(&mut text, style, out);
    }

    fn apply(&mut self, delimiter: &str, mut style: Style) -> Style {
        match delimiter {
            "||" => {
                style.spoiler = Some(self.spoilers);
                self.spoilers += 1;
            }
            "**" => style.bold = true,
            "__" => style.underline = true,
            "~~" => style.strike = true,
            _ => style.italic = true,
        }
        style
    }
}

fn flush(text: &mut String, style: Style, out: &mut Vec<Inline>) {
    if !text.is_empty() {
        out.push(Inline { kind: Kind::Text(std::mem::take(text)), style });
    }
}

/// `<@id>`, `<@!id>`, `<@&id>`, `<#id>`, `<:name:id>`, `<a:name:id>`, `<t:unix:f>`, `<https://…>`.
fn special(rest: &str) -> Option<(Kind, usize)> {
    let body = rest.strip_prefix('<')?;
    let end = body.find('>')?;
    let inner = &body[..end];
    let kind = if let Some(id) = inner.strip_prefix("@&") {
        digits(id).then(|| Kind::Role(id.to_owned()))?
    } else if let Some(id) = inner.strip_prefix("@!").or_else(|| inner.strip_prefix('@')) {
        digits(id).then(|| Kind::User(id.to_owned()))?
    } else if let Some(id) = inner.strip_prefix('#') {
        digits(id).then(|| Kind::Channel(id.to_owned()))?
    } else if let Some(emoji) = inner.strip_prefix("a:") {
        custom_emoji(emoji, true)?
    } else if let Some(emoji) = inner.strip_prefix(':') {
        custom_emoji(emoji, false)?
    } else if let Some(timestamp) = inner.strip_prefix("t:") {
        let (unix, format) = timestamp.split_once(':').unwrap_or((timestamp, "f"));
        let mut chars = format.chars();
        let format = chars.next().filter(|c| "tTdDfFR".contains(*c) && chars.next().is_none())?;
        Kind::Timestamp { unix: unix.parse().ok()?, format }
    } else if is_url(inner) && !inner.contains(char::is_whitespace) {
        Kind::Link(inner.to_owned())
    } else {
        return None;
    };
    Some((kind, end + 2))
}

fn custom_emoji(emoji: &str, animated: bool) -> Option<Kind> {
    let (name, id) = emoji.split_once(':')?;
    let valid_name = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    (valid_name && digits(id)).then(|| Kind::Emoji { id: id.to_owned(), name: name.to_owned(), animated })
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// `` `code` `` or ``` ``code`` ```.
fn code_span(rest: &str) -> Option<(&str, usize)> {
    let ticks = rest.bytes().take_while(|&b| b == b'`').count();
    if !(1..=2).contains(&ticks) {
        return None;
    }
    let fence = &rest[..ticks];
    let close = rest[ticks..].find(fence)?;
    let code = &rest[ticks..ticks + close];
    (!code.is_empty()).then_some((code, 2 * ticks + close))
}

/// Bare URL starting a word; trailing punctuation is left out.
fn link(s: &str, i: usize) -> Option<(Kind, usize)> {
    let rest = &s[i..];
    if !is_url(rest) || s[..i].chars().next_back().is_some_and(|c| !c.is_whitespace() && c != '(') {
        return None;
    }
    let mut url = &rest[..rest.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(rest.len())];
    loop {
        let unbalanced_paren = url.ends_with(')') && url.matches('(').count() < url.matches(')').count();
        match url.chars().next_back() {
            Some(c) if ".,:;!?\"'".contains(c) || unbalanced_paren => url = &url[..url.len() - 1],
            _ => break,
        }
    }
    let scheme_only = url == "https://" || url == "http://";
    (!scheme_only).then(|| (Kind::Link(url.to_owned()), url.len()))
}

/// Returns the delimiter opening at `s[i..]` and the length of its content.
fn delimited(s: &str, i: usize) -> Option<(&'static str, usize)> {
    let rest = &s[i..];
    for delimiter in DELIMITERS {
        let Some(after) = rest.strip_prefix(delimiter) else { continue };
        let word_char_before = s[..i].chars().next_back().is_some_and(char::is_alphanumeric);
        if delimiter == "_" && word_char_before {
            continue; // snake_case
        }
        if delimiter == "*" && after.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(close) = find_closing(after, delimiter) else { continue };
        if close == 0 {
            continue;
        }
        let after_close = &after[close + delimiter.len()..];
        if delimiter == "_" && after_close.chars().next().is_some_and(char::is_alphanumeric) {
            continue;
        }
        return Some((delimiter, close));
    }
    None
}

/// Finds a closing delimiter, skipping escapes and code spans. In a run like
/// `***`, the delimiter closes at the end of the run.
fn find_closing(hay: &str, delimiter: &str) -> Option<usize> {
    let bytes = hay.as_bytes();
    let marker = delimiter.as_bytes()[0];
    let width = delimiter.len();
    let mut p = 0;
    while p < bytes.len() {
        match bytes[p] {
            b'\\' => p += 1 + hay[p + 1..].chars().next().map_or(0, char::len_utf8),
            b'`' => p += code_span(&hay[p..]).map_or(1, |(_, len)| len),
            b if b == marker => {
                let start = p;
                while p < bytes.len() && bytes[p] == marker {
                    p += 1;
                }
                let run = p - start;
                if run == width || (run > width && (width == 2 || run == 3)) {
                    return Some(p - width);
                }
            }
            _ => p += 1,
        }
    }
    None
}

fn is_jumbo(blocks: &[Block]) -> bool {
    let [Block::Line { kind: LineKind::Plain, inlines }] = blocks else { return false };
    let mut count = 0;
    for inline in inlines {
        match &inline.kind {
            Kind::Emoji { .. } => count += 1,
            Kind::Text(text) => {
                for c in text.chars() {
                    if c.is_whitespace() || is_emoji_joiner(c) {
                        continue;
                    }
                    if !is_emoji(c) {
                        return false;
                    }
                    count += 1;
                }
            }
            _ => return false,
        }
    }
    (1..=JUMBO_LIMIT).contains(&count)
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF | 0x2B00..=0x2BFF)
}

/// Variation selector, ZWJ, keycap, skin tones and tag characters.
fn is_emoji_joiner(c: char) -> bool {
    matches!(c as u32, 0xFE0F | 0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

/// Replaces `:name:` with the current server's custom emoji or a Unicode emoji.
/// Code spans and existing `<:name:id>` tokens are left alone.
pub fn expand_shortcodes(text: &str, custom: &[CustomEmoji]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut in_code = false;
    while let Some(pos) = rest.find([':', '`']) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        if after.starts_with('`') {
            in_code = !in_code;
            out.push('`');
            rest = &after[1..];
            continue;
        }
        let candidate = &after[1..];
        let inside_token = out.ends_with('<') || out.ends_with("<a");
        if !in_code && !inside_token {
            if let Some(end) = candidate.find(':') {
                let name = &candidate[..end];
                if let Some(replacement) = is_shortcode(name).then(|| lookup(name, custom)).flatten() {
                    out.push_str(&replacement);
                    rest = &candidate[end + 1..];
                    continue;
                }
            }
        }
        out.push(':');
        rest = candidate;
    }
    out.push_str(rest);
    out
}

fn is_shortcode(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_-+".contains(c))
}

fn lookup(name: &str, custom: &[CustomEmoji]) -> Option<String> {
    custom
        .iter()
        .find(|e| e.name == name)
        .map(CustomEmoji::token)
        .or_else(|| emojis::get_by_shortcode(name).map(|e| e.as_str().to_owned()))
}

/// The `:query` being typed at the end of the composer, without the colon.
pub fn shortcode_query(composer: &str) -> Option<&str> {
    let colon = composer.rfind(':')?;
    let query = &composer[colon + 1..];
    let starts_word = composer[..colon].chars().next_back().is_none_or(char::is_whitespace);
    (starts_word && query.chars().count() >= 2 && is_shortcode(query)).then_some(query)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Unicode(String),
    Custom(CustomEmoji),
}

pub struct Suggestion {
    pub label: String,
    /// Replaces `:query` in the composer.
    pub insert: String,
    pub pick: Pick,
}

pub fn suggestions(query: &str, custom: &[CustomEmoji], limit: usize) -> Vec<Suggestion> {
    let query = query.to_lowercase();
    let custom = custom.iter().filter(|e| e.name.to_lowercase().contains(&query)).map(|e| Suggestion {
        label: format!(":{}:", e.name),
        insert: format!(":{}: ", e.name),
        pick: Pick::Custom(e.clone()),
    });
    let unicode = emojis::iter().filter_map(|e| {
        let shortcode = e.shortcodes().find(|s| s.starts_with(&query))?;
        Some(Suggestion {
            label: format!("{} :{shortcode}:", e.as_str()),
            insert: e.as_str().to_owned(),
            pick: Pick::Unicode(e.as_str().to_owned()),
        })
    });
    custom.chain(unicode).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str, style: Style) -> Inline {
        Inline { kind: Kind::Text(s.to_owned()), style }
    }

    fn line(content: &str) -> Vec<Inline> {
        match parse(content).blocks.as_slice() {
            [Block::Line { inlines, .. }] => inlines.clone(),
            other => panic!("expected one line, got {other:?}"),
        }
    }

    const BOLD: Style = Style { bold: true, italic: false, underline: false, strike: false, spoiler: None };
    const ITALIC: Style = Style { bold: false, italic: true, underline: false, strike: false, spoiler: None };

    #[test]
    fn nested_emphasis() {
        assert_eq!(line("a **b** c"), vec![text("a ", Style::default()), text("b", BOLD), text(" c", Style::default())]);
        assert_eq!(line("***x***"), vec![text("x", Style { bold: true, italic: true, ..Style::default() })]);
        assert_eq!(line("*a* _b_"), vec![text("a", ITALIC), text(" ", Style::default()), text("b", ITALIC)]);
    }

    #[test]
    fn snake_case_and_unclosed_are_literal() {
        assert_eq!(line("snake_case_name"), vec![text("snake_case_name", Style::default())]);
        assert_eq!(line("2 * 3 = 6"), vec![text("2 * 3 = 6", Style::default())]);
        assert_eq!(line("**open"), vec![text("**open", Style::default())]);
        assert_eq!(line(r"\*not\*"), vec![text("*not*", Style::default())]);
    }

    #[test]
    fn code_is_not_formatted() {
        assert_eq!(
            line("`**x**` y"),
            vec![
                Inline { kind: Kind::Code("**x**".into()), style: Style::default() },
                text(" y", Style::default())
            ]
        );
        let body = parse("before\n```rust\nfn main() {}\n```\nafter");
        assert_eq!(body.blocks.len(), 3);
        assert_eq!(body.blocks[1], Block::Code("fn main() {}".into()));
        assert_eq!(line("```unclosed"), vec![text("```unclosed", Style::default())]);
    }

    #[test]
    fn mentions_emoji_timestamps() {
        let kinds: Vec<Kind> = line("<@1> <@!2> <@&3> <#4> <:wave:5> <a:dance:6> <t:1700000000:R>")
            .into_iter()
            .map(|i| i.kind)
            .filter(|k| !matches!(k, Kind::Text(_)))
            .collect();
        assert_eq!(
            kinds,
            vec![
                Kind::User("1".into()),
                Kind::User("2".into()),
                Kind::Role("3".into()),
                Kind::Channel("4".into()),
                Kind::Emoji { id: "5".into(), name: "wave".into(), animated: false },
                Kind::Emoji { id: "6".into(), name: "dance".into(), animated: true },
                Kind::Timestamp { unix: 1_700_000_000, format: 'R' },
            ]
        );
        assert_eq!(line("<@abc>"), vec![text("<@abc>", Style::default())]);
    }

    #[test]
    fn links_and_spoilers() {
        assert_eq!(
            line("see https://example.com/a_(b)."),
            vec![
                text("see ", Style::default()),
                Inline { kind: Kind::Link("https://example.com/a_(b)".into()), style: Style::default() },
                text(".", Style::default()),
            ]
        );
        let spoilers: Vec<Option<u16>> = line("||a|| ||b||").iter().map(|i| i.style.spoiler).collect();
        assert_eq!(spoilers, vec![Some(0), None, Some(1)]);
    }

    #[test]
    fn line_kinds_and_jumbo() {
        let body = parse("# Title\n> quoted\n- item\n-# small");
        let kinds: Vec<LineKind> = body
            .blocks
            .iter()
            .map(|b| match b {
                Block::Line { kind, .. } => *kind,
                Block::Code(_) => panic!(),
            })
            .collect();
        assert_eq!(kinds, vec![LineKind::Heading(1), LineKind::Quote, LineKind::Bullet, LineKind::Subtext]);
        assert!(parse("😄 👍🏽 <:wave:5>").jumbo);
        assert!(!parse("hi 😄").jumbo);
    }

    #[test]
    fn shortcodes() {
        let custom = [CustomEmoji { id: "9".into(), name: "party".into(), animated: true }];
        assert_eq!(expand_shortcodes("hi :smile: :party:", &custom), "hi 😄 <a:party:9>");
        assert_eq!(expand_shortcodes("`:smile:` 12:30:45 <:smile:1>", &custom), "`:smile:` 12:30:45 <:smile:1>");
        assert_eq!(shortcode_query("hello :sm"), Some("sm"));
        assert_eq!(shortcode_query("12:30"), None);
        assert_eq!(shortcode_query(":s"), None);
    }
}

//! Wrapping a theme's `style.css` so it applies only while its theme is active: the
//! theme's font faces, the file's own top-level `@font-face` and `@keyframes` rules
//! (keyframes renamed `<id>--<name>`), then `[data-theme="<id>"] { ... }` holding the
//! colour scheme, the ribbon tilt, the tokens and the rest of the file, scoped by native
//! CSS nesting.
//!
//! A small tokeniser (comments, strings, `url(...)`, at-keywords, brackets) finds the
//! rules to hoist and the `url()`s to check; it does not parse selectors. It refuses
//! what would let a rule escape the wrapper: an unclosed comment, string or bracket and
//! a closing bracket without its opener.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::theme_json::{ColorScheme, ThemeError, ThemeJson};

/// The most bytes a theme's `style.css` may hold.
pub const MAX_STYLE_BYTES: usize = 1_048_576;

/// Refuse a `style.css` of `n` bytes over [`MAX_STYLE_BYTES`].
pub fn check_size(n: usize) -> Result<(), ThemeError> {
    match n > MAX_STYLE_BYTES {
        true => Err(style_err(format!("{n} bytes, more than the 1 MiB limit"))),
        false => Ok(()),
    }
}

/// A refusal of the theme's `style.css`.
fn style_err(error: impl Into<String>) -> ThemeError {
    ThemeError::new("style.css", error)
}

/// Wrap `css` (a leading UTF-8 BOM is dropped) as the theme's served `style.css`:
///
/// 1. one `@font-face` per entry of the theme's `fonts`;
/// 2. the file's top-level `@font-face` rules, unchanged;
/// 3. the file's top-level `@keyframes` (and vendor `@-*-keyframes`) rules, renamed
///    `<id>--<name>`;
/// 4. `[data-theme="<id>"] {`, the colour scheme, `--ribbon-tilt`, each token, then the
///    rest of the file verbatim with the `animation`/`animation-name` values' names of
///    hoisted keyframes renamed, then `}`.
///
/// `files` is the set of files the theme serves: a `url()` must name one of them (or be
/// a `data:` URL). An empty `css` still gives the font faces and the wrapper. Every
/// refusal names `style.css`, except a token or font value that would break the
/// wrapper, which names its field.
pub fn wrap_style(
    theme: &ThemeJson,
    css: &str,
    files: &BTreeSet<String>,
) -> Result<String, ThemeError> {
    check_size(css.len())?;
    let css = css.strip_prefix('\u{feff}').unwrap_or(css);
    for (key, value) in &theme.tokens {
        check_fragment(&format!("tokens.{key}"), value)?;
    }
    for (i, f) in theme.fonts.iter().enumerate() {
        check_fragment(&format!("fonts[{i}].family"), &format!("\"{}\"", f.family))?;
        check_fragment(&format!("fonts[{i}].weight"), &f.weight)?;
    }
    let toks = Lexer::new(css, "style.css", true).run()?;
    for t in &toks {
        match &t.kind {
            Kind::AtKeyword(name) if name.eq_ignore_ascii_case("import") => {
                return Err(style_err("@import is not allowed"));
            }
            Kind::Url { raw, value } => {
                if let Some(why) = url_problem(value, files) {
                    return Err(style_err(format!("url({raw}): {why}")));
                }
            }
            _ => {}
        }
    }

    let hoisted = hoist(&toks);
    let id = &theme.id;
    let names: BTreeSet<&str> = hoisted.iter().filter_map(|h| h.name(&toks)).collect();
    let mut out = String::new();
    for f in &theme.fonts {
        let _ = writeln!(
            out,
            "@font-face {{ font-family: \"{}\"; src: url(\"{}\"); font-weight: {}; \
             font-display: swap; }}",
            f.family, f.file, f.weight
        );
    }
    for h in hoisted.iter().filter(|h| h.name.is_none()) {
        out.push_str(&css[toks[h.from].start..toks[h.to - 1].end]);
        out.push('\n');
    }
    for h in hoisted.iter().filter(|h| h.name.is_some()) {
        let (start, end) = (toks[h.from].start, toks[h.to - 1].end);
        match h.name {
            Some(Some(k)) => {
                out.push_str(&css[start..toks[k].start]);
                out.push_str(&renamed(id, &toks[k], css));
                out.push_str(&css[toks[k].end..end]);
            }
            _ => out.push_str(&css[start..end]),
        }
        out.push('\n');
    }
    let _ = writeln!(out, "[data-theme=\"{id}\"] {{");
    let scheme = match theme.color_scheme {
        ColorScheme::Dark => "dark",
        ColorScheme::Light => "light",
    };
    let _ = writeln!(out, "  color-scheme: {scheme};");
    let _ = writeln!(out, "  --ribbon-tilt: {}deg;", theme.layout.ribbon_tilt_deg);
    for (key, value) in &theme.tokens {
        let _ = writeln!(out, "  {key}: {value};");
    }
    let body = body(css, &toks, &hoisted, &names, id);
    out.push_str(&body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("}\n");
    Ok(out)
}

/// Refuse a value written into the wrapper that would not stay one value: an unclosed
/// comment, string or bracket, or a stray `\` that would escape what follows it.
fn check_fragment(field: &str, value: &str) -> Result<(), ThemeError> {
    let toks = Lexer::new(value, field, false).run()?;
    let stray = toks
        .iter()
        .any(|t| t.kind == Kind::Other && &value[t.start..t.end] == "\\");
    match stray {
        true => Err(ThemeError::new(field, "a stray `\\` is not allowed")),
        false => Ok(()),
    }
}

/// What is wrong with a `url()` target (escapes decoded), `None` when it is a `data:`
/// URL or names a file of `files`.
fn url_problem(value: &str, files: &BTreeSet<String>) -> Option<&'static str> {
    let v = value.trim();
    if v.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
        return None;
    }
    // Browsers read `\` in a URL as `/`.
    let v = v.replace('\\', "/");
    if v.starts_with('/') {
        return Some("an absolute URL is not allowed");
    }
    if has_scheme(&v) {
        return Some("a URL scheme other than data: is not allowed");
    }
    let path = v.split(['?', '#']).next().unwrap_or_default();
    let Some(path) = percent_decode(path) else {
        return Some("bad percent-encoding");
    };
    let path = path.replace('\\', "/");
    let segments: Vec<&str> = path.split('/').collect();
    if segments.contains(&"..") {
        return Some("a path may not leave the theme folder");
    }
    let joined = segments
        .into_iter()
        .filter(|s| *s != ".")
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        return Some("names no file");
    }
    match files.contains(&joined) {
        true => None,
        false => Some("no such file in the theme folder"),
    }
}

/// Whether `v` starts with `<scheme>:`, the scheme a letter then letters, digits, `+`,
/// `-` or `.`.
fn has_scheme(v: &str) -> bool {
    let Some((scheme, _)) = v.split_once(':') else {
        return false;
    };
    let mut cs = scheme.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `%XX` decoded; `None` for a bad escape or bytes that are not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One top-level rule lifted out of the wrapper: tokens `from..to`.
struct Hoisted {
    from: usize,
    to: usize,
    /// `None` for an `@font-face`; for an `@keyframes`, the token naming it (`None` for
    /// a rule without a name).
    name: Option<Option<usize>>,
}

impl Hoisted {
    /// The keyframes' decoded name.
    fn name<'a>(&self, toks: &'a [Tok]) -> Option<&'a str> {
        match &toks[(*self.name.as_ref()?)?].kind {
            Kind::Ident(n) | Kind::Str(n) => Some(n),
            _ => None,
        }
    }
}

/// The top-level (bracket depth 0) `@font-face` and `@keyframes` rules, in file order.
fn hoist(toks: &[Tok]) -> Vec<Hoisted> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].kind {
            Kind::Open(_) => depth += 1,
            Kind::Close(_) => depth = depth.saturating_sub(1),
            Kind::AtKeyword(n) if depth == 0 => {
                let n = n.to_ascii_lowercase();
                let face = n == "font-face";
                let vendor = n.starts_with('-') && n.ends_with("-keyframes");
                let frames = n == "keyframes" || vendor;
                if (face || frames)
                    && let Some(to) = block_end(toks, i)
                {
                    let named = |k: &usize| matches!(toks[*k].kind, Kind::Ident(_) | Kind::Str(_));
                    let name = frames.then(|| {
                        toks[i + 1..to]
                            .iter()
                            .position(|t| !matches!(t.kind, Kind::Ws | Kind::Comment))
                            .map(|k| i + 1 + k)
                            .filter(named)
                    });
                    out.push(Hoisted { from: i, to, name });
                    i = to;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The end (exclusive) of the at-rule starting at token `at`: one past the `}` closing
/// its block, `None` when a `;` ends it first or it has no block.
fn block_end(toks: &[Tok], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut block = false;
    for (j, t) in toks.iter().enumerate().skip(at + 1) {
        match t.kind {
            Kind::Open(c) => {
                block |= depth == 0 && c == '{';
                depth += 1;
            }
            Kind::Close(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && block {
                    return Some(j + 1);
                }
            }
            Kind::Semi if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// A keyframes name token renamed `<id>--<name>`, as written (an identifier or a
/// string, its quotes kept).
fn renamed(id: &str, t: &Tok, css: &str) -> String {
    let src = &css[t.start..t.end];
    match t.kind {
        Kind::Str(_) => format!("{}{id}--{}", &src[..1], &src[1..]),
        _ => format!("{id}--{src}"),
    }
}

/// The file without its hoisted rules, the names of hoisted keyframes in
/// `animation`/`animation-name` values renamed.
fn body(css: &str, toks: &[Tok], hoisted: &[Hoisted], names: &BTreeSet<&str>, id: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut skip = hoisted.iter().peekable();
    let mut depth = 0usize;
    // The depth of the `animation` value being read, if any.
    let mut value: Option<usize> = None;
    let mut pending = false;
    let mut i = 0;
    while i < toks.len() {
        if let Some(h) = skip.next_if(|h| h.from == i) {
            i = h.to;
            continue;
        }
        let t = &toks[i];
        let src = &css[t.start..t.end];
        let was_pending = std::mem::take(&mut pending);
        let in_value = value == Some(depth);
        match &t.kind {
            Kind::Ws | Kind::Comment => pending = was_pending,
            Kind::Ident(n) if value.is_none() && is_animation_property(n) => pending = true,
            Kind::Colon if was_pending => value = Some(depth),
            Kind::Ident(n)
                if in_value
                    && names.contains(n.as_str())
                    && !matches!(toks.get(i + 1).map(|t| &t.kind), Some(Kind::Open('('))) =>
            {
                out.push_str(&renamed(id, t, css));
                i += 1;
                continue;
            }
            Kind::Str(n) if in_value && names.contains(n.as_str()) => {
                out.push_str(&renamed(id, t, css));
                i += 1;
                continue;
            }
            Kind::Open(c) => {
                if *c == '{' {
                    value = None;
                }
                depth += 1;
            }
            Kind::Close(_) => {
                depth = depth.saturating_sub(1);
                if value.is_some_and(|d| depth < d) {
                    value = None;
                }
            }
            Kind::Semi if in_value => value = None,
            Kind::Other if in_value && src == "!" => value = None,
            _ => {}
        }
        out.push_str(src);
        i += 1;
    }
    out
}

/// `animation` or `animation-name`, maybe vendor-prefixed (`-webkit-animation`).
fn is_animation_property(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = match n.strip_prefix('-').and_then(|r| r.split_once('-')) {
        Some((_, rest)) => rest.to_string(),
        None => n,
    };
    n == "animation" || n == "animation-name"
}

/// A token's kind; identifiers, at-keywords, strings and `url()` targets carry their
/// value with escapes decoded.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Ws,
    Comment,
    Ident(String),
    AtKeyword(String),
    Str(String),
    /// `url(...)`: the target as written, quotes stripped, and decoded.
    Url {
        raw: String,
        value: String,
    },
    /// `(`, `[` or `{`.
    Open(char),
    /// `)`, `]` or `}`.
    Close(char),
    Colon,
    Semi,
    /// Anything else: one character, or a number with its unit.
    Other,
}

/// A token: its kind and its byte range in the text.
#[derive(Clone, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

/// The tokeniser.
struct Lexer<'a> {
    s: &'a str,
    cs: Vec<(usize, char)>,
    i: usize,
    field: &'a str,
    /// Prefix errors with the line they are on.
    lines: bool,
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0c')
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t') || is_newline(c)
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str, field: &'a str, lines: bool) -> Self {
        Self {
            s,
            cs: s.char_indices().collect(),
            i: 0,
            field,
            lines,
        }
    }

    fn peek(&self, k: usize) -> Option<char> {
        self.cs.get(self.i + k).map(|&(_, c)| c)
    }

    /// The byte offset of the character at `i` (the text's end past the last).
    fn byte(&self, i: usize) -> usize {
        self.cs.get(i).map_or(self.s.len(), |&(b, _)| b)
    }

    fn err(&self, at: usize, what: &str) -> ThemeError {
        let msg = match self.lines {
            true => {
                let line = self.s[..at].matches('\n').count() + 1;
                format!("line {line}: {what}")
            }
            false => what.to_string(),
        };
        ThemeError::new(self.field, msg)
    }

    /// Whether a valid escape (`\` not before a newline or the end) starts at `i + k`.
    fn escape_at(&self, k: usize) -> bool {
        self.peek(k) == Some('\\') && self.peek(k + 1).is_some_and(|c| !is_newline(c))
    }

    /// Whether an identifier starts at `i + k`.
    fn ident_at(&self, k: usize) -> bool {
        match self.peek(k) {
            Some('-') => {
                self.peek(k + 1)
                    .is_some_and(|c| is_name_start(c) || c == '-')
                    || self.escape_at(k + 1)
            }
            Some(c) if is_name_start(c) => true,
            _ => self.escape_at(k),
        }
    }

    /// Whether a number starts at `i`.
    fn number_at(&self) -> bool {
        let digit = |k| self.peek(k).is_some_and(|c: char| c.is_ascii_digit());
        match self.peek(0) {
            Some(c) if c.is_ascii_digit() => true,
            Some('.') => digit(1),
            Some('+' | '-') => digit(1) || (self.peek(1) == Some('.') && digit(2)),
            _ => false,
        }
    }

    /// Consume an escape (at a valid one) and return the character it stands for.
    fn escape(&mut self) -> char {
        self.i += 1;
        let mut hex = String::new();
        while hex.len() < 6 && self.peek(0).is_some_and(|c| c.is_ascii_hexdigit()) {
            hex.push(self.peek(0).unwrap_or_default());
            self.i += 1;
        }
        if hex.is_empty() {
            let c = self.peek(0).unwrap_or('\u{fffd}');
            self.i += 1;
            return c;
        }
        if self.peek(0) == Some('\r') && self.peek(1) == Some('\n') {
            self.i += 2;
        } else if self.peek(0).is_some_and(is_ws) {
            self.i += 1;
        }
        u32::from_str_radix(&hex, 16)
            .ok()
            .filter(|&v| v != 0)
            .and_then(char::from_u32)
            .unwrap_or('\u{fffd}')
    }

    /// Consume an identifier's characters and return it decoded.
    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_name(c) => {
                    out.push(c);
                    self.i += 1;
                }
                Some('\\') if self.escape_at(0) => out.push(self.escape()),
                _ => return out,
            }
        }
    }

    /// Consume a string (at its opening quote) and return it decoded.
    fn string(&mut self) -> Result<String, ThemeError> {
        let start = self.byte(self.i);
        let quote = self.peek(0).unwrap_or('"');
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Err(self.err(start, "unclosed string")),
                Some(c) if c == quote => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(c) if is_newline(c) => return Err(self.err(start, "unclosed string")),
                Some('\\') => match self.peek(1) {
                    None => return Err(self.err(start, "unclosed string")),
                    Some('\r') if self.peek(2) == Some('\n') => self.i += 3,
                    Some(c) if is_newline(c) => self.i += 2,
                    Some(_) => out.push(self.escape()),
                },
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Consume `url(` ... `)` (at the `(`) and return the target as written and decoded.
    fn url(&mut self, start: usize) -> Result<Kind, ThemeError> {
        let bad = |l: &Self, what: &str| l.err(start, &format!("url(...): {what}"));
        self.i += 1;
        while self.peek(0).is_some_and(is_ws) {
            self.i += 1;
        }
        if let Some('"' | '\'') = self.peek(0) {
            let from = self.byte(self.i) + 1;
            let value = self.string()?;
            let raw = self.s[from..self.byte(self.i) - 1].to_string();
            while self.peek(0).is_some_and(is_ws) {
                self.i += 1;
            }
            if self.peek(0) != Some(')') {
                return Err(bad(self, "not closed by `)`"));
            }
            self.i += 1;
            return Ok(Kind::Url { raw, value });
        }
        let from = self.byte(self.i);
        let mut value = String::new();
        loop {
            match self.peek(0) {
                None => return Err(bad(self, "not closed by `)`")),
                Some(')') => break,
                Some(c) if is_ws(c) => {
                    let to = self.byte(self.i);
                    while self.peek(0).is_some_and(is_ws) {
                        self.i += 1;
                    }
                    if self.peek(0) != Some(')') {
                        return Err(bad(self, "not closed by `)`"));
                    }
                    self.i += 1;
                    let raw = self.s[from..to].to_string();
                    return Ok(Kind::Url { raw, value });
                }
                Some('"' | '\'' | '(') => return Err(bad(self, "a quote or `(` in a bare URL")),
                Some('\\') if self.escape_at(0) => value.push(self.escape()),
                Some('\\') => return Err(bad(self, "a stray `\\`")),
                Some(c) => {
                    value.push(c);
                    self.i += 1;
                }
            }
        }
        let raw = self.s[from..self.byte(self.i)].to_string();
        self.i += 1;
        Ok(Kind::Url { raw, value })
    }

    /// The next token's kind, consumed.
    fn next(&mut self, start: usize) -> Result<Kind, ThemeError> {
        let c = self.peek(0).unwrap_or_default();
        if is_ws(c) {
            while self.peek(0).is_some_and(is_ws) {
                self.i += 1;
            }
            return Ok(Kind::Ws);
        }
        if c == '/' && self.peek(1) == Some('*') {
            let rest = &self.s[start + 2..];
            let Some(end) = rest.find("*/") else {
                return Err(self.err(start, "unclosed comment"));
            };
            let end = start + 2 + end + 2;
            while self.byte(self.i) < end {
                self.i += 1;
            }
            return Ok(Kind::Comment);
        }
        if c == '"' || c == '\'' {
            return self.string().map(Kind::Str);
        }
        if c == '@' && self.ident_at(1) {
            self.i += 1;
            return Ok(Kind::AtKeyword(self.name()));
        }
        if self.number_at() {
            self.i += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '.') {
                self.i += 1;
            }
            if self.ident_at(0) {
                self.name();
            } else if self.peek(0) == Some('%') {
                self.i += 1;
            }
            return Ok(Kind::Other);
        }
        if self.ident_at(0) {
            let name = self.name();
            if self.peek(0) == Some('(') && name.eq_ignore_ascii_case("url") {
                return self.url(start);
            }
            return Ok(Kind::Ident(name));
        }
        self.i += 1;
        Ok(match c {
            '(' | '[' | '{' => Kind::Open(c),
            ')' | ']' | '}' => Kind::Close(c),
            ':' => Kind::Colon,
            ';' => Kind::Semi,
            _ => Kind::Other,
        })
    }

    /// Every token of the text; brackets must pair up.
    fn run(mut self) -> Result<Vec<Tok>, ThemeError> {
        let mut toks = Vec::new();
        let mut open: Vec<(char, usize)> = Vec::new();
        while self.i < self.cs.len() {
            let start = self.byte(self.i);
            let kind = self.next(start)?;
            match kind {
                Kind::Open(c) => open.push((c, start)),
                Kind::Close(c) => {
                    let want = match c {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    match open.pop() {
                        Some((o, _)) if o == want => {}
                        _ => return Err(self.err(start, &format!("unmatched `{c}`"))),
                    }
                }
                _ => {}
            }
            toks.push(Tok {
                kind,
                start,
                end: self.byte(self.i),
            });
        }
        match open.pop() {
            Some((c, at)) => Err(self.err(at, &format!("unclosed `{c}`"))),
            None => Ok(toks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes::theme_json::parse;

    fn theme() -> ThemeJson {
        parse(
            r##"{
                "id": "t",
                "name": "T",
                "description": "A test theme.",
                "color_scheme": "dark",
                "layout": { "rail": true, "ribbon_tilt_deg": -15 },
                "tokens": { "--accent": "#fff" },
                "ramp": [[0, [0, 0, 0]], [1, [255, 255, 255]]],
                "fonts": [{ "family": "Face", "file": "Face.woff2", "weight": "400 700" }]
            }"##,
        )
        .unwrap()
    }

    fn files() -> BTreeSet<String> {
        [
            "theme.json",
            "style.css",
            "Face.woff2",
            "bg.png",
            "img/dot.svg",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn wrap(css: &str) -> String {
        wrap_style(&theme(), css, &files()).unwrap_or_else(|e| panic!("{css:?}: {e}"))
    }

    fn refused(css: &str) -> ThemeError {
        match wrap_style(&theme(), css, &files()) {
            Ok(out) => panic!("{css:?} was accepted: {out}"),
            Err(e) => e,
        }
    }

    /// The byte offset of `needle` in `out`, which must hold it.
    fn at(out: &str, needle: &str) -> usize {
        out.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {out}"))
    }

    #[test]
    fn wrap_scopes_the_file() {
        let out = wrap(".x { color: red; }\n");
        let open = at(&out, "[data-theme=\"t\"] {");
        assert!(at(&out, ".x { color: red; }") > open, "{out}");
        assert_eq!(out.trim_end().chars().last(), Some('}'), "{out}");
    }

    #[test]
    fn tokens_and_layout_come_first() {
        let out = wrap(".first { color: red; }\n");
        let rule = at(&out, ".first");
        let open = at(&out, "[data-theme=\"t\"] {");
        for decl in [
            "color-scheme: dark;",
            "--ribbon-tilt: -15deg;",
            "--accent: #fff;",
        ] {
            let i = at(&out, decl);
            assert!(open < i && i < rule, "{decl} in {out}");
        }
    }

    #[test]
    fn a_theme_without_style_css_still_gets_one() {
        let out = wrap("");
        at(&out, "@font-face");
        at(&out, "--accent: #fff;");
        assert_eq!(out.trim_end().chars().last(), Some('}'), "{out}");
    }

    #[test]
    fn fonts_become_font_faces() {
        let out = wrap("");
        let face = "@font-face { font-family: \"Face\"; src: url(\"Face.woff2\"); \
                    font-weight: 400 700; font-display: swap; }";
        assert!(at(&out, face) < at(&out, "[data-theme"), "{out}");
    }

    #[test]
    fn font_face_and_keyframes_are_hoisted() {
        let css = "@font-face { font-family: \"Mine\"; src: url(\"Face.woff2\"); }\n\
                   @keyframes pulse { from { opacity: 0; } to { opacity: 1; } }\n\
                   @media (prefers-reduced-motion: no-preference) {\n\
                   .a { animation: pulse 2s infinite; }\n\
                   .b { animation: 2s pulsex; }\n\
                   .c { animation-name: pulse; }\n\
                   }\n";
        let out = wrap(css);
        let wrapper = at(&out, "[data-theme");
        let mine = at(
            &out,
            "@font-face { font-family: \"Mine\"; src: url(\"Face.woff2\"); }",
        );
        let declared = at(&out, "font-family: \"Face\"");
        let kf = at(
            &out,
            "@keyframes t--pulse { from { opacity: 0; } to { opacity: 1; } }",
        );
        assert!(declared < mine && mine < kf && kf < wrapper, "{out}");
        assert!(
            at(&out, "animation: t--pulse 2s infinite;") > wrapper,
            "{out}"
        );
        assert!(at(&out, "animation: 2s pulsex;") > wrapper, "{out}");
        assert!(at(&out, "animation-name: t--pulse;") > wrapper, "{out}");
        assert!(!out.contains("@keyframes pulse"), "{out}");
        assert_eq!(out.matches("@keyframes").count(), 1, "{out}");
        assert_eq!(out.matches("font-family: \"Mine\"").count(), 1, "{out}");
    }

    #[test]
    fn nested_keyframes_stay_where_they_are() {
        let css = "@media print { @keyframes blink { to { opacity: 0; } } }\n\
                   .a { animation: blink 1s; }\n";
        let out = wrap(css);
        let wrapper = at(&out, "[data-theme");
        assert!(
            at(&out, "@media print { @keyframes blink {") > wrapper,
            "{out}"
        );
        at(&out, "animation: blink 1s;");
    }

    #[test]
    fn vendor_keyframes_are_hoisted() {
        let out = wrap(
            "@-webkit-keyframes spin { to { opacity: 0; } }\n.a { -webkit-animation: spin 1s; }\n",
        );
        assert!(at(&out, "@-webkit-keyframes t--spin {") < at(&out, "[data-theme"));
        at(&out, "-webkit-animation: t--spin 1s;");
    }

    #[test]
    fn wrap_ignores_braces_and_at_rules_in_comments_and_strings() {
        let css = "/* } @import url(x) */\n.a::after { content: \"}{@import\"; }\n.b { content: '\\'}'; }\n";
        let out = wrap(css);
        assert!(
            at(&out, "/* } @import url(x) */") > at(&out, "[data-theme"),
            "{out}"
        );
        at(&out, ".a::after { content: \"}{@import\"; }");
        at(&out, ".b { content: '\\'}'; }");
    }

    #[test]
    fn media_blocks_and_nesting_survive() {
        let css = "@media (prefers-reduced-motion: no-preference) { .x { & .y {} } }";
        let out = wrap(css);
        assert!(at(&out, css) > at(&out, "[data-theme"), "{out}");
    }

    #[test]
    fn import_is_refused() {
        for css in [
            "@import \"other.css\";",
            "@import url(other.css);",
            ".a { @import \"x.css\"; }",
            "@IMPORT \"x.css\";",
            "@\\69mport \"x.css\";",
        ] {
            let e = refused(css);
            assert_eq!(e.field, "style.css", "{css}: {e}");
            assert_eq!(e.error, "@import is not allowed", "{css}: {e}");
        }
    }

    #[test]
    fn url_escape_is_refused() {
        for (css, target) in [
            (".a { background: url(../x.png); }", "../x.png"),
            (".a { background: url(/x.png); }", "/x.png"),
            (
                ".a { background: url(https://e/x.png); }",
                "https://e/x.png",
            ),
            (".a { background: url(\"sub/../../x\"); }", "sub/../../x"),
            (".a { background: url(missing.png); }", "missing.png"),
            (".a { background: url('//e/x.png'); }", "//e/x.png"),
            (".a { background: url(%2e%2e/x.png); }", "%2e%2e/x.png"),
            (".a { background: URL(\"..\\\\x.png\"); }", "..\\\\x.png"),
            (".a { background: url(\\2e\\2e/x.png); }", "\\2e\\2e/x.png"),
        ] {
            let e = refused(css);
            assert_eq!(e.field, "style.css", "{css}: {e}");
            let head = format!("url({target}): ");
            assert!(e.error.starts_with(&head), "{css}: {e}");
        }
    }

    #[test]
    fn data_urls_pass() {
        let css = ".a { background: url(\"data:image/svg+xml;utf8,<svg/>\"); }\n\
                   .b { background: url(data:image/png;base64,AAAA); }\n";
        let out = wrap(css);
        at(&out, css.lines().next().unwrap());
    }

    #[test]
    fn url_to_a_folder_file_passes() {
        let out = wrap(
            ".a { background: url(\"bg.png\"); }\n\
             .b { background: url(img/dot.svg); }\n\
             .c { background: url('./bg.png?v=1#x'); }\n",
        );
        at(&out, "url(\"bg.png\")");
        at(&out, "url(img/dot.svg)");
    }

    #[test]
    fn oversized_css_is_refused() {
        let css = format!(".a{{}}{}", " ".repeat(MAX_STYLE_BYTES + 1 - 4));
        assert_eq!(css.len(), 1_048_577);
        let e = refused(&css);
        assert_eq!(e.field, "style.css");
        assert!(e.error.contains("1 MiB"), "{e}");
        assert_eq!(
            e.to_string(),
            "style.css: 1048577 bytes, more than the 1 MiB limit"
        );
        assert!(wrap_style(&theme(), &css[..MAX_STYLE_BYTES], &files()).is_ok());
    }

    #[test]
    fn wrap_strips_a_bom() {
        let out = wrap("\u{feff}.a { color: red; }");
        assert!(!out.contains('\u{feff}'), "{out:?}");
        at(&out, ".a { color: red; }");
    }

    #[test]
    fn crlf_survives() {
        let css = ".a {\r\n  color: red;\r\n}\r\n";
        let out = wrap(css);
        at(&out, css);
    }

    #[test]
    fn unbalanced_css_is_refused() {
        for css in [
            ".a { color: red; }\n} html { color: red; }",
            ".a { color: red;",
            ".a { content: \"x }",
            "/* open",
            ".a { width: calc(1px + 2px; }",
        ] {
            let e = refused(css);
            assert_eq!(e.field, "style.css", "{css}: {e}");
        }
    }

    #[test]
    fn a_token_or_font_that_breaks_the_wrapper_is_refused() {
        let mut t = theme();
        t.tokens.insert("--accent".into(), "#fff /*".into());
        let e = wrap_style(&t, "", &files()).unwrap_err();
        assert_eq!(e.field, "tokens.--accent", "{e}");
        let mut t = theme();
        t.fonts[0].weight = "400\\".into();
        let e = wrap_style(&t, "", &files()).unwrap_err();
        assert_eq!(e.field, "fonts[0].weight", "{e}");
    }
}

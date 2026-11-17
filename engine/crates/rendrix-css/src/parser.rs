//! Stylesheet parser: rules, declarations, `@media`, `@keyframes`, `@font-face`, `@supports`, `@layer`.
//! Error recovery follows the CSS spec's spirit: an invalid rule or declaration is dropped, the rest survives.

use crate::selector::{split_top_level, Selector};

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Lower-cased property name; custom properties (`--x`) keep their case.
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone)]
pub struct Keyframes {
    pub name: String,
    /// Sorted by offset (0.0..=1.0).
    pub frames: Vec<(f32, Vec<Declaration>)>,
}

#[derive(Debug, Clone)]
pub struct FontFace {
    pub family: String,
    pub src: String,
    pub weight: Option<u16>,
    pub italic: bool,
}

/// A parsed `@supports` condition. Evaluated by the cascade against the property
/// parser so unsupported declarations are genuinely rejected, not flattened away.
#[derive(Debug, Clone, PartialEq)]
pub enum SupportsCondition {
    Declaration(String, String),
    Selector(String),
    Not(Box<SupportsCondition>),
    And(Vec<SupportsCondition>),
    Or(Vec<SupportsCondition>),
    /// An unrecognised function form; evaluates to false.
    Unsupported,
}

#[derive(Debug, Clone)]
pub enum Rule {
    Style(StyleRule),
    Media(MediaQueryList, Vec<Rule>),
    Supports(SupportsCondition, Vec<Rule>),
    /// `@layer name { ... }` (anonymous when the name is `None`).
    Layer(Option<String>, Vec<Rule>),
    /// `@layer a, b;` — registers layer names in declaration order.
    LayerStatement(Vec<String>),
    /// `@container <name>? <size-condition> { ... }`. The condition reuses media
    /// feature syntax (min-width/max-width/etc.) evaluated against the container box.
    Container(String, MediaQueryList, Vec<Rule>),
    Keyframes(Keyframes),
    FontFace(FontFace),
    /// `@import "x.css" [media]` — resolved against bundled assets in `rendrix-core`.
    /// Stored as (url, media prelude). Rules from the imported sheet splice in place.
    Import(String, String),
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn parse(source: &str) -> Stylesheet {
        let clean = strip_comments(source);
        Stylesheet { rules: parse_rules(&clean) }
    }
}

/// P2: parse-once cache keyed by content hash. `refresh_rules` used to re-tokenise every
/// stylesheet on every resize; now unchanged sheets are parsed once and only the cheap
/// `RuleSet` filter is rebuilt under the new `MediaContext`.
pub fn parse_cached(source: &str) -> Arc<Stylesheet> {
    thread_local! {
        static CACHE: RefCell<HashMap<u64, Arc<Stylesheet>>> = RefCell::new(HashMap::new());
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let key = h.finish();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
        // Bounded: drop the whole generation if it grows without bound (e.g. HMR churn).
        if c.len() >= 64 {
            c.clear();
        }
        let sheet = Arc::new(Stylesheet::parse(source));
        c.insert(key, sheet.clone());
        sheet
    })
}

/// The user-agent stylesheet, parsed once per thread instead of on every style refresh.
pub fn ua_sheet() -> Arc<Stylesheet> {
    static UA: OnceLock<Arc<Stylesheet>> = OnceLock::new();
    UA.get_or_init(|| Arc::new(Stylesheet::parse(crate::ua::USER_AGENT_CSS))).clone()
}

pub fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut last = 0;
    while i < b.len() {
        if let Some(q) = quote {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b[i] == b'"' || b[i] == b'\'' {
            quote = Some(b[i]);
            i += 1;
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            out.push_str(&s[last..i]);
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            last = i.min(b.len());
        } else {
            i += 1;
        }
    }
    out.push_str(&s[last.min(s.len())..]);
    out
}

/// Find the index of the `}` matching the `{` at `open`.
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < b.len() {
        if let Some(q) = quote {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == q {
                quote = None;
            }
        } else {
            match b[i] {
                b'"' | b'\'' => quote = Some(b[i]),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn parse_rules(s: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let trimmed = rest.trim_start();
        i += rest.len() - trimmed.len();
        if i >= s.len() {
            break;
        }
        let rest = &s[i..];
        // At-rule without a block, e.g. @import / @charset / @layer statement.
        let brace = find_unquoted(rest, '{');
        let semi = find_unquoted(rest, ';');
        if rest.starts_with('@') {
            match (brace, semi) {
                (_, Some(sp)) if brace.is_none_or(|b| sp < b) => {
                    let stmt = s[i..i + sp].trim().to_string();
                    if let Some((url, media)) = parse_import(&stmt) {
                        rules.push(Rule::Import(url, media));
                    } else if let Some(rest) = stmt.strip_prefix("@layer").or_else(|| stmt.strip_prefix("@LAYER")) {
                        let names: Vec<String> = rest.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
                        if !names.is_empty() {
                            rules.push(Rule::LayerStatement(names));
                        }
                    }
                    i += sp + 1;
                    continue;
                }
                (Some(bp), _) => {
                    let open = i + bp;
                    let Some(close) = matching_brace(s, open) else { break };
                    let prelude = s[i..open].trim();
                    let body = &s[open + 1..close];
                    parse_at_rule(prelude, body, &mut rules);
                    i = close + 1;
                    continue;
                }
                _ => break,
            }
        }
        let Some(bp) = brace else { break };
        let open = i + bp;
        let Some(close) = matching_brace(s, open) else { break };
        let selector_text = s[i..open].trim();
        let body = &s[open + 1..close];
        let selectors = Selector::parse_list(selector_text);
        // Parent declarations ignore nested rule blocks (stripped first).
        let declarations = parse_declarations(&strip_nested(body));
        if !selectors.is_empty() {
            if declarations.len() > 0 || !body.contains('{') {
                rules.push(Rule::Style(StyleRule { selectors, declarations }));
            } else if declarations.is_empty() && !body.contains('{') {
                // Empty rule: keep nothing.
            } else {
                rules.push(Rule::Style(StyleRule { selectors, declarations }));
            }
            // Nested rules: `parent { color: red; & .child { ... } .bare { ... } @media ... { ... } }`
            // Expand by combining parent + child selectors (`&` = parent, else descendant).
            for nested in expand_nested(selector_text, body) {
                rules.push(nested);
            }
        } else if !selector_text.is_empty() && body.contains('{') {
            // Parent selector failed to parse but body has nested rules (e.g. `&` at
            // top level is invalid); try expanding raw text so valid children survive.
            for nested in expand_nested(selector_text, body) {
                rules.push(nested);
            }
        }
        i = close + 1;
    }
    rules
}

/// Parse `@import "url" [media];` / `@import url(x) [media];`. Returns (url, media prelude).
fn parse_import(stmt: &str) -> Option<(String, String)> {
    let mut s = stmt.trim();
    if !s.to_ascii_lowercase().starts_with("@import") {
        return None;
    }
    s = s["@import".len()..].trim();
    if s.is_empty() {
        return None;
    }
    let (url, rest) = if s.to_ascii_lowercase().starts_with("url(") {
        let Some(end) = s.find(')') else { return None };
        let inner = s[4..end].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        (inner, s[end + 1..].trim().to_string())
    } else if s.starts_with('"') || s.starts_with('\'') {
        let q = s.as_bytes()[0] as char;
        let Some(end) = s[1..].find(q).map(|p| p + 1) else { return None };
        (s[1..end].to_string(), s[end + 1..].trim().to_string())
    } else {
        // Bare URL up to whitespace (rare, but accept).
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        (s[..end].to_string(), s[end..].trim().to_string())
    };
    if url.is_empty() {
        return None;
    }
    Some((url, rest))
}

/// Expand nested rules inside a style block. `parent` is the raw selector prelude,
/// `body` the raw block content. Returns style/media rules with combined selectors.
fn expand_nested(parent: &str, body: &str) -> Vec<Rule> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        // Skip declarations / whitespace up to next `;` or `{`.
        let brace = find_unquoted(rest, '{');
        let semi = find_unquoted(rest, ';');
        match (brace, semi) {
            (Some(_), Some(sp)) if brace.is_none_or(|b| sp < b) => {
                i += sp + 1;
                continue;
            }
            (None, Some(sp)) => {
                i += sp + 1;
                continue;
            }
            (None, None) => break,
            (Some(bp), _) => {
                let open = i + bp;
                let prelude = body[i..open].trim();
                let Some(close) = matching_brace(body, open) else { break };
                let inner = &body[open + 1..close];
                if prelude.is_empty() {
                    i = close + 1;
                    continue;
                }
                if prelude.starts_with('@') {
                    // Nested at-rule (e.g. `@media` inside a rule): scope inner
                    // style selectors to the parent, then keep the at-rule wrapper.
                    // Textual prefixing preserves selector source (no serialization).
                    let scoped = scope_nested_at_inner(parent, inner);
                    let wrapped = format!("{} {{{}}}", prelude, scoped);
                    for r in parse_rules(&wrapped) {
                        out.push(r);
                    }
                } else {
                    // Nested style rule: `&` = parent, else `parent child`.
                    for p in split_top_level(parent, ',') {
                        let p = p.trim();
                        if p.is_empty() {
                            continue;
                        }
                        for c in split_top_level(prelude, ',') {
                            let c = c.trim();
                            if c.is_empty() {
                                continue;
                            }
                            let combined = if c.contains('&') {
                                c.replace('&', p)
                            } else {
                                format!("{} {}", p, c)
                            };
                            let selectors = Selector::parse_list(&combined);
                            if selectors.is_empty() {
                                continue;
                            }
                            let declarations = parse_declarations(inner);
                            if !declarations.is_empty() {
                                out.push(Rule::Style(StyleRule { selectors, declarations }));
                            }
                            // Recurse for deeper nesting.
                            for r in expand_nested(&combined, inner) {
                                out.push(r);
                            }
                        }
                    }
                }
                i = close + 1;
            }
        }
    }
    out
}

/// Remove nested `{...}` blocks from a declaration body so only the rule's own
/// declarations survive `parse_declarations` (which splits on `;`).
fn strip_nested(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        let brace = find_unquoted(rest, '{');
        let semi = find_unquoted(rest, ';');
        match (brace, semi) {
            (Some(bp), Some(sp)) if sp < bp => {
                out.push_str(&body[i..i + sp + 1]);
                i += sp + 1;
            }
            (Some(bp), _) => {
                let open = i + bp;
                // Keep text before the nested prelude? No — nested prelude is not a
                // declaration; skip the whole `prelude { ... }` segment.
                let Some(close) = matching_brace(body, open) else { break };
                i = close + 1;
            }
            (None, Some(sp)) => {
                out.push_str(&body[i..i + sp + 1]);
                i += sp + 1;
            }
            (None, None) => {
                // Trailing declaration without `;` (only if it has no block).
                out.push_str(&body[i..]);
                break;
            }
        }
    }
    out
}

/// Scope the top-level style preludes inside a nested at-rule body with `parent`.
/// `parent .child` (or `&`-substituted) so `@media` inside a rule keeps its scope.
fn scope_nested_at_inner(parent: &str, inner: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < inner.len() {
        let rest = &inner[i..];
        let brace = find_unquoted(rest, '{');
        let semi = find_unquoted(rest, ';');
        match (brace, semi) {
            (Some(_), Some(sp)) if brace.is_none_or(|b| sp < b) => {
                // Declaration (only valid if the at-rule accepts them, e.g. @font-face):
                // keep verbatim.
                out.push_str(&inner[i..i + sp + 1]);
                out.push(' ');
                i += sp + 1;
            }
            (None, Some(sp)) => {
                out.push_str(&inner[i..i + sp + 1]);
                out.push(' ');
                i += sp + 1;
            }
            (None, None) => {
                out.push_str(&inner[i..]);
                break;
            }
            (Some(bp), _) => {
                let open = i + bp;
                let prelude = inner[i..open].trim();
                let Some(close) = matching_brace(inner, open) else { break };
                let body = &inner[open + 1..close];
                if prelude.starts_with('@') {
                    // Nested at-rule inside at-rule: recurse scoping.
                    let scoped = scope_nested_at_inner(parent, body);
                    out.push_str(prelude);
                    out.push_str(" {");
                    out.push_str(&scoped);
                    out.push_str("} ");
                } else {
                    for p in split_top_level(parent, ',') {
                        let p = p.trim();
                        if p.is_empty() {
                            continue;
                        }
                        for c in split_top_level(prelude, ',') {
                            let c = c.trim();
                            if c.is_empty() {
                                continue;
                            }
                            let combined = if c.contains('&') { c.replace('&', p) } else { format!("{} {}", p, c) };
                            out.push_str(&combined);
                            out.push_str(" {");
                            out.push_str(body);
                            out.push_str("} ");
                        }
                    }
                }
                i = close + 1;
            }
        }
    }
    out
}

fn find_unquoted(s: &str, target: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == target && depth <= 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Split on a whitespace-delimited keyword (`and` / `or`), ignoring parens and quotes.
fn split_keyword(s: &str, kw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let mut i = 0usize;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        let clen = c.len_utf8();
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += clen;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
                i += clen;
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
                i += clen;
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
                i += clen;
            }
            _ => {
                let after = i + kw.len();
                if depth == 0
                    && s[i..].to_ascii_lowercase().starts_with(kw)
                    && (cur.is_empty() || cur.ends_with(char::is_whitespace))
                    && (after >= s.len() || s[after..].starts_with(char::is_whitespace) || s[after..].starts_with('('))
                {
                    out.push(std::mem::take(&mut cur).trim().to_string());
                    i += kw.len();
                    while i < s.len() && s[i..].starts_with(char::is_whitespace) {
                        i += s[i..].chars().next().unwrap().len_utf8();
                    }
                    continue;
                }
                cur.push(c);
                i += clen;
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// If `s` is entirely wrapped in one pair of parentheses, return its inside.
fn outer_parens(s: &str) -> Option<&str> {
    let t = s.trim();
    if !t.starts_with('(') || !t.ends_with(')') {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in t.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i + 1 != t.len() {
                    return None;
                }
            }
            _ => {}
        }
    }
    if depth == 0 { Some(&t[1..t.len() - 1]) } else { None }
}

/// Parse an `@supports` prelude into a boolean condition tree.
pub fn parse_supports_condition(s: &str) -> SupportsCondition {
    let s = s.trim();
    let ors = split_keyword(s, "or");
    if ors.len() > 1 {
        return SupportsCondition::Or(ors.iter().map(|p| parse_supports_condition(p)).collect());
    }
    let ands = split_keyword(s, "and");
    if ands.len() > 1 {
        return SupportsCondition::And(ands.iter().map(|p| parse_supports_condition(p)).collect());
    }
    if let Some(rest) = s.strip_prefix("not ").or_else(|| s.strip_prefix("NOT ")) {
        return SupportsCondition::Not(Box::new(parse_supports_condition(rest)));
    }
    if let Some(inner) = outer_parens(s) {
        let inner = inner.trim();
        if let Some(rest) = inner.strip_prefix("selector(").or_else(|| inner.strip_prefix("SELECTOR(")) {
            return rest.strip_suffix(')').map(|r| SupportsCondition::Selector(r.to_string())).unwrap_or(SupportsCondition::Unsupported);
        }
        // A nested condition group (contains another parenthesised term).
        if inner.contains('(') {
            return parse_supports_condition(inner);
        }
        if let Some((name, value)) = inner.split_once(':') {
            return SupportsCondition::Declaration(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    SupportsCondition::Unsupported
}

fn parse_at_rule(prelude: &str, body: &str, out: &mut Vec<Rule>) {
    let (name, args) = match prelude.find(|c: char| c.is_whitespace() || c == '(') {
        Some(p) => (&prelude[1..p], prelude[p..].trim()),
        None => (&prelude[1..], ""),
    };
    match name.to_ascii_lowercase().as_str() {
        "media" => out.push(Rule::Media(MediaQueryList::parse(args), parse_rules(body))),
        "supports" => out.push(Rule::Supports(parse_supports_condition(args), parse_rules(body))),
        "layer" => {
            let name = args.trim();
            out.push(Rule::Layer(if name.is_empty() { None } else { Some(name.to_string()) }, parse_rules(body)));
        }
        "container" if !body.trim().is_empty() => {
            // Split optional name from the size condition. A bare `(…)` starts the condition.
            let a = args.trim();
            let (name, cond) = if a.starts_with('(') {
                (String::new(), a.to_string())
            } else {
                match a.find(|c: char| c == '(') {
                    Some(p) => (a[..p].trim().trim_matches(|c| c == '"' || c == '\'').to_string(), a[p..].to_string()),
                    None => (a.to_string(), String::new()),
                }
            };
            out.push(Rule::Container(name, MediaQueryList::parse(&cond), parse_rules(body)));
        }
        "keyframes" | "-webkit-keyframes" => {
            let mut frames = Vec::new();
            let mut i = 0;
            while let Some(bp) = find_unquoted(&body[i..], '{') {
                let open = i + bp;
                let Some(close) = matching_brace(body, open) else { break };
                let decls = parse_declarations(&body[open + 1..close]);
                for sel in body[i..open].split(',') {
                    let off = match sel.trim().to_ascii_lowercase().as_str() {
                        "from" => Some(0.0),
                        "to" => Some(1.0),
                        t => t.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()).map(|v| v / 100.0),
                    };
                    if let Some(o) = off.filter(|o| o.is_finite()) {
                        frames.push((o.clamp(0.0, 1.0), decls.clone()));
                    }
                }
                i = close + 1;
            }
            frames.sort_by(|a, b| a.0.total_cmp(&b.0));
            out.push(Rule::Keyframes(Keyframes { name: args.trim_matches(|c| c == '"' || c == '\'').to_string(), frames }));
        }
        "font-face" => {
            let decls = parse_declarations(body);
            let get = |n: &str| decls.iter().find(|d| d.name == n).map(|d| d.value.clone());
            if let (Some(family), Some(src)) = (get("font-family"), get("src")) {
                out.push(Rule::FontFace(FontFace {
                    family: family.trim_matches(|c| c == '"' || c == '\'').to_string(),
                    src,
                    weight: get("font-weight").and_then(|w| match w.as_str() {
                        "bold" => Some(700),
                        "normal" => Some(400),
                        n => n.parse().ok(),
                    }),
                    italic: get("font-style").is_some_and(|s| s == "italic" || s == "oblique"),
                }));
            }
        }
        _ => {}
    }
}

pub fn parse_declarations(body: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in split_top_level(body, ';') {
        let Some(colon) = part.find(':') else { continue };
        let name = part[..colon].trim();
        let mut value = part[colon + 1..].trim().to_string();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let mut important = false;
        let lower = value.to_ascii_lowercase();
        if let Some(p) = lower.rfind("!important") {
            if lower[p + 10..].trim().is_empty() {
                important = true;
                value = value[..p].trim().to_string();
            }
        }
        let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
        out.push(Declaration { name, value, important });
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Media queries
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct MediaContext {
    pub width: f32,
    pub height: f32,
    pub dpr: f32,
    pub dark: bool,
    pub reduced_motion: bool,
}

impl Default for MediaContext {
    fn default() -> Self {
        MediaContext { width: 800.0, height: 600.0, dpr: 1.0, dark: false, reduced_motion: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MediaQueryList {
    queries: Vec<MediaQuery>,
}

#[derive(Debug, Clone)]
struct MediaQuery {
    negated: bool,
    media_type: String,
    features: Vec<(String, Option<String>)>,
}

impl MediaQueryList {
    pub fn parse(s: &str) -> Self {
        let mut queries = Vec::new();
        for q in s.split(',') {
            let mut q = q.trim().to_ascii_lowercase();
            if q.is_empty() {
                continue;
            }
            let mut negated = false;
            if let Some(r) = q.strip_prefix("not ") {
                negated = true;
                q = r.to_string();
            }
            if let Some(r) = q.strip_prefix("only ") {
                q = r.to_string();
            }
            let mut media_type = "all".to_string();
            let mut features = Vec::new();
            for tok in q.split(" and ") {
                let tok = tok.trim();
                if let Some(inner) = tok.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
                    match inner.split_once(':') {
                        Some((k, v)) => features.push((k.trim().to_string(), Some(v.trim().to_string()))),
                        None => features.push((inner.trim().to_string(), None)),
                    }
                } else if !tok.is_empty() {
                    media_type = tok.to_string();
                }
            }
            queries.push(MediaQuery { negated, media_type, features });
        }
        MediaQueryList { queries }
    }

    pub fn matches(&self, cx: &MediaContext) -> bool {
        if self.queries.is_empty() {
            return true;
        }
        self.queries.iter().any(|q| {
            let type_ok = matches!(q.media_type.as_str(), "all" | "screen");
            let ok = type_ok && q.features.iter().all(|(k, v)| feature_matches(k, v.as_deref(), cx));
            ok != q.negated
        })
    }
}

fn px(v: &str, cx: &MediaContext) -> Option<f32> {
    let _ = cx;
    if let Some(n) = v.strip_suffix("px") {
        n.trim().parse().ok()
    } else if let Some(n) = v.strip_suffix("em").or_else(|| v.strip_suffix("rem")) {
        n.trim().parse::<f32>().ok().map(|x| x * 16.0)
    } else {
        v.parse().ok()
    }
}

fn feature_matches(k: &str, v: Option<&str>, cx: &MediaContext) -> bool {
    match (k, v) {
        ("min-width", Some(v)) => px(v, cx).is_some_and(|x| cx.width >= x),
        ("max-width", Some(v)) => px(v, cx).is_some_and(|x| cx.width <= x),
        ("width", Some(v)) => px(v, cx).is_some_and(|x| (cx.width - x).abs() < 0.5),
        ("min-height", Some(v)) => px(v, cx).is_some_and(|x| cx.height >= x),
        ("max-height", Some(v)) => px(v, cx).is_some_and(|x| cx.height <= x),
        ("height", Some(v)) => px(v, cx).is_some_and(|x| (cx.height - x).abs() < 0.5),
        ("orientation", Some("landscape")) => cx.width >= cx.height,
        ("orientation", Some("portrait")) => cx.width < cx.height,
        ("prefers-color-scheme", Some("dark")) => cx.dark,
        ("prefers-color-scheme", Some("light")) => !cx.dark,
        ("prefers-reduced-motion", Some("reduce")) => cx.reduced_motion,
        ("prefers-reduced-motion", Some("no-preference")) => !cx.reduced_motion,
        ("min-resolution", Some(v)) | ("-webkit-min-device-pixel-ratio", Some(v)) => {
            let n = v.trim_end_matches("dppx").trim_end_matches('x').parse::<f32>().unwrap_or(f32::MAX);
            cx.dpr >= n
        }
        ("hover", Some("hover")) | ("pointer", Some("fine")) => true,
        ("color", None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_statements_parse_to_import_rules() {
        let sheet = Stylesheet::parse("@import \"base.css\"; @import url(theme.css) screen; .a { color: red }");
        assert_eq!(sheet.rules.len(), 3);
        match &sheet.rules[0] {
            Rule::Import(url, mq) => {
                assert_eq!(url, "base.css");
                assert!(mq.is_empty());
            }
            r => panic!("expected Import, got {r:?}"),
        }
        match &sheet.rules[1] {
            Rule::Import(url, mq) => {
                assert_eq!(url, "theme.css");
                assert_eq!(mq, "screen");
            }
            r => panic!("expected Import, got {r:?}"),
        }
        assert!(matches!(&sheet.rules[2], Rule::Style(_)));
    }

    #[test]
    fn nesting_expands_with_parent_scope() {
        let sheet = Stylesheet::parse(".card { color: red; .title { color: blue; } &:hover { color: green; } }");
        assert_eq!(sheet.rules.len(), 3, "parent + 2 nested: {sheet:?}");
        let mut matched = 0;
        for r in &sheet.rules {
            if let Rule::Style(sr) = r {
                let decl = sr.declarations.iter().find(|d| d.name == "color").map(|d| d.value.as_str());
                match decl {
                    Some("red") => matched |= 1,
                    Some("blue") => matched |= 2,
                    Some("green") => matched |= 4,
                    other => panic!("unexpected color {other:?}"),
                }
            }
        }
        assert_eq!(matched, 7, "parent + descendant + &-scoped rules all present");
    }

    #[test]
    fn nested_media_keeps_at_rule_wrapper_and_scope() {
        let sheet = Stylesheet::parse(".card { @media (min-width: 600px) { .title { color: blue; } } }");
        assert_eq!(sheet.rules.len(), 2, "empty parent + 1 media: {sheet:?}");
        match &sheet.rules[1] {
            Rule::Media(q, inner) => {
                assert!(format!("{q:?}").contains("600") || q.matches(&MediaContext { width: 800.0, ..Default::default() }));
                assert_eq!(inner.len(), 1);
                assert!(matches!(&inner[0], Rule::Style(_)));
            }
            r => panic!("expected Media, got {r:?}"),
        }
    }

    #[test]
    fn parses_rules_and_recovers() {
        let css = r#"
            /* hi */ .a, #b > c { color: red; margin : 4px !important; ; bogus }
            @import url(x.css);
            @media (min-width: 600px) and (max-width: 900px) { .m { display:none } }
            @keyframes spin { from { opacity: 0 } 50% { opacity: .5 } to { opacity: 1 } }
            .broken { color: blue
        "#;
        let sheet = Stylesheet::parse(css);
        let styles = sheet.rules.iter().filter(|r| matches!(r, Rule::Style(_))).count();
        assert_eq!(styles, 1, "unterminated trailing rule is dropped");
        match &sheet.rules[0] {
            Rule::Style(r) => {
                assert_eq!(r.selectors.len(), 2);
                assert_eq!(r.declarations.len(), 2);
                assert!(r.declarations[1].important);
                assert_eq!(r.declarations[1].value, "4px");
            }
            _ => panic!(),
        }
        let kf = sheet.rules.iter().find_map(|r| if let Rule::Keyframes(k) = r { Some(k) } else { None }).unwrap();
        assert_eq!(kf.frames.len(), 3);
        assert_eq!(kf.frames[1].0, 0.5);
    }

    #[test]
    fn keyframes_with_non_finite_offsets_do_not_panic() {
        let sheet = Stylesheet::parse("@keyframes k { nan% { opacity: 0 } inf% { opacity: 1 } to { opacity: 1 } 50% { opacity: .5 } }");
        let kf = sheet.rules.iter().find_map(|r| if let Rule::Keyframes(k) = r { Some(k) } else { None }).unwrap();
        assert_eq!(kf.frames.len(), 2);
    }

    #[test]
    fn parse_cache_returns_shared_instances() {
        // P2: identical source parses once; different sources do not alias.
        let a = parse_cached(".a { color: red }");
        let b = parse_cached(".a { color: red }");
        assert!(Arc::ptr_eq(&a, &b));
        let c = parse_cached(".a { color: blue }");
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(ua_sheet().rules.len() > 10);
        assert!(Arc::ptr_eq(&ua_sheet(), &ua_sheet()));
    }

    #[test]
    fn media_evaluation() {
        let q = MediaQueryList::parse("screen and (min-width: 600px) and (max-width: 900px), print");
        let mut cx = MediaContext::default();
        cx.width = 700.0;
        assert!(q.matches(&cx));
        cx.width = 1000.0;
        assert!(!q.matches(&cx));
        assert!(MediaQueryList::parse("(prefers-color-scheme: dark)").matches(&MediaContext { dark: true, ..cx }));
        assert!(!MediaQueryList::parse("not screen").matches(&cx));
    }

    #[test]
    fn declarations_keep_custom_property_case() {
        let d = parse_declarations("--Brand-Color: #fff; COLOR: red");
        assert_eq!(d[0].name, "--Brand-Color");
        assert_eq!(d[1].name, "color");
    }

    #[test]
    fn semicolons_inside_url_survive() {
        let d = parse_declarations("background: url(\"data:image/png;base64,AAA\"); color: red");
        assert_eq!(d.len(), 2);
    }
}

//! Minimal source-map (v3) consumer for remapping error stacks to original sources.
//!
//! Only what the bundler emits is supported: no `sections`, a single entry in `sources`, and
//! `mappings` with the standard per-segment fields (generated column, source index, original
//! line, original column, optional name index). Anything else fails [`SourceMap::parse`] and the
//! caller keeps the unmapped stack — remapping must never break error reporting.

use std::collections::HashMap;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decoded mappings for one emitted file: per generated line (0-based), segments of
/// `(generated_column, original_line_0based, original_column_0based)` sorted by column.
pub struct SourceMap {
    source: String,
    lines: Vec<Vec<(u32, u32, u32)>>,
}

fn vlq_value(c: u8) -> Option<u32> {
    B64.iter().position(|&b| b == c).map(|i| i as u32)
}

/// Decode one `mappings` segment into its signed field deltas.
fn decode_segment(seg: &str) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    let mut value: u32 = 0;
    let mut shift = 0;
    for c in seg.bytes() {
        let d = vlq_value(c)?;
        value |= (d & 31) << shift;
        shift += 5;
        if d & 32 == 0 {
            let neg = value & 1 == 1;
            let v = (value >> 1) as i64;
            out.push(if neg { -v } else { v });
            value = 0;
            shift = 0;
        }
    }
    if shift != 0 {
        return None;
    }
    Some(out)
}

impl SourceMap {
    /// Parse a source-map JSON document as emitted by the bundler.
    pub fn parse(json: &str) -> Result<SourceMap, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("invalid source map: {e}"))?;
        if v.get("version").and_then(|x| x.as_u64()) != Some(3) {
            return Err("unsupported source map version (need v3)".into());
        }
        if v.get("sections").is_some() {
            return Err("indexed source maps are not supported".into());
        }
        let source = v
            .get("sources")
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.as_str())
            .ok_or("source map has no sources")?
            .to_string();
        let mappings = v.get("mappings").and_then(|m| m.as_str()).ok_or("source map has no mappings")?;
        let mut lines = Vec::new();
        let mut src = 0i64;
        let mut src_line = 0i64;
        let mut src_col = 0i64;
        for line in mappings.split(';') {
            let mut segs = Vec::new();
            let mut gen_col = 0i64;
            if !line.is_empty() {
                for seg in line.split(',') {
                    let f = decode_segment(seg).ok_or("bad VLQ in mappings")?;
                    if f.len() < 4 {
                        return Err("segment with no original position".into());
                    }
                    gen_col += f[0];
                    src += f[1];
                    src_line += f[2];
                    src_col += f[3];
                    if src != 0 || gen_col < 0 || src_line < 0 || src_col < 0 {
                        return Err("mapping outside the single-source map".into());
                    }
                    segs.push((gen_col as u32, src_line as u32, src_col as u32));
                }
            }
            lines.push(segs);
        }
        Ok(SourceMap { source, lines })
    }

    /// Original source this map points at.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Map a 1-based generated `(line, column)` to the 1-based `(source, line, column)`.
    /// Returns `None` when the generated line has no segments (e.g. synthetic wrapper code).
    pub fn remap(&self, line: u32, col: u32) -> Option<(&str, u32, u32)> {
        let segs = self.lines.get(line.checked_sub(1)? as usize)?;
        let col0 = col.saturating_sub(1);
        let mut best: Option<(u32, u32, u32)> = None;
        for &(g, l, c) in segs {
            if g <= col0 {
                best = Some((g, l, c));
            } else {
                break;
            }
        }
        best.map(|(_, l, c)| (self.source.as_str(), l + 1, c + 1))
    }
}

/// Remap `file:line:col` frames in a QuickJS/V8-style stack (`    at name (file:line:col)`)
/// for every file present in `maps`. Frames without a map, or positions with no segment, are
/// kept verbatim.
pub fn remap_stack(maps: &HashMap<String, SourceMap>, stack: &str) -> String {
    if maps.is_empty() {
        return stack.to_string();
    }
    stack
        .split_inclusive('\n')
        .map(|line| {
            let (body, nl) = match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            };
            format!("{}{nl}", remap_frame(maps, body))
        })
        .collect()
}

fn remap_frame(maps: &HashMap<String, SourceMap>, line: &str) -> String {
    // Prefer the parenthesised location: `at name (file:line:col)`.
    if let Some(open) = line.rfind('(') {
        if let Some(close) = line[open..].find(')') {
            let inner = &line[open + 1..open + close];
            if let Some(mapped) = remap_location(maps, inner) {
                return format!("{}({mapped}){}", &line[..open], &line[open + close + 1..]);
            }
        }
    }
    // Fall back to a bare trailing `at file:line:col` frame.
    if let Some(tail) = line.rsplit(' ').next() {
        if let Some(mapped) = remap_location(maps, tail.strip_prefix("at ").unwrap_or(tail)) {
            // Only rewrite when the tail really is the whole location (not `at name`).
            if tail.contains(':') {
                return format!("{}{mapped}", &line[..line.len() - tail.len()]);
            }
        }
    }
    line.to_string()
}

fn remap_location(maps: &HashMap<String, SourceMap>, loc: &str) -> Option<String> {
    let mut parts: Vec<&str> = loc.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let col: u32 = parts.pop()?.parse().ok()?;
    let line: u32 = parts.pop()?.parse().ok()?;
    let file = parts.join(":");
    let map = maps.get(&file)?;
    let (src, l, c) = map.remap(line, col)?;
    Some(format!("{src}:{l}:{c}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_vlq_segments() {
        assert_eq!(decode_segment("AAAA"), Some(vec![0, 0, 0, 0]));
        assert_eq!(decode_segment("AACA"), Some(vec![0, 0, 1, 0]));
        assert_eq!(decode_segment("CAAC"), Some(vec![1, 0, 0, 1]));
        assert_eq!(decode_segment("AACpB"), Some(vec![0, 0, 1, -20]));
        assert!(decode_segment("g").is_none(), "dangling continuation fails");
        assert!(decode_segment("****").is_none(), "non-base64 fails");
    }

    #[test]
    fn remaps_generated_positions_to_original_sources() {
        let m = SourceMap::parse(r#"{"version":3,"sources":["src/a.ts"],"names":[],"mappings":"AAAA;AACA"}"#).unwrap();
        assert_eq!(m.source(), "src/a.ts");
        assert_eq!(m.remap(1, 1), Some(("src/a.ts", 1, 1)));
        assert_eq!(m.remap(2, 5), Some(("src/a.ts", 2, 1)));
        assert_eq!(m.remap(3, 1), None, "generated line without segments stays unmapped");
        assert_eq!(m.remap(0, 1), None, "line 0 is invalid");
    }

    #[test]
    fn rejects_unsupported_maps() {
        assert!(SourceMap::parse("not json").is_err());
        assert!(SourceMap::parse(r#"{"version":2,"sources":["a"],"mappings":"AAAA"}"#).is_err());
        assert!(SourceMap::parse(r#"{"version":3,"sources":[],"mappings":"AAAA"}"#).is_err());
        assert!(SourceMap::parse(r#"{"version":3,"sources":["a"],"mappings":"A"}"#).is_err());
    }

    #[test]
    fn parses_a_real_tsc_map() {
        // Emitted by `tsc` for `const x: number = 1;\nexport default x;`.
        let m = SourceMap::parse(r#"{"version":3,"file":"main.js","sourceRoot":"","sources":["main.ts"],"names":[],"mappings":"AAAA,MAAM,CAAC,GAAW,CAAC,CAAC;AACpB,eAAe,CAAC,CAAC"}"#).unwrap();
        assert_eq!(m.remap(1, 1), Some(("main.ts", 1, 1)));
        assert!(m.remap(2, 1).is_some());
    }

    #[test]
    fn remaps_stacks_only_for_known_files() {
        let mut maps = HashMap::new();
        // Deltas accumulate across lines: the second line's zero deltas keep pointing at
        // original line 2 rather than advancing to line 3.
        maps.insert("main.js".into(), SourceMap::parse(r#"{"version":3,"sources":["src/main.ts"],"names":[],"mappings":"AACA;AAAA"}"#).unwrap());
        let stack = "    at boom (main.js:1:30)\n    at <anonymous> (main.js:2:1)\n    at other (lib.js:1:1)";
        let out = remap_stack(&maps, stack);
        assert_eq!(out, "    at boom (src/main.ts:2:1)\n    at <anonymous> (src/main.ts:2:1)\n    at other (lib.js:1:1)");
        assert_eq!(remap_stack(&HashMap::new(), stack), stack, "empty map set is a no-op");
    }
}

//! Nesting pumps: documents that repeat a path of a real XML document until it
//! is thousands of levels deep, for checking that a reader of untrusted XML
//! never recurses once per level.
//!
//! `roxmltree` itself parses iteratively, but code that walks the tree it
//! returns can still recurse per element, and on a deep enough file that
//! overflows the stack — which aborts the process rather than panicking, so no
//! `catch_unwind` recovers it. Such a walk only recurses along the paths its
//! grammar recurses through (`<Choose>` → `<When>` → `<Choose>` in MSBuild), so
//! a hand-written deep `<i><i><i>…` document misses it. A pump finds it
//! without knowing the grammar: for every element `D` and ancestor `A` at most a
//! few levels above it, the document with `A`'s content up to `D` repeated
//! `k` times, so every path the inputs contain recurs `k` deep. Feed the
//! pumps an input that holds every construct of the grammar.
//!
//! Each pump is well-formed: element `A` is `prefix D suffix`, where `prefix`
//! holds the start tags from `A` down to `D`'s parent and `suffix` their end
//! tags, so `prefixᵏ D suffixᵏ` balances.

use std::collections::HashSet;

/// One pumped document.
pub struct Pump {
    /// The element path that recurs, `A/…/D`, for naming a failure.
    pub path: String,
    /// How many times the path recurs.
    pub repeats: usize,
    /// The pumped document.
    pub text: String,
}

/// Every pump of `source` along a path of at most `max_span` element levels
/// (`A` a proper ancestor of `D`, `depth(D) - depth(A) <= max_span`), each path
/// repeated so the document is about `levels` elements deeper, but never so
/// many times that the pumped region exceeds `byte_budget`. A path is pumped
/// once, at its first occurrence: the same tag path elsewhere in the document
/// recurses through the same code.
///
/// Panics if `source` is not well-formed XML: the pumps of a malformed input
/// test nothing.
pub fn pumps(source: &str, max_span: usize, levels: usize, byte_budget: usize) -> Vec<Pump> {
    let doc = roxmltree::Document::parse(source)
        .unwrap_or_else(|e| panic!("a pump input must be well-formed XML: {e}"));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in doc.descendants().filter(roxmltree::Node::is_element) {
        let mut path = vec![d.tag_name().name().to_owned()];
        for (span, a) in (1..=max_span).zip(d.ancestors().skip(1).filter(|n| n.is_element())) {
            path.insert(0, a.tag_name().name().to_owned());
            let key = path.join("/");
            if !seen.insert(key.clone()) {
                continue;
            }
            let (a_range, d_range) = (a.range(), d.range());
            let prefix = &source[a_range.start..d_range.start];
            let suffix = &source[d_range.end..a_range.end];
            let unit = prefix.len() + suffix.len();
            let repeats = (levels / span).min(byte_budget / unit.max(1));
            if repeats < 2 {
                continue;
            }
            let mut text = String::with_capacity(source.len() + unit * repeats);
            text.push_str(&source[..a_range.start]);
            for _ in 0..repeats {
                text.push_str(prefix);
            }
            text.push_str(&source[d_range.clone()]);
            for _ in 0..repeats {
                text.push_str(suffix);
            }
            text.push_str(&source[a_range.end..]);
            out.push(Pump {
                path: key,
                repeats,
                text,
            });
        }
    }
    out
}

/// The stack the pumps run on: Rust's default for a spawned thread, and what
/// every test thread gets. A reader that survives a pump here survives it on
/// any thread the LSP runs it on.
pub const PUMP_STACK_BYTES: usize = 2 << 20;

/// Run `f` on a fresh thread with a [`PUMP_STACK_BYTES`] stack, returning its
/// result, or re-raising its panic on the caller's thread.
///
/// A stack overflow in `f` aborts the whole test binary: that is the failure
/// the pumps exist to show, and it is loud, if blunt. Each case prints its path
/// to stderr before it runs, so the last line before the abort names it.
pub fn on_pump_stack<T: Send + 'static>(label: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    eprintln!("pump: {label}");
    let handle = std::thread::Builder::new()
        .name(format!("pump {label}"))
        .stack_size(PUMP_STACK_BYTES)
        .spawn(f)
        .expect("spawn a pump thread");
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deepest element nesting in `text` (an element at top level is 1).
    fn depth(text: &str) -> usize {
        let doc = roxmltree::Document::parse(text).expect("a pump is well-formed");
        doc.descendants()
            .map(|n| n.ancestors().filter(|a| a.is_element()).count())
            .max()
            .unwrap_or(0)
    }

    /// The depth of the deepest element at or below the first element named
    /// `name`, counted from the document's top.
    fn deepest_under(source: &str, name: &str) -> usize {
        let doc = roxmltree::Document::parse(source).unwrap();
        let a = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == name)
            .unwrap();
        a.descendants()
            .map(|n| n.ancestors().filter(|a| a.is_element()).count())
            .max()
            .unwrap()
    }

    #[test]
    fn every_pump_is_well_formed_and_as_deep_as_it_claims() {
        // Every tag is distinct, so a path's first segment names its `A`.
        let source = r#"<?xml version="1.0"?>
<!-- lead -->
<P xmlns="urn:x"><A k="v"><B>t<C/><![CDATA[<D>]]></B><B2/></A><E>x</E></P>"#;
        let base = depth(source);
        let pumps = pumps(source, 3, 100, 1 << 20);
        let paths: Vec<&str> = pumps.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "P/A", "A/B", "P/A/B", "B/C", "A/B/C", "P/A/B/C", "A/B2", "P/A/B2", "P/E"
            ]
        );
        for pump in &pumps {
            let span = pump.path.matches('/').count();
            assert_eq!(pump.repeats, 100 / span, "{}", pump.path);
            // The innermost copy of `A` sits `(repeats - 1) * span` levels
            // below the original, with all of `A`'s content inside it.
            let a = pump.path.split('/').next().unwrap();
            assert_eq!(
                depth(&pump.text),
                base.max(deepest_under(source, a) + (pump.repeats - 1) * span),
                "{}",
                pump.path
            );
        }
    }

    #[test]
    fn the_byte_budget_caps_the_repeats() {
        let source = "<a><b>0123456789</b></a>";
        let [pump] = <[Pump; 1]>::try_from(pumps(source, 1, 1_000_000, 70))
            .ok()
            .expect("one path");
        assert_eq!(pump.path, "a/b");
        // The repeated unit is `<a>` and `</a>`: 7 bytes.
        assert_eq!(pump.repeats, 10);
    }
}

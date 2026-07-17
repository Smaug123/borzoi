//! A parser for the `$( … )` property-expression sub-language, with a narrow,
//! individually-pinned evaluator.
//!
//! This is the general-parser half of `docs/completed/property-expression-plan.md`.
//! Parsing is *total over the grammar* and commits to no value: it turns a
//! `$(…)` interior into an [`Expr`] AST (a root — a property reference or a
//! `[Type]::Member(...)` static call — followed by a chain of member accesses
//! and array indexers), or returns `None` for anything that doesn't fit the
//! shape. Evaluation is where the never-over-resolve invariant lives: only the
//! members in the dispatch tables below reduce to a [`Value`]; a chain that
//! reaches any member (or receiver type) we don't model aborts with
//! [`Unsupported`], which the caller surfaces as
//! [`Issue::Unsupported`] with the expression left literal — exactly today's
//! contract. Undefined *property* references stay [`Issue::Undefined`]
//! (substituted empty), not an abort.
//!
//! The scanner ([`scan_paren`]/[`scan_quote`]) is the one place the grammar's
//! nesting is modelled: a `$(…)` may appear inside a `'…'` string literal
//! (with its own quotes and parens), and a string literal may appear inside a
//! `(…)` argument list. The two mutually-recursive scanners handle that
//! uniformly, replacing the flat quote/paren counting the string-prefix
//! matchers used.
//!
//! Which members actually evaluate is the *only* thing that grows across the
//! plan's stages: Stage 2 migrated today's functions onto the dispatch tables
//! unchanged; Stage 3 added `Split`/indexers/`Length`/`ToString`/
//! `[System.Version]::Parse` + `.Major`/`.Minor`/`.Build`/`EnsureTrailingSlash`
//! by extending the tables and the [`Value`] variants (the typed value model is
//! what keeps these safe — a nested string argument is admitted only when it
//! reduces to a [`Value::Str`]), with no parser change.

use super::escaping::Escaped;
use super::{Issue, PropertyMap};

// ============================================================================
// Nesting-aware scanners
// ============================================================================
//
// Structural characters (`( ) ' ` " $ [ ]`) are all ASCII, so scanning the
// byte slice never splits a multi-byte UTF-8 char: a non-ASCII byte is >= 0x80
// and matches none of them, so it is skipped like any other content byte.

/// MSBuild accepts `'`, `` ` ``, and `"` interchangeably as function
/// string-literal delimiters (the .NET SDK's own targets use backticks:
/// `` $([MSBuild]::IsOSPlatform(`Windows`)) ``). A string closes only at its
/// *own* delimiter; the other two are ordinary text inside it (oracle-pinned).
fn is_string_delim(b: u8) -> bool {
    matches!(b, b'\'' | b'`' | b'"')
}

/// `i` points at the byte *after* an opening `(`. Return the index of the
/// matching `)`, treating nested string literals and `$(…)` expansions as
/// opaque balanced groups (their inner parens/quotes don't count). `None` if
/// the group never closes.
fn scan_paren(b: &[u8], mut i: usize) -> Option<usize> {
    while i < b.len() {
        match b[i] {
            d if is_string_delim(d) => i = scan_quote(b, i + 1, d)? + 1,
            b'$' if b.get(i + 1) == Some(&b'(') => i = scan_paren(b, i + 2)? + 1,
            b'(' => i = scan_paren(b, i + 1)? + 1,
            b')' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// `i` points at the byte *after* an opening string delimiter `delim`. Return
/// the index of the closing `delim`. A *balanced* `$(…)` inside the string is
/// skipped as a group, so a quote that belongs to a nested expression
/// (`'$(X.Split('-')[0])'`) does not close the outer string. An *unbalanced*
/// `$(` is literal text — MSBuild treats a `$(` with no matching `)` as
/// ordinary characters (`'$('` is the two-char string `$(`, and
/// `$(P.Contains('$('))` evaluates), so we fall back to advancing past the `$`
/// rather than failing the whole scan. `None` only if the string itself never
/// closes.
fn scan_quote(b: &[u8], mut i: usize, delim: u8) -> Option<usize> {
    while i < b.len() {
        match b[i] {
            b'$' if b.get(i + 1) == Some(&b'(') => {
                i = match scan_paren(b, i + 2) {
                    Some(close) => close + 1,
                    None => i + 1,
                };
            }
            d if d == delim => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// The extent of a `$(…)` whose opening `$(` has just been consumed: `after`
/// begins at the byte after `(`, and the returned index is the matching `)`
/// within it. The nesting-aware replacement for the old flat `find_balanced_close`.
pub(super) fn find_close(after: &str) -> Option<usize> {
    scan_paren(after.as_bytes(), 0)
}

/// Split a function argument list on *top-level* commas — commas inside nested
/// strings, parens, or `$(…)` don't separate arguments. Each part is trimmed.
/// `None` if the argument text is malformed (an unbalanced string/paren).
///
/// Only a genuinely *empty* argument text is zero arguments (`Func()`); a
/// *whitespace* argument text is **one** (whitespace) argument (`Func( )`),
/// matching MSBuild — which rejects `IsRunningFromVisualStudio( )` (a zero-arg
/// intrinsic handed one arg) while accepting `IsRunningFromVisualStudio()`.
fn split_args(s: &str) -> Option<Vec<&str>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            d if is_string_delim(d) => i = scan_quote(b, i + 1, d)? + 1,
            b'$' if b.get(i + 1) == Some(&b'(') => i = scan_paren(b, i + 2)? + 1,
            b'(' => i = scan_paren(b, i + 1)? + 1,
            b',' => {
                parts.push(s[start..i].trim());
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    parts.push(s[start..].trim());
    Some(parts)
}

// ============================================================================
// AST
// ============================================================================

/// A parsed `$( … )` interior.
struct Expr<'a> {
    root: Root<'a>,
    links: Vec<Link<'a>>,
}

enum Root<'a> {
    /// `$(Foo)` / `$(Foo.Bar…)` — the chain's receiver is property `Foo`'s
    /// value as a string.
    Property(&'a str),
    /// `$([Ns.Type]::Member(args)…)` — a static property function.
    Static {
        type_name: &'a str,
        member: Member<'a>,
    },
}

enum Link<'a> {
    Member(Member<'a>),
    /// `[n]` — an indexer. On a `Split` array it selects an element (a
    /// string); on a string it selects a character.
    Index(&'a str),
}

struct Member<'a> {
    name: &'a str,
    /// `None` = paren-less property access (`.Major`); `Some` = method call
    /// (`.Split('.')`, `.Foo()`), the args as raw (untrimmed-of-quotes) slices.
    args: Option<Vec<&'a str>>,
}

// ============================================================================
// Parser
// ============================================================================

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// Whether `name` may be promoted from the environment and read back through
/// `$(Name)`: ASCII alphanumerics, `_`, or `-`, starting with a letter or `_`.
///
/// Two constraints meet here. The body must fit this module's `$(…)` reference
/// grammar, or nothing could read the property anyway (`LC_ALL=C zsh` exports
/// like `%exit_code`, or names with dots, which our grammar reads as member
/// access). The *first* character must additionally be a letter or `_` because
/// MSBuild only promotes an environment variable whose name is a valid XML
/// element name (`Utilities.GetEnvironmentProperties`:
/// `XmlUtilities.IsValidElementName(name)`), and XML names may not start with a
/// digit or `-`. Unix happily exports `1FOO=bar`; MSBuild leaves `$(1FOO)`
/// empty (probed against dotnet msbuild 10.0.301), so seeding it would commit a
/// value the real build never has.
pub(crate) fn is_referenceable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_') && bytes.all(is_name_byte)
}

/// Parse a `$(…)` interior into an [`Expr`], or `None` if it doesn't fit the
/// grammar (whereupon the caller leaves it literal + [`Issue::Unsupported`]).
/// The whole of `inner` must be consumed.
fn parse(inner: &str) -> Option<Expr<'_>> {
    let b = inner.as_bytes();
    let (root, mut pos) = parse_root(inner, b)?;
    let mut links = Vec::new();
    while pos < b.len() {
        let (link, next) = parse_link(inner, b, pos)?;
        links.push(link);
        pos = next;
    }
    Some(Expr { root, links })
}

fn parse_root<'a>(inner: &'a str, b: &[u8]) -> Option<(Root<'a>, usize)> {
    if b.first() == Some(&b'[') {
        // `[Type]::Member(args)` static call.
        let close = b.iter().position(|&c| c == b']')?;
        let type_name = &inner[1..close];
        if type_name.is_empty() {
            return None;
        }
        let after = close + 1;
        let rest = b.get(after..)?;
        let rest = rest.strip_prefix(b"::")?;
        let member_start = after + 2;
        let (member, pos) = parse_member(inner, b, member_start)?;
        // A static call is always a method (it must carry `(...)`); a bare
        // `[Type]::Field` static property is not in our grammar.
        member.args.as_ref()?;
        let _ = rest;
        Some((Root::Static { type_name, member }, pos))
    } else {
        // Property receiver: a maximal name run. Members/indexers follow.
        let len = b.iter().take_while(|&&c| is_name_byte(c)).count();
        if len == 0 {
            return None;
        }
        Some((Root::Property(&inner[..len]), len))
    }
}

/// Parse one chain link at `pos`: `.member` (optionally `(...)`) or `[index]`.
fn parse_link<'a>(inner: &'a str, b: &[u8], pos: usize) -> Option<(Link<'a>, usize)> {
    match b.get(pos)? {
        b'.' => {
            let (member, next) = parse_member(inner, b, pos + 1)?;
            Some((Link::Member(member), next))
        }
        b'[' => {
            let close = pos + 1 + b[pos + 1..].iter().position(|&c| c == b']')?;
            let index = inner[pos + 1..close].trim();
            Some((Link::Index(index), close + 1))
        }
        _ => None,
    }
}

/// Parse a member reference starting at `pos` (just past the `.` or `::`): a
/// name, then optionally whitespace and a parenthesised argument list.
fn parse_member<'a>(inner: &'a str, b: &[u8], pos: usize) -> Option<(Member<'a>, usize)> {
    let name_len = b[pos..].iter().take_while(|&&c| is_name_byte(c)).count();
    if name_len == 0 {
        return None;
    }
    let name = &inner[pos..pos + name_len];
    let mut after = pos + name_len;
    // MSBuild tolerates whitespace before the argument list: `$(P.Contains ('x'))`.
    let ws = b[after..]
        .iter()
        .take_while(|&&c| c.is_ascii_whitespace())
        .count();
    if b.get(after + ws) == Some(&b'(') {
        after += ws;
        // `scan_paren` returns the absolute index of the matching `)`.
        let close = scan_paren(b, after + 1)?;
        let args = split_args(&inner[after + 1..close])?;
        Some((
            Member {
                name,
                args: Some(args),
            },
            close + 1,
        ))
    } else {
        // Paren-less: a property access (`.Major`). No whitespace consumed.
        Some((Member { name, args: None }, after))
    }
}

// ============================================================================
// Values
// ============================================================================

/// A value flowing through a property-function chain. The receiver is typed
/// (so `.Major` is only valid on a version, an indexer only on an array…);
/// arguments stay raw text and are interpreted per-function. Typing is what
/// keeps the never-over-resolve invariant honest: a member evaluates only on
/// the receiver type MSBuild would bind it to, and a nested string argument is
/// admitted only when it reduces to a [`Value::Str`] (see [`eval_string_arg`]).
enum Value {
    Str(String),
    Bool(bool),
    /// A .NET `Int32` result (`String.Length`, `Version.Major`, …). Version
    /// components can be `-1` (an absent field), so this is signed.
    Int(i64),
    /// A single character from a string indexer (`$(Foo[0])`). Distinct from a
    /// one-character `Str`: .NET `Char` has no `.Length`, so
    /// `$(Foo[0].Length)` *errors* — keeping `Char` separate makes that chain
    /// abort rather than commit `1`.
    Char(char),
    /// A `System.Version`, held as its 2–4 `Int32` components. Rendered (and
    /// `.ToString()`-ed) by joining them with `.` (leading zeros already
    /// dropped by the numeric parse), matching .NET `Version.ToString()`.
    Version(Vec<i64>),
    /// The result of `String.Split(char-set)`. Only reachable through an
    /// indexer (`[n]` → element) or `.Length`; a *terminal* array renders as
    /// `"System.String[]"` in MSBuild, an unstable representation we decline to
    /// commit to (see [`Value::render`]).
    StrArray(Vec<String>),
}

impl Value {
    /// Render as MSBuild splices the chain result into surrounding text.
    /// `Err(Unsupported)` for a terminal [`Value::StrArray`]: rather than
    /// commit to .NET's `"System.String[]"` array `ToString()` (host/runtime
    /// dependent), leave the whole expression unsupported.
    fn render(self) -> Result<String, Unsupported> {
        Ok(match self {
            Value::Str(s) => s,
            // MSBuild renders a `bool` via `bool.ToString()`: capital-T/F.
            Value::Bool(b) => if b { "True" } else { "False" }.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Char(c) => c.to_string(),
            Value::Version(v) => render_version(&v),
            Value::StrArray(_) => return Err(Unsupported),
        })
    }
}

/// Join a `Version`'s components with `.`, matching .NET `Version.ToString()`
/// (which prints exactly the fields the value was constructed with).
fn render_version(components: &[i64]) -> String {
    components
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// Evaluation aborts to `Unsupported` for any shape outside the pinned
/// dispatch tables (unknown member, unmodelled receiver type, wrong arity, an
/// argument we can't reduce, a collation we can't reproduce). It is *not* used
/// for undefined property references — those stay `Issue::Undefined` and
/// evaluation continues on `""`.
struct Unsupported;

// ============================================================================
// Public entry
// ============================================================================

/// Evaluate a `$(…)` interior. `Some((rendered, issues))` when the whole chain
/// reduces (any undefined property references collected as `issues`); `None`
/// when the shape is unparseable or reaches an unmodelled member/type, which
/// the caller renders as [`Issue::Unsupported`] + literal passthrough.
/// A reduced `$(…)` expression: its rendered text, back in the escaped domain,
/// plus the issues raised.
///
/// MSBuild escapes the *string* a property function returns, but hands back the
/// `Char` a string indexer yields **raw** — so with `<Pct>100%</Pct>` (pinned
/// against `dotnet msbuild` 10.0.301):
///
/// | expression | result | why |
/// | --- | --- | --- |
/// | `$(Pct.ToString())20b` | `100%20b` | function result, escaped → `%` inert |
/// | `$(Pct.Substring(3))20b` | `%20b` | same |
/// | `$(Pct.Split('z')[0])20b` | `100%20b` | same |
/// | `$(Pct[3])20b` | `" b"` | **Char, raw** → `%` + `20` unescapes to a space |
///
/// The distinction lives in *how the value re-enters the domain* — escaped for
/// a function result, raw for the `Char` — so callers splice an `Escaped` and
/// need no flag to interpret it.
pub(super) struct Evaluated {
    pub value: Escaped,
    pub issues: Vec<Issue>,
}

/// Whether a **decoded** value contains a character whose downstream handling
/// the escaped-value work does not model — in which case committing an
/// expression that produced or consumed it would break certain-implies-exact.
///
/// E3 lets escape-bearing expressions commit, so a `%XX` can now decode to a
/// character that used to be shielded behind the wholesale decline. Three
/// unmodelled behaviours ride on such characters, each oracle-confirmed:
///
/// - a **backslash** (non-Windows only) meets MSBuild's `MaybeAdjustFilePath`,
///   a cwd-dependent `\\`→`/` rewrite (`docs/msbuild-unix-path-fixup-plan.md`),
///   and our path helpers join with `/` where .NET on Windows uses `\\`;
/// - a **C0/C1 control** (incl. DEL) makes .NET's culture-sensitive string
///   comparisons (`StartsWith`/`Contains`/`EndsWith`) ignore it, where we compare
///   ordinally — `'%01a'.StartsWith('a')` is *true* in MSBuild, false for us;
/// - a **NUL** makes `Path.GetFullPath` throw, failing the whole evaluation,
///   where our lexical normaliser would commit a value.
///
/// Declining on any of these is fail-safe. The backslash worlds are the
/// path-fixup branch's to model and may later lift; the control/NUL cases are
/// .NET culture behaviour and outright errors, out of this crate's scope.
///
/// Checked on the **decoded** string, because a control or NUL exists only after
/// `%XX` decoding (a backslash is escape-neutral and appears either way). This is
/// the single guard the `property_expr_diff` sweep now exercises with
/// decode-to-special-char values, so the class stays closed mechanically.
fn has_unix_backslash(decoded: &str) -> bool {
    !cfg!(windows) && decoded.contains('\\')
}

/// A `Combine` argument value whose committed result depends on the process cwd.
///
/// A (live) backslash makes MSBuild's splice-level fixup eligible; when it fires
/// (the value's first segment exists) it runs `ConvertToUnixSlashes`, which
/// `\`→`/`-converts **and collapses every separator run** — anywhere in the
/// value, not only adjacent to the backslash (`P = a//b\c` collapses to
/// `a/b/c`). `combine_path` only does the non-collapsing `\`→`/`, so when the
/// collapse changes the value the two worlds (fixup fired / not) diverge and the
/// result is cwd-dependent — decline. A backslash with no collapsible run
/// anywhere (`obj\`, `a\b`) collapses to the same thing and commits.
///
/// (Conservative for a *literal* argument, which is not splice-fixed and so is
/// actually cwd-independent even with a run — but the decoded value can't tell a
/// literal from a splice, and over-declining is fail-safe.)
fn combine_arg_is_cwd_dependent(s: &str) -> bool {
    s.contains('\\') && super::path_fixup::convert_to_unix_slashes(s) != s.replace('\\', "/")
}

/// A C0/C1 control (incl. NUL and DEL) that .NET's **culture-sensitive** string
/// comparison (`StartsWith`/`Contains`/`EndsWith`) treats specially — ignoring
/// some entirely — where we compare ordinally: `'%01a'.StartsWith('a')` is *true*
/// in MSBuild, false for us. We do not model the culture collation, so an operand
/// carrying one declines. Only the comparison methods consult this; `Length`,
/// number parsing (`AreFeaturesEnabled`) and the path normalisers handle these
/// characters exactly and keep committing.
fn has_culture_sensitive_control(decoded: &str) -> bool {
    decoded.chars().any(|c| c.is_control())
}

pub(super) fn evaluate(
    inner: &str,
    props: &PropertyMap,
    fs_probes_allowed: bool,
) -> Option<Evaluated> {
    // Stage E3 of `docs/msbuild-escaped-value-plan.md`. The evaluator runs
    // *inside* the escaped domain, mirroring MSBuild's `Expander`
    // (`Expander.cs:3982/4010/4129`): a property splice contributes its escaped
    // value (leave-escaped), a `.NET` method unescapes its receiver and each
    // argument exactly once at the call, and the method's result is re-escaped.
    // So a [`Value::Str`] always holds **escaped** text — see [`eval_string_member`]
    // — and this used to be a wholesale decline of any escape-bearing expression.
    let expr = parse(inner)?;
    let mut issues = Vec::new();
    let value = eval(&expr, props, fs_probes_allowed, &mut issues).ok()?;
    // A `Char` (from a string indexer) is the one result MSBuild hands back
    // *raw*; every other result is already escaped (a re-escaped function return,
    // a spliced property value, or an escape-neutral number/version render), so
    // it re-enters the domain verbatim.
    let is_char = matches!(value, Value::Char(_));
    let rendered = value.render().ok()?;
    // A backslash in the result reaches MSBuild's unix-only `MaybeAdjustFilePath`
    // (`docs/msbuild-unix-path-fixup-plan.md`): on a non-Windows host it rewrites
    // `\`→`/` when the value's first segment exists as a directory relative to
    // the MSBuild process's cwd — which we do not model. So `$(P.ToString())`
    // with `P=.%5cx` decodes to `.\x` here but is `./x` in MSBuild (oracle:
    // `.` exists). Decode already happened above (E3), so the backslash is now
    // *observable* to that pass; decline rather than commit a value it might
    // rewrite. On Windows the fixup is inert, so no decline. This is the seventh
    // finding reaching expression results; the fixup's own branch may later model
    // the worlds and lift this.
    // The result crosses a `$(…)` expansion boundary; a decoded backslash in it
    // meets the unix path fixup (`docs/msbuild-unix-path-fixup-plan.md`).
    if has_unix_backslash(&rendered) {
        return None;
    }
    let value = if is_char {
        let mut out = Escaped::default();
        out.push_unescaped_raw(&rendered);
        out
    } else {
        Escaped::from_xml(rendered)
    };
    Some(Evaluated { value, issues })
}

fn eval(
    expr: &Expr<'_>,
    props: &PropertyMap,
    fs: bool,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    let mut value = eval_root(&expr.root, props, fs, issues)?;
    for link in &expr.links {
        value = eval_link(value, link, props, issues)?;
    }
    Ok(value)
}

fn eval_root(
    root: &Root<'_>,
    props: &PropertyMap,
    fs: bool,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    match root {
        Root::Property(name) => {
            // An undefined property is the empty string, reported (matching
            // MSBuild's unset-property rule and the string-method pin).
            match props.get(name) {
                // A property splice contributes its **escaped** value
                // (leave-escaped, like MSBuild's property expansion): the
                // eventual `.NET` method unescapes it once at the call, and a
                // splice into a string argument composes escaped and unescapes
                // once at the argument boundary. Either way the value stays in
                // the domain here.
                Some(v) => Ok(Value::Str(v.as_escaped().to_string())),
                None => {
                    issues.push(Issue::Undefined {
                        name: (*name).to_string(),
                    });
                    Ok(Value::Str(String::new()))
                }
            }
        }
        Root::Static { type_name, member } => eval_static(type_name, member, props, fs, issues),
    }
}

fn eval_link(
    receiver: Value,
    link: &Link<'_>,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    match link {
        Link::Member(member) => eval_member(receiver, member, props, issues),
        Link::Index(index) => eval_index(receiver, index),
    }
}

/// `receiver[index]`: element on a `Split` array, character on a string.
/// The index must be a bare non-negative integer literal; an out-of-range
/// index is [`Unsupported`] (MSBuild errors the build there — the fail-safe
/// direction). A string is indexed only when ASCII, since .NET indexes UTF-16
/// code units where Rust indexes bytes/scalars; the two agree only for ASCII.
fn eval_index(receiver: Value, index: &str) -> Result<Value, Unsupported> {
    let idx: usize = index.trim().parse().map_err(|_| Unsupported)?;
    match receiver {
        Value::StrArray(parts) => parts
            .into_iter()
            .nth(idx)
            .map(Value::Str)
            .ok_or(Unsupported),
        Value::Str(s) => {
            // MSBuild indexes the *unescaped* string (`$(P[3])` with `P=a%20b`
            // indexes `a b`, which has no index 3 — an error we decline). The
            // selected `Char` is handed back raw, MSBuild's one un-escaped
            // result (the E1 hole), so it is not re-escaped.
            let s = super::escaping::unescape(&s);
            if !s.is_ascii() || has_unix_backslash(&s) {
                return Err(Unsupported);
            }
            s.as_bytes()
                .get(idx)
                .map(|&b| Value::Char(b as char))
                .ok_or(Unsupported)
        }
        _ => Err(Unsupported),
    }
}

/// `receiver.Member(args?)`: dispatch on the receiver's *type*, so a member
/// evaluates only where MSBuild would bind it. `.ToString()` is uniform across
/// scalars and handled first; everything else routes by type.
fn eval_member(
    receiver: Value,
    member: &Member<'_>,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    // `.ToString()` — a no-arg call rendering the scalar as a string. Pinned
    // for `Str`/`Int`/`Version`; `StrArray.ToString()` is the unstable
    // `"System.String[]"` (declined), and `Bool`/`Char` aren't a real consumer
    // shape (declined — a safe partiality).
    if member.name.eq_ignore_ascii_case("ToString") && matches!(member.args.as_deref(), Some([])) {
        return match receiver {
            // `.ToString()` is a `.NET` call like any other: unescape the
            // receiver, and re-escape the (identical) result. For a value with a
            // bare `%` this is not the identity — `100%` decodes to `100%` and
            // re-escapes to `100%25`, so the percent stays inert against a
            // following body, exactly as MSBuild's escaped function result does.
            Value::Str(s) => Ok(str_result(&super::escaping::unescape(&s))),
            // Numbers and versions render to escape-neutral text.
            Value::Int(n) => Ok(Value::Str(n.to_string())),
            Value::Version(v) => Ok(Value::Str(render_version(&v))),
            _ => Err(Unsupported),
        };
    }
    match receiver {
        Value::Str(recv) => eval_string_member(&recv, member, props, issues),
        Value::Version(v) => eval_version_member(&v, member),
        Value::StrArray(parts) => eval_array_member(&parts, member),
        Value::Int(_) | Value::Bool(_) | Value::Char(_) => Err(Unsupported),
    }
}

/// `System.String` instance members MSBuild binds: the `bool`-returning
/// comparisons, `TrimStart`, `Split`, and the paren-less `Length`.
/// A string result re-entering the escaped domain. Every `.NET` method that
/// returns a string escapes its result (`Expander.cs:4129`), so the receiver of
/// the next chain link decodes it back to exactly this text.
fn str_result(s: &str) -> Value {
    Value::Str(super::escaping::escape(s))
}

fn eval_string_member(
    recv: &str,
    member: &Member<'_>,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    // The receiver arrives escaped and leaves the domain here, once
    // (`Expander.cs:3982`) — `$(P.Length)` with `P=a%20b` is 3, the length of
    // the decoded `a b`. String *arguments* likewise arrive unescaped from
    // [`eval_string_arg`], and every string *result* is re-escaped on the way
    // out ([`str_result`]) so the next chain link decodes it back exactly
    // (`unescape(escape(s)) == s`).
    let recv = &super::escaping::unescape(recv);
    // The receiver is a `$()` expansion, so a decoded backslash meets the unix
    // path fixup. (Culture-sensitive controls are declined per-method below, so
    // `Length` still commits on a control-bearing receiver — it counts the
    // control, exactly as MSBuild does.)
    if has_unix_backslash(recv) {
        return Err(Unsupported);
    }
    let name = member.name;
    let Some(args) = member.args.as_deref() else {
        // Paren-less: `String.Length`. .NET `Length` counts UTF-16 code units,
        // which equals the byte length only for ASCII — decline otherwise
        // rather than commit a count that could diverge on a surrogate pair.
        if name.eq_ignore_ascii_case("Length") {
            return if recv.is_ascii() {
                Ok(Value::Int(recv.len() as i64))
            } else {
                Err(Unsupported)
            };
        }
        return Err(Unsupported);
    };

    if let Some(method) = StringBoolMethod::from_name(name) {
        let [needle] = args else {
            return Err(Unsupported);
        };
        let needle = eval_string_arg(needle, props, issues)?;
        // MSBuild's comparison is culture-sensitive; a control in either operand
        // is where ordinal and culture diverge, so decline (finding: `%01a`
        // starts-with `a` in MSBuild, not for us).
        if has_culture_sensitive_control(recv) || has_culture_sensitive_control(&needle) {
            return Err(Unsupported);
        }
        return method
            .eval(recv, &needle)
            .map(Value::Bool)
            .ok_or(Unsupported);
    }
    if name.eq_ignore_ascii_case("TrimStart") {
        let [chars] = args else {
            return Err(Unsupported);
        };
        let chars = eval_string_arg(chars, props, issues)?;
        // `String.TrimStart(params char[])` with an *empty* char set trims
        // Unicode whitespace instead (`'  abc'.TrimStart('')` → `'abc'`), whose
        // exact set (.NET `Char.IsWhiteSpace`) we don't commit to reproducing —
        // decline rather than trim nothing (which over-commits). Non-empty
        // char sets trim exactly those characters.
        if chars.is_empty() {
            return Err(Unsupported);
        }
        return Ok(str_result(recv.trim_start_matches(|c| chars.contains(c))));
    }
    if name.eq_ignore_ascii_case("Split") {
        let [charset] = args else {
            return Err(Unsupported);
        };
        let charset = eval_string_arg(charset, props, issues)?;
        // MSBuild binds `Split('…')` to `String.Split(params char[])`: the
        // argument is a *set of characters*, empty entries kept. An *empty* set
        // means "split on whitespace" (like `TrimStart('')`), whose exact set
        // we don't reproduce — decline. A non-ASCII set risks the UTF-16 vs
        // scalar mismatch, so decline that too; an ASCII set splits a string of
        // any content faithfully (ASCII separators are single code units).
        if charset.is_empty() || !charset.is_ascii() {
            return Err(Unsupported);
        }
        let set: Vec<char> = charset.chars().collect();
        // Each part re-enters the domain escaped: an indexer then decodes the
        // one it selects, and any following member decodes its receiver.
        let parts = recv
            .split(|c| set.contains(&c))
            .map(super::escaping::escape)
            .collect();
        return Ok(Value::StrArray(parts));
    }
    Err(Unsupported)
}

/// `System.Version` instance members: the paren-less `.Major`/`.Minor`/
/// `.Build` components. An absent `Build` (a 2-component version) is `-1`,
/// matching .NET. `.Revision` and any other member are declined.
fn eval_version_member(components: &[i64], member: &Member<'_>) -> Result<Value, Unsupported> {
    // These are properties, not methods — a parenthesised form is not the
    // shape MSBuild binds, so decline it.
    if member.args.is_some() {
        return Err(Unsupported);
    }
    let name = member.name;
    if name.eq_ignore_ascii_case("Major") {
        return Ok(Value::Int(components[0]));
    }
    if name.eq_ignore_ascii_case("Minor") {
        return Ok(Value::Int(components[1]));
    }
    if name.eq_ignore_ascii_case("Build") {
        return Ok(Value::Int(components.get(2).copied().unwrap_or(-1)));
    }
    Err(Unsupported)
}

/// The one array member we pin: the paren-less `.Length` (element count).
fn eval_array_member(parts: &[String], member: &Member<'_>) -> Result<Value, Unsupported> {
    if member.args.is_none() && member.name.eq_ignore_ascii_case("Length") {
        return Ok(Value::Int(parts.len() as i64));
    }
    Err(Unsupported)
}

// ============================================================================
// Static dispatch (`[Type]::Member(args)`)
// ============================================================================

fn eval_static(
    type_name: &str,
    member: &Member<'_>,
    props: &PropertyMap,
    fs: bool,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    let args = member.args.as_deref().ok_or(Unsupported)?;
    let member_name = member.name;
    match type_name {
        "MSBuild" => eval_msbuild_static(member_name, args, props, fs, issues),
        "System.IO.Path" if member_name.eq_ignore_ascii_case("Combine") => {
            if !path_args_are_bare(args) {
                return Err(Unsupported);
            }
            // `reject_escaped_backslash = true`: an escaped `%5c` survives MSBuild's
            // fixup as a literal `\`, which `combine_path` would wrongly convert.
            let parts =
                super::eval_exact_path_args(&join_args(args), props, true).ok_or(Unsupported)?;
            // A `Combine` result is `\`→`/` converted on a non-Windows host — a
            // *lone* backslash comes back slash-form regardless of cwd (oracle
            // 2026-07-13: `Combine('a\b','c')`, `Combine('/missing','obj\')`, …
            // are cwd-independent), and `combine_path` already produces it. But a
            // value bearing a collapsible separator run *is* cwd-dependent when it
            // arrives via a `$(…)` splice (see `combine_arg_is_cwd_dependent`):
            // `Combine('$(P)','c')` with `P=a\/b` or `P=a//b\c` is `a/b/c…` from a
            // cwd containing the first segment but `a//b/c…` otherwise
            // (oracle-pinned). Decline those; the SDK's lone `obj\` still commits.
            if !cfg!(windows) && parts.iter().any(|p| combine_arg_is_cwd_dependent(p)) {
                return Err(Unsupported);
            }
            super::combine_path(&parts)
                .map(|p| str_result(&p))
                .ok_or(Unsupported)
        }
        "System.IO.Path" if member_name.eq_ignore_ascii_case("IsPathRooted") => {
            let [arg] = args else { return Err(Unsupported) };
            // `eval_string_arg`'s declines (nested functions, indexer-to-`Char`,
            // delimiter-in-literal) all still apply; we only opt out of its
            // blanket *backslash* decline, because a backslash changes rootedness
            // only in the *leading* position. There — and only there — we cannot
            // tell a live `\a` (the unix fixup roots it → True) from an escaped
            // `%5ca` (stays `\a`, unrooted → False), since both decode to `\a`, so
            // decline a leading backslash; commit the rest. `is_path_rooted`
            // already matches MSBuild exactly for the committed shapes (oracle
            // 2026-07-13, both cwds: `obj\`, `a\b`, `C:\a` → False; `\a`, `/a` → True).
            let s = eval_string_arg_allowing_boundary_backslash(arg, props, issues)?;
            if !cfg!(windows) && s.starts_with('\\') {
                return Err(Unsupported);
            }
            is_path_rooted(&s).map(Value::Bool).ok_or(Unsupported)
        }
        "System.Version" if member_name.eq_ignore_ascii_case("Parse") => {
            let [arg] = args else { return Err(Unsupported) };
            let s = eval_string_arg(arg, props, issues)?;
            parse_version(&s).map(Value::Version).ok_or(Unsupported)
        }
        "System.String" if member_name.eq_ignore_ascii_case("IsNullOrEmpty") => {
            let [arg] = args else { return Err(Unsupported) };
            // `eval_string_arg` returns the *decoded* argument (MSBuild binds
            // property-function arguments in the unescaped domain — pinned:
            // `$(Esc).Length == 1` for `Esc=%20`). `IsNullOrEmpty` is
            // domain-insensitive anyway: unescaping never maps a non-empty
            // string to empty, so emptiness of the decoded value equals
            // emptiness of the escaped one. An undefined nested read
            // substitutes to "" and is reported through `issues`, matching
            // MSBuild's undefined-is-empty (→ True).
            let s = eval_string_arg(arg, props, issues)?;
            Ok(Value::Bool(s.is_empty()))
        }
        _ => Err(Unsupported),
    }
}

/// `[System.Version]::Parse(s)` → the 2–4 `Int32` components. Strict subset of
/// .NET `Version.Parse`: exactly 2–4 dot-separated fields, each all-ASCII-digit
/// and ≤ `Int32.MaxValue`. This deliberately declines the fringes .NET accepts
/// (leading/trailing whitespace, `+`-signs) — declining is the fail-safe
/// direction, and never accepting what MSBuild rejects is the invariant.
/// A single field, an empty field, five fields, a negative, or an overflowing
/// component all error the build in MSBuild, so all become `None`.
fn parse_version(s: &str) -> Option<Vec<i64>> {
    let fields: Vec<&str> = s.split('.').collect();
    if !(2..=4).contains(&fields.len()) {
        return None;
    }
    let mut components = Vec::with_capacity(fields.len());
    for field in fields {
        if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: i64 = field.parse().ok()?;
        if n > i32::MAX as i64 {
            return None;
        }
        components.push(n);
    }
    Some(components)
}

fn eval_msbuild_static(
    member_name: &str,
    args: &[&str],
    props: &PropertyMap,
    fs: bool,
    issues: &mut Vec<Issue>,
) -> Result<Value, Unsupported> {
    if member_name.eq_ignore_ascii_case("GetTargetFrameworkIdentifier") {
        let [tfm] = args else { return Err(Unsupported) };
        let tfm = eval_string_arg(tfm, props, issues)?;
        return super::infer_target_framework_identifier(&tfm)
            .map(|id| Value::Str(id.to_string()))
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("GetTargetFrameworkVersion") {
        let (tfm_arg, min_parts) = match args {
            [tfm] => (*tfm, 2),
            [tfm, parts] => (
                *tfm,
                super::parse_target_framework_version_part_count(parts).ok_or(Unsupported)?,
            ),
            _ => return Err(Unsupported),
        };
        let tfm = eval_string_arg(tfm_arg, props, issues)?;
        return super::infer_target_framework_version(&tfm, min_parts)
            .map(Value::Str)
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("GetTargetPlatformIdentifier") {
        let [tfm] = args else { return Err(Unsupported) };
        let tfm = eval_string_arg(tfm, props, issues)?;
        return super::infer_target_platform_identifier(&tfm)
            .map(Value::Str)
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("GetTargetPlatformVersion") {
        // Same `(tfm, minParts = 2)` shape as `GetTargetFrameworkVersion`.
        let (tfm_arg, min_parts) = match args {
            [tfm] => (*tfm, 2),
            [tfm, parts] => (
                *tfm,
                super::parse_target_framework_version_part_count(parts).ok_or(Unsupported)?,
            ),
            _ => return Err(Unsupported),
        };
        let tfm = eval_string_arg(tfm_arg, props, issues)?;
        return super::infer_target_platform_version(&tfm, min_parts)
            .map(Value::Str)
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("IsRunningFromVisualStudio") {
        // A `bool` intrinsic — this evaluator models the dotnet CLI toolset,
        // never the VS host. Keeping it typed (not a `"false"` string) matters:
        // it renders `False` (MSBuild `bool.ToString()`), and a chained string
        // member like `.Contains` is a type error MSBuild rejects, so a `Bool`
        // receiver makes the chain abort rather than over-commit on `"false"`.
        return if args.is_empty() {
            Ok(Value::Bool(false))
        } else {
            Err(Unsupported)
        };
    }
    if member_name.eq_ignore_ascii_case("IsOSPlatform") {
        let [arg] = args else { return Err(Unsupported) };
        let name = eval_string_arg(arg, props, issues)?;
        return is_os_platform(&name).map(Value::Bool).ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("AreFeaturesEnabled") {
        // ChangeWaves: a wave is enabled iff it is *strictly below* the
        // disable threshold, compared with .NET `Version` semantics —
        // missing components read as -1, so `999.999.0` is *above* the
        // default `999.999` sentinel (`ChangeWave.EnableAllFeatures`).
        // Pinned: `17.10`, `99.99`, `999.998.999.999`, trimmed
        // `'17.10 '` → True; `999.999.0`, `1000.0` → False;
        // `banana`/empty → project error (decline).
        //
        // The threshold comes from `MSBUILDDISABLEFEATURESFROMVERSION`,
        // an *environment* input MSBuild reads directly — never from a
        // project write (the property name is reserved). MSBuild exposes
        // the canonicalised applied threshold as the reserved
        // `MSBuildDisableFeaturesFromVersion` property, and the walker's
        // environment seeding stores exactly that visible value: the
        // `999.999` enable-all sentinel (`ChangeWaves.DisabledWave`)
        // when the variable is unset or set-but-empty, and *nothing* when
        // it is set to anything else — a set value is clamped against a
        // version-dependent wave rotation (env-var stub probes against
        // dotnet msbuild 10.0.301, 2026-07-11: unset/`banana` →
        // `999.999`, `17.4` → `17.10`, `5.0` → `17.10`, `17.11` →
        // `17.12`) that we do not model. So: evaluate only when the table
        // holds exactly the sentinel. An undefined name — an unseeded
        // walk, or a genuinely set variable — cannot rule out an ambient
        // threshold, so it declines.
        let [arg] = args else { return Err(Unsupported) };
        match props
            .get_unescaped("MSBuildDisableFeaturesFromVersion")
            .as_deref()
        {
            Some("999.999") => {}
            _ => return Err(Unsupported),
        }
        let s = eval_string_arg(arg, props, issues)?;
        // The wave string is parsed by .NET's number machinery, whose
        // whitespace set is **ASCII** — so `'17.10 '` parses but an NBSP- or
        // EM-SPACE-padded spelling is a project *error* (oracle-pinned).
        // `str::trim` is Unicode-wide and would strip those, committing `True`
        // where the real build fails; `trim_ascii` keeps the padding, so
        // `parse_version` rejects it and we decline. (`trim_ascii` omits the
        // vertical tab that .NET accepts, so a `\v`-padded wave declines
        // instead of committing — a safe partiality, not a wrong answer.)
        let wave = parse_version(s.trim_ascii()).ok_or(Unsupported)?;
        return Ok(Value::Bool(version_lt(&wave, &[999, 999])));
    }
    if let Some(op) = version_compare_op(member_name) {
        // `[MSBuild]::Version{Equals,NotEquals,GreaterThan,…}(a, b)`: compare
        // two version strings through the shared
        // [`super::compare_msbuild_versions`] (also the condition evaluator's,
        // so the two paths cannot drift). Missing components read as 0
        // (`1.0` == `1.0.0`), a leading `v`/`V` and a `-`/`+` suffix are
        // stripped, and a malformed operand is an MSB4184 error we decline —
        // distinct from `version_lt`'s `-1`-missing `.NET Version` semantics,
        // which `AreFeaturesEnabled` uses.
        let [a, b] = args else {
            return Err(Unsupported);
        };
        let a = eval_version_arg(a, props, issues)?;
        let b = eval_version_arg(b, props, issues)?;
        let ordering = super::compare_msbuild_versions(&a, &b).map_err(|()| Unsupported)?;
        return Ok(Value::Bool(op(ordering)));
    }
    if member_name.eq_ignore_ascii_case("EnsureTrailingSlash") {
        let [arg] = args else { return Err(Unsupported) };
        let s = eval_string_arg(arg, props, issues)?;
        return ensure_trailing_slash(&s)
            .map(|p| str_result(&p))
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("NormalizePath") {
        if !path_args_are_bare(args) {
            return Err(Unsupported);
        }
        let parts =
            super::eval_exact_path_args(&join_args(args), props, false).ok_or(Unsupported)?;
        return super::normalize_path(&parts)
            .map(|p| str_result(&p))
            .ok_or(Unsupported);
    }
    if member_name.eq_ignore_ascii_case("GetDirectoryNameOfFileAbove") {
        if !fs {
            return Err(Unsupported);
        }
        let [start, file] = args else {
            return Err(Unsupported);
        };
        if !path_args_are_bare(&[start, file]) {
            return Err(Unsupported);
        }
        let start = super::eval_exact_path_arg(start, props, false).ok_or(Unsupported)?;
        let file = super::eval_exact_path_arg(file, props, false).ok_or(Unsupported)?;
        return super::get_directory_name_of_file_above(&start, &file)
            .map(|p| str_result(&p))
            .ok_or(Unsupported);
    }
    Err(Unsupported)
}

/// The comparison predicate for an `[MSBuild]::Version*` intrinsic, or `None`
/// if `member_name` is not one of the family. Each maps the `Ordering` of the
/// two parsed versions to the boolean MSBuild returns.
fn version_compare_op(member_name: &str) -> Option<fn(std::cmp::Ordering) -> bool> {
    use std::cmp::Ordering;
    let op: fn(Ordering) -> bool = if member_name.eq_ignore_ascii_case("VersionEquals") {
        |o| o == Ordering::Equal
    } else if member_name.eq_ignore_ascii_case("VersionNotEquals") {
        |o| o != Ordering::Equal
    } else if member_name.eq_ignore_ascii_case("VersionGreaterThan") {
        |o| o == Ordering::Greater
    } else if member_name.eq_ignore_ascii_case("VersionGreaterThanOrEquals") {
        |o| o != Ordering::Less
    } else if member_name.eq_ignore_ascii_case("VersionLessThan") {
        |o| o == Ordering::Less
    } else if member_name.eq_ignore_ascii_case("VersionLessThanOrEquals") {
        |o| o != Ordering::Greater
    } else {
        return None;
    };
    Some(op)
}

/// A version argument to an `[MSBuild]::Version*` intrinsic. The general
/// string-arg path ([`eval_string_arg`]) covers a quoted literal and a bare
/// `$(…)` expression (the SDK's `$(TargetPlatformVersion)` shape); MSBuild also
/// admits a **bare unquoted literal** (`0.0`, `10.0`, `2` — the right-hand side
/// of the SDK's `VersionEquals($(TPV), 0.0)`), which the string-arg path
/// declines. Such a literal is expanded to itself; it leaves the escaped domain
/// once here, mirroring a quoted literal. An unquoted arg that *embeds* an
/// expansion (`$(X).0`) is outside the pinned envelope and declines.
fn eval_version_arg(
    arg: &str,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<String, Unsupported> {
    let arg = arg.trim();
    if arg.starts_with(['\'', '`', '"']) || arg.starts_with("$(") {
        return eval_string_arg(arg, props, issues);
    }
    if arg.contains("$(") {
        return Err(Unsupported);
    }
    Ok(super::escaping::unescape(arg))
}

/// Strict `<` over .NET `Version` component semantics: absent
/// components read as `-1`, so `999.998.999` < `999.999` but
/// `999.999.0` > `999.999`.
fn version_lt(a: &[i64], b: &[i64]) -> bool {
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(-1);
        let y = b.get(i).copied().unwrap_or(-1);
        match x.cmp(&y) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => continue,
        }
    }
    false
}

/// `[System.IO.Path]::IsPathRooted(s)`: pinned against dotnet msbuild
/// 10.0.300 on a unix host — `'/a/b'` → True, `'\a'` → True (MSBuild's
/// expander treats a leading backslash as rooted here too),
/// `'//server/share'` → True, `'a/b'` / `''` / `' /a'` (leading space) /
/// `'C:\a'` / `'C:/a'` / `'~/x'` → False, and an undefined-property arg
/// (empty string) → False without error.
///
/// Unix only: Windows rooting rules (drive letters, UNC) differ and are
/// unverified against the oracle, so we decline there rather than guess.
#[cfg(not(windows))]
fn is_path_rooted(s: &str) -> Option<bool> {
    Some(s.starts_with('/') || s.starts_with('\\'))
}

#[cfg(windows)]
fn is_path_rooted(_s: &str) -> Option<bool> {
    None
}

/// `[MSBuild]::IsOSPlatform(name)`: case-insensitive match of `name`
/// against the platform of the machine the build runs on — which is the
/// machine *we* run on, so the mapping is a compile-time fact. Pinned
/// against dotnet msbuild 10.0.300 on a macOS host: `'osx'`, `'OSX'`,
/// and `'macos'` → True; `'darwin'`, `'linux'`, `'windows'`, `'ios'`,
/// `'freebsd'`, `'garbage name'` → False; `''` → project error (decline).
/// Hosts outside the verified mapping decline entirely.
fn is_os_platform(name: &str) -> Option<bool> {
    if name.is_empty() {
        return None;
    }
    // MSBuild matches platform names under *invariant* uppercasing, so a
    // non-ASCII spelling can hit an ASCII platform name (`oſx` — U+017F
    // uppercases to `S` — is True on macOS, oracle-pinned 2026-07-11).
    // We compare ASCII-only and would commit a wrong False; decline.
    if !name.is_ascii() {
        return None;
    }
    let aliases: &[&str] = match std::env::consts::OS {
        "macos" => &["osx", "macos"],
        "linux" => &["linux"],
        "windows" => &["windows"],
        "freebsd" => &["freebsd"],
        _ => return None,
    };
    Some(aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias)))
}

/// `[MSBuild]::EnsureTrailingSlash(s)`: normalise `\` → `/` and append the
/// host directory separator if the value doesn't already end in one; `''`
/// stays `''`. Pinned against dotnet msbuild 10.0.300 on a unix host
/// (`'a\b'` → `a/b/`, `'a\'` → `a/`, `'/a/b/'` → `/a/b/`, `'a'` → `a/`).
///
/// Unix only: on a Windows host the separator flips and the `\`/`/`
/// normalisation is unverified against the oracle (which ran on unix), so we
/// decline there rather than guess — the fail-safe direction.
#[cfg(not(windows))]
fn ensure_trailing_slash(s: &str) -> Option<String> {
    if s.is_empty() {
        return Some(String::new());
    }
    let mut normalised = s.replace('\\', "/");
    if !normalised.ends_with('/') {
        normalised.push('/');
    }
    Some(normalised)
}

#[cfg(windows)]
fn ensure_trailing_slash(_s: &str) -> Option<String> {
    None
}

// ============================================================================
// String comparison methods
// ============================================================================

/// The three `System.String` `bool`-returning instance methods MSBuild binds.
/// Their comparison semantics differ per method — see [`Self::eval`].
#[derive(Clone, Copy)]
enum StringBoolMethod {
    Contains,
    StartsWith,
    EndsWith,
}

impl StringBoolMethod {
    fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("Contains") {
            Some(Self::Contains)
        } else if name.eq_ignore_ascii_case("StartsWith") {
            Some(Self::StartsWith)
        } else if name.eq_ignore_ascii_case("EndsWith") {
            Some(Self::EndsWith)
        } else {
            None
        }
    }

    /// `None` (→ `Unsupported`) exactly when we cannot commit faithfully:
    /// `StartsWith`/`EndsWith` are culture-sensitive in .NET, so an ordinal
    /// (Rust) match agrees only when no ignorable/combining character can
    /// participate — an empty needle, or pure-ASCII operands. `Contains` is
    /// ordinal and always commits.
    fn eval(self, value: &str, needle: &str) -> Option<bool> {
        let ordinal_matches_culture = needle.is_empty() || (value.is_ascii() && needle.is_ascii());
        match self {
            Self::Contains => Some(value.contains(needle)),
            Self::StartsWith if ordinal_matches_culture => Some(value.starts_with(needle)),
            Self::EndsWith if ordinal_matches_culture => Some(value.ends_with(needle)),
            _ => None,
        }
    }
}

// ============================================================================
// Argument evaluation
// ============================================================================

/// Evaluate a string-typed argument: a single-quoted literal whose body is
/// literal text interleaved with nested `$(…)` expansions, **each of which must
/// itself reduce to a [`Value::Str`]**. `Err(Unsupported)` if it isn't a single
/// quoted literal, if a literal chunk carries a stray `'` (MSBuild would end the
/// string there and choke on the remainder), or if any nested expansion yields
/// a non-string value.
///
/// The value-typed rule is the load-bearing correctness boundary, and it maps
/// MSBuild's actual behaviour exactly (verified against dotnet msbuild 10.0.300):
/// a nested `$(…)` in a string argument is coerced to the parameter's
/// `System.String` type, and MSBuild performs *no* implicit conversion of a
/// non-string result. So `Contains('$(V.Split('-')[0])')` (element is a string)
/// and `Parse('$(V.TrimStart('v'))')` evaluate, while `Contains('$(V.Length)')`
/// (int), `Contains('$(V.Split('-'))')` (array), and
/// `Contains('$([Version]::Parse('1.2').Major)')` (int) all *error* the build —
/// and our `eval` yields exactly `Int`/`StrArray`/… for those, so requiring
/// `Value::Str` declines precisely where MSBuild refuses. A `Char` (from a
/// string indexer) *is* accepted by MSBuild here, but we conservatively decline
/// it (a safe partiality, not a real consumer shape).
fn eval_string_arg(
    arg: &str,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<String, Unsupported> {
    eval_string_arg_inner(arg, props, issues, false)
}

/// [`eval_string_arg`] but tolerating a **boundary backslash** — a backslash in
/// a nested `$(…)` result crossing into the argument. String methods must
/// decline it (the unix path fixup / escaped-vs-live split makes the received
/// value ambiguous), but `IsPathRooted` handles that ambiguity itself: it maps
/// the whole class to a single bit and declines only the *leading*-backslash
/// case, where alone the fixup can flip rootedness. So it needs the raw value,
/// not an upstream decline.
fn eval_string_arg_allowing_boundary_backslash(
    arg: &str,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
) -> Result<String, Unsupported> {
    eval_string_arg_inner(arg, props, issues, true)
}

fn eval_string_arg_inner(
    arg: &str,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
    allow_boundary_backslash: bool,
) -> Result<String, Unsupported> {
    let arg = arg.trim();
    // Any of MSBuild's three string delimiters opens a literal; only the
    // same delimiter closes it (the other two are ordinary text inside).
    for delim in ['\'', '`', '"'] {
        if let Some(body) = arg.strip_prefix(delim).and_then(|s| s.strip_suffix(delim)) {
            return eval_str_template(body, delim, props, issues, allow_boundary_backslash);
        }
    }
    // An unquoted argument that is exactly one `$(…)` expression — the
    // SDK's `IsPathRooted($(MSBuildProjectExtensionsPath))` shape.
    // MSBuild coerces the expression's value to string; we accept only
    // a `Str` result (no numeric/bool coercion pinned).
    if let Some(after) = arg.strip_prefix("$(")
        && let Some(close) = find_close(after)
        && after[close + 1..].is_empty()
    {
        let expr = parse(&after[..close]).ok_or(Unsupported)?;
        return match eval(&expr, props, false, issues)? {
            // The evaluated value is escaped; an argument leaves the domain once
            // at binding (`Expander.cs:4010`). A backslash in this nested `$()`
            // result meets the unix path fixup before the outer function sees it
            // — declined here unless the caller opted to handle it (see
            // `eval_string_arg_allowing_boundary_backslash`).
            Value::Str(s) => {
                let decoded = super::escaping::unescape(&s);
                if !allow_boundary_backslash && has_unix_backslash(&decoded) {
                    return Err(Unsupported);
                }
                Ok(decoded)
            }
            _ => Err(Unsupported),
        };
    }
    Err(Unsupported)
}

/// Evaluate a quoted-argument body: literal text with embedded `$(…)`
/// expansions, each required to reduce to a [`Value::Str`]. A literal chunk
/// containing the delimiter that opened the string is rejected (in MSBuild
/// that quote would terminate the string literal, leaving a malformed
/// argument — the *other* two delimiter characters are ordinary text); an
/// unbalanced `$(` is ordinary literal text (`'$('`), consistent with the
/// scanner.
fn eval_str_template(
    body: &str,
    delim: char,
    props: &PropertyMap,
    issues: &mut Vec<Issue>,
    allow_boundary_backslash: bool,
) -> Result<String, Unsupported> {
    let mut out = String::new();
    let mut rest = body;
    while let Some(idx) = rest.find("$(") {
        let literal = &rest[..idx];
        if literal.contains(delim) {
            return Err(Unsupported);
        }
        out.push_str(literal);
        let after = &rest[idx + 2..];
        match find_close(after) {
            Some(close) => {
                let inner = &after[..close];
                let expr = parse(inner).ok_or(Unsupported)?;
                // No filesystem access from a string argument; the fs
                // capability rides with the outer call only where the pinned
                // function needs it.
                match eval(&expr, props, false, issues)? {
                    // A backslash in this nested `$()` result meets the unix path
                    // fixup before binding (a literal backslash in the arg *text*
                    // does not — the path normalisers handle it, so it is not
                    // guarded here). Declined unless the caller opted to handle
                    // the boundary backslash itself (`IsPathRooted`).
                    Value::Str(s)
                        if !allow_boundary_backslash
                            && has_unix_backslash(&super::escaping::unescape(&s)) =>
                    {
                        return Err(Unsupported);
                    }
                    // Already escaped at its leaf — spliced verbatim.
                    Value::Str(s) => out.push_str(&s),
                    _ => return Err(Unsupported),
                }
                rest = &after[close + 1..];
            }
            None => {
                // Unbalanced `$(`: literal text; keep scanning past it.
                out.push_str("$(");
                rest = after;
            }
        }
    }
    if rest.contains(delim) {
        return Err(Unsupported);
    }
    out.push_str(rest);
    // The argument is composed **in the escaped domain** — literal chunks are
    // escaped body text, and each `$(…)` splice contributed its escaped value —
    // then unescaped exactly once at the boundary (`Expander.cs:4010`). This is
    // why an escape that only *materialises* during composition is decoded
    // correctly: `'a%$(N)b'` with `N=20` composes `a%20b` and unescapes to
    // `a b`, and `'a$(N)b'` with `N=%2520` composes `a%2520b` and unescapes to
    // `a%20b` — each exactly once, oracle-pinned.
    Ok(super::escaping::unescape(&out))
}

/// Path-function arguments (`Combine`/`NormalizePath`/`GetDirectoryNameOfFileAbove`)
/// are held to the same bare-reference restriction as string arguments: a
/// nested property *function* in a path arg — `Combine('$([MSBuild]::…())','b')`
/// — is one MSBuild rejects (verified against dotnet msbuild 10.0.300), so
/// decline rather than commit. Real SDK path calls only ever pass literals and
/// bare `$(Name)` references. Each `arg` is checked as raw text (quoted or not),
/// since [`only_bare_property_refs`] scans for `$(…)` regardless of surrounding
/// quotes.
fn path_args_are_bare(args: &[&str]) -> bool {
    args.iter().all(|a| only_bare_property_refs(a))
}

/// Whether every *balanced* `$(…)` in `body` is a bare property reference
/// (`$(Name)`), i.e. the body would expand using only property lookups. An
/// unbalanced `$(` is literal text (`'$('`) and doesn't disqualify the body —
/// scanning stops there, as the path helpers pass it through literally.
fn only_bare_property_refs(body: &str) -> bool {
    let mut rest = body;
    while let Some(idx) = rest.find("$(") {
        let after = &rest[idx + 2..];
        match find_close(after) {
            Some(close) => {
                let inner = &after[..close];
                if inner.is_empty() || !inner.bytes().all(is_name_byte) {
                    return false;
                }
                rest = &after[close + 1..];
            }
            // Unbalanced `$(`: literal from here on.
            None => break,
        }
    }
    true
}

/// Re-join split arguments with commas so the path helpers (which re-split with
/// their own argument grammar) see their original text. The helpers' splitting
/// is quote/paren-aware, so path arguments never legitimately contain a
/// top-level comma of their own; a round-trip through the shared `,` is exact.
fn join_args(args: &[&str]) -> String {
    args.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> PropertyMap {
        let mut m = PropertyMap::new();
        for (k, v) in pairs {
            m.insert(*k, *v);
        }
        m
    }

    /// Convenience: evaluate a full `$(inner)` interior with fs probing off,
    /// taking the result to its point of use (the evaluator hands back an
    /// [`Escaped`]; a function result re-enters the domain escaped, so this
    /// unescapes it back to the text MSBuild would show).
    fn ev(inner: &str, props: &PropertyMap) -> Option<(String, Vec<Issue>)> {
        evaluate(inner, props, false).map(|e| (e.value.unescape(), e.issues))
    }

    /// Stage E3 of `docs/msbuild-escaped-value-plan.md`: the evaluator runs
    /// inside the escaped domain — unescape receiver/args in, escape result out —
    /// instead of declining any escape-bearing expression. Every row pinned
    /// against `dotnet msbuild` 10.0.301 (2026-07-12).
    #[test]
    fn a_spliced_receiver_is_unescaped_before_the_member() {
        // `$(P.Length)` with `P=a%20b` is 3 (the decoded `a b`), and exactly
        // once: `a%2520b` decodes to `a%20b`, length 5.
        assert_eq!(
            ev("P.Length", &map(&[("P", "a%20b")])),
            Some(("3".into(), vec![]))
        );
        assert_eq!(
            ev("P.Length", &map(&[("P", "a%2520b")])),
            Some(("5".into(), vec![]))
        );
    }

    #[test]
    fn a_literal_argument_is_unescaped_before_the_function() {
        assert_eq!(
            ev("[System.IO.Path]::IsPathRooted('%2fabc')", &map(&[])),
            Some(("True".into(), vec![]))
        );
    }

    #[test]
    fn an_argument_composes_escaped_and_unescapes_once() {
        // The load-bearing case: a template composes in the escaped domain and
        // unescapes once, so an escape materialising *across* a literal/splice
        // boundary decodes exactly once. `Combine('a$(N)b','c')` with `N=%2520`
        // composes `a%2520b` → `a%20b` → `a%20b/c`; `Combine('a%$(N)b','c')` with
        // `N=20` composes `a%20b` → `a b` → `a b/c`. Decoding per-chunk would get
        // the first wrong; not decoding would get the second wrong.
        assert_eq!(
            ev(
                "[System.IO.Path]::Combine('a$(N)b','c')",
                &map(&[("N", "%2520")])
            ),
            Some(("a%20b/c".into(), vec![]))
        );
        assert_eq!(
            ev(
                "[System.IO.Path]::Combine('a%$(N)b','c')",
                &map(&[("N", "20")])
            ),
            Some(("a b/c".into(), vec![]))
        );
        // A splice trailing `%` against a literal leading hex composes too.
        assert_eq!(
            ev(
                "[System.IO.Path]::Combine('a$(N)20b','c')",
                &map(&[("N", "x%")])
            ),
            Some(("ax b/c".into(), vec![]))
        );
        // A literal escape decodes: `Combine('a%2fb','c')` → `a/b/c`.
        assert_eq!(
            ev("[System.IO.Path]::Combine('a%2fb','c')", &map(&[])),
            Some(("a/b/c".into(), vec![]))
        );
    }

    #[test]
    fn a_function_result_is_re_escaped_so_its_percent_stays_inert() {
        // `$(P.TrimStart('z'))20b` with `P=100%`: P decodes to `100%`,
        // `TrimStart('z')` trims nothing, and the result re-escapes to `100%25`
        // — so the body's trailing `20b` cannot compose an escape with it, and
        // the value is `100%20b`. The E1 function-result rule, now a consequence
        // of E3. Checked at the `substitute` layer, where the body composes.
        let (out, issues) =
            super::super::substitute("$(P.TrimStart('z'))20b", &map(&[("P", "100%")]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(out.unescape(), "100%20b");
    }

    #[test]
    fn a_string_indexer_decodes_its_receiver() {
        // `$(P[3])` indexes the *unescaped* receiver: `100%`[3] is `%`, handed
        // back raw so it can still compose with the body (`$(P[3])20b` → the body
        // sees `%` + `20b`). And `a%20b`[3] indexes the decoded `a b`, which has
        // no index 3 — declined, as MSBuild errors there.
        assert_eq!(
            ev("P[3]", &map(&[("P", "100%")])),
            Some(("%".into(), vec![]))
        );
        assert_eq!(ev("P[3]", &map(&[("P", "a%20b")])), None);
    }

    /// An escaped backslash decodes to a `\\`, which reaches MSBuild's unix-only
    /// path fixup (`MaybeAdjustFilePath`) — a cwd-dependent rewrite we do not
    /// model (`docs/msbuild-unix-path-fixup-plan.md`). So on a non-Windows host
    /// an expression whose result carries a backslash declines rather than
    /// committing a value MSBuild might rewrite: `$(P.ToString())` with `P=.%5cx`
    /// is `./x` in MSBuild (the `.` directory exists), not the `.\\x` we would
    /// otherwise commit.
    #[test]
    #[cfg(not(windows))]
    fn a_backslash_result_declines_pending_the_path_fixup() {
        assert_eq!(ev("P.ToString()", &map(&[("P", ".%5cx")])), None);
        assert_eq!(
            ev("[System.IO.Path]::Combine('a%5cb','c')", &map(&[])),
            None
        );
        // A literal backslash from a plain value is the same exposure, same
        // decline — not new to E3, but now consistently handled here.
        assert_eq!(ev("P.ToString()", &map(&[("P", "a\\b")])), None);
        // No backslash: unaffected, still commits.
        assert_eq!(
            ev("P.ToString()", &map(&[("P", "a%20b")])),
            Some(("a b".into(), vec![]))
        );

        // The fixup applies to *every* `$(…)` expansion, so a backslash in a
        // nested result bound as an argument declines too — MSBuild adjusts the
        // nested `.\x` to `./x` before the outer call (oracle: `Hay.Contains(...)`
        // is True; we would commit False).
        assert_eq!(
            ev(
                "Hay.Contains($(P.ToString()))",
                &map(&[("P", ".%5cx"), ("Hay", "./x")])
            ),
            None
        );
        // …and a `Char` from an indexer that decodes to a backslash: `$(P[0])foo`
        // with `P=%5c` is `/foo` in MSBuild (the fixup), not the `\foo` we would
        // otherwise commit.
        let (_out, issues) = super::super::substitute("$(P[0])foo", &map(&[("P", "%5c")]));
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, Issue::Unsupported { .. })),
            "a backslash Char must decline: {issues:?}"
        );
    }

    /// Stage E3 admits escape-bearing expressions, so a `%XX` can decode to a
    /// character whose downstream handling this crate does not model. Each
    /// declines rather than committing a wrong value; the `property_expr_diff`
    /// sweep exercises these generatively so the class stays closed. All
    /// oracle-pinned (2026-07-12).
    #[test]
    #[cfg(not(windows))]
    fn decoded_special_characters_decline() {
        // A C0 control in a culture-sensitive comparison: `'%01a'.StartsWith('a')`
        // is true in MSBuild (the collation ignores U+0001), false for us.
        assert_eq!(ev("P.StartsWith('a')", &map(&[("P", "%01a")])), None);
        // …but a plain receiver still commits, and `Length` counts the control.
        assert_eq!(
            ev("P.StartsWith('a')", &map(&[("P", "abc")])),
            Some(("True".into(), vec![]))
        );
        assert_eq!(
            ev("P.Length", &map(&[("P", "%01a")])),
            Some(("2".into(), vec![]))
        );

        // A decoded backslash in `IsPathRooted` is the escaped-vs-live split
        // (fixup runs on escaped text): `%5cabc` is not rooted (False) in MSBuild,
        // where our helper treats a leading `\` as rooted — so decline.
        assert_eq!(
            ev("[System.IO.Path]::IsPathRooted('%5cabc')", &map(&[])),
            None
        );

        // A decoded NUL makes `Path.GetFullPath` throw; `NormalizePath` declines
        // it — where a decoded backslash it *can* normalise still commits.
        assert_eq!(
            ev("[MSBuild]::NormalizePath('/a%00b','c')", &map(&[])),
            None
        );
        assert_eq!(
            ev("[MSBuild]::NormalizePath('/a%5cb','c%5cf')", &map(&[])),
            Some(("/a/b/c/f".into(), vec![]))
        );
    }

    // --- scanners ---------------------------------------------------------

    #[test]
    fn find_close_handles_nested_quotes_and_parens() {
        // The F# SDK composite: the matching `)` is the final byte.
        let after = "[System.Version]::Parse('$(V.Split('-')[0])').Major)";
        assert_eq!(find_close(after), Some(after.len() - 1));
        // A quoted close-paren doesn't close the expression early.
        let after = "P.Contains(')'))";
        assert_eq!(find_close(after), Some(after.len() - 1));
    }

    #[test]
    fn split_args_respects_nesting() {
        assert_eq!(split_args("'a','b'"), Some(vec!["'a'", "'b'"]));
        // A comma inside a nested string / expression is not a separator.
        assert_eq!(split_args("'a,b'"), Some(vec!["'a,b'"]));
        assert_eq!(
            split_args("'$(X.Split(',')[0])'"),
            Some(vec!["'$(X.Split(',')[0])'"])
        );
        // Empty arg text is zero args (`Func()`); whitespace is one arg
        // (`Func( )`), trimmed to "".
        assert_eq!(split_args(""), Some(vec![]));
        assert_eq!(split_args("  "), Some(vec![""]));
    }

    // --- AreFeaturesEnabled: the ChangeWaves threshold --------------------
    //
    // `MSBuildDisableFeaturesFromVersion` is *reserved* (the oracle
    // rejects injecting it: "property is reserved, and cannot be
    // modified"), so the differential harness can only exercise the
    // declining side. The value pins below cite per-value probes against
    // `dotnet msbuild` 10.0.300 via the condition-oracle `eval` op,
    // 2026-07-09: 17.10 → True, 99.99 → True, '17.10 ' → True,
    // 999.998.999.999 → True, 999.999.0 → False, 1000.0 → False,
    // banana/'' → project error. The *visible* reserved value was probed
    // with env-var stub projects against `dotnet msbuild` 10.0.301,
    // 2026-07-11: unset/`banana` env → `999.999`
    // (`ChangeWaves.DisabledWave`), and every set value is canonicalised
    // before it becomes visible (`5.0` clamps up to the lowest rotation
    // wave `17.10`; `17.11` rounds up to `17.12`), with
    // `AreFeaturesEnabled(w)` = w strictly below the visible value
    // (`17.8` → True, `17.10` → False under a `17.10` threshold).

    #[test]
    fn are_features_enabled_pins_the_sentinel_threshold() {
        let p = map(&[("MSBuildDisableFeaturesFromVersion", "999.999")]);
        for (wave, expected) in [
            ("'17.10'", "True"),
            ("'99.99'", "True"),
            ("'17.10 '", "True"),
            ("'999.998.999.999'", "True"),
            ("'999.999.0'", "False"),
            ("'1000.0'", "False"),
        ] {
            assert_eq!(
                ev(&format!("[MSBuild]::AreFeaturesEnabled({wave})"), &p),
                Some((expected.into(), vec![])),
                "wave {wave}"
            );
        }
    }

    #[test]
    fn are_features_enabled_declines_junk_waves() {
        let p = map(&[("MSBuildDisableFeaturesFromVersion", "999.999")]);
        for wave in ["'banana'", "''", "'17'"] {
            assert_eq!(
                ev(&format!("[MSBuild]::AreFeaturesEnabled({wave})"), &p),
                None,
                "wave {wave}"
            );
        }
    }

    #[test]
    fn are_features_enabled_wave_padding_is_ascii_only() {
        // The wave string reaches .NET's number parsing, whose whitespace set
        // is ASCII-only — so MSBuild *accepts* space/tab/CR/LF padding but
        // *rejects* a Unicode-padded spelling (oracle, 10.0.301, 2026-07-11:
        // `'17.10 '`, `' 17.10'`, `'\t17.10\r\n'` → True; NBSP- and
        // EM-SPACE-padded → project error). Rust's `str::trim` is
        // Unicode-wide, so trimming with it would strip the NBSP and commit
        // `True` where the real build fails. (MSBuild's *expression source*
        // whitespace is separately Unicode-tolerant — `$(P.Contains(<NBSP>'a'))`
        // evaluates — so only this value-level trim is ASCII-bound.)
        let p = map(&[("MSBuildDisableFeaturesFromVersion", "999.999")]);
        for wave in ["'17.10 '", "' 17.10'", "'\t17.10\r\n'"] {
            assert_eq!(
                ev(&format!("[MSBuild]::AreFeaturesEnabled({wave})"), &p),
                Some(("True".into(), vec![])),
                "ASCII-padded wave {wave:?} must commit"
            );
        }
        for wave in ["'17.10\u{a0}'", "'\u{2003}17.10'", "'\u{a0}17.10\u{a0}'"] {
            assert_eq!(
                ev(&format!("[MSBuild]::AreFeaturesEnabled({wave})"), &p),
                None,
                "Unicode-padded wave {wave:?} must decline (MSBuild errors)"
            );
        }
    }

    // --- Version{Equals,…} comparison family ------------------------------
    //
    // MSBuild's `[MSBuild]::Version*` intrinsics compare two version strings.
    // Semantics pinned against `dotnet msbuild` 10.0.301 (2026-07-13, stub
    // `<Message>` projects): 1–4 dot-separated numeric components; **missing
    // components compare as 0** (so `1.0` == `1.0.0` and `5` == `5.0.0.0` —
    // distinct from the `-1`-missing `.NET Version` semantics `version_lt` /
    // `AreFeaturesEnabled` use); a single leading `v`/`V` is stripped;
    // surrounding whitespace is trimmed; and an empty, non-numeric, or
    // >4-component string is a project **error** (MSB4184 "Version string was
    // not in a correct format"), which we decline.

    #[test]
    fn version_comparisons_commit_the_oracle_values() {
        let p = map(&[]);
        for (expr, expected) in [
            ("[MSBuild]::VersionEquals('1.0','1.0')", "True"),
            // missing components read as 0, not -1
            ("[MSBuild]::VersionEquals('1.0','1.0.0')", "True"),
            ("[MSBuild]::VersionEquals('0.0','0')", "True"),
            ("[MSBuild]::VersionEquals('5','5.0.0.0')", "True"),
            // leading v/V stripped; surrounding whitespace trimmed
            ("[MSBuild]::VersionEquals('v1.2','1.2')", "True"),
            ("[MSBuild]::VersionEquals(' 1.2 ','1.2')", "True"),
            ("[MSBuild]::VersionNotEquals('1.0','2.0')", "True"),
            ("[MSBuild]::VersionNotEquals('1.0','1.0.0')", "False"),
            (
                "[MSBuild]::VersionGreaterThanOrEquals('10.0','10.0')",
                "True",
            ),
            (
                "[MSBuild]::VersionGreaterThanOrEquals('9.0','10.0')",
                "False",
            ),
            (
                "[MSBuild]::VersionGreaterThanOrEquals('10.0.1','10.0')",
                "True",
            ),
            ("[MSBuild]::VersionGreaterThan('1.2.3.4','1.2.3')", "True"),
            ("[MSBuild]::VersionGreaterThan('10.0','10.0')", "False"),
            ("[MSBuild]::VersionLessThan('1.0','1.0.0')", "False"),
            ("[MSBuild]::VersionLessThanOrEquals('1.0','1.0.0')", "True"),
            ("[MSBuild]::VersionLessThanOrEquals('9.0','10.0')", "True"),
            // A prerelease/metadata suffix is dropped at the first `-`/`+`, and
            // the comparison is numeric-only (not SemVer): `1.0.0` neither
            // exceeds nor trails `1.0.0-preview` (oracle 10.0.301, 2026-07-13).
            (
                "[MSBuild]::VersionEquals('10.0.100-preview.1','10.0.100')",
                "True",
            ),
            ("[MSBuild]::VersionEquals('3+meta','3')", "True"),
            (
                "[MSBuild]::VersionGreaterThan('1.2.3-beta','1.2.2')",
                "True",
            ),
            (
                "[MSBuild]::VersionGreaterThan('1.0.0','1.0.0-preview')",
                "False",
            ),
            // Unicode whitespace is trimmed here (unlike the ASCII-only
            // `AreFeaturesEnabled` path): an NBSP-padded operand parses.
            ("[MSBuild]::VersionEquals('\u{a0}1.2','1.2')", "True"),
        ] {
            assert_eq!(ev(expr, &p), Some((expected.into(), vec![])), "expr {expr}");
        }
    }

    #[test]
    fn version_comparisons_decline_malformed_strings() {
        // Empty, non-numeric, and >4-component strings are MSB4184 errors in
        // the real build, so we decline rather than commit a value.
        let p = map(&[]);
        for expr in [
            "[MSBuild]::VersionEquals('','1.0')",
            "[MSBuild]::VersionEquals('1.x','1.0')",
            "[MSBuild]::VersionEquals('1.2.3.4.5','1.2.3.4.5')",
            "[MSBuild]::VersionGreaterThanOrEquals('10.0','bad')",
        ] {
            assert_eq!(ev(expr, &p), None, "expr {expr} must decline");
        }
    }

    // --- GetTargetPlatformIdentifier / GetTargetPlatformVersion -----------
    //
    // Pinned against `dotnet msbuild` 10.0.301 (2026-07-13). The platform-free
    // case of a recognised base TFM commits (`""` / `0.0`); a platform-bearing
    // TFM (`net8.0-windows`) and an unrecognised base decline — the
    // platform-moniker/version parse envelope is not yet pinned, and the plain
    // SDK chain (net10.0 / net8.0 / netstandard2.x) never targets a platform.
    // `GetTargetPlatformVersion`'s min-part count floors at 1 (`, 0` -> `0`) and
    // trims trailing zeros, exactly like its `GetTargetFrameworkVersion` sibling.
    #[test]
    fn get_target_platform_functions_commit_the_platform_free_case() {
        let p = map(&[]);
        for (expr, expected) in [
            ("[MSBuild]::GetTargetPlatformIdentifier('net10.0')", ""),
            ("[MSBuild]::GetTargetPlatformIdentifier('net8.0')", ""),
            (
                "[MSBuild]::GetTargetPlatformIdentifier('netstandard2.0')",
                "",
            ),
            ("[MSBuild]::GetTargetPlatformIdentifier('net472')", ""),
            ("[MSBuild]::GetTargetPlatformVersion('net10.0')", "0.0"),
            ("[MSBuild]::GetTargetPlatformVersion('net10.0', 2)", "0.0"),
            ("[MSBuild]::GetTargetPlatformVersion('net10.0', 1)", "0"),
            ("[MSBuild]::GetTargetPlatformVersion('net10.0', 0)", "0"),
            (
                "[MSBuild]::GetTargetPlatformVersion('net10.0', 4)",
                "0.0.0.0",
            ),
            (
                "[MSBuild]::GetTargetPlatformVersion('netstandard2.0', 2)",
                "0.0",
            ),
        ] {
            assert_eq!(ev(expr, &p), Some((expected.into(), vec![])), "expr {expr}");
        }
    }

    #[test]
    fn get_target_platform_functions_decline_platform_bearing_and_unrecognised() {
        let p = map(&[]);
        for expr in [
            // Platform-bearing: the moniker/version parse is not yet pinned.
            "[MSBuild]::GetTargetPlatformIdentifier('net8.0-windows')",
            "[MSBuild]::GetTargetPlatformVersion('net8.0-windows10.0.19041.0')",
            // Unrecognised base (MSBuild returns ``; we conservatively decline).
            "[MSBuild]::GetTargetPlatformIdentifier('garbage')",
            "[MSBuild]::GetTargetPlatformVersion('garbage')",
            // >4-part count is an error in the real build.
            "[MSBuild]::GetTargetPlatformVersion('net10.0', 5)",
        ] {
            assert_eq!(ev(expr, &p), None, "expr {expr} must decline");
        }
    }

    #[test]
    fn are_features_enabled_declines_without_the_visible_sentinel() {
        // An undefined `MSBuildDisableFeaturesFromVersion` cannot rule
        // out an ambient MSBUILDDISABLEFEATURESFROMVERSION. A defined
        // non-sentinel value means change waves are in play under a
        // rotation clamp we do not model, so the seeding leaves the
        // property undefined and the read declines (`17.0` would clamp
        // to the rotation's lowest wave; probed `17.4` → `17.10`). The
        // empty string is not a state MSBuild ever exposes (the visible
        // default is the `999.999` sentinel), and the sentinel must match
        // byte-exactly — a padded `999.999 ` is not it; both decline.
        for threshold in [
            None,
            Some("17.0"),
            Some("17.10"),
            Some(""),
            Some("999.999 "),
        ] {
            let p = match threshold {
                None => PropertyMap::new(),
                Some(t) => map(&[("MSBuildDisableFeaturesFromVersion", t)]),
            };
            assert_eq!(
                ev("[MSBuild]::AreFeaturesEnabled('17.10')", &p),
                None,
                "threshold {threshold:?}"
            );
        }
    }

    // --- %XX escapes ------------------------------------------------------
    //
    // MSBuild unescapes `%` + two hex digits before a property function
    // sees the text — for literal arguments, for property values spliced
    // into arguments or used as receivers, and even for an escape pair
    // *composed* by expansion (`'a%$(N)b'` with N=20 → the function sees
    // `a b`). All pinned against `dotnet msbuild` 10.0.301 via the
    // condition-oracle `expand` op, 2026-07-11: `IsPathRooted('%2fabc')`
    // → True, `IsOSPlatform('%6fSX')` → True on macOS,
    // `EnsureTrailingSlash('a%20b')` → `a b/`,
    // `IsPathRooted($(P))` with `P=%2fabc` → True, `$(P.Length)` with
    // `P=a%20b` → 3, `EnsureTrailingSlash('a%$(N)b')` with `N=20` →
    // `a b/`. We don't model unescaping, so an escape-bearing string
    // entering the evaluator declines rather than commits a value
    // computed from the raw text.

    #[test]
    fn escape_bearing_expression_text_commits() {
        // An escape in the *expression text* — a literal argument — decodes at
        // the call and the expression commits, instead of the old wholesale
        // decline (stage E3). Oracle-pinned.
        let p = PropertyMap::new();
        assert_eq!(
            ev("[System.IO.Path]::IsPathRooted('%2fabc')", &p),
            Some(("True".into(), vec![]))
        );
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('a%20b')", &p),
            Some(("a b/".into(), vec![]))
        );
        // `IsOSPlatform('%6fSX')` decodes to `oSX`, which MSBuild uppercases to
        // OSX: it commits a bool (host-dependent value — covered against the
        // oracle in `property_expr_diff`), and crucially no longer declines.
        assert!(
            ev("[MSBuild]::IsOSPlatform('%6fSX')", &p).is_some(),
            "an escaped platform name is modelled, not declined"
        );
    }

    #[test]
    fn escape_bearing_property_values_commit() {
        // An escape reaching the evaluator through a *property value* commits
        // too — as an unquoted argument, a method receiver, a quoted-argument
        // splice, and an escape that only materialises after composition. All
        // oracle-pinned (stage E3).
        let p = map(&[("Esc", "a%20b"), ("Dir", "%2fabc"), ("N", "20")]);
        assert_eq!(
            ev("[System.IO.Path]::IsPathRooted($(Dir))", &p),
            Some(("True".into(), vec![]))
        );
        assert_eq!(ev("Esc.Length", &p), Some(("3".into(), vec![])));
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('$(Esc)')", &p),
            Some(("a b/".into(), vec![]))
        );
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('a%$(N)b')", &p),
            Some(("a b/".into(), vec![]))
        );
    }

    #[test]
    fn literal_percent_without_hex_pair_commits() {
        // A `%` not followed by two hex digits is literal in MSBuild
        // (oracle: `$(P.Length)` with `P=100%` → 4, `'a%zb'` → `a%zb/`).
        assert_eq!(
            ev("P.Length", &map(&[("P", "100%")])),
            Some(("4".into(), vec![]))
        );
        assert_eq!(
            ev(
                "[MSBuild]::EnsureTrailingSlash('a%zb')",
                &PropertyMap::new()
            ),
            Some(("a%zb/".into(), vec![]))
        );
    }

    // --- backtick / double-quote string literals ---------------------------
    //
    // MSBuild accepts `'`, `` ` ``, and `"` interchangeably as function
    // string-literal delimiters, and the real SDK uses backticks
    // (`$([MSBuild]::IsOSPlatform(`Windows`))` in
    // `Microsoft.NET.RuntimeIdentifierInference.targets`). Oracle-pinned
    // (`expand` op, dotnet msbuild 10.0.301, 2026-07-11):
    // ``IsOSPlatform(`osx`)`` → True, `IsOSPlatform("osx")` → True,
    // ``$(P.Contains(`x`))`` with `P=axb` → True,
    // ``EnsureTrailingSlash(`$(Dir)b`)`` with `Dir=a/` → `a/b/`, and a
    // single quote inside a backtick literal is ordinary text
    // (``$(P.Contains(`'`))`` with `P=a'b` → True).

    #[test]
    fn backtick_and_double_quote_args_commit() {
        let p = map(&[("P", "axb"), ("Dir", "a/"), ("Q", "a'b")]);
        for (inner, expected) in [
            ("P.Contains(`x`)", "True"),
            ("P.Contains(\"x\")", "True"),
            ("[MSBuild]::EnsureTrailingSlash(`$(Dir)b`)", "a/b/"),
            ("Q.Contains(`'`)", "True"),
        ] {
            assert_eq!(ev(inner, &p), Some((expected.into(), vec![])), "{inner}");
        }
        // The SDK's exact spelling (host-dependent result, shape must
        // evaluate): on this host `Windows` only matches a Windows host.
        let expected = if cfg!(windows) { "True" } else { "False" };
        assert_eq!(
            ev("[MSBuild]::IsOSPlatform(`Windows`)", &PropertyMap::new()),
            Some((expected.into(), vec![]))
        );
    }

    #[test]
    fn quote_delimiters_do_not_close_each_other() {
        // A backtick inside a single-quoted literal is ordinary text and
        // vice versa; a delimiter closes only its own string.
        let p = map(&[("P", "a`b")]);
        assert_eq!(ev("P.Contains('`')", &p), Some(("True".into(), vec![])));
        // Scanner extent: a close-paren inside a backtick literal does
        // not close the call.
        let p = map(&[("P", "a)b")]);
        assert_eq!(ev("P.Contains(`)`)", &p), Some(("True".into(), vec![])));
    }

    #[test]
    fn escape_bearing_path_args_decode_when_spliced() {
        // The path-function argument layer (`eval_exact_path_arg` in
        // `properties/mod.rs`) splices property values via plain
        // `substitute`, not the guarded expression evaluator — so it
        // needs its own escape guard. With `P=a%2fb`, MSBuild unescapes
        // to `a/b` before combining (oracle: Combine(`$(P)`,`b`) →
        // `a/b/b`); committing the raw text would silently redirect an
        // import path.
        // A *spliced* property value now decodes at the argument leaf, which
        // is exactly what MSBuild does, so these commit the right path instead
        // of declining.
        let p = map(&[("P", "a%2fb")]);
        for inner in [
            "[System.IO.Path]::Combine('$(P)','b')",
            "[System.IO.Path]::Combine(`$(P)`,`b`)",
        ] {
            assert_eq!(ev(inner, &p), Some(("a/b/b".into(), vec![])), "{inner}");
        }
        // An escape written *in the expression text* decodes at the call now
        // too (stage E3): `Combine('a%2fb','c')` is the decoded `a/b` combined
        // with `c`. (`NormalizePath` of a *relative* path still declines — its
        // base is the MSBuild process cwd, which we do not model — so it is not
        // an escaping decline.)
        assert_eq!(
            ev("[System.IO.Path]::Combine('a%2fb','c')", &p),
            Some(("a/b/c".into(), vec![]))
        );
        assert_eq!(ev("[MSBuild]::NormalizePath('a%2fb','c')", &p), None);
        // A `%` without a trailing hex pair stays literal and commits.
        assert_eq!(
            ev("[System.IO.Path]::Combine('a%zb','c')", &p),
            Some(("a%zb/c".into(), vec![]))
        );
    }

    // --- IsOSPlatform: non-ASCII spellings ---------------------------------

    #[test]
    fn is_os_platform_declines_non_ascii_names() {
        // MSBuild compares platform names under *invariant* uppercasing,
        // so `oſx` (U+017F LATIN SMALL LETTER LONG S, which uppercases
        // to `S`) matches on macOS (oracle-pinned: → True, 2026-07-11).
        // We compare ASCII-only and would commit a wrong False, so any
        // non-ASCII name declines instead.
        assert_eq!(
            ev("[MSBuild]::IsOSPlatform('o\u{17f}x')", &PropertyMap::new()),
            None
        );
    }

    // --- migration parity: string methods ---------------------------------

    #[test]
    fn contains_commits_ordinally() {
        let p = map(&[("P", "1.2.3")]);
        assert_eq!(ev("P.Contains('.')", &p), Some(("True".into(), vec![])));
        assert_eq!(ev("P.Contains('{')", &p), Some(("False".into(), vec![])));
    }

    #[test]
    fn contains_is_case_sensitive() {
        assert_eq!(
            ev("P.Contains('A')", &map(&[("P", "abc")])),
            Some(("False".into(), vec![]))
        );
    }

    #[test]
    fn undefined_receiver_reports_and_operates_on_empty() {
        assert_eq!(
            ev("Missing.Contains('x')", &PropertyMap::new()),
            Some((
                "False".into(),
                vec![Issue::Undefined {
                    name: "Missing".into()
                }]
            ))
        );
    }

    #[test]
    fn starts_ends_with_bail_on_non_ascii() {
        // Non-ASCII operand → Unsupported (None), matching the culture bail.
        assert_eq!(
            ev("P.StartsWith('\u{200b}abc')", &map(&[("P", "abc")])),
            None
        );
        assert_eq!(ev("P.EndsWith('c')", &map(&[("P", "\u{200b}abc")])), None);
        // Empty needle always commits, even for a non-ASCII receiver.
        assert_eq!(
            ev("P.StartsWith('')", &map(&[("P", "café")])),
            Some(("True".into(), vec![]))
        );
    }

    #[test]
    fn contains_is_ordinal_for_non_ascii() {
        let p = map(&[("P", "café")]);
        assert_eq!(ev("P.Contains('é')", &p), Some(("True".into(), vec![])));
        assert_eq!(ev("P.Contains('É')", &p), Some(("False".into(), vec![])));
    }

    #[test]
    fn method_name_case_insensitive_and_whitespace_tolerant() {
        let p = map(&[("P", "1.2.3")]);
        for inner in ["P.contains('.')", "P.CONTAINS('.')", "P.Contains ('.')"] {
            assert_eq!(ev(inner, &p), Some(("True".into(), vec![])), "{inner}");
        }
    }

    #[test]
    fn argument_substitution_and_nested_method_in_arg() {
        // `$()` inside the needle is expanded first.
        assert_eq!(
            ev("P.Contains('$(N)')", &map(&[("P", "1.2.3"), ("N", ".2.")])),
            Some(("True".into(), vec![]))
        );
        // A method-name substring inside the literal is not mistaken for a call.
        assert_eq!(
            ev("P.StartsWith('.Contains(')", &map(&[("P", "x")])),
            Some(("False".into(), vec![]))
        );
    }

    // --- migration parity: chaining is now structural ---------------------

    #[test]
    fn chain_ending_in_unsupported_member_is_unsupported() {
        // `.ToString()` / `.Replace()` are not pinned → the whole chain aborts,
        // exactly as the old flat matchers rejected any chained call.
        assert_eq!(
            ev("P.Contains('a').ToString()", &map(&[("P", "abc")])),
            None
        );
        assert_eq!(
            ev(
                "TargetFrameworkVersion.TrimStart('vV').Replace('.', '_')",
                &map(&[("TargetFrameworkVersion", "v8.0")])
            ),
            None
        );
    }

    #[test]
    fn chain_of_supported_methods_evaluates() {
        // Two pinned methods chained now reduce (safe: both pinned, MSBuild-exact).
        assert_eq!(
            ev("P.TrimStart('v').Contains('.')", &map(&[("P", "v1.2")])),
            Some(("True".into(), vec![]))
        );
    }

    // --- migration parity: TFM inference & TrimStart ----------------------

    #[test]
    fn tfm_inference_and_trim_start() {
        assert_eq!(
            ev(
                "[MSBuild]::GetTargetFrameworkVersion('net8.0')",
                &PropertyMap::new()
            ),
            Some(("8.0".into(), vec![]))
        );
        assert_eq!(
            ev(
                "[MSBuild]::GetTargetFrameworkIdentifier('net8.0')",
                &PropertyMap::new()
            ),
            Some((".NETCoreApp".into(), vec![]))
        );
        assert_eq!(
            ev(
                "TargetFrameworkVersion.TrimStart('vV')",
                &map(&[("TargetFrameworkVersion", "v10.0")])
            ),
            Some(("10.0".into(), vec![]))
        );
        // A `bool` intrinsic: renders capital `False` (MSBuild `bool.ToString()`,
        // pinned against dotnet msbuild 10.0.300).
        assert_eq!(
            ev(
                "[MSBuild]::IsRunningFromVisualStudio()",
                &PropertyMap::new()
            ),
            Some(("False".into(), vec![]))
        );
    }

    #[test]
    fn literal_dollar_paren_in_argument_is_not_a_reference() {
        // A `$(` inside a quoted argument that doesn't balance is literal text,
        // not a (failed) property reference — MSBuild evaluates the call
        // (pinned: `$(P.Contains('$('))` is True when P holds `$(`).
        assert_eq!(
            ev("P.Contains('$(')", &map(&[("P", "x$(y")])),
            Some(("True".into(), vec![]))
        );
        assert_eq!(
            ev("P.Contains('$(')", &map(&[("P", "abc")])),
            Some(("False".into(), vec![]))
        );
    }

    #[test]
    fn string_arg_admits_bare_refs_declines_non_string_nested() {
        // A needle may be literal text plus bare `$(Name)` references — those
        // MSBuild reliably accepts (they yield strings), so we commit.
        assert_eq!(
            ev("P.Contains('$(N)')", &map(&[("P", "abc"), ("N", "b")])),
            Some(("True".into(), vec![]))
        );
        assert_eq!(
            ev("P.Contains('a$(N)b')", &map(&[("P", "zaYbz"), ("N", "Y")])),
            Some(("True".into(), vec![]))
        );
        // A nested member that yields a *non-string* (a bool here, an int/array
        // below) is declined — MSBuild errors on it (no implicit conversion to
        // the string parameter), so committing would over-resolve. Covers the
        // inner-quote form, the no-quote static-function form, and a paren-less
        // member.
        assert_eq!(
            ev(
                "P.Contains('$(Q.Contains('a'))')",
                &map(&[("P", "xTruey"), ("Q", "a")])
            ),
            None
        );
        assert_eq!(
            ev(
                "P.TrimStart('x').Contains('$([MSBuild]::IsRunningFromVisualStudio())')",
                &map(&[("P", "xabc")])
            ),
            None
        );
        assert_eq!(
            ev(
                "P.Contains('$(Q.Length)')",
                &map(&[("P", "a3b"), ("Q", "abc")])
            ),
            None
        );
    }

    #[test]
    fn empty_trim_start_argument_is_unsupported() {
        // `TrimStart('')` trims Unicode whitespace in .NET, not nothing — we
        // decline rather than commit to a no-op. `'  abc'.TrimStart('') ` is
        // `'abc'` in MSBuild, and the chained form must not commit to `  abc`.
        assert_eq!(ev("P.TrimStart('')", &map(&[("P", "  abc")])), None);
        assert_eq!(
            ev("P.TrimStart('').TrimStart('a')", &map(&[("P", "  abc")])),
            None
        );
        // A non-empty char set still trims exactly those.
        assert_eq!(
            ev("P.TrimStart(' ')", &map(&[("P", "  abc")])),
            Some(("abc".into(), vec![]))
        );
    }

    #[test]
    fn nested_function_in_path_arg_is_unsupported() {
        // Same bare-reference rule as string args: a nested property function
        // in a path-function argument is declined (MSBuild rejects
        // `Combine('$([MSBuild]::…())','b')`); a bare `$(Dir)` ref still works.
        assert_eq!(
            ev(
                "[System.IO.Path]::Combine('$([MSBuild]::IsRunningFromVisualStudio())','b')",
                &PropertyMap::new()
            ),
            None
        );
        assert_eq!(
            ev(
                "[System.IO.Path]::Combine('$(Dir)','b')",
                &map(&[("Dir", "/a")])
            ),
            Some(("/a/b".into(), vec![]))
        );
    }

    #[test]
    fn whitespace_only_arg_list_is_one_argument() {
        // `Func( )` is one (whitespace) argument, not zero — a zero-arg
        // intrinsic rejects it, unlike `Func()`.
        assert_eq!(
            ev(
                "[MSBuild]::IsRunningFromVisualStudio()",
                &PropertyMap::new()
            ),
            Some(("False".into(), vec![]))
        );
        assert_eq!(
            ev(
                "[MSBuild]::IsRunningFromVisualStudio( )",
                &PropertyMap::new()
            ),
            None
        );
    }

    #[test]
    fn string_member_on_bool_intrinsic_is_unsupported() {
        // `IsRunningFromVisualStudio()` is a bool; MSBuild rejects a string
        // member on it, so the chain must abort rather than commit on "false".
        assert_eq!(
            ev(
                "[MSBuild]::IsRunningFromVisualStudio().Contains('f')",
                &PropertyMap::new()
            ),
            None
        );
    }

    // --- the deliberate dotted-name correction ----------------------------

    #[test]
    fn dotted_name_is_member_access_not_a_property_name() {
        // `$(A.B)`: today's fast path read "A.B" as a property (→ Undefined);
        // the parser reads it as `.B` on property `A`, an unknown member →
        // Unsupported (MSBuild itself errors on this).
        assert_eq!(ev("A.B", &PropertyMap::new()), None);
        // A paren-less access to an *unknown* member is likewise a member, not
        // a property name — and, being unmodelled, stays Unsupported.
        assert_eq!(ev("Foo.Bogus", &map(&[("Foo", "abc")])), None);
    }

    // --- Stage 3: the new pinned evaluators (all pinned against msbuild) ---

    #[test]
    fn string_length_and_indexer() {
        // Paren-less `.Length` → int; `[n]` → the char at that position.
        assert_eq!(
            ev("Foo.Length", &map(&[("Foo", "abc")])),
            Some(("3".into(), vec![]))
        );
        assert_eq!(
            ev("Foo.Length", &map(&[("Foo", "")])),
            Some(("0".into(), vec![]))
        );
        assert_eq!(
            ev("Foo[0]", &map(&[("Foo", "abc")])),
            Some(("a".into(), vec![]))
        );
        assert_eq!(
            ev("Foo[2]", &map(&[("Foo", "abc")])),
            Some(("c".into(), vec![]))
        );
        // Out-of-range index → MSBuild errors → Unsupported (fail-safe).
        assert_eq!(ev("Foo[9]", &map(&[("Foo", "abc")])), None);
        // A `Char` has no `.Length` (MSBuild errors) — the chain must abort.
        assert_eq!(ev("Foo[0].Length", &map(&[("Foo", "abc")])), None);
        // Non-ASCII receiver declines (UTF-16 vs scalar indexing/length).
        assert_eq!(ev("Foo.Length", &map(&[("Foo", "café")])), None);
        assert_eq!(ev("Foo[0]", &map(&[("Foo", "é")])), None);
    }

    #[test]
    fn string_split_char_set() {
        // `Split('…')` is `String.Split(params char[])`: a char *set*, empty
        // entries kept; only reachable through an index (or `.Length`).
        let p = map(&[("V", "10.1.300-beta.1")]);
        assert_eq!(ev("V.Split('-')[0]", &p), Some(("10.1.300".into(), vec![])));
        assert_eq!(ev("V.Split('-')[1]", &p), Some(("beta.1".into(), vec![])));
        // Multi-char set splits on any member; empty entries kept.
        assert_eq!(
            ev("V.Split('-_')[1]", &map(&[("V", "a-b_c")])),
            Some(("b".into(), vec![]))
        );
        assert_eq!(
            ev("V.Split('--')[1]", &map(&[("V", "a--b")])),
            Some(("".into(), vec![]))
        );
        // `.Length` of the array; a chained string member on the element.
        assert_eq!(
            ev("V.Split('-').Length", &map(&[("V", "a-b-c")])),
            Some(("3".into(), vec![]))
        );
        assert_eq!(
            ev("V.Split('-')[0].Length", &map(&[("V", "abcd-ef")])),
            Some(("4".into(), vec![]))
        );
        // Terminal array (no index) → MSBuild's `System.String[]`; we decline.
        assert_eq!(ev("V.Split('-')", &map(&[("V", "a-b")])), None);
        // Out-of-range → Unsupported; empty char set (whitespace split) declined.
        assert_eq!(ev("V.Split('-')[9]", &map(&[("V", "a-b")])), None);
        assert_eq!(ev("V.Split('')[0]", &map(&[("V", "abc")])), None);
    }

    #[test]
    fn system_version_parse_and_components() {
        let empty = PropertyMap::new();
        assert_eq!(
            ev("[System.Version]::Parse('10.1.300').Major", &empty),
            Some(("10".into(), vec![]))
        );
        assert_eq!(
            ev("[System.Version]::Parse('10.1.300').Minor", &empty),
            Some(("1".into(), vec![]))
        );
        assert_eq!(
            ev("[System.Version]::Parse('10.1.300').Build", &empty),
            Some(("300".into(), vec![]))
        );
        // An absent Build (2-component version) is -1, matching .NET.
        assert_eq!(
            ev("[System.Version]::Parse('1.2').Build", &empty),
            Some(("-1".into(), vec![]))
        );
        // Terminal Version renders / `.ToString()`s as the joined components.
        assert_eq!(
            ev("[System.Version]::Parse('1.02.3')", &empty),
            Some(("1.2.3".into(), vec![]))
        );
        assert_eq!(
            ev("[System.Version]::Parse('1.2').Major.ToString()", &empty),
            Some(("1".into(), vec![]))
        );
        // MSBuild-error shapes → Unsupported: <2 or >4 fields, overflow, sign,
        // whitespace, unknown component member.
        for bad in [
            "[System.Version]::Parse('10').Major",
            "[System.Version]::Parse('').Major",
            "[System.Version]::Parse('1.2.3.4.5').Major",
            "[System.Version]::Parse('2147483648.1').Major",
            "[System.Version]::Parse('-1.2').Major",
            "[System.Version]::Parse(' 1.2 ').Major",
            "[System.Version]::Parse('1.2').Revision",
        ] {
            assert_eq!(ev(bad, &empty), None, "{bad}");
        }
        // Int32.MaxValue component is accepted.
        assert_eq!(
            ev("[System.Version]::Parse('2147483647.1').Major", &empty),
            Some(("2147483647".into(), vec![]))
        );
    }

    // EnsureTrailingSlash is pinned against the oracle on a unix host only; the
    // Windows separator semantics are unverified, so the evaluator declines
    // there (see `ensure_trailing_slash`). Gate the value pin accordingly, and
    // pin the Windows decline separately — mirroring the path-function tests'
    // `#[cfg(windows)]`/`#[cfg(not(windows))]` split.
    #[cfg(not(windows))]
    #[test]
    fn ensure_trailing_slash_pin() {
        let empty = PropertyMap::new();
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('/a/b')", &empty),
            Some(("/a/b/".into(), vec![]))
        );
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('/a/b/')", &empty),
            Some(("/a/b/".into(), vec![]))
        );
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('a')", &empty),
            Some(("a/".into(), vec![]))
        );
        // Backslashes normalise to `/` before the trailing-slash check.
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('a\\b')", &empty),
            Some(("a/b/".into(), vec![]))
        );
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('a\\')", &empty),
            Some(("a/".into(), vec![]))
        );
        // Empty maps to empty.
        assert_eq!(
            ev("[MSBuild]::EnsureTrailingSlash('')", &empty),
            Some(("".into(), vec![]))
        );
    }

    #[cfg(windows)]
    #[test]
    fn ensure_trailing_slash_declines_on_windows() {
        // Windows separator behaviour is unverified against the oracle, so the
        // evaluator declines rather than guess.
        assert_eq!(
            ev(
                "[MSBuild]::EnsureTrailingSlash('/a/b')",
                &PropertyMap::new()
            ),
            None
        );
    }

    #[test]
    fn value_typed_string_argument_admits_string_yielding_nested() {
        // A nested `$(…)` that yields a *string* is admitted (MSBuild coerces
        // the string parameter): the FSCore `Split('-')[0]` shape, and a
        // nested `TrimStart`.
        assert_eq!(
            ev(
                "[System.Version]::Parse('$(FSCore.Split('-')[0])').Major",
                &map(&[("FSCore", "10.1.300-beta.1")])
            ),
            Some(("10".into(), vec![]))
        );
        assert_eq!(
            ev(
                "P.Contains('$(V.Split('-')[0])')",
                &map(&[("P", "z10y"), ("V", "10-1")])
            ),
            Some(("True".into(), vec![]))
        );
        assert_eq!(
            ev(
                "[System.Version]::Parse('$(V.TrimStart('v'))').Major",
                &map(&[("V", "v8.0")])
            ),
            Some(("8".into(), vec![]))
        );
        // A nested `$(…)` that yields a non-string (int/array) is *declined* —
        // MSBuild errors on it, so committing would over-resolve.
        assert_eq!(
            ev(
                "P.Contains('$(V.Length)')",
                &map(&[("P", "z"), ("V", "abc")])
            ),
            None
        );
        assert_eq!(
            ev(
                "P.Contains('$(V.Split('-'))')",
                &map(&[("P", "z"), ("V", "a-b")])
            ),
            None
        );
        assert_eq!(
            ev(
                "P.Contains('$([System.Version]::Parse('1.2').Major)')",
                &map(&[("P", "z")])
            ),
            None
        );
    }
}

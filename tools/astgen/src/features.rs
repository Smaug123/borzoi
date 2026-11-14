//! The language-feature table, `crates/cst/language-features.tsv`: its parser,
//! and the `LanguageFeature` module generated from it.
//!
//! The table is the single source for which FCS language feature introduced
//! which syntax and what borzoi does about it (`docs/ast-versioning-plan.md`,
//! "The feature table"). This module reads it at compile time, validates every
//! cell, and emits `crates/cst/src/syntax/generated/features.rs`. The facade
//! generator in the crate root reads the same rows for its kind intervals
//! ([`live_kind_introduced`]), so the parser's gates and the facades cannot
//! disagree about a kind.
//!
//! `borzoi-cst`'s `language_features` test holds the table to the compiler
//! source: every FCS feature has a row with FCS's version and display name, and
//! the `fcs_sites` column equals a census of the parse pipeline. It reads the
//! rows through [`feature_table`], so the table has one parser.

/// The table, as checked in. `include_str!` makes a table edit rebuild this
/// crate, so the staleness test always sees the current table.
const TABLE: &str = include_str!("../../../crates/cst/language-features.tsv");

/// Repo-root-relative path of the table.
pub const TABLE_PATH: &str = "crates/cst/language-features.tsv";

/// Repo-root-relative path of the generated module.
pub const OUTPUT_PATH: &str = "crates/cst/src/syntax/generated/features.rs";

/// The header row, which every table must open with (after its comments).
/// `notes`, the last column, may be omitted from a row.
pub const HEADER: [&str; 9] = [
    "feature",
    "introduced",
    "fcs_stage",
    "fcs_effect",
    "borzoi",
    "engaged",
    "fcs_sites",
    "display_name",
    "notes",
];

/// Every version a row may name, ascending: its spelling in the table, the
/// `LanguageVersion` variant, and FCS's decimal (`previewVersion = 9999`).
pub(crate) const VERSIONS: &[(&str, &str, f64)] = &[
    ("4.6", "V4_6", 4.6),
    ("4.7", "V4_7", 4.7),
    ("5.0", "V5_0", 5.0),
    ("6.0", "V6_0", 6.0),
    ("7.0", "V7_0", 7.0),
    ("8.0", "V8_0", 8.0),
    ("9.0", "V9_0", 9.0),
    ("10.0", "V10_0", 10.0),
    ("11.0", "V11_0", 11.0),
    ("preview", "Preview", 9999.0),
];

/// Where FCS consults a feature (`fcs_stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Lex,
    LexFilter,
    Parse,
    PostParse,
    Check,
}

impl Stage {
    const ALL: [(&'static str, Stage, &'static str); 5] = [
        ("lex", Stage::Lex, "Lex"),
        ("lexfilter", Stage::LexFilter, "LexFilter"),
        ("parse", Stage::Parse, "Parse"),
        ("post-parse", Stage::PostParse, "PostParse"),
        ("check", Stage::Check, "Check"),
    ];

    fn parse(text: &str) -> Result<Stage, String> {
        Self::ALL
            .iter()
            .find(|(t, _, _)| *t == text)
            .map(|(_, s, _)| *s)
            .ok_or_else(|| format!("unknown fcs_stage {text:?}"))
    }

    fn variant(self) -> &'static str {
        Self::ALL.iter().find(|(_, s, _)| *s == self).unwrap().2
    }
}

/// What FCS does below a feature's version (`fcs_effect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Diagnose,
    Reshape,
    Severity,
}

impl Effect {
    fn parse(text: &str) -> Result<Effect, String> {
        match text {
            "Diagnose" => Ok(Effect::Diagnose),
            "Reshape" => Ok(Effect::Reshape),
            "Severity" => Ok(Effect::Severity),
            _ => Err(format!("unknown fcs_effect {text:?}")),
        }
    }
}

/// One item of the `borzoi` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modelling {
    /// A `SyntaxKind` the feature introduced.
    Kind(String),
    /// A gate on a construct with no kind of its own, named by its detector.
    Positional(String),
    /// The parse itself depends on the version.
    Shape,
    /// A directive or other trivia.
    Trivia,
    /// Not modelled, and why.
    Unmodelled(String),
}

/// The `engaged` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engaged {
    /// The row's gates and kind intervals are in force.
    Live,
    /// Recorded but inert, and why: no gate, no interval.
    Deferred(String),
    /// An `Unmodelled` row.
    NotModelled,
}

/// One row of the table.
#[derive(Debug, Clone)]
pub struct FeatureRow {
    /// FCS's `LanguageFeature` case name.
    pub name: String,
    /// The version text: `"4.7"` … `"11.0"`, or `"preview"`.
    pub introduced: String,
    pub stages: Vec<Stage>,
    /// Empty for a `check` row.
    pub effects: Vec<Effect>,
    pub borzoi: Vec<Modelling>,
    pub engaged: Engaged,
    /// `(path under src/Compiler, use count)`, sorted by path.
    pub sites: Vec<(String, usize)>,
    pub display_name: String,
    pub notes: String,
}

impl FeatureRow {
    /// The `SyntaxKind` names of the row's `Kind` items.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.borzoi.iter().filter_map(|m| match m {
            Modelling::Kind(k) => Some(k.as_str()),
            _ => None,
        })
    }

    /// Whether the row's gates and intervals are in force.
    pub fn is_live(&self) -> bool {
        self.engaged == Engaged::Live
    }

    /// Whether borzoi gates the feature where it occurs: a live row with a
    /// `Kind` or `Positional` item.
    pub fn is_positionally_gated(&self) -> bool {
        self.is_live()
            && self
                .borzoi
                .iter()
                .any(|m| matches!(m, Modelling::Kind(_) | Modelling::Positional(_)))
    }

    /// FCS's decimal for the row's version (`preview` is 9999).
    pub fn introduced_decimal(&self) -> f64 {
        VERSIONS
            .iter()
            .find(|(t, _, _)| *t == self.introduced)
            .unwrap()
            .2
    }
}

/// Render a `fcs_sites` cell from a census: what the table must say.
pub fn sites_cell(sites: &[(String, usize)]) -> String {
    if sites.is_empty() {
        return "-".to_string();
    }
    sites
        .iter()
        .map(|(path, n)| format!("{path}={n}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Split `text` on `sep` outside parentheses, so a reason may contain it.
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => {
                out.push(&text[start..i]);
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// `Head(arg)` → `arg`, if `item` has that head.
fn call<'a>(item: &'a str, head: &str) -> Option<&'a str> {
    item.strip_prefix(head)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn parse_modelling(cell: &str) -> Result<Vec<Modelling>, String> {
    let mut out = Vec::new();
    for item in split_top_level(cell, '+') {
        let m = if let Some(kind) = call(item, "Kind") {
            let well_formed = kind.starts_with(|c: char| c.is_ascii_uppercase())
                && kind
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if !well_formed {
                return Err(format!("Kind({kind}) does not name a SyntaxKind"));
            }
            Modelling::Kind(kind.to_string())
        } else if let Some(detector) = call(item, "Positional") {
            Modelling::Positional(detector.to_string())
        } else if let Some(reason) = call(item, "Unmodelled") {
            Modelling::Unmodelled(reason.to_string())
        } else if item == "Shape" {
            Modelling::Shape
        } else if item == "Trivia" {
            Modelling::Trivia
        } else {
            return Err(format!(
                "unknown borzoi item {item:?}: expected Kind(K), Positional(detector), \
                 Shape, Trivia or Unmodelled(reason)"
            ));
        };
        if let Modelling::Positional(arg) | Modelling::Unmodelled(arg) = &m
            && arg.trim().is_empty()
        {
            return Err(format!("{item:?} needs a detector or reason"));
        }
        out.push(m);
    }
    let unmodelled = out.iter().any(|m| matches!(m, Modelling::Unmodelled(_)));
    if unmodelled && out.len() > 1 {
        return Err("Unmodelled(…) must stand alone in the borzoi column".to_string());
    }
    Ok(out)
}

fn parse_sites(cell: &str) -> Result<Vec<(String, usize)>, String> {
    if cell == "-" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for item in cell.split(',') {
        let (path, count) = item
            .split_once('=')
            .ok_or_else(|| format!("fcs_sites item {item:?} is not path=count"))?;
        let count: usize = count
            .parse()
            .map_err(|_| format!("fcs_sites count in {item:?} is not a number"))?;
        if count == 0 || path.is_empty() {
            return Err(format!(
                "fcs_sites item {item:?} must name a path and a count ≥ 1"
            ));
        }
        out.push((path.to_string(), count));
    }
    if !out.windows(2).all(|w| w[0].0 < w[1].0) {
        return Err(format!(
            "fcs_sites paths must be sorted and distinct: {cell:?}"
        ));
    }
    Ok(out)
}

fn parse_row(cells: &[&str]) -> Result<FeatureRow, String> {
    if cells.len() != HEADER.len() && cells.len() != HEADER.len() - 1 {
        return Err(format!(
            "{} columns, expected {} (or {} without notes)",
            cells.len(),
            HEADER.len(),
            HEADER.len() - 1
        ));
    }
    let name = cells[0];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("feature name {name:?} is not an FCS case name"));
    }
    let introduced = cells[1];
    if !VERSIONS.iter().any(|(t, _, _)| *t == introduced) {
        return Err(format!("unknown introduced version {introduced:?}"));
    }
    let stages = split_top_level(cells[2], '+')
        .into_iter()
        .map(Stage::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let check_only = stages == [Stage::Check];
    if !check_only && stages.contains(&Stage::Check) {
        return Err(
            "`check` is for a feature the parse pipeline never consults; it \
                    does not combine with another stage"
                .to_string(),
        );
    }
    let effects = if cells[3] == "-" {
        Vec::new()
    } else {
        split_top_level(cells[3], '+')
            .into_iter()
            .map(Effect::parse)
            .collect::<Result<Vec<_>, _>>()?
    };
    if check_only != effects.is_empty() {
        return Err("fcs_effect is `-` exactly when fcs_stage is `check`".to_string());
    }
    let borzoi = parse_modelling(cells[4])?;
    let unmodelled = matches!(borzoi[..], [Modelling::Unmodelled(_)]);
    let engaged = match cells[5] {
        "live" => Engaged::Live,
        "-" => Engaged::NotModelled,
        other => match call(other, "deferred") {
            Some(reason) if !reason.trim().is_empty() => Engaged::Deferred(reason.to_string()),
            _ => {
                return Err(format!(
                    "engaged {other:?}: expected live, deferred(reason) or -"
                ));
            }
        },
    };
    if unmodelled != (engaged == Engaged::NotModelled) {
        return Err("engaged is `-` exactly when borzoi is Unmodelled(…)".to_string());
    }
    let sites = parse_sites(cells[6])?;
    let display_name = cells[7];
    if display_name.is_empty() {
        return Err("display_name is empty".to_string());
    }
    if display_name.contains("``") {
        return Err(
            "display_name cannot hold a double backtick: the generated doc \
                    comment quotes it in one"
                .to_string(),
        );
    }
    Ok(FeatureRow {
        name: name.to_string(),
        introduced: introduced.to_string(),
        stages,
        effects,
        borzoi,
        engaged,
        sites,
        display_name: display_name.to_string(),
        notes: cells.get(8).copied().unwrap_or("").to_string(),
    })
}

/// Parse a table. Panics, naming the line, on any malformed cell.
pub fn parse_table(text: &str) -> Vec<FeatureRow> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.starts_with('#') && !l.trim().is_empty());
    let (_, header) = lines.next().expect("the feature table has a header row");
    assert_eq!(
        header.split('\t').collect::<Vec<_>>(),
        HEADER,
        "{TABLE_PATH}: the header row must be exactly the columns of `HEADER`",
    );
    let mut rows: Vec<FeatureRow> = Vec::new();
    for (i, line) in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        let row =
            parse_row(&cells).unwrap_or_else(|e| panic!("{TABLE_PATH}:{}: {e}\n  {line}", i + 1));
        assert!(
            rows.iter().all(|r| r.name != row.name),
            "{TABLE_PATH}:{}: feature {} has two rows",
            i + 1,
            row.name,
        );
        for kind in row.kinds() {
            if let Some(other) = rows.iter().find(|r| r.kinds().any(|k| k == kind)) {
                panic!(
                    "{TABLE_PATH}:{}: {kind} is claimed by both {} and {}",
                    i + 1,
                    other.name,
                    row.name,
                );
            }
        }
        rows.push(row);
    }
    rows
}

/// The checked-in table's rows, in table order.
pub fn feature_table() -> Vec<FeatureRow> {
    parse_table(TABLE)
}

/// The version (as FCS's decimal) that introduced the kind named `kind`, from
/// the live `Kind` rows; `None` for a kind no live row claims, which is legal
/// at every version.
pub fn live_kind_introduced(rows: &[FeatureRow], kind: &str) -> Option<f64> {
    rows.iter()
        .filter(|r| r.is_live())
        .find(|r| r.kinds().any(|k| k == kind))
        .map(FeatureRow::introduced_decimal)
}

/// Generate `crates/cst/src/syntax/generated/features.rs` from `rows`.
pub fn generate_features(rows: &[FeatureRow]) -> String {
    let mut out = String::from(
        "//! GENERATED by `tools/astgen` from `crates/cst/language-features.tsv` — do\n\
         //! not edit by hand. Edit the table, then regenerate with\n\
         //! `cargo run -p borzoi-astgen --example generate`; kept honest by\n\
         //! borzoi-astgen's `tests/up_to_date.rs` staleness test.\n\
         //!\n\
         //! FCS's language features and what the parser does with each\n\
         //! (`docs/ast-versioning-plan.md`, \"The feature table\").\n\n\
         use crate::language_version::LanguageVersion;\n\
         use crate::syntax::SyntaxKind;\n\n",
    );

    out.push_str(
        "/// Where FCS consults a feature: the `fcs_stage` column.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n\
         pub enum FcsStage {\n\
         /// The lexer (`lex.fsl`, and `WarnScopes.fs` for `#nowarn`).\n\
         Lex,\n\
         /// The offside rule (`LexFilter.fs`).\n\
         LexFilter,\n\
         /// The parser (`pars.fsy`, `ParseHelpers.fs`).\n\
         Parse,\n\
         /// After the parse, before checking (`SyntaxTreeOps.fs` from\n\
         /// `ParseAndCheckInputs.fs`).\n\
         PostParse,\n\
         /// Only the type checker or a later phase.\n\
         Check,\n\
         }\n\n\
         /// What FCS does with a source using a feature below its version: the\n\
         /// `fcs_effect` column.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n\
         pub enum FcsEffect {\n\
         /// A diagnostic over the tree it builds at every version\n\
         /// (`CheckLanguageFeatureAndRecover`).\n\
         Diagnose,\n\
         /// A hard error with recovery, a relabelled token, or a different\n\
         /// token stream: the tree depends on the version.\n\
         Reshape,\n\
         /// An existing diagnostic appears or changes severity.\n\
         Severity,\n\
         }\n\n",
    );

    out.push_str(
        "/// An FCS language feature (`LanguageFeature` in `LanguageFeatures.fs`),\n\
         /// one variant per row of `crates/cst/language-features.tsv`.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n\
         pub enum LanguageFeature {\n",
    );
    for row in rows {
        out.push_str(&format!(
            // A double-backtick code span: the names hold single backticks, `<`,
            // `[` and URLs, none of which markdown may interpret.
            "/// F# {}: `` {} `` (its FS3350 name).\n{},\n",
            row.introduced, row.display_name, row.name
        ));
    }
    out.push_str("}\n\n");

    let list = |names: Vec<&str>| {
        names
            .iter()
            .map(|n| format!("LanguageFeature::{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let positional: Vec<&str> = rows
        .iter()
        .filter(|r| r.is_positionally_gated())
        .map(|r| r.name.as_str())
        .collect();

    out.push_str("impl LanguageFeature {\n");
    out.push_str(&format!(
        "/// Every feature, in table order.\n\
         pub const ALL: [LanguageFeature; {}] = [{}];\n\n",
        rows.len(),
        list(rows.iter().map(|r| r.name.as_str()).collect()),
    ));
    out.push_str(&format!(
        "/// The features the parser gates where they occur: the live rows with a\n\
         /// `Kind` or `Positional` item. A parse at a version supporting all of\n\
         /// them draws no such gate.\n\
         pub const POSITIONAL_GATES: [LanguageFeature; {}] = [{}];\n\n",
        positional.len(),
        list(positional.clone()),
    ));

    out.push_str(
        "/// The first version with the feature (FCS's `features` map).\n\
         pub fn introduced(self) -> LanguageVersion {\nmatch self {\n",
    );
    for (text, variant, _) in VERSIONS {
        let names: Vec<&str> = rows
            .iter()
            .filter(|r| r.introduced == *text)
            .map(|r| r.name.as_str())
            .collect();
        if names.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{} => LanguageVersion::{variant},\n",
            names
                .iter()
                .map(|n| format!("LanguageFeature::{n}"))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    out.push_str("}\n}\n\n");

    out.push_str(
        "/// FCS's name for the feature in its FS3350 \"Feature '…' is not\n\
         /// available\" message (`FSComp.txt`).\n\
         pub fn display_name(self) -> &'static str {\nmatch self {\n",
    );
    for row in rows {
        out.push_str(&format!(
            "LanguageFeature::{} => {:?},\n",
            row.name, row.display_name
        ));
    }
    out.push_str("}\n}\n\n");

    out.push_str(
        "/// Where FCS consults the feature.\n\
         pub fn fcs_stages(self) -> &'static [FcsStage] {\nmatch self {\n",
    );
    for row in rows {
        let stages: Vec<String> = row
            .stages
            .iter()
            .map(|s| format!("FcsStage::{}", s.variant()))
            .collect();
        out.push_str(&format!(
            "LanguageFeature::{} => &[{}],\n",
            row.name,
            stages.join(", ")
        ));
    }
    out.push_str("}\n}\n\n");

    out.push_str(
        "/// What FCS does below the feature's version; empty when only the type\n\
         /// checker or a later phase consults it.\n\
         pub fn fcs_effects(self) -> &'static [FcsEffect] {\nmatch self {\n",
    );
    for row in rows {
        let effects: Vec<String> = row
            .effects
            .iter()
            .map(|e| format!("FcsEffect::{e:?}"))
            .collect();
        out.push_str(&format!(
            "LanguageFeature::{} => &[{}],\n",
            row.name,
            effects.join(", ")
        ));
    }
    out.push_str("}\n}\n}\n\n");

    out.push_str(
        "/// The [`LanguageFeature`] whose introduction made a node of `kind` legal,\n\
         /// from the table's live `Kind` rows; `None` for a kind legal at every\n\
         /// version. The parser gates each such node below its feature's version\n\
         /// (`parser::feature_gates`), and the `vN` facades leave it out below it.\n\
         pub fn kind_feature(kind: SyntaxKind) -> Option<LanguageFeature> {\nmatch kind {\n",
    );
    for row in rows.iter().filter(|r| r.is_live()) {
        for kind in row.kinds() {
            out.push_str(&format!(
                "SyntaxKind::{kind} => Some(LanguageFeature::{}),\n",
                row.name
            ));
        }
    }
    out.push_str("_ => None,\n}\n}\n\n");

    let deferred: Vec<String> = rows
        .iter()
        .filter(|r| matches!(r.engaged, Engaged::Deferred(_)))
        .flat_map(|r| {
            r.kinds()
                .map(|k| format!("(SyntaxKind::{k}, LanguageFeature::{})", r.name))
                .collect::<Vec<_>>()
        })
        .collect();
    out.push_str(&format!(
        "/// The kinds of the table's deferred `Kind` rows, with their feature: a\n\
         /// feature introduced each, but neither the gates nor the facades act on\n\
         /// it yet, so [`kind_feature`] does not return it.\n\
         pub const DEFERRED_KIND_FEATURES: [(SyntaxKind, LanguageFeature); {}] = [{}];\n",
        deferred.len(),
        deferred.join(", "),
    ));
    crate::reformat(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &str) -> String {
        format!("# comment\n{}\n{rows}", HEADER.join("\t"))
    }

    #[test]
    fn the_checked_in_table_parses() {
        let rows = feature_table();
        assert!(rows.iter().any(|r| r.name == "WhileBang"));
    }

    #[test]
    fn a_row_may_omit_notes() {
        let rows = parse_table(&table(
            "WhileBang\t8.0\tparse\tDiagnose\tKind(WHILE_BANG_EXPR)\tlive\tpars.fsy=1\t'while!' expression",
        ));
        assert_eq!(rows[0].notes, "");
        assert_eq!(rows[0].sites, [("pars.fsy".to_string(), 1)]);
    }

    #[test]
    fn a_reason_may_contain_the_separator() {
        let rows = parse_table(&table(
            "X\t8.0\tparse\tDiagnose\tUnmodelled(a + b (c))\t-\t-\tx\t",
        ));
        assert_eq!(
            rows[0].borzoi,
            [Modelling::Unmodelled("a + b (c)".to_string())]
        );
    }

    #[test]
    #[should_panic(expected = "engaged is `-` exactly when")]
    fn a_modelled_row_must_say_whether_it_is_live() {
        parse_table(&table(
            "X\t8.0\tparse\tDiagnose\tKind(WHILE_BANG_EXPR)\t-\t-\tx\t",
        ));
    }

    #[test]
    #[should_panic(expected = "must stand alone")]
    fn unmodelled_stands_alone() {
        parse_table(&table(
            "X\t8.0\tparse\tDiagnose\tShape+Unmodelled(r)\t-\t-\tx\t",
        ));
    }

    #[test]
    #[should_panic(expected = "fcs_effect is `-` exactly when")]
    fn a_check_row_has_no_effect() {
        parse_table(&table("X\t8.0\tcheck\tDiagnose\tUnmodelled(r)\t-\t-\tx\t"));
    }

    #[test]
    #[should_panic(expected = "claimed by both")]
    fn a_kind_belongs_to_one_feature() {
        parse_table(&table(
            "X\t8.0\tparse\tDiagnose\tKind(K)\tlive\t-\tx\t\n\
             Y\t9.0\tparse\tDiagnose\tKind(K)\tlive\t-\ty\t",
        ));
    }

    #[test]
    fn deferred_kinds_have_no_interval() {
        let rows = parse_table(&table(
            "X\t8.0\tparse\tDiagnose\tKind(A)\tlive\t-\tx\t\n\
             Y\t5.0\tcheck\t-\tKind(B)\tdeferred(later)\t-\ty\t",
        ));
        assert_eq!(live_kind_introduced(&rows, "A"), Some(8.0));
        assert_eq!(live_kind_introduced(&rows, "B"), None);
    }
}

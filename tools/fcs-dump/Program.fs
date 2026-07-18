module FcsDump.Program

// The FCS public tokenizer API (`FSharpLexer`, `FSharpLexerFlags`,
// `FSharpTokenKind`) is annotated `[<Experimental>]`. Combined with the
// fsproj's TreatWarningsAsErrors that turns every reference into a build
// error; suppress just FS0057 for this file.
#nowarn "57"

open System
open System.IO
open System.Reflection
open System.Text.Json
open System.Text.Json.Serialization

open Microsoft.FSharp.Reflection

open FSharp.Compiler.CodeAnalysis
open FSharp.Compiler.Syntax
open FSharp.Compiler.Symbols
open FSharp.Compiler.Symbols.FSharpExprPatterns
open FSharp.Compiler.Text
open FSharp.Compiler.Tokenization
open FSharp.Compiler.Xml

/// Compact JSON shape for `FSharp.Compiler.Text.pos`: `{Line, Col}`.
type PosConverter() =
    inherit JsonConverter<pos>()
    override _.Read(_, _, _) = failwith "fcs-dump: pos read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteStartObject()
        writer.WriteNumber("Line", value.Line)
        writer.WriteNumber("Col", value.Column)
        writer.WriteEndObject()

/// Compact JSON shape for `FSharp.Compiler.Text.range`. Bypasses the
/// `StartRange`/`EndRange` self-properties on `range` which would otherwise
/// cycle the serializer.
type RangeConverter() =
    inherit JsonConverter<range>()
    override _.Read(_, _, _) = failwith "fcs-dump: range read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteStartObject()
        writer.WriteString("File", value.FileName)
        writer.WritePropertyName("Start")
        writer.WriteStartObject()
        writer.WriteNumber("Line", value.StartLine)
        writer.WriteNumber("Col", value.StartColumn)
        writer.WriteEndObject()
        writer.WritePropertyName("End")
        writer.WriteStartObject()
        writer.WriteNumber("Line", value.EndLine)
        writer.WriteNumber("Col", value.EndColumn)
        writer.WriteEndObject()
        writer.WriteEndObject()

/// JSON shape for `System.Decimal`. The default System.Text.Json encoding
/// writes a decimal as a JSON number (e.g. `1.0` -> `1.0`), but downstream
/// JSON readers that round-trip through `f64` lose the trailing-zero scale
/// (`1.0m` and `1.00m` parse to different decimals and FCS preserves that
/// distinction). Emit the decimal as a JSON string using
/// `decimal.ToString(InvariantCulture)`, which preserves scale.
type DecimalConverter() =
    inherit JsonConverter<decimal>()
    override _.Read(_, _, _) = failwith "fcs-dump: decimal read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteStringValue(value.ToString(System.Globalization.CultureInfo.InvariantCulture))

/// JSON shape for `System.Double` (FCS's `SynConst.Double`). The default
/// System.Text.Json encoding writes the shortest round-trippable *text*
/// (e.g. `1.5430806348152437`), but the downstream Rust harness reads that
/// number through `serde_json`, whose float parser is not always correctly
/// rounded — `1.5430806348152437` decodes one ULP low there, so a bit-exact
/// comparison against our own (correctly-rounded) parse spuriously diverges.
/// Emit the IEEE-754 bit pattern as an integer instead: `serde_json` reads
/// integers exactly, so no float text round-trip is involved and the compared
/// bits are precisely the ones FCS held. `DoubleToInt64Bits` is the natural
/// inverse of the Rust side's `f64::to_bits` (mod the `i64`/`u64` reinterpret).
type DoubleConverter() =
    inherit JsonConverter<double>()
    override _.Read(_, _, _) = failwith "fcs-dump: double read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteNumberValue(System.BitConverter.DoubleToInt64Bits value)

/// JSON shape for `System.Single` (FCS's `SynConst.Single`). Same rationale as
/// [`DoubleConverter`] — emit the 32-bit IEEE-754 pattern as an integer so the
/// Rust harness reconstructs the exact `f32` bits without a float text parse.
type SingleConverter() =
    inherit JsonConverter<single>()
    override _.Read(_, _, _) = failwith "fcs-dump: single read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteNumberValue(System.BitConverter.SingleToInt32Bits value)

/// JSON shape for `System.Char` (FCS's `SynConst.Char`). A .NET `char` is a
/// UTF-16 code unit, not necessarily a Unicode scalar value: recovery paths can
/// carry a lone surrogate such as U+D800. The default System.Text.Json string
/// encoding would replace that with U+FFFD, collapsing distinct code units.
/// Emit the raw 16-bit unit as a JSON number instead.
type CharConverter() =
    inherit JsonConverter<char>()
    override _.Read(_, _, _) = failwith "fcs-dump: char read not supported"
    override _.Write(writer, value, _options) =
        writer.WriteNumberValue(int value)

let private writeUtf16Units (writer: Utf8JsonWriter) (value: string) =
    writer.WriteStartArray()
    for ch in value do
        writer.WriteNumberValue(int ch)
    writer.WriteEndArray()

let private writeAdjacentTagWithStringUnits
    (writer: Utf8JsonWriter)
    (value: obj)
    (unionType: Type)
    (options: JsonSerializerOptions)
    (isStringPayload: string -> int -> bool)
    =
    let unionCase, fields = FSharpValue.GetUnionFields(value, unionType)
    writer.WriteStartObject()
    writer.WriteString("Case", unionCase.Name)
    if fields.Length > 0 then
        writer.WritePropertyName("Fields")
        writer.WriteStartArray()
        fields
        |> Array.iteri (fun i field ->
            if isStringPayload unionCase.Name i then
                match field with
                | :? string as s -> writeUtf16Units writer s
                | null -> writer.WriteNullValue()
                | other ->
                    failwithf
                        "fcs-dump: expected string payload for %s field %d, got %s"
                        unionCase.Name
                        i
                        (other.GetType().FullName)
            else
                match field with
                | null -> writer.WriteNullValue()
                | nonNull -> JsonSerializer.Serialize(writer, nonNull, nonNull.GetType(), options))
        writer.WriteEndArray()
    writer.WriteEndObject()

/// JSON shape for `SynConst.String`, preserving the raw UTF-16 code units of
/// FCS's .NET string payload. Other `SynConst` cases keep the same AdjacentTag
/// shape as `JsonFSharpConverter`.
type SynConstConverter() =
    inherit JsonConverter<SynConst>()
    override _.Read(_, _, _) = failwith "fcs-dump: SynConst read not supported"
    override _.Write(writer, value, options) =
        writeAdjacentTagWithStringUnits
            writer
            value
            typeof<SynConst>
            options
            (fun caseName fieldIndex -> caseName = "String" && fieldIndex = 0)

/// JSON shape for `SynInterpolatedStringPart.String`, preserving raw UTF-16
/// units for literal interpolation fragments.
type SynInterpolatedStringPartConverter() =
    inherit JsonConverter<SynInterpolatedStringPart>()
    override _.Read(_, _, _) =
        failwith "fcs-dump: SynInterpolatedStringPart read not supported"
    override _.Write(writer, value, options) =
        writeAdjacentTagWithStringUnits
            writer
            value
            typeof<SynInterpolatedStringPart>
            options
            (fun caseName fieldIndex -> caseName = "String" && fieldIndex = 0)

/// JSON shape for hash-directive string arguments (`#I`, `#load`, ...),
/// preserving their raw UTF-16 payload.
type ParsedHashDirectiveArgumentConverter() =
    inherit JsonConverter<ParsedHashDirectiveArgument>()
    override _.Read(_, _, _) =
        failwith "fcs-dump: ParsedHashDirectiveArgument read not supported"
    override _.Write(writer, value, options) =
        writeAdjacentTagWithStringUnits
            writer
            value
            typeof<ParsedHashDirectiveArgument>
            options
            (fun caseName fieldIndex -> caseName = "String" && fieldIndex = 0)

/// Normalised JSON shape for `PreXmlDoc`. The DU's `PreXmlDoc(pos, collector)`
/// case wraps an opaque `XmlDocCollector` whose state is private, so the
/// generic union converter would dump an empty record and silently drop the
/// XML doc content. `.ToXmlDoc(check=false, paramNamesOpt=None)` collapses
/// every case (Direct/Merge/Empty/Collector) into a flat `XmlDoc` with
/// `UnprocessedLines` and `Range`; emit that.
type PreXmlDocConverter() =
    inherit JsonConverter<PreXmlDoc>()
    override _.Read(_, _, _) = failwith "fcs-dump: PreXmlDoc read not supported"
    override _.Write(writer, value, options) =
        let xml = value.ToXmlDoc(false, None)
        writer.WriteStartObject()
        writer.WriteBoolean("IsEmpty", value.IsEmpty)
        writer.WritePropertyName("Lines")
        writer.WriteStartArray()
        for line in xml.UnprocessedLines do
            writer.WriteStringValue(line)
        writer.WriteEndArray()
        writer.WritePropertyName("Range")
        JsonSerializer.Serialize(writer, xml.Range, options)
        writer.WriteEndObject()

let private buildOptionsCore (indented: bool) =
    let o = JsonSerializerOptions(WriteIndented = indented)
    // System.Text.Json's default `MaxDepth = 64` is well below the
    // nesting depth of real F# ASTs (Program.fs itself blows past it),
    // so the dump aborts mid-serialise with `JsonException: object cycle
    // detected`. The AST is a tree, not a graph; bump the cap.
    o.MaxDepth <- 4096
    o.Converters.Add(RangeConverter())
    o.Converters.Add(PosConverter())
    o.Converters.Add(PreXmlDocConverter())
    o.Converters.Add(DecimalConverter())
    o.Converters.Add(DoubleConverter())
    o.Converters.Add(SingleConverter())
    o.Converters.Add(CharConverter())
    o.Converters.Add(SynConstConverter())
    o.Converters.Add(SynInterpolatedStringPartConverter())
    o.Converters.Add(ParsedHashDirectiveArgumentConverter())
    o.Converters.Add(JsonFSharpConverter(JsonUnionEncoding.AdjacentTag))
    o

let private buildOptions () = buildOptionsCore true
let private buildOptionsCompact () = buildOptionsCore false

let private isScriptPath (absolute: string) =
    let ext =
        match Path.GetExtension(absolute) with
        | null -> ""
        | e -> e.ToLowerInvariant()
    ext = ".fsx" || ext = ".fsscript"

/// The caller's `<LangVersion>` pin (`BORZOI_FCS_LANGVERSION`, a single
/// canonical token such as `preview` / `7.0`), or `None` when unset. Applied to
/// `FSharpParsingOptions.LangVersionText` by [`withLangVersion`] so the *parse*
/// (hence the lex-filter's `StrictIndentation` feature gate, which governs both
/// the FS0058 severity and whether an offside push is kept) takes the requested
/// version. Unset means the SDK/FCS default, which our `LanguageVersion::DEFAULT`
/// already agrees with.
let private langVersionEnv () : string option =
    match Option.ofObj (Environment.GetEnvironmentVariable "BORZOI_FCS_LANGVERSION") with
    | None -> None
    | Some s when s.Trim() = "" -> None
    | Some s -> Some(s.Trim())

/// Apply the [`langVersionEnv`] pin to a parsing-options record's
/// `LangVersionText`, leaving it untouched when unset.
let private withLangVersion (opts: FSharpParsingOptions) : FSharpParsingOptions =
    match langVersionEnv () with
    | None -> opts
    | Some v -> { opts with LangVersionText = v }

let private dumpAst (absolute: string) (defines: string list) =
    let text = File.ReadAllText(absolute)
    let sourceText = SourceText.ofString text
    let checker = FSharpChecker.Create()
    let isScript = isScriptPath absolute

    let parsingOptions0, _ =
        checker.GetParsingOptionsFromCommandLineArgs(
            [ absolute ],
            isInteractive = isScript)

    // Thread the requested conditional-compilation symbols so `#if FOO`
    // selects the active branch the caller intends. For a `.fs` file the
    // command-line options carry no defines, so this is the full set FCS
    // sees (plus its implicit editing defines, which our fixtures avoid).
    let parsingOptions =
        { parsingOptions0 with
            ConditionalDefines = defines }
        |> withLangVersion

    let parseResults =
        checker.ParseFile(absolute, sourceText, parsingOptions, userOpName = "fcs-dump")
        |> Async.RunSynchronously

    let payload =
        {| ParseTree = parseResults.ParseTree
           Diagnostics =
               parseResults.Diagnostics
               |> Array.map (fun d ->
                   {| Severity = d.Severity.ToString()
                      Message = d.Message
                      ErrorNumber = d.ErrorNumber
                      Range = d.Range |})
           ParseHadErrors = parseResults.ParseHadErrors
           IsScript = isScript |}

    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Batch AST dump. Reads file paths from stdin, one per line, and emits one
/// JSON object per file to stdout (JSONL) carrying the same payload as the
/// single-file [`dumpAst`] plus a `Path` field for correlation. On per-file
/// failure emits `{Path, Error}` so the consumer can correlate without
/// aborting the batch. Amortises the ~150 ms .NET startup *and* the
/// `FSharpChecker` construction over a whole corpus run.
///
/// Equivalent to running single-file `ast` on each path: `ConditionalDefines`
/// stays empty and FCS's service parser supplies the implicit per-file-kind
/// symbols (`COMPILED`/`EDITING` for `.fs`/`.fsi`, `INTERACTIVE`/`EDITING` for
/// `.fsx`). The Rust differential sweep
/// (`crates/cst/tests/parser_corpus_diff.rs`) defines the matching `.fs`/`.fsi`
/// set so the two compare like-for-like.
let private dumpAstBatchCore () =
    let options = buildOptionsCompact ()
    let checker = FSharpChecker.Create()

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()
        if path <> "" then
            // Serialise *inside* the guard: a deeply-nested AST can make
            // System.Text.Json throw (or, beyond the stack, overflow — see
            // `dumpAstBatch`), and one bad file must not abort the batch.
            let json =
                try
                    let absolute = Path.GetFullPath path
                    let text = File.ReadAllText(absolute)
                    let sourceText = SourceText.ofString text
                    let isScript = isScriptPath absolute
                    let parsingOptions0, _ =
                        checker.GetParsingOptionsFromCommandLineArgs(
                            [ absolute ],
                            isInteractive = isScript)
                    // Keep `ConditionalDefines` empty, exactly as single-file
                    // `dumpAst` does: `FSharpChecker.ParseFile` (the service
                    // parser) then adds the implicit symbols for the file kind
                    // itself — `COMPILED` + `EDITING` for a compiled `.fs`/`.fsi`,
                    // `INTERACTIVE` + `EDITING` for a `.fsx` script. The Rust
                    // differential sweep defines the matching `.fs`/`.fsi` set.
                    // (Forcing a symbol here would instead desync `.fsx` from the
                    // single-file dump.)
                    let parsingOptions =
                        { parsingOptions0 with ConditionalDefines = [] }
                        |> withLangVersion
                    let parseResults =
                        checker.ParseFile(absolute, sourceText, parsingOptions, userOpName = "fcs-dump-ast-batch")
                        |> Async.RunSynchronously
                    let payload =
                        {| Path = path
                           ParseTree = parseResults.ParseTree
                           Diagnostics =
                               parseResults.Diagnostics
                               |> Array.map (fun d ->
                                   {| Severity = d.Severity.ToString()
                                      Message = d.Message
                                      ErrorNumber = d.ErrorNumber
                                      Range = d.Range |})
                           ParseHadErrors = parseResults.ParseHadErrors
                           IsScript = isScript |}
                    JsonSerializer.Serialize(payload, options)
                with ex ->
                    JsonSerializer.Serialize({| Path = path; Error = ex.Message |}, options)
            Console.Out.WriteLine(json)
        line <- Console.In.ReadLine()

/// Batch AST dump driver. Runs [`dumpAstBatchCore`] on a thread with a large
/// stack: `System.Text.Json` recurses ~5 native frames per JSON nesting level,
/// and the corpus contains files whose ASTs nest ~1000 levels deep — enough to
/// overflow the default 1 MB stack and kill the whole batch (an uncatchable
/// `StackOverflowException`). 512 MB clears that by orders of magnitude.
let private dumpAstBatch () =
    let mutable captured : exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try dumpAstBatchCore ()
                with ex -> captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Parse-only throughput benchmark. Reads file paths from stdin (one per line),
/// loads every file into memory and precomputes its `FSharpParsingOptions`
/// ONCE, then parses the whole set `iterations` times. Per-iteration wall time
/// goes to stderr; a checksum + file count to stdout.
///
/// Only the `ParseFile` loop is timed — file IO and option building happen once
/// up front, outside the timed region — so this measures parsing, not the
/// harness. Mirrors [`dumpAstBatchCore`]'s empty `ConditionalDefines` +
/// implicit per-file-kind symbols so the parsed set matches the Rust-side
/// `parse_bench` example like-for-like.
///
/// `cache` selects what is measured:
///   * `false` — the `FSharpChecker` parse cache is disabled, so every call is
///     a real from-scratch parse (the like-for-like comparison against a Rust
///     parse that has no cache). .NET startup + JIT are paid by iteration 1;
///     iterations 2+ are JIT-warm.
///   * `true` — the parse cache is enabled and `FCS_ParseFileCacheSize` (default
///     2) is raised before the `FSharpChecker` is built so the whole input set
///     stays resident. Iteration 1 populates the cache; iterations 2+ are served
///     from it.
///
///     CAVEAT — a large resident set does NOT measure realistic cache-hit
///     latency. FCS's `MruCache` (`AgedLookup`, InternalCollections.fs) is
///     F#-`list`-backed: every hit runs `FilterAndHold` + `Promote` +
///     `AssignWithStrength`, each an O(resident) list rebuild. So a whole-corpus
///     cached pass is O(n^2) and its per-file time is dominated by MRU
///     bookkeeping, not by handing back a parse. Per-file hit latency measured
///     this way (µs/file): ~10 at 2 files, ~22 at 16, ~39 at 128, ~89 at 1024,
///     ~650 at 6344. FCS's cache is a small working-set cache (default 2), so
///     feed this mode a SMALL working set — the files an editor would keep open
///     — for a representative hit latency (~10–20 µs/file, far below a
///     from-scratch parse). The whole-corpus figure is a scaling probe, not a
///     headline number.
///
/// The `parses=` column reports the number of *real* parse operations that hit
/// the parser this iteration. `FSharpChecker.ActualParseFileCount` increments
/// only on a cache-enabled miss (the uncached branch parses directly without
/// touching it), so the counter is meaningful only under `cache = true`; there
/// it is ~one-per-file on iteration 1 and 0 thereafter, proving the warm
/// iterations are genuine hits rather than silent evictions. Under
/// `cache = false` every non-failing file is parsed, so `parses=` is derived as
/// `files - fails` instead (a flat ~one-per-file every iteration).
let private parseBenchCore (cache: bool) (iterations: int) =
    // Must precede FSharpChecker.Create(): `parseFileCacheSize` is read from the
    // environment when the background compiler's MRU cache is constructed. The
    // default (2) would evict every entry long before the next pass re-requested
    // it, turning a warm-cache sweep into all misses. Raising it keeps the input
    // resident — but note the O(resident) per-hit cost documented above: this is
    // for holding a small working set, not the whole corpus.
    if cache then
        Environment.SetEnvironmentVariable("FCS_ParseFileCacheSize", "1000000")

    let checker = FSharpChecker.Create()

    // Strict UTF-8 decoder: throws on invalid bytes (so a UTF-16 / code-page
    // file is skipped, not silently re-encoded) and does not strip a BOM.
    // Mirrors the Rust side's `String::from_utf8`, so both benchmarks parse the
    // identical file set with byte-identical source rather than diverging on the
    // corpus's handful of non-UTF-8 files.
    let utf8Strict = System.Text.UTF8Encoding(false, true)

    let cases = ResizeArray<string * ISourceText * FSharpParsingOptions>()
    let mutable nonUtf8 = 0
    let mutable ioErr = 0
    let mutable totalBytes = 0L
    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()
        if path <> "" then
            try
                let absolute = Path.GetFullPath path
                let bytes = File.ReadAllBytes(absolute)
                let decoded = utf8Strict.GetString(bytes)
                // Strip a leading UTF-8 BOM (kept by GetString) so FCS parses
                // BOM-free source, as its real consumers do; the Rust side strips
                // the same, keeping the parsed source byte-identical.
                let text =
                    if decoded.Length > 0 && int decoded.[0] = 0xFEFF then
                        decoded.Substring(1)
                    else
                        decoded
                totalBytes <- totalBytes + int64 (System.Text.Encoding.UTF8.GetByteCount text)
                let sourceText = SourceText.ofString text
                let isScript = isScriptPath absolute
                let parsingOptions0, _ =
                    checker.GetParsingOptionsFromCommandLineArgs([ absolute ], isInteractive = isScript)
                let parsingOptions =
                    { parsingOptions0 with ConditionalDefines = [] } |> withLangVersion
                cases.Add(absolute, sourceText, parsingOptions)
            with
            | :? System.Text.DecoderFallbackException -> nonUtf8 <- nonUtf8 + 1
            | _ -> ioErr <- ioErr + 1
        line <- Console.In.ReadLine()

    eprintfn
        "fcs: loaded %d files (%d bytes); skipped %d non-utf8, %d io-err; cache=%b"
        cases.Count
        totalBytes
        nonUtf8
        ioErr
        cache

    // FCS's MruCache is O(resident) per hit (see the doc comment). Past a small
    // working set the per-file hit time is dominated by MRU list bookkeeping, not
    // by handing back a parse, so a large-resident cached run is a scaling probe,
    // not a realistic hit-latency figure.
    if cache && cases.Count > 64 then
        eprintfn
            "fcs: WARNING cache=true with %d resident files — per-hit cost is O(resident); this measures MRU churn, not realistic cache-hit latency (use a small working set)"
            cases.Count

    let mutable checksum = 0L
    for it in 1 .. iterations do
        let parsesBefore = FSharpChecker.ActualParseFileCount
        let sw = System.Diagnostics.Stopwatch.StartNew()
        let mutable local = 0L
        let mutable fails = 0
        for (absolute, sourceText, parsingOptions) in cases do
            try
                let r =
                    checker.ParseFile(absolute, sourceText, parsingOptions, cache = cache, userOpName = "fcs-dump-parse-bench")
                    |> Async.RunSynchronously
                // Touch the result so nothing is optimised away.
                local <- local + int64 r.Diagnostics.Length
                match r.ParseTree with
                | ParsedInput.ImplFile _ -> local <- local + 1L
                | ParsedInput.SigFile _ -> local <- local + 2L
            with _ ->
                // A throwing ParseFile is still counted in files/s and MB/s, so
                // surface it (the Rust side's `panics=` twin): a non-zero count
                // means the reported throughput divides by a time that parsed
                // fewer files, and the run should not be trusted as-is.
                fails <- fails + 1
        sw.Stop()
        // Real parse operations this iteration. ActualParseFileCount tracks only
        // cache-enabled misses, so it is meaningful only when caching; uncached,
        // every non-failing file is genuinely parsed (files - fails).
        let parses =
            if cache then FSharpChecker.ActualParseFileCount - parsesBefore
            else cases.Count - fails
        checksum <- local
        let secs = sw.Elapsed.TotalSeconds
        eprintfn
            "fcs: iter %d/%d  %.3f ms  (%.0f files/s, %.1f MB/s)  parses=%d fails=%d"
            it
            iterations
            sw.Elapsed.TotalMilliseconds
            (float cases.Count / secs)
            ((float totalBytes / 1e6) / secs)
            parses
            fails
    printfn "fcs checksum=%d files=%d" checksum cases.Count

/// [`parseBenchCore`] on a 512 MB-stack thread, exactly as [`dumpAstBatch`]:
/// FCS's recursive-descent parser can nest ~1000 levels deep on real corpus
/// files and would otherwise overflow the default 1 MB stack.
let private parseBench (cache: bool) (iterations: int) =
    let mutable captured : exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try parseBenchCore cache iterations
                with ex -> captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Token-stream dump.
///
/// `withLexFilter = false` returns the raw `lex.fsl` output (one token
/// per logical lexeme). `withLexFilter = true` runs LexFilter on top,
/// which rewrites/inserts virtual tokens for the offside rule, type
/// application disambiguation, etc. — the stream the parser actually
/// consumes.
///
/// `compact = false` emits the JSON payload (`{ WithLexFilter, Tokens }`).
/// `compact = true` emits a plain-text view — one token per line,
/// `Kind\tStartLine:StartCol-EndLine:EndCol` — for eyeballing the stream
/// (offside virtuals included) against our lex-filter without a JSON reader.
/// The tab keeps `cut -f1` yielding the bare kind sequence. Single-file only:
/// the batch path stays JSONL for the Rust harness.
let private dumpTokens (absolute: string) (withLexFilter: bool) (compact: bool) =
    let text = File.ReadAllText(absolute)
    let sourceText = SourceText.ofString text

    // FSharpLexerFlags.Default = Compiling | SkipTrivia | UseLexFilter.
    // Keep SkipTrivia: despite its name, the `skip` parameter in lex.fsl
    // also gates whether compound constructs like strings get aggregated
    // (see lex.fsl ~line 605: `if not skip then STRING_TEXT(...) else
    // singleQuoteString...`). Without it, `"hello"` lexes as three tokens
    // (StringText / Identifier / StringText) instead of one String, which
    // is useless as an oracle. Trivia (whitespace, comments) gets dropped
    // — diff harnesses on the Rust side must filter the same tokens.
    let flags =
        let base' = FSharpLexerFlags.Compiling ||| FSharpLexerFlags.SkipTrivia
        if withLexFilter then base' ||| FSharpLexerFlags.UseLexFilter else base'

    let tokens = ResizeArray<_>()
    FSharpLexer.Tokenize(
        sourceText,
        (fun tok -> tokens.Add({| Kind = tok.Kind.ToString(); Range = tok.Range |})),
        filePath = absolute,
        flags = flags)

    if compact then
        let sb = System.Text.StringBuilder()
        for t in tokens do
            let r = t.Range
            sb
                .Append(t.Kind)
                .Append('\t')
                .Append(r.StartLine)
                .Append(':')
                .Append(r.StartColumn)
                .Append('-')
                .Append(r.EndLine)
                .Append(':')
                .Append(r.EndColumn)
                .Append('\n')
            |> ignore
        Console.Out.Write(sb.ToString())
    else
        let payload =
            {| WithLexFilter = withLexFilter
               Tokens = tokens.ToArray() |}

        let json = JsonSerializer.Serialize(payload, buildOptions ())
        Console.Out.Write(json)
        Console.Out.WriteLine()

/// Batch token dump. Reads file paths from stdin, one per line, and emits one
/// JSON object per file to stdout (JSONL). On per-file failure emits
/// `{Path, Error}` so the consumer can correlate but doesn't abort the batch.
/// Amortises the ~150 ms .NET startup cost over a whole corpus run.
let private dumpTokensBatch (withLexFilter: bool) =
    let options = buildOptionsCompact ()
    let flags =
        let base' = FSharpLexerFlags.Compiling ||| FSharpLexerFlags.SkipTrivia
        if withLexFilter then base' ||| FSharpLexerFlags.UseLexFilter else base'

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()
        if path <> "" then
            let payload =
                try
                    let absolute = Path.GetFullPath path
                    let text = File.ReadAllText(absolute)
                    let sourceText = SourceText.ofString text
                    let tokens = ResizeArray<_>()
                    FSharpLexer.Tokenize(
                        sourceText,
                        (fun tok -> tokens.Add({| Kind = tok.Kind.ToString(); Range = tok.Range |})),
                        filePath = absolute,
                        flags = flags)
                    box {| Path = path
                           WithLexFilter = withLexFilter
                           Tokens = tokens.ToArray() |}
                with ex ->
                    box {| Path = path
                           WithLexFilter = withLexFilter
                           Error = ex.Message |}
            let json = JsonSerializer.Serialize(payload, options)
            Console.Out.WriteLine(json)
        line <- Console.In.ReadLine()

// ============================================================================
// Entities dump (assembly-reader differential test oracle)
// ============================================================================

/// Strip the ECMA-335 backtick-arity suffix from each `/`-separated segment
/// of a type name. Mirrors `strip_arity` in `ecma335_assembly.rs`. Only
/// strips when the suffix consists entirely of digits; a backtick followed
/// by anything else is a legal CLR identifier character.
let private stripAritySuffix (name: string) =
    name.Split('/')
    |> Array.map (fun seg ->
        match seg.LastIndexOf '`' with
        | -1 -> seg
        | i when i < seg.Length - 1
                 && seg.Substring(i + 1) |> Seq.forall System.Char.IsDigit ->
            seg.Substring(0, i)
        | _ -> seg)
    |> String.concat "/"

/// Whether `t` is a *real* by-reference type (`byref<T>` / `inref` / `outref`,
/// i.e. `ref`/`in`/`out`) — one that wraps exactly one referent.
///
/// `FSharpEntity.IsByRef` is broader: FCS's `isByrefTyconRef`
/// (`TypedTreeOps.ExprConstruction.fs`) also returns `true` for the byref-*like*
/// intrinsics `System.TypedReference`, `System.ArgIterator`, and
/// `System.RuntimeArgumentHandle`, none of which is generic (they carry zero
/// type arguments). Those are ordinary named value types on the wire — a
/// `TypedReference` sits in a signature as `ELEMENT_TYPE_TYPEDBYREF`, the other
/// two as `ELEMENT_TYPE_VALUETYPE <token>` — and the Rust reader surfaces them
/// as `System.TypedReference` / `System.ArgIterator` / … with no `byref`
/// wrapper. So the diff must treat only the one-referent case as a byref; the
/// zero-arg intrinsics render as their plain named type (and the byref/out
/// prefix is suppressed). Guarding on the arity also protects the callers'
/// `GenericArguments.[0]` access.
let private isRealByref (t: FSharpType) : bool =
    t.HasTypeDefinition && t.TypeDefinition.IsByRef && t.GenericArguments.Count = 1

/// Render an [`FSharpType`] to the stable string shape used by both sides of
/// the assembly-reader diff (see `crates/assembly/src/test_support.rs`):
///
/// - named types         → `Namespace.Name` with `<T, U>` for generic args
/// - arrays              → `T[]` for rank 1, `T[,]` for higher
/// - byref               → `T&`
/// - generic parameters  → `!T<n>` (type) / `!!M<n>` (method), resolved by
///                          name against the enclosing scope's typar lists.
///                          The Rust side carries positional indices on
///                          [`TypeRef::Var`]; FCS exposes only names on the
///                          public surface, so we look up the index from
///                          the typar's name in the parent's
///                          `GenericParameters`.
///
/// `typeTypars` / `methodTypars` are the enclosing type's and method's
/// formal type parameters respectively. Pass `[]` for both at sites where
/// no typar can appear (e.g. an entity's own name in a metadata position
/// outside any signature); a stray typar in such a context fails loud
/// rather than silently emitting an invalid `!T<n>`.
///
/// FCS uses `+` between an outer/nested type and its leaf; the Rust side
/// uses `/`. We normalise to `/` so the strings agree.
let rec private renderTypeInScope
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (t: FSharpType) =
    // FCS represents `System.Object` through the F# `obj` abbreviation
    // entity (defined in FSharp.Core), and `TryFullName` returns `None`
    // for abbreviation entities (see Symbols.fs:`TryFullName`). Without
    // unwrapping we'd render `obj` instead of `System.Object`, which the
    // Rust side never emits. Strip every layer of abbreviation before
    // continuing — this is also what F#'s display contexts do internally
    // for cross-language consumption.
    if t.IsAbbreviation then
        renderTypeInScope typeTypars methodTypars t.AbbreviatedType
    elif t.IsGenericParameter then
        // FCS exposes the parameter as a named typar (e.g. `'T`); the Rust
        // side carries a positional index plus an `is_method` flag. Resolve
        // by identity, not by name: legal IL allows a method typar to shadow
        // a same-named type typar (`C<T>.M<T>` referencing the outer `!0`),
        // and `FSharpGenericParameter.Equals` preserves the underlying
        // typar identity (via `typarRefEq`), so reference-equality matches
        // the right scope. Method scope is checked first so an actual
        // method-typar reference wins when names also collide.
        let tp = t.GenericParameter
        let findIndex (xs: System.Collections.Generic.IList<FSharpGenericParameter>) =
            let mutable found = None
            let mutable i = 0
            while found.IsNone && i < xs.Count do
                if xs.[i].Equals(tp) then found <- Some i
                i <- i + 1
            found
        match findIndex methodTypars with
        | Some i -> sprintf "!!M%d" i
        | None ->
            match findIndex typeTypars with
            | Some i -> sprintf "!T%d" i
            | None ->
                failwithf "fcs-dump entities: typar `%s` not found in any enclosing scope" tp.Name
    elif t.IsTupleType then
        // An F# tuple is structural in FCS — no `TypeDefinition`, just the
        // element types — but it compiles to `System.Tuple`n` (reference
        // tuples) or `System.ValueTuple`n` (struct tuples), and the Rust
        // side reads that IL shape. Render the compiled form so the two
        // sides agree. Arities beyond 7 nest the tail through an eighth
        // `TRest` argument in IL while FCS keeps the flat element list;
        // that re-nesting is not ported, so fail loud rather than emit a
        // flat rendering the Rust side would never produce.
        let args = t.GenericArguments
        if args.Count > 7 then
            failwithf "fcs-dump entities: tuple arity %d (> 7 nests via TRest; not ported)" args.Count
        let name = if t.IsStructTupleType then "System.ValueTuple" else "System.Tuple"
        let rendered =
            args
            |> Seq.map (renderTypeInScope typeTypars methodTypars)
            |> String.concat ", "
        sprintf "%s<%s>" name rendered
    elif t.HasTypeDefinition then
        let td = t.TypeDefinition
        if td.IsArrayType then
            let rank = td.ArrayRank
            if t.GenericArguments.Count <> 1 then
                failwithf "fcs-dump entities: array type with %d generic args" t.GenericArguments.Count
            let elem = renderTypeInScope typeTypars methodTypars t.GenericArguments.[0]
            let commas = String.replicate (rank - 1) ","
            sprintf "%s[%s]" elem commas
        elif isRealByref t then
            sprintf "%s&" (renderTypeInScope typeTypars methodTypars t.GenericArguments.[0])
        else
            // Prefer TryFullName ("System.Int32"); fall back to DisplayName
            // for entities FCS reports without a full name (anonymous,
            // unresolved). FCS uses `+` between an outer type and its
            // nested leaf — Rust uses `/`, so normalise. This branch also
            // catches the byref-*like* zero-arg intrinsics
            // (`System.TypedReference` / `ArgIterator` / `RuntimeArgumentHandle`)
            // that `isRealByref` deliberately excludes — see its comment.
            let baseName =
                td.TryFullName
                |> Option.defaultWith (fun () -> td.DisplayName)
            let baseName = baseName.Replace('+', '/')
            // ECMA-335 backtick-arity suffix on the *outermost* segment
            // before the first generic arg gets stripped by both sides.
            let baseName = stripAritySuffix baseName
            if t.GenericArguments.Count > 0 then
                let args =
                    t.GenericArguments
                    |> Seq.map (renderTypeInScope typeTypars methodTypars)
                    |> String.concat ", "
                sprintf "%s<%s>" baseName args
            else
                baseName
    else
        failwithf "fcs-dump entities: unrecognised FSharpType: %s" (t.Format(FSharpDisplayContext.Empty))

/// Render an [`FSharpType`] with no enclosing typar scope. Any generic
/// parameter encountered fails loud. Use this only at sites where the type
/// is metadata-position (an entity FQN reference at the top level, for
/// instance) and a typar would indicate a bug.
let private renderType (t: FSharpType) : string =
    let empty : System.Collections.Generic.IList<FSharpGenericParameter> = upcast ResizeArray<_>()
    renderTypeInScope empty empty t

/// Whether an [`FSharpType`] position can meaningfully carry a nullable
/// annotation — used both by [`fcsTypeNullnessSuffix`] here and by the
/// position-level [`resolveFcsPositionNullability`] further down. Treat
/// the type as annotable when it is *not* a value type (reference types,
/// typars, arrays); byref-wrapping is unwrapped before this is called so
/// a `ref string` parameter looks like its referent.
let rec private fcsTypeIsAnnotable (t: FSharpType) : bool =
    if t.IsAbbreviation then
        // See through F# type abbreviations (`int` = `System.Int32`,
        // `string` = `System.String`): annotability is a property of the
        // *underlying* type. Without this an inner abbreviation like the `int`
        // in `int option` is treated as annotable and wrongly decorated `!`,
        // even though `System.Int32` is a value type. The outer-position path
        // (`resolveFcsPositionNullability`) doesn't hit this because it resolves
        // abbreviations before consulting annotability.
        fcsTypeIsAnnotable t.AbbreviatedType
    elif t.IsGenericParameter then
        true
    elif not t.HasTypeDefinition then
        // Tuples / functions / measure / etc — none of these carry the
        // nullable shape, so consult the type-def-bearing path only.
        false
    else
        not t.TypeDefinition.IsValueType

/// Map an [`FSharpType`]'s per-node nullness annotation to the suffix the
/// Rust-side renderer emits. FCS's importer has already run
/// `Nullness.ImportILTypeWithNullness` over the IL type tree by the time
/// we see the `FSharpType`, so `HasNullAnnotation` / `IsNullAmbivalent`
/// hold the *per-position* nullness — equivalent to the per-byte walk
/// the IL-side `walkIlTypeWithNullness` does manually. The result is
/// gated on annotability: value types (non-generic structs,
/// `System.Nullable<T>`, etc.) report no suffix even though the
/// underlying nullness slot may be `KnownWithoutNull`.
let private fcsTypeNullnessSuffix (t: FSharpType) : string =
    if not (fcsTypeIsAnnotable t) then ""
    elif t.HasNullAnnotation then "?"
    else
        // Both remaining states — ambivalent AND `KnownWithoutNull` —
        // render as no suffix. This function only serves F#-*native*
        // (pickle-read) signature positions (IL-imported members go
        // through `walkIlTypeWithNullness`), and for those
        // `KnownWithoutNull` is the pickle's *language-level* default on
        // every F#-declared reference type, present even when the
        // assembly was compiled without the nullness feature. The IL
        // carries no `NullableAttribute` in that (overwhelmingly common)
        // case, so the Rust side correctly reports Oblivious; emitting
        // `!` here would diverge on e.g. the inner `string` of a plain
        // `string option` return. `?` stays: it arises only from an
        // explicit `| null`, which requires the nullness feature and
        // therefore comes with a real IL attribute the Rust side reads.
        // If a `<Nullable>enable</Nullable>` F# fixture ever lands, the
        // `!` mapping must become mode-aware (suffix only when the
        // position carries a real IL annotation).
        ""

/// Recursive sibling of [`renderTypeInScope`] that emits per-node
/// nullability suffixes on *inner* descents (generic args, array
/// elements). The outermost suffix is left to the caller, which composes
/// it from the precedence ladder (`resolveFcsPositionNullability`) — this
/// keeps the outer byte path identical to the Rust-side position-level
/// `nullability` field, and only widens to per-node decoration for the
/// composite type positions phase 4m.3 introduces. Mirrors the IL-side
/// `walkIlTypeWithNullness`.
let rec private renderTypeInScopeWithInnerNullness
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (t: FSharpType) =
    walkFcsType false typeTypars methodTypars t

and private walkFcsType
    (emitSelf: bool)
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (t: FSharpType) =
    let body =
        if t.IsAbbreviation then
            walkFcsType false typeTypars methodTypars t.AbbreviatedType
        elif t.IsGenericParameter then
            let tp = t.GenericParameter
            let findIndex (xs: System.Collections.Generic.IList<FSharpGenericParameter>) =
                let mutable found = None
                let mutable i = 0
                while found.IsNone && i < xs.Count do
                    if xs.[i].Equals(tp) then found <- Some i
                    i <- i + 1
                found
            match findIndex methodTypars with
            | Some i -> sprintf "!!M%d" i
            | None ->
                match findIndex typeTypars with
                | Some i -> sprintf "!T%d" i
                | None ->
                    failwithf "fcs-dump entities: typar `%s` not found in any enclosing scope" tp.Name
        elif t.IsTupleType then
            // Structural tuple → its compiled `System.Tuple`n` /
            // `System.ValueTuple`n` form, mirroring the branch in
            // `renderTypeInScope` (see the comment there for the arity-7
            // `TRest` refusal). Elements are inner positions, so they
            // descend with `emitSelf = true` like any generic argument.
            let args = t.GenericArguments
            if args.Count > 7 then
                failwithf "fcs-dump entities: tuple arity %d (> 7 nests via TRest; not ported)" args.Count
            let name = if t.IsStructTupleType then "System.ValueTuple" else "System.Tuple"
            let rendered =
                args
                |> Seq.map (walkFcsType true typeTypars methodTypars)
                |> String.concat ", "
            sprintf "%s<%s>" name rendered
        elif t.HasTypeDefinition then
            let td = t.TypeDefinition
            if td.IsArrayType then
                let rank = td.ArrayRank
                if t.GenericArguments.Count <> 1 then
                    failwithf "fcs-dump entities: array type with %d generic args" t.GenericArguments.Count
                let elem = walkFcsType true typeTypars methodTypars t.GenericArguments.[0]
                let commas = String.replicate (rank - 1) ","
                sprintf "%s[%s]" elem commas
            elif isRealByref t then
                sprintf "%s&" (walkFcsType true typeTypars methodTypars t.GenericArguments.[0])
            else
                // Also the byref-*like* zero-arg intrinsics (`isRealByref`
                // excludes them): they render as their plain named type here.
                let baseName =
                    td.TryFullName
                    |> Option.defaultWith (fun () -> td.DisplayName)
                let baseName = baseName.Replace('+', '/')
                let baseName = stripAritySuffix baseName
                if t.GenericArguments.Count > 0 then
                    // System.Nullable<T> mirrors the F# compiler's
                    // `isSystemNullable` early-out (`import.fs:270-274`):
                    // the outer wrapper carries no nullness suffix, and
                    // for FCS-surfaced F# types we render the inner `T`
                    // without descending into its nullness annotation.
                    // The IL-side walker, by contrast, recurses into `T`
                    // (matching `import.fs:334`) because Roslyn-emitted
                    // byte[] payloads include bytes for the inner
                    // positions. No F# fixture currently exercises a
                    // non-trivial inner shape here; revisit if one lands.
                    let isNullable =
                        match td.TryFullName with
                        | Some "System.Nullable" -> true
                        | _ -> false
                    let argRenderer =
                        if isNullable then
                            renderTypeInScope typeTypars methodTypars
                        else
                            walkFcsType true typeTypars methodTypars
                    let args =
                        t.GenericArguments
                        |> Seq.map argRenderer
                        |> String.concat ", "
                    sprintf "%s<%s>" baseName args
                else
                    baseName
        else
            failwithf "fcs-dump entities: unrecognised FSharpType: %s" (t.Format(FSharpDisplayContext.Empty))
    if emitSelf then body + fcsTypeNullnessSuffix t else body

/// FCS exposes 4 accessibility predicates (Public/Private/Internal/Protected);
/// ProtectedOr/AndInternal aren't surfaced. For *entities* (types) and F#
/// methods that's the only signal we have, so the projection there stays
/// 4-way; if a future fixture hits the missing cases for an entity we'll
/// need a richer FCS query (TcGlobals + AccessorDomain).
///
/// For IL-imported methods, see [`ilMethodAccessString`] — we read the raw
/// ECMA-335 bits directly via reflection rather than going through this
/// 4-way surface, because the surface collapses `protected ctor` to
/// `Public` and `protected internal` to `Protected`.
let private accessString (a: FSharpAccessibility) =
    if a.IsPublic then "Public"
    elif a.IsPrivate then "Private"
    elif a.IsInternal then "Internal"
    elif a.IsProtected then "Protected"
    else
        failwithf "fcs-dump entities: unhandled FSharpAccessibility (none of Public/Private/Internal/Protected set)"

// ============================================================================
// Attribute-aware access projection for IL-imported methods
// ============================================================================
//
// FCS's public `FSharpAccessibility` surface is lossy for IL-imported
// methods (see `Symbols.fs:Accessibility` for `M`/`C`: it routes through
// `getApproxFSharpAccessibilityOfMember`, which folds
// `Family`/`FamilyOrAssembly`/`Public` together into a 4-way enum then
// applies the `IsProtectedAccessibility` flag — which itself excludes
// constructors, so a `protected` ctor lands in the `IsPublic` bucket and
// `protected internal` methods collapse to `IsProtected`). The raw bits
// are still reachable: the `FSharpMemberOrFunctionOrValue` carries a
// `MethInfo` internally, which for IL methods is `ILMeth(_, ilMethInfo,
// _)`, and `ilMethInfo.RawMetadata.Attributes` is a public BCL
// `System.Reflection.MethodAttributes` flags value that holds the exact
// ECMA-335 `MemberAccessMask` bits.
//
// We reach those via reflection because the relevant FCS types
// (`FSharp.Compiler.Symbols.FSharpMemberOrValData`, `Infos.MethInfo`,
// `Infos.ILMethInfo`, `AbstractIL.IL.ILMethodDef`) are all marked
// `internal`. The `FSharpMemberOrFunctionOrValue.Data` accessor is
// public-on-the-IL but hidden from FCS's `.fsi`, so it's available to
// reflection without the F# type checker noticing.
//
// Reflection is fragile to FCS internal renames; we fail loud on each
// lookup so a regression surfaces as a clean test failure naming the
// missing member rather than as a silently-incorrect Access string.

let private reflectInstanceFlags =
    BindingFlags.GetProperty
    ||| BindingFlags.Public
    ||| BindingFlags.NonPublic
    ||| BindingFlags.Instance

let private getProp (o: obj) (name: string) : obj =
    let t = o.GetType()
    match t.GetProperty(name, reflectInstanceFlags) with
    | null ->
        failwithf "fcs-dump entities: reflection failed to locate property %s on %s" name t.FullName
    | pi ->
        match pi.GetValue(o) with
        | null ->
            failwithf "fcs-dump entities: reflection got null reading %s on %s" name t.FullName
        | v -> v

/// Same as [`getProp`] but tolerates null returns. Use this for F#
/// `option`-typed properties (`None` compiles to a literal `null` for
/// reference-typed payloads, so reading `ILPropertyDef.GetMethod` on a
/// set-only property legitimately yields null) and any other slot
/// documented as nullable in the FCS surface.
let private tryGetProp (o: obj) (name: string) : objnull =
    let t = o.GetType()
    match t.GetProperty(name, reflectInstanceFlags) with
    | null ->
        failwithf "fcs-dump entities: reflection failed to locate property %s on %s" name t.FullName
    | pi -> pi.GetValue(o)

/// Unwrap an `objnull` (typically a DU field or a reflected method
/// return value) into a non-null `obj`. Fails loud on null so callers
/// downstream don't carry a nullness obligation through every reflection
/// step.
let private nonNullObj (o: objnull) : obj =
    match o with
    | null -> failwith "fcs-dump entities: unexpected null obj from reflection"
    | v -> v

/// Read the raw ECMA-335 `MemberAccessMask` bits for an IL-imported
/// method. Returns `None` for non-IL methods (F#-defined values, provided
/// methods, etc.), in which case the caller falls back to the FCS public
/// `Accessibility`.
let private tryReadIlMethodAccess (m: FSharpMemberOrFunctionOrValue) : System.Reflection.MethodAttributes option =
    // `FSharpMemberOrFunctionOrValue.Data` is `member Data = d` in
    // `Symbols.fs` line ~2328. The .fsi opacity hides it from F#
    // consumers but the IL stays public, so a reflection lookup
    // succeeds. Return type is the internal `FSharpMemberOrValData` DU.
    let data = getProp m "Data"
    let dataType = data.GetType().FullName
    // Match the M and C cases; everything else is non-IL by construction.
    let isMethodLike =
        let isProp (name: string) : bool =
            getProp data name :?> bool
        isProp "IsM" || isProp "IsC"
    if not isMethodLike then
        None
    else
        // Both M and C wrap a single `MethInfo` exposed as `Item`.
        let methInfo = getProp data "Item"
        // `MethInfo` is itself a DU with cases FSMeth/ILMeth/...; only
        // ILMeth carries an `ilMethInfo` slot the F# code path here can
        // use. The case discriminator is `IsILMeth`.
        let isIlMeth = getProp methInfo "IsILMeth" :?> bool
        if not isIlMeth then
            None
        else
            let ilMethInfo = getProp methInfo "ilMethInfo"
            let ilMethodDef = getProp ilMethInfo "RawMetadata"
            // `ILMethodDef.Attributes` is the BCL flags type — fully
            // public, so we cast back to it directly. Mask off only the
            // access bits; the caller doesn't care about static/virtual/
            // ... here.
            let attrs = getProp ilMethodDef "Attributes" :?> System.Reflection.MethodAttributes
            Some (attrs &&& System.Reflection.MethodAttributes.MemberAccessMask)

/// Project the masked `MethodAttributes` access bits to the 6-way access
/// string used by both sides of the diff. The Rust normaliser drops
/// `Private`/`Internal`/`ProtectedAndInternal` via
/// `accessible_from_some_fsharp_code`, so we only need to emit the kept
/// half exactly; the dropped half is still rendered (we don't filter on
/// this side — that's the Rust normaliser's job for parity with the
/// Rust-side projection).
let private accessStringFromAttributes (attrs: System.Reflection.MethodAttributes) : string =
    match attrs with
    | System.Reflection.MethodAttributes.Public -> "Public"
    | System.Reflection.MethodAttributes.Private -> "Private"
    | System.Reflection.MethodAttributes.Family -> "Protected"
    | System.Reflection.MethodAttributes.Assembly -> "Internal"
    | System.Reflection.MethodAttributes.FamORAssem -> "ProtectedOrInternal"
    | System.Reflection.MethodAttributes.FamANDAssem -> "ProtectedAndInternal"
    | System.Reflection.MethodAttributes.PrivateScope ->
        // PrivateScope == CompilerControlled in ECMA-335; the Rust-side
        // reader rejects this with `UnsupportedEcmaLayout` since
        // compiler-controlled members aren't reachable from F# source.
        // Match that behaviour on this side so a future fixture hitting
        // it fails loud rather than producing a divergent diff.
        failwith "fcs-dump entities: MethodAttributes.PrivateScope (CompilerControlled) is unsupported"
    | other ->
        failwithf "fcs-dump entities: unrecognised masked MethodAttributes: %A" other

/// Access string for a (possibly IL-imported) member method. For IL
/// methods, reads the raw ECMA-335 bits; for everything else (F# values,
/// provided methods), falls back to the 4-way `Accessibility` surface.
let private memberAccessString (m: FSharpMemberOrFunctionOrValue) : string =
    match tryReadIlMethodAccess m with
    | Some attrs -> accessStringFromAttributes attrs
    | None -> accessString m.Accessibility

// ============================================================================
// IL-imported field projection
// ============================================================================
//
// FCS's public `FSharpEntity.FSharpFields` only emits a `value__` row for
// IL-enum types; for plain IL-imported classes it walks
// `entity.AllFieldsAsList`, which contains only F#-defined fields and is
// therefore empty. The raw IL field rows are still reachable through the
// `FSharpEntity.Entity` accessor (a `TyconRef`, hidden from FCS's `.fsi`
// but public at the IL level) → `IsILTycon` → `ILTyconRawMetadata`
// (`ILTypeDef`) → `Fields.AsList()` (`ILFieldDef list`).
//
// `ILFieldDef` carries `Name` (public), `FieldType: ILType` and
// `Attributes: System.Reflection.FieldAttributes` (BCL flags type — fully
// public). All three are publicly callable through reflection. `ILType`
// is a closed F# DU; we walk it with `FSharpValue.GetUnionFields` and
// render only the cases that show up in IL-field signatures (Value /
// Boxed and the wrapping Array / Byref / Ptr). The remaining cases
// (TypeVar / Modified / FunctionPointer) fail loud so a future fixture
// hitting them surfaces as a clean test failure rather than as a silent
// rendering divergence.

/// Render an [`ILType`] to the same string shape `renderType` produces
/// for [`FSharpType`]. Walked via `FSharp.Reflection` because the DU
/// constructors are internal to FCS.
///
/// `numTypeTypars` is the enclosing type's generic arity. ECMA-335 stores
/// typar references in IL signatures as a flat index space: class-typar
/// indices `[0, numTypeTypars)` come first; method-typar indices follow.
/// FCS's `ILType.TypeVar n` mirrors that layout (see `ilread.fs:2632-2637`,
/// where the reader emits `TypeVar n` for `et_VAR` and `TypeVar (n +
/// numTypars)` for `et_MVAR`). The signature byte that disambiguated them
/// in the binary is gone by the time we see `ILType`, so we recover the
/// distinction by comparing the index against `numTypeTypars`. The Rust
/// side carries the same encoding in [`TypeRef::Var`] and emits `!T<i>`
/// for class typars / `!!M<i>` for method typars.
/// Peel the `ILType.Modified` chain off the front of an IL type, mirroring the
/// Rust-side `classify_mods` (`crates/assembly/src/ecma335_assembly.rs`) and
/// with it ECMA-335 II.7.1.1: an *optional* modifier (`modopt`) may be ignored
/// by a tool that does not understand it, so it is dropped; a *required* one
/// (`modreq`) must be understood, so an unrecognised one fails loud here — the
/// Rust side drops the member, and an oracle that instead emitted it would make
/// the diff disagree.
///
/// `ILType.Modified (required: bool, modifier: ILTypeRef, unmodified: ILType)`.
/// Returns `(readonlyRef, isVolatile, unmodified)`: the two `modreq`s a real
/// compiler emits. Both are position-sensitive (a read-only marker is only
/// meaningful over a byref, `volatile` only on a field), so — exactly as on the
/// Rust side — the caller decides whether what it got is legal where it stands.
let rec private splitIlModifiers (t: obj) : (bool * string) list * obj =
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(t, t.GetType(), bindings)
    if case.Name <> "Modified" then
        ([], t)
    else
        let required = fields.[0] :?> bool
        let modifierName = getProp (nonNullObj fields.[1]) "FullName" :?> string
        let rest, unmodified = splitIlModifiers (nonNullObj fields.[2])
        ((required, modifierName) :: rest, unmodified)

/// Classify the modifier chain: drop the ignorable `modopt`s, recognise the two
/// `modreq`s a real compiler emits, fail loud on any other required one.
let private peelIlModifiers (positionLabel: string) (t: obj) : bool * bool * obj =
    let mods, unmodified = splitIlModifiers t
    let mutable readonlyRef = false
    let mutable isVolatile = false
    for required, name in mods do
        if required then
            match name with
            | "System.Runtime.InteropServices.InAttribute" -> readonlyRef <- true
            | "System.Runtime.CompilerServices.IsVolatile" -> isVolatile <- true
            | other ->
                failwithf
                    "fcs-dump entities: unrecognised required custom modifier `%s` at %s"
                    other positionLabel
    (readonlyRef, isVolatile, unmodified)

let rec private renderIlTypeInScope (numTypeTypars: int) (t: obj) : string =
    let unionType = t.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(t, unionType, bindings)
    match case.Name with
    | "Void" -> "System.Void"
    | "Value"
    | "Boxed" ->
        // Both wrap a single `ILTypeSpec`. ILTypeSpec carries an
        // `Enclosing : string list` plus a leaf `Name` (with the
        // namespace folded into the first enclosing entry when nested,
        // or into the leaf when non-nested). Join with `/` to match the
        // Rust normaliser's nested-type encoding; strip the ECMA-335
        // backtick-arity suffix per segment.
        let tspec = nonNullObj fields.[0]
        let typeRef = getProp tspec "TypeRef"
        let enclosing =
            getProp typeRef "Enclosing" :?> System.Collections.IEnumerable
            |> Seq.cast<string>
            |> Seq.toList
        let leafName = getProp typeRef "Name" :?> string
        let joined = String.concat "/" (enclosing @ [ leafName ])
        let baseName = stripAritySuffix joined
        let genericArgs =
            getProp tspec "GenericArgs" :?> System.Collections.IEnumerable
            |> Seq.cast<obj>
            |> Seq.toList
        if List.isEmpty genericArgs then
            baseName
        else
            sprintf "%s<%s>" baseName (genericArgs |> List.map (renderIlTypeInScope numTypeTypars) |> String.concat ", ")
    | "Array" ->
        let shape = nonNullObj fields.[0]
        let rank = getProp shape "Rank" :?> int
        let elem = renderIlTypeInScope numTypeTypars (nonNullObj fields.[1])
        sprintf "%s[%s]" elem (String.replicate (rank - 1) ",")
    | "Byref" -> sprintf "%s&" (renderIlTypeInScope numTypeTypars (nonNullObj fields.[0]))
    | "Ptr" -> sprintf "%s*" (renderIlTypeInScope numTypeTypars (nonNullObj fields.[0]))
    | "TypeVar" ->
        let idx = int (fields.[0] :?> uint16)
        if idx < numTypeTypars then
            sprintf "!T%d" idx
        else
            sprintf "!!M%d" (idx - numTypeTypars)
    | "Modified" ->
        // Peel per II.7.1.1 (see `peelIlModifiers`), then render what is left.
        // A read-only byref (`modreq(InAttribute)` over `Byref`) renders
        // `readonly T&`, matching `render_type` on the Rust side; `volatile` is
        // a field-level marker with no place in a type rendering, and reaching
        // it here means it sat somewhere the Rust projector refuses.
        let readonlyRef, isVolatile, unmodified = peelIlModifiers "an IL type" t
        if isVolatile then
            failwith "fcs-dump entities: `volatile` modifier outside a field type"
        let innerCase, _ =
            FSharpValue.GetUnionFields(
                unmodified,
                unmodified.GetType(),
                BindingFlags.Public ||| BindingFlags.NonPublic
            )
        if readonlyRef && innerCase.Name <> "Byref" then
            failwith "fcs-dump entities: read-only-ref modifier (`modreq(InAttribute)`) not over a byref"
        let rendered = renderIlTypeInScope numTypeTypars unmodified
        if readonlyRef then sprintf "readonly %s" rendered else rendered
    | "FunctionPointer" ->
        failwith "fcs-dump entities: IL field of function-pointer type (ILType.FunctionPointer) not supported"
    | other ->
        failwithf "fcs-dump entities: unhandled IL type case '%s'" other

/// Render an [`ILType`] outside any generic scope. Any `TypeVar` encountered
/// fails loud — use [`renderIlTypeInScope`] when the signature can legally
/// reference type-parameters.
let private renderIlType (t: obj) : string = renderIlTypeInScope 0 t

/// Project an [`ILFieldDef`]'s access mask to the 6-way string used by
/// both sides of the diff. ECMA-335 packs the access into
/// `FieldAttributes.FieldAccessMask`; the numeric layout matches
/// `MethodAttributes.MemberAccessMask` but the type differs, so we mask
/// against `FieldAttributes` constants explicitly.
let private fieldAccessStringFromAttributes (attrs: System.Reflection.FieldAttributes) : string =
    match attrs &&& System.Reflection.FieldAttributes.FieldAccessMask with
    | System.Reflection.FieldAttributes.Public -> "Public"
    | System.Reflection.FieldAttributes.Private -> "Private"
    | System.Reflection.FieldAttributes.Family -> "Protected"
    | System.Reflection.FieldAttributes.Assembly -> "Internal"
    | System.Reflection.FieldAttributes.FamORAssem -> "ProtectedOrInternal"
    | System.Reflection.FieldAttributes.FamANDAssem -> "ProtectedAndInternal"
    | System.Reflection.FieldAttributes.PrivateScope ->
        // PrivateScope == CompilerControlled in ECMA-335; the Rust-side
        // reader rejects this with `UnsupportedEcmaLayout`. Match here
        // so a future fixture hitting it fails loud rather than
        // silently collapsing to "Public".
        failwith "fcs-dump entities: FieldAttributes.PrivateScope (CompilerControlled) is unsupported"
    | other ->
        failwithf "fcs-dump entities: unrecognised masked FieldAttributes: %A" other

/// If `e` is backed by IL metadata, return its [`ILTypeDef`] (as an
/// untyped `obj` — `ILTypeDef` is internal to FCS, so we deal with it
/// only through reflection). Returns `None` for F#-defined entities,
/// whose fields are reachable through the public `FSharpFields` surface.
let private tryGetIlTypeDef (e: FSharpEntity) : obj option =
    // `FSharpEntity.Entity` returns the underlying `TyconRef`. The
    // `.fsi` hides the accessor from F# consumers but the IL stays
    // public, so a reflection lookup succeeds. `TyconRef.IsILTycon`
    // and `TyconRef.ILTyconRawMetadata` delegate through `Deref` so we
    // can call them on the ref directly.
    let tyconRef = getProp e "Entity"
    let isIlTycon = getProp tyconRef "IsILTycon" :?> bool
    if isIlTycon then Some (getProp tyconRef "ILTyconRawMetadata") else None

/// Read the [`ILFieldDef`] rows on an [`ILTypeDef`]. `Fields.AsList()`
/// is internal to FCS but the IL stays public; invoke it through
/// reflection so we get the same enumeration FCS itself uses for
/// IL-enum projection (`Symbols.fs:FSharpFields`).
let private ilTypeDefFields (ilTypeDef: obj) : obj list =
    let fields = getProp ilTypeDef "Fields"
    let asList =
        let t = fields.GetType()
        match t.GetMethod("AsList", reflectInstanceFlags) with
        | null ->
            failwithf "fcs-dump entities: reflection failed to locate ILFieldDefs.AsList on %s" t.FullName
        | mi -> nonNullObj (mi.Invoke(fields, [||]))
    asList :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// Mirror FCS's `AccessibleFromSomeFSharpCode` predicate for fields.
/// The diff oracle compares only what both sides can observe; the Rust
/// normaliser drops `Private` / `Internal` / `ProtectedAndInternal` on
/// its side, and the FCS-side public surface (`MembersFunctionsAndValues`,
/// the `FSharpFields` path for F# records) already filters these out.
/// Our raw IL walk doesn't, so apply the same filter here to keep the
/// JSON contract honest — see the comment on `fixture_my_lib_json` in
/// `tests/assembly_diff.rs`.
let private ilFieldAccessibleFromSomeFSharpCode (attrs: System.Reflection.FieldAttributes) : bool =
    match attrs &&& System.Reflection.FieldAttributes.FieldAccessMask with
    | System.Reflection.FieldAttributes.Public
    | System.Reflection.FieldAttributes.Family
    | System.Reflection.FieldAttributes.FamORAssem -> true
    | System.Reflection.FieldAttributes.Private
    | System.Reflection.FieldAttributes.Assembly
    | System.Reflection.FieldAttributes.FamANDAssem
    | System.Reflection.FieldAttributes.PrivateScope -> false
    | other ->
        failwithf "fcs-dump entities: unrecognised masked FieldAttributes: %A" other

/// Probe an IL row's `CustomAttrs : ILAttributes` slot for a custom
/// attribute whose constructor's declaring type matches `fullName`. Used
/// by [`projectIlField`] and [`projectIlProperty`] to detect C# 11's
/// `[RequiredMemberAttribute]` on the raw IL surface — the FCS-side
/// `FSharpAttribute` collection that [`hasAttributeByFullName`] reads is
/// only available on entities and members, not on `ILFieldDef` /
/// `ILPropertyDef`.
///
/// `ILAttributes` is a struct (`[<Struct>]` in `il.fsi`); when we read
/// it with `getProp` (which calls `PropertyInfo.GetValue`) it is boxed,
/// and the `AsArray()` call below returns a fresh `ILAttribute[]` so
/// there's no aliasing concern. Each `ILAttribute` is a DU
/// (`Encoded`/`Decoded`) but both cases carry the constructor's
/// `Method : ILMethodSpec` — reading the declaring `ILTypeRef.FullName`
/// avoids having to special-case the DU shape.
let private hasIlAttributeByFullName (fullName: string) (ilCarrier: obj) : bool =
    let attrs = getProp ilCarrier "CustomAttrs"
    let asArray =
        let t = attrs.GetType()
        match t.GetMethod("AsArray", System.Type.EmptyTypes) with
        | null ->
            failwithf "fcs-dump entities: ILAttributes.AsArray() not found on %s" t.FullName
        | mi -> mi
    let arr = nonNullObj (asArray.Invoke(attrs, [||])) :?> System.Array
    let mutable i = 0
    let mutable found = false
    while not found && i < arr.Length do
        let attr = nonNullObj (arr.GetValue(i))
        let method = getProp attr "Method"
        let methodRef = getProp method "MethodRef"
        let declTypeRef = getProp methodRef "DeclaringTypeRef"
        let typeName = getProp declTypeRef "FullName" :?> string
        if typeName = fullName then found <- true
        i <- i + 1
    found

let private hasIlRequiredMemberAttribute (ilCarrier: obj) =
    hasIlAttributeByFullName
        "System.Runtime.CompilerServices.RequiredMemberAttribute"
        ilCarrier

// `projectIlField` lives further down (just above `projectIlProperty`)
// because it depends on the phase-4m.2 nullability helpers
// (`resolveIlPositionNullability` / `nullabilitySuffix`) defined
// alongside the IL attribute decoders.

// ============================================================================
// IL-imported property projection
// ============================================================================
//
// FCS's public `FSharpMemberOrFunctionOrValue.IsProperty` surface for
// IL-imported types is incomplete for the diff oracle: per
// `Infos.fs:GetterAccessibility` / `SetterAccessibility` (lines ~1900-1922
// in the F# compiler), an `ILProp`'s accessor accessibility is hard-coded
// to `Public`. That collapses `protected internal int X { get; set; }`
// and `public int X { get; set; }` to the same surface, which would
// silently agree with neither projector on a fixture that exercises a
// non-public property.
//
// The fix is the same reflection trick the method-access path uses
// (`tryReadIlMethodAccess` above), but applied to the raw IL surface
// for properties: walk `ILTypeDef.Properties.AsList()` directly and read
// each accessor's `MethodAttributes` by looking it up against
// `ILTypeDef.Methods.AsList()` by name.

/// Read the [`ILGenericParameterDef`] rows on an [`ILTypeDef`].
/// `ILTypeDef.GenericParams` is a plain `ILGenericParameterDef list` —
/// already a materialised list, so we read it directly rather than going
/// through an `AsList()` shim like the multi-map collections do.
let private ilTypeDefGenericParams (ilTypeDef: obj) : obj list =
    getProp ilTypeDef "GenericParams" :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// Read the [`ILGenericParameterDef`] rows on an [`ILMethodDef`]. Same
/// shape as [`ilTypeDefGenericParams`] — `GenericParams` is an inline
/// `ILGenericParameterDef list`.
let private ilMethodDefGenericParams (ilMethodDef: obj) : obj list =
    getProp ilMethodDef "GenericParams" :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// Project an [`ILGenericParameterDef`] (as untyped `obj`) to the JSON
/// shape the Rust normaliser reads (see `NormalisedGenericParameter` in
/// `crates/assembly/src/test_support.rs`):
///
/// ```json
/// { "Declaration": "out T", "Constraints": ["class", "new()", "..."] }
/// ```
///
/// `numTypeTypars` is the enclosing type's generic arity, threaded into
/// the [`ILType`] renderer so a constraint that itself references a typar
/// (e.g. `where U : IComparable<T>` would carry an `IComparable<!T0>`
/// constraint) projects correctly.
///
/// FCS's `ILGenericParameterDef` carries everything we need on its public
/// surface (`il.fs:1916-1927`): `Name`, `Variance` (DU
/// `NonVariant`/`CoVariant`/`ContraVariant`), the three boolean special
/// constraints (`HasReferenceTypeConstraint` / `HasNotNullableValueTypeConstraint`
/// / `HasDefaultConstructorConstraint`), the typed constraint list
/// `Constraints : ILTypes`, and `CustomAttrs : ILAttributes` for typar-level
/// attribute markers like `IsUnmanagedAttribute`. The C# 13 / F# 9
/// `allows ref struct` anti-constraint is read off `HasAllowsRefStruct`
/// (the `AllowByRefLike` `0x0020` flag bit) and emitted as the additive
/// `allows ref struct` constraint token, mirroring
/// `TypeParameter::allows_ref_struct` on the Rust side.
///
/// `unmanaged` is emitted as an *additive* constraint token alongside
/// `struct`: in IL the unmanaged constraint sets both the value-type bit
/// AND `IsUnmanagedAttribute` on the typar, so both tokens land in the
/// constraint set. Mirrors `TypeParameter::is_unmanaged` on the Rust side.

/// Returns `true` iff `t` is the F# IL `Boxed(System.ValueType)` shape —
/// the unparameterised reference to `System.ValueType`. `System.ValueType`
/// is a reference type in IL (it's the base class of every value type,
/// but ITSELF a class), so the F# IL surface represents it as
/// `ILType.Boxed(...)` rather than `ILType.Value(...)`. Used by
/// [`isUnmanagedModreqOnValueType`] to validate the canonical
/// `where T : unmanaged` constraint shape.
let private isBoxedSystemValueType (t: obj) : bool =
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(t, t.GetType(), bindings)
    if case.Name <> "Boxed" then
        false
    else
        let tspec = nonNullObj fields.[0]
        let typeRef = getProp tspec "TypeRef"
        let fullName = getProp typeRef "FullName" :?> string
        let genericArgsCount =
            getProp tspec "GenericArgs" :?> System.Collections.IEnumerable
            |> Seq.cast<obj>
            |> Seq.length
        fullName = "System.ValueType" && genericArgsCount = 0

/// Returns `true` iff `t` is exactly the C#/F# compilers' canonical
/// encoding of `where T : unmanaged` — an `ILType.Modified` with
/// `req = true`, modifier `System.Runtime.InteropServices.UnmanagedType`,
/// and underlying type `Boxed(System.ValueType)`. Anything else (a bare
/// `Modified`, a `modreq` of a different modifier, a `modreq` applied to
/// a non-`System.ValueType` constraint) returns `false` and the caller
/// falls through to `renderIlTypeInScope`, which fails loud on
/// unsupported `Modified` shapes — matches the Rust side's refuse-loud
/// arm in `project_generic_parameter`.
let private isUnmanagedModreqOnValueType (t: obj) : bool =
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(t, t.GetType(), bindings)
    if case.Name <> "Modified" then
        false
    else
        let req = fields.[0] :?> bool
        let modifier = nonNullObj fields.[1]
        let modifierFullName = getProp modifier "FullName" :?> string
        let unmodified = nonNullObj fields.[2]
        req
        && modifierFullName = "System.Runtime.InteropServices.UnmanagedType"
        && isBoxedSystemValueType unmodified

/// Find the ILAttribute on `ilCarrier`'s `CustomAttrs` whose constructor's
/// declaring type FullName equals `fullName`. Returns the raw ILAttribute
/// (as `obj`) or `None`. Refuses loud if more than one matching attribute
/// is present on the carrier — mirrors the Rust side's
/// `read_nullable_byte_attribute` duplicate guard so a hand-rolled assembly
/// can't smuggle a second decoded payload past the diff oracle.
let private tryFindIlAttributeByFullName
    (fullName: string)
    (ilCarrier: obj)
    (carrierLabel: string)
    : obj option
    =
    let attrs = getProp ilCarrier "CustomAttrs"
    let asArray =
        let t = attrs.GetType()
        match t.GetMethod("AsArray", System.Type.EmptyTypes) with
        | null ->
            failwithf "fcs-dump entities: ILAttributes.AsArray() not found on %s" t.FullName
        | mi -> mi
    let arr = nonNullObj (asArray.Invoke(attrs, [||])) :?> System.Array
    let mutable hits: obj list = []
    for i in 0 .. arr.Length - 1 do
        let attr = nonNullObj (arr.GetValue(i))
        let method = getProp attr "Method"
        let methodRef = getProp method "MethodRef"
        let declTypeRef = getProp methodRef "DeclaringTypeRef"
        let typeName = getProp declTypeRef "FullName" :?> string
        if typeName = fullName then
            hits <- attr :: hits
    match hits with
    | [] -> None
    | [ a ] -> Some a
    | _ ->
        failwithf
            "fcs-dump entities: %s on %s appears more than once on the same carrier"
            fullName
            carrierLabel

/// Pull the raw `byte[]` blob out of an `ILAttribute.Encoded(_, data, _)`
/// case. FCS's metadata reader populates the `Encoded` case with the
/// raw bytes but does NOT eagerly decode them — `ILAttribute.Elements`
/// reads the `elements: ILAttribElem list` slot, which is left empty by
/// the reader. Decoding requires either a call to the `internal`
/// `decodeILAttribData` (not accessible to consumers) or parsing the blob
/// ourselves.
///
/// For our well-known nullable shapes the blob format is fully
/// specified by ECMA-335 II.23.3 — see [`decodeNullableByteBlob`]. We
/// only need the raw bytes here.
///
/// Returns `None` for `Decoded` attributes (defensive — we'd then read
/// `fixedArgs` instead, but that path isn't reached by anything the
/// metadata reader produces).
let private tryGetEncodedAttributeBytes (attr: obj) : byte[] option =
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(attr, attr.GetType(), bindings)
    match case.Name with
    | "Encoded" -> Some(nonNullObj fields.[1] :?> byte[])
    | _ -> None

/// Decode the well-known "single byte" or "byte[]" shape used by the
/// `Nullable*` family of attributes. The blob format (ECMA-335 II.23.3)
/// is:
///   - 2-byte prolog: `0x01 0x00`
///   - fixed args, in declared order
///   - 2-byte little-endian named-args count (always `0x00 0x00` for
///     the nullable attributes the C# compiler emits)
///
/// For `[NullableAttribute(byte)]` and `[NullableContextAttribute(byte)]`
/// the single fixed arg is one raw byte → total blob is 5 bytes.
///
/// For `[NullableAttribute(byte[])]` the single fixed arg is a 4-byte
/// little-endian length followed by `N` raw bytes, optionally with
/// `0xFFFFFFFF` for the null-array case.
///
/// `Single b` / `Vector bs` express which case we saw; everything else
/// (malformed prolog, leftover bytes, non-empty named args) is refused
/// loud so the diff can't tip over into silent agreement on a malformed
/// payload.
type private NullableAttributePayload =
    | Single of byte
    | Vector of byte[] option

let private decodeNullableByteBlob (label: string) (data: byte[]) : NullableAttributePayload =
    if data.Length < 4 then
        failwithf
            "fcs-dump entities: %s blob is too short (%d bytes; need prolog + named-args count)"
            label
            data.Length
    if data.[0] <> 0x01uy || data.[1] <> 0x00uy then
        failwithf
            "fcs-dump entities: %s blob has malformed prolog 0x%02x 0x%02x (expected 0x01 0x00)"
            label
            (int data.[0])
            (int data.[1])
    // Named args count occupies the trailing two bytes (LE uint16).
    let namedCount =
        (int data.[data.Length - 2])
        ||| ((int data.[data.Length - 1]) <<< 8)
    if namedCount <> 0 then
        failwithf
            "fcs-dump entities: %s blob carries %d named arg(s) (expected 0)"
            label
            namedCount
    let fixedArgsBytes = data.Length - 4
    if fixedArgsBytes = 1 then
        Single data.[2]
    elif fixedArgsBytes >= 4 then
        // 4-byte LE length prefix. 0xFFFFFFFF encodes a null array.
        let len =
            (uint32 data.[2])
            ||| ((uint32 data.[3]) <<< 8)
            ||| ((uint32 data.[4]) <<< 16)
            ||| ((uint32 data.[5]) <<< 24)
        if len = 0xFFFFFFFFu then
            if fixedArgsBytes <> 4 then
                failwithf
                    "fcs-dump entities: %s blob declares null-array but carries %d trailing bytes"
                    label
                    (fixedArgsBytes - 4)
            Vector None
        else
            let n = int len
            if fixedArgsBytes <> 4 + n then
                failwithf
                    "fcs-dump entities: %s byte[] blob length-prefix %d disagrees with payload \
                     size %d"
                    label
                    n
                    (fixedArgsBytes - 4)
            let bs = Array.sub data 6 n
            Vector(Some bs)
    else
        failwithf
            "fcs-dump entities: %s blob carries %d fixed-arg bytes (expected 1 for byte form or \
             >=4 for byte[] form)"
            label
            fixedArgsBytes

let private validateNullableByte (label: string) (b: byte) : byte =
    if b > 2uy then
        failwithf
            "fcs-dump entities: %s carries byte value %d (expected 0/1/2)"
            label
            (int b)
    b

/// Per-node nullability source: the payload of `[NullableAttribute]` in
/// one of its two shapes. The scalar form (or the equivalent length-1
/// vector) broadcasts the same byte to every annotable position the
/// walker visits. The vector form supplies one byte per annotable
/// position in pre-order DFS over the type tree (matches the F#
/// compiler's `Nullness.ImportILTypeWithNullness` walk in
/// `dotnet/fsharp/src/Compiler/Checking/import.fs`).
type private NullableByteSource =
    | NullScalar of byte
    | NullVector of byte[]

/// Length of a [`NullableByteSource`] in the vector sense — useful for
/// the post-walk "did we consume the whole vector?" assertion. A scalar
/// reports `1` (every read returns the same byte; the walker only consumes
/// once-per-position anyway).
let private nullableSourceLength (source: NullableByteSource) : int =
    match source with
    | NullScalar _ -> 1
    | NullVector bs -> bs.Length

/// Consume one byte from `source` at position `idx`, advancing `idx`.
/// Scalar form returns the same byte for every index. Vector form fails
/// loud if the walker would index past the supplied bytes — that's a
/// structural mismatch between Roslyn's emission and our walker.
let private consumeNullableByte
    (source: NullableByteSource)
    (idx: int ref)
    (label: string)
    : byte
    =
    let i = !idx
    idx := i + 1
    match source with
    | NullScalar b -> b
    | NullVector bs ->
        if i >= bs.Length then
            failwithf
                "fcs-dump entities: %s — NullableAttribute byte[] payload exhausted \
                 (the pre-order walk demands more bytes than supplied)"
                label
        bs.[i]

/// Decode the payload of `[NullableAttribute]` into a [`NullableByteSource`].
/// Bytes outside `0`/`1`/`2` are refused loud, matching the Rust-side
/// guard. A length-1 vector collapses to scalar (broadcast equivalence —
/// the F# walker uses `Idx = 0` for all reads when length 1). A null /
/// length-0 vector returns `None` — treat as "attribute absent" and let
/// the precedence ladder apply the scope default.
let private tryReadIlNullableByteSource
    (positionLabel: string)
    (ilCarrier: obj)
    : NullableByteSource option
    =
    match
        tryFindIlAttributeByFullName
            "System.Runtime.CompilerServices.NullableAttribute"
            ilCarrier
            positionLabel
    with
    | None -> None
    | Some attr ->
        let label = sprintf "NullableAttribute on %s" positionLabel
        match tryGetEncodedAttributeBytes attr with
        | None ->
            failwithf
                "fcs-dump entities: %s is in unexpected Decoded form (metadata-loaded \
                 attributes should be Encoded)"
                label
        | Some bytes ->
            match decodeNullableByteBlob label bytes with
            | Single b -> Some(NullScalar(validateNullableByte label b))
            | Vector None -> None
            | Vector(Some bs) ->
                let validated = bs |> Array.map (validateNullableByte label)
                match validated.Length with
                | 0 -> None
                | 1 -> Some(NullScalar validated.[0])
                | _ -> Some(NullVector validated)

/// Single-byte view of [`tryReadIlNullableByteSource`] — used by callsites
/// that only ever see the scalar form (typar-direct
/// `[NullableAttribute(byte)]`). Refuses the byte[] composite form loud,
/// since the F# walker treats typar position as a single annotable byte
/// (`import.fs::Nullness.evaluateFirstOrderNullnessAndAdvance` consumes
/// exactly one byte for a `TType_var` and the typar carrier never sees
/// the vector form Roslyn reserves for composite type positions).
let private tryReadIlNullableByte (positionLabel: string) (ilCarrier: obj) : byte option =
    match tryReadIlNullableByteSource positionLabel ilCarrier with
    | None -> None
    | Some(NullScalar b) -> Some b
    | Some(NullVector _) ->
        failwithf
            "fcs-dump entities: NullableAttribute on %s uses the byte[] form (per-position \
             vector); only the single-byte form is legal at typar position"
            positionLabel

/// Phase-4m.1 thin wrapper: typar-scoped label for the existing typar
/// call site.
let private readTyparNullableAttributeByte (typarName: string) (ilCarrier: obj) : byte option =
    tryReadIlNullableByte (sprintf "typar `%s`" typarName) ilCarrier

/// Decode the byte payload of `[NullableContextAttribute(byte)]` on a
/// scope (method or type). The context attribute is always single-byte —
/// it's Roslyn's condensed form when many positions in the same scope
/// share the same annotation. Refuse loud on missing/extra payload or
/// invalid byte values, matching the Rust-side
/// `detect_nullable_context_attribute` guards.
let private readNullableContextAttributeByte (scopeLabel: string) (ilCarrier: obj) : byte option =
    match
        tryFindIlAttributeByFullName
            "System.Runtime.CompilerServices.NullableContextAttribute"
            ilCarrier
            scopeLabel
    with
    | None -> None
    | Some attr ->
        let label = sprintf "NullableContextAttribute on %s" scopeLabel
        match tryGetEncodedAttributeBytes attr with
        | None ->
            failwithf
                "fcs-dump entities: %s is in unexpected Decoded form (metadata-loaded \
                 attributes should be Encoded)"
                label
        | Some bytes ->
            match decodeNullableByteBlob label bytes with
            | Single b -> Some(validateNullableByte label b)
            | Vector _ ->
                failwithf
                    "fcs-dump entities: %s uses the byte[] form (only the single-byte form \
                     is legal for this attribute)"
                    label

/// Read the `NullableAttribute(byte)` or `NullableAttribute(byte[])`
/// outer byte from an `FSharpAttribute` collection. Mirrors
/// [`tryReadIlNullableByte`] but works against FCS's *decoded* attribute
/// surface — used for the parameter and return-type positions, which are
/// reached through `FSharpParameter.Attributes` rather than the raw IL
/// row. The byte returned is the *outer* annotation only — inner
/// per-node nullability for composite positions (`List<string?>` etc.)
/// is read from `FSharpType.HasNullAnnotation` / `IsNullAmbivalent`,
/// which FCS's importer has already populated via
/// `Nullness.ImportILTypeWithNullness`.
let private tryReadFcsNullableByte
    (positionLabel: string)
    (attrs: System.Collections.Generic.IList<FSharpAttribute>)
    : byte option
    =
    let matches =
        attrs
        |> Seq.filter (fun a ->
            a.AttributeType.TryFullName = Some "System.Runtime.CompilerServices.NullableAttribute")
        |> Seq.toList
    match matches with
    | [] -> None
    | _ :: _ :: _ ->
        failwithf
            "fcs-dump entities: NullableAttribute on %s appears more than once \
             on the same carrier"
            positionLabel
    | [ a ] ->
        let label = sprintf "NullableAttribute on %s" positionLabel
        let args = a.ConstructorArguments
        if args.Count <> 1 then
            failwithf
                "fcs-dump entities: %s has %d ctor arg(s); expected exactly 1"
                label
                args.Count
        let _, value = args.[0]
        match value with
        | :? byte as b -> Some(validateNullableByte label b)
        | :? (obj[]) as arr ->
            if arr.Length = 0 then None
            else
                match arr.[0] with
                | :? byte as b -> Some(validateNullableByte label b)
                | other ->
                    let tyName =
                        match box other with
                        | null -> "<null>"
                        | x ->
                            match x.GetType().FullName with
                            | null -> x.GetType().Name
                            | n -> n
                    failwithf
                        "fcs-dump entities: %s byte[] element 0 has unexpected runtime type %s"
                        label
                        tyName
        | :? (byte[]) as bs ->
            if bs.Length = 0 then None
            else Some(validateNullableByte label bs.[0])
        | other ->
            let tyName =
                match other with
                | null -> "<null>"
                | x ->
                    match x.GetType().FullName with
                    | null -> x.GetType().Name
                    | n -> n
            failwithf
                "fcs-dump entities: %s ctor arg has unexpected runtime type %s"
                label
                tyName

/// Render a resolved nullable byte as the suffix the Rust-side
/// normaliser emits after a type rendering. The empty string for
/// `None`/`Some 0` keeps the existing pre-4m.1 fixture output stable;
/// `!` and `?` were chosen to mirror the C# user-facing syntax for
/// not-annotated vs annotated reference types.
let private nullabilitySuffix (n: byte option) : string =
    match n with
    | None
    | Some 0uy -> ""
    | Some 1uy -> "!"
    | Some 2uy -> "?"
    | Some b ->
        failwithf
            "fcs-dump entities: internal — unexpected validated nullable byte %d"
            (int b)

/// Whether an [`ILType`] position can meaningfully carry a nullable
/// annotation. Mirrors `type_is_annotable` on the Rust side: value-type
/// primitives, byrefs and pointers are not annotable; reference-type
/// primitives, arrays, typars and named types are. We treat `Boxed` (a
/// reference to a class or boxed value) as annotable and `Value` (a
/// value-type by reference) as not. `Named` value-types reach us as
/// `Value`, so the same caveat the Rust side notes — a real `Named` struct
/// can't be distinguished from a class without resolving the typedef —
/// only bites for the FCS path (see [`fcsTypeIsAnnotable`]).
let private ilTypeIsAnnotable (t: obj) : bool =
    let unionType = t.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, _ = FSharpValue.GetUnionFields(t, unionType, bindings)
    match case.Name with
    | "Boxed" -> true
    | "Array" -> true
    | "TypeVar" -> true
    | "Value" -> false
    | "Byref" -> false
    | "Ptr" -> false
    | "Void" -> false
    | "Modified" -> false
    | "FunctionPointer" -> false
    | _ -> false

/// Apply the precedence ladder for a non-typar position whose attributes
/// live on a raw IL carrier (field, property, event, parameter or return
/// `ParameterMetadata`). Mirrors `resolve_position_nullability` on the
/// Rust side.
let private resolveIlPositionNullability
    (positionLabel: string)
    (ilCarrier: obj)
    (positionType: obj)
    (enclosingContext: byte option)
    : byte option
    =
    match tryReadIlNullableByte positionLabel ilCarrier with
    | Some _ as direct -> direct
    | None ->
        if ilTypeIsAnnotable positionType then
            enclosingContext
        else
            None

/// Apply the precedence ladder for a non-typar position whose attributes
/// live on an `FSharpAttribute` list (parameter / return). The
/// `positionType` is the *referent* — byref unwrapping must happen
/// before this is called, matching the Rust-side gate.
let private resolveFcsPositionNullability
    (positionLabel: string)
    (attrs: System.Collections.Generic.IList<FSharpAttribute>)
    (positionType: FSharpType)
    (enclosingContext: byte option)
    : byte option
    =
    match tryReadFcsNullableByte positionLabel attrs with
    | Some _ as direct -> direct
    | None ->
        if fcsTypeIsAnnotable positionType then
            enclosingContext
        else
            None

/// Examine an [`ILType`] of `Value` shape to see whether it names
/// `System.Nullable``1`. The F# compiler's `Nullness.isSystemNullable`
/// (at `import.fs:270-274`) governs the outer-byte decision here: the
/// outer wrapper consumes no byte (`evaluateFirstOrderNullnessAndAdvance`
/// returns `flags` unchanged at `import.fs:281`), but the inner `T` is
/// still walked normally — the `Boxed | Value` branch at
/// `import.fs:334` unconditionally recurses into `tspec.GenericArgs`.
let private ilTypeSpecIsSystemNullable (tspec: obj) : bool =
    let typeRef = getProp tspec "TypeRef"
    let enclosing =
        getProp typeRef "Enclosing" :?> System.Collections.IEnumerable
        |> Seq.cast<string>
        |> Seq.toList
    let leafName = getProp typeRef "Name" :?> string
    // ECMA-335 emits `System.Nullable` with the `1` arity suffix in
    // its `Name` field. Two encoded shapes appear in practice and the
    // F# importer accepts both (`import.fs:270-274`'s `isSystemNullable`):
    //   `Name="Nullable``1"; Enclosing=["System"]`     — typical
    //   `Name="System.Nullable``1"; Enclosing=[]`     — flattened
    let stripped = stripAritySuffix leafName
    (enclosing = [ "System" ] && stripped = "Nullable")
    || (List.isEmpty enclosing && stripped = "System.Nullable")

/// Walk an [`ILType`] in pre-order DFS, consuming bytes from `source`
/// in lockstep with the annotable positions the walker visits.
/// Produces a rendered string with embedded per-node nullability suffixes
/// (`!`, `?`, or none). Mirrors the F# compiler's
/// `Nullness.ImportILTypeWithNullness` walk
/// (`dotnet/fsharp/src/Compiler/Checking/import.fs:276-360`) and the
/// Rust side's nullness-aware type walk (`walk_nullable_sig`).
///
/// Byte-consumption rules per ECMA-335 II.23.2.12 / Roslyn emission:
///   - `Boxed` (reference to class or boxed value): 1 byte consumed,
///     mapped via `0/1/2 → ""/"!"/"?"`.
///   - `Value` for non-generic value type: 0 bytes; no suffix.
///   - `Value` for generic value type *other than* `System.Nullable`:
///     1 byte consumed and **discarded** (forced to no-suffix);
///     args walked normally. Mirrors `import.fs:282`.
///   - `Value` for `System.Nullable<T>`: 0 bytes for the wrapper itself
///     (matches `isSystemNullable` early-out at `import.fs:281`), but the
///     inner `T` is still walked normally because the importer's
///     `Boxed | Value` branch unconditionally recurses into
///     `tspec.GenericArgs` (`import.fs:334`). Roslyn emits per-position
///     bytes for `T` accordingly.
///   - `Array` / `Vector`: 1 byte consumed for the array itself; element
///     walked normally.
///   - `Byref`: 0 bytes for the wrapper; referent walked normally.
///   - `Ptr` / `FunctionPointer`: pointer interiors are not walked with
///     nullness (mirrors `import.fs:342-348` which recurses without
///     `evaluateFirstOrderNullnessAndAdvance`). The wrapper consumes no
///     byte; the referent is rendered structurally only.
///   - `TypeVar`: 1 byte consumed (typar position is annotable).
let rec private walkIlTypeWithNullness
    (numTypeTypars: int)
    (source: NullableByteSource)
    (idx: int ref)
    (t: obj)
    : string
    =
    let unionType = t.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(t, unionType, bindings)
    let nullByteToSuffix (b: byte) : string =
        match b with
        | 0uy -> ""
        | 1uy -> "!"
        | 2uy -> "?"
        | other -> failwithf "fcs-dump entities: internal — unexpected validated nullable byte %d" (int other)
    match case.Name with
    | "Void" -> "System.Void"
    | "Value"
    | "Boxed" ->
        let tspec = nonNullObj fields.[0]
        let typeRef = getProp tspec "TypeRef"
        let enclosing =
            getProp typeRef "Enclosing" :?> System.Collections.IEnumerable
            |> Seq.cast<string>
            |> Seq.toList
        let leafName = getProp typeRef "Name" :?> string
        let joined = String.concat "/" (enclosing @ [ leafName ])
        let baseName = stripAritySuffix joined
        let genericArgs =
            getProp tspec "GenericArgs" :?> System.Collections.IEnumerable
            |> Seq.cast<obj>
            |> Seq.toList
        let isValueType = case.Name = "Value"
        let isNullable = isValueType && ilTypeSpecIsSystemNullable tspec
        let suffix =
            if isValueType then
                if isNullable then
                    "" // System.Nullable: no byte consumed
                elif List.isEmpty genericArgs then
                    "" // non-generic value type: no byte consumed
                else
                    // generic value type: consume one byte but discard
                    let _ =
                        consumeNullableByte source idx (sprintf "generic value type `%s` (byte discarded)" baseName)
                    ""
            else
                let b = consumeNullableByte source idx (sprintf "reference type `%s`" baseName)
                nullByteToSuffix b
        let renderedArgs =
            // `System.Nullable<T>` only skips the OUTER byte (see
            // `isSystemNullable` early-out at `import.fs:281`); the
            // inner `T` still walks normally because the importer's
            // `Boxed | Value` branch unconditionally recurses into
            // `tspec.GenericArgs` (`import.fs:334`). So a payload like
            // `Nullable<KeyValuePair<string?, int>>` consumes bytes for
            // the inner KVP (discarded), the `string` arg, and the
            // `int` arg (0 bytes) — even though the outer Nullable
            // itself consumes nothing.
            if List.isEmpty genericArgs then
                []
            else
                genericArgs |> List.map (walkIlTypeWithNullness numTypeTypars source idx)
        let withArgs =
            if List.isEmpty renderedArgs then baseName
            else sprintf "%s<%s>" baseName (String.concat ", " renderedArgs)
        sprintf "%s%s" withArgs suffix
    | "Array" ->
        let b = consumeNullableByte source idx "array"
        let suffix = nullByteToSuffix b
        let shape = nonNullObj fields.[0]
        let rank = getProp shape "Rank" :?> int
        let elem = walkIlTypeWithNullness numTypeTypars source idx (nonNullObj fields.[1])
        // The outer-array suffix sits *after* the brackets: `T?[]` is
        // `<T's render><T's suffix><array brackets><array's outer suffix>`.
        // Matches `render_type` on the Rust side, which prints
        // `System.String?[]!` for `string?[]` rather than
        // `System.String?![]`.
        sprintf "%s[%s]%s" elem (String.replicate (rank - 1) ",") suffix
    | "Byref" ->
        // Byref wrapper consumes no byte; the referent walks normally.
        // Callers are expected to have already unwrapped the byref before
        // applying the position-level annotability gate, but emit a
        // tolerant render here in case a future fixture lands a byref in
        // an inner generic position.
        sprintf "%s&" (walkIlTypeWithNullness numTypeTypars source idx (nonNullObj fields.[0]))
    | "Ptr" ->
        // Pointer interiors are not walked with nullness — render the
        // referent structurally to preserve the rendered shape, but do
        // not consume any bytes.
        sprintf "%s*" (renderIlTypeInScope numTypeTypars (nonNullObj fields.[0]))
    | "TypeVar" ->
        let b = consumeNullableByte source idx "typar"
        let suffix = nullByteToSuffix b
        let i = int (fields.[0] :?> uint16)
        let name =
            if i < numTypeTypars then sprintf "!T%d" i
            else sprintf "!!M%d" (i - numTypeTypars)
        sprintf "%s%s" name suffix
    | "Modified" ->
        // A modifier is not an annotable position: FCS threads the flag cursor
        // straight through it (`import.fs:350`), as does Roslyn's encoder, so
        // peeling consumes no byte. The two recognised markers are *outer*-only
        // (the byref-position and field paths peel them before the walk starts),
        // so meeting one here means it sat at a nested position — which the Rust
        // `walk_nullable_sig` refuses. Mirror that.
        let readonlyRef, isVolatile, unmodified =
            peelIlModifiers "a nested type position" (nonNullObj t)
        if readonlyRef || isVolatile then
            failwith
                "fcs-dump entities: recognised custom modifier at a nested type position — unsupported"
        walkIlTypeWithNullness numTypeTypars source idx unmodified
    | "FunctionPointer" ->
        failwith "fcs-dump entities: IL field of function-pointer type (ILType.FunctionPointer) not supported"
    | other ->
        failwithf "fcs-dump entities: unhandled IL type case '%s'" other

/// Returns `true` iff the outermost node of an [`ILType`] consumes a
/// nullable byte (and therefore produces a suffix character) during the
/// walk. Generic value types consume a byte but discard it (forcing the
/// outer to `Oblivious`, suffix empty) — so they read as
/// non-suffix-emitting here. Mirrors the table in [`walkIlTypeWithNullness`].
let private ilTypeOuterEmitsSuffix (positionType: obj) : bool =
    let unionType = positionType.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, _ = FSharpValue.GetUnionFields(positionType, unionType, bindings)
    match case.Name with
    | "Value" -> false // value types: byte may be consumed but the suffix is always empty
    | "Boxed" | "Array" | "TypeVar" -> true
    | _ -> false // Byref, Ptr, FunctionPointer, Modified, Void

/// Top-level entry for rendering an [`ILType`] at a non-typar position
/// (field, property, event, parameter, return) with per-node nullability
/// suffixes embedded. Applies the precedence ladder (direct
/// `[NullableAttribute]` → enclosing `[NullableContextAttribute]` byte
/// → Oblivious) to derive the [`NullableByteSource`], walks the type
/// tree, and asserts the byte vector was fully consumed.
///
/// Returns `(body, outerSuffix)` — most callers just want
/// `body + outerSuffix` (see [`renderIlTypeWithNullness`]). The byref-
/// return rendering path needs them separately so it can re-insert the
/// `&` between body and outer suffix: the Rust-side renders byref
/// returns as `T&{suffix}`, never `T{suffix}&`.
let private renderIlTypeWithNullnessSplit
    (numTypeTypars: int)
    (positionLabel: string)
    (ilCarrier: obj)
    (positionType: obj)
    (enclosingContext: byte option)
    : string * string
    =
    let source =
        match tryReadIlNullableByteSource positionLabel ilCarrier with
        | Some s -> Some s
        | None ->
            // No direct attribute. Broadcast the enclosing scope's
            // `NullableContextAttribute` byte (if any) across every
            // annotable position visited by the walk. The walker
            // itself decides per-node whether to consume a byte, so we
            // can pass the source unconditionally — non-annotable
            // outers (non-generic value types, `System.Nullable<T>`)
            // are correctly handled by the walker without an outer
            // gate, while generic value types correctly propagate the
            // context byte into their reference-typed args.
            match enclosingContext with
            | Some b -> Some(NullScalar b)
            | None -> None
    match source with
    | None ->
        // Nothing to walk. Use the existing renderer for the
        // suffix-free render; outer position is Oblivious / no annotation.
        renderIlTypeInScope numTypeTypars positionType, ""
    | Some s ->
        let idx = ref 0
        let rendered = walkIlTypeWithNullness numTypeTypars s idx positionType
        // Scalar form broadcasts the same byte to every annotable
        // position and never "exhausts"; the vector form must be fully
        // consumed (matches the structural-error refusal on the Rust
        // side).
        match s with
        | NullScalar _ -> ()
        | NullVector bs ->
            if !idx <> bs.Length then
                failwithf
                    "fcs-dump entities: NullableAttribute on %s carries %d byte(s) but the \
                     pre-order walk consumed %d (length mismatch)"
                    positionLabel
                    bs.Length
                    !idx
        // Determine the outer suffix from the position type. The walker
        // appends it at the very end of the rendered body, so we can
        // peel it off by length. The outer byte is byte index 0 of the
        // source (for `Vector`) or the broadcast byte (for `Scalar`).
        let outerByte =
            match s with
            | NullScalar b -> b
            | NullVector bs -> if bs.Length = 0 then 0uy else bs.[0]
        let outerSuffix =
            if ilTypeOuterEmitsSuffix positionType then
                match outerByte with
                | 0uy -> ""
                | 1uy -> "!"
                | 2uy -> "?"
                | _ -> ""
            else
                ""
        if outerSuffix = "" then
            rendered, ""
        else
            rendered.Substring(0, rendered.Length - outerSuffix.Length), outerSuffix

/// Composed form of [`renderIlTypeWithNullnessSplit`] returning
/// `body + outerSuffix`. The default for callers that don't need to
/// re-arrange the byref `&` around the suffix.
let private renderIlTypeWithNullness
    (numTypeTypars: int)
    (positionLabel: string)
    (ilCarrier: obj)
    (positionType: obj)
    (enclosingContext: byte option)
    : string
    =
    let body, suffix =
        renderIlTypeWithNullnessSplit numTypeTypars positionLabel ilCarrier positionType enclosingContext
    body + suffix

/// Whether a position's IL carrier (field, parameter, or method return) carries
/// one of the *attribute* encodings of a read-only byref: `[IsReadOnly]` (C# 7.2
/// `in`, a `ref readonly` field/return) or `[RequiresLocation]` (C# 12
/// `ref readonly` parameter). Mirrors the Rust `has_readonly_ref_attribute`;
/// callers OR it with the `modreq(InAttribute)` they peeled off the signature,
/// and consult it only for a position they have already established is a byref.
let private hasReadonlyRefAttribute (ilCarrier: obj) : bool =
    hasIlAttributeByFullName "System.Runtime.CompilerServices.IsReadOnlyAttribute" ilCarrier
    || hasIlAttributeByFullName "System.Runtime.CompilerServices.RequiresLocationAttribute" ilCarrier

/// Render a byref-capable outer position (a field or property/indexer type)
/// with per-node nullness, returning `(rendered, isVolatile)`. A top-level
/// `ILType.Byref` (`ref T`) renders as `T&{suffix}` — the byref wrapper carries
/// no nullness, so the referent's position suffix sits *after* the `&`, matching
/// the Rust side's `render_type(ByRef(t)) + nullability_suffix` and the
/// byref-*return* path ([`renderReturnType`]). Anything else renders straight
/// through. Without the byref-strip the plain walk would emit `T{suffix}&`
/// (suffix inside the `&`), diverging from the Rust model for a
/// nullable-reference referent (`ref string?` → `System.String&?`, not
/// `System.String?&`).
///
/// Custom modifiers are peeled first ([`peelIlModifiers`]), mirroring the Rust
/// `walk_byref_position`: a `modreq(InAttribute)` over the byref makes it
/// read-only (`readonly T&`, a C# `ref readonly` field/indexer), and a
/// `modreq(IsVolatile)` is handed back to the caller — only a *field* may
/// interpret it, so a property carrying one must fail loud.
let private renderPositionTypeWithByref
    (numTypeTypars: int)
    (positionLabel: string)
    (ilCarrier: obj)
    (positionType: obj)
    (enclosingContext: byte option)
    : string * bool
    =
    let modifierReadonly, isVolatile, positionType = peelIlModifiers positionLabel positionType
    // Read-only-ness has two encodings and the Rust model unions them: the
    // `modreq(InAttribute)` (emitted only where the CLI must match on it — a
    // byref return, the property type mirroring it) and, otherwise, an
    // `[IsReadOnly]` attribute on the position (a `ref readonly` field). See
    // `has_readonly_ref_attribute` in `ecma335_assembly.rs`.
    let readonlyRef = modifierReadonly || hasReadonlyRefAttribute ilCarrier
    let unionType = positionType.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(positionType, unionType, bindings)
    if case.Name = "Byref" then
        let referent = nonNullObj fields.[0]
        let body, suffix =
            renderIlTypeWithNullnessSplit numTypeTypars positionLabel ilCarrier referent enclosingContext
        let prefix = if readonlyRef then "readonly " else ""
        (sprintf "%s%s&%s" prefix body suffix), isVolatile
    else
        if readonlyRef then
            failwithf
                "fcs-dump entities: read-only-ref modifier (`modreq(InAttribute)`) not over a byref at %s"
                positionLabel
        (renderIlTypeWithNullness numTypeTypars positionLabel ilCarrier positionType enclosingContext),
        isVolatile

let private projectIlGenericParameter
    (numTypeTypars: int)
    (contextNullability: byte option)
    (ilGenericParameterDef: obj)
    : objnull
    =
    let name = getProp ilGenericParameterDef "Name" :?> string
    let variance = getProp ilGenericParameterDef "Variance"
    let varianceType = variance.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let varianceCase, _ = FSharpValue.GetUnionFields(variance, varianceType, bindings)
    let declaration =
        match varianceCase.Name with
        | "NonVariant" -> name
        | "CoVariant" -> sprintf "out %s" name
        | "ContraVariant" -> sprintf "in %s" name
        | other ->
            failwithf "fcs-dump entities: unhandled ILGenericVariance case '%s'" other
    let hasClass = getProp ilGenericParameterDef "HasReferenceTypeConstraint" :?> bool
    let hasStruct = getProp ilGenericParameterDef "HasNotNullableValueTypeConstraint" :?> bool
    let hasNew = getProp ilGenericParameterDef "HasDefaultConstructorConstraint" :?> bool
    // The C# 13 / F# 9 `allows ref struct` anti-constraint — the
    // `AllowByRefLike` (`0x0020`) bit. Independent of the other special
    // constraints; mirrors `TypeParameter::allows_ref_struct` on the Rust side.
    let hasAllowsRefStruct = getProp ilGenericParameterDef "HasAllowsRefStruct" :?> bool
    let mutable hasUnmanaged =
        hasIlAttributeByFullName
            "System.Runtime.CompilerServices.IsUnmanagedAttribute"
            ilGenericParameterDef
    let typeConstraints =
        getProp ilGenericParameterDef "Constraints" :?> System.Collections.IEnumerable
        |> Seq.cast<obj>
        |> Seq.choose (fun t ->
            if isUnmanagedModreqOnValueType t then
                hasUnmanaged <- true
                None
            else
                Some(renderIlTypeInScope numTypeTypars t))
        |> Seq.toArray
    // Mirror the Rust-side guard: `unmanaged` is documented as
    // additive on top of `struct`, never standalone. If
    // `IsUnmanagedAttribute` (or the modreq path that folds into
    // `hasUnmanaged`) is present without `HasNotNullableValueTypeConstraint`,
    // the metadata is malformed — refuse loud so the two projectors stay in
    // lockstep on this edge case.
    if hasUnmanaged && not hasStruct then
        failwithf
            "fcs-dump entities: generic parameter `%s` carries the `unmanaged` \
             signal (IsUnmanagedAttribute or modreq(UnmanagedType)) without the \
             value-type special-constraint bit — `unmanaged` is additive on top \
             of `struct`, never standalone"
            name
    // Phase 4m.1: a typar-direct `[NullableAttribute(byte)]` wins; otherwise
    // the enclosing scope's `[NullableContextAttribute(byte)]` (passed in as
    // `contextNullability`) supplies the default. Byte 0 → no token,
    // 1 → `notnull`, 2 → `nullable`. Mirrors the Rust-side
    // `direct.or(context).unwrap_or(Oblivious)` fold.
    let directNullability = readTyparNullableAttributeByte name ilGenericParameterDef
    let effectiveNullability =
        match directNullability with
        | Some b -> Some b
        | None -> contextNullability
    let nullabilityToken =
        match effectiveNullability with
        | Some 1uy -> Some "notnull"
        | Some 2uy -> Some "nullable"
        | _ -> None
    let constraints =
        [|
            if hasClass then "class"
            if hasStruct then "struct"
            if hasNew then "new()"
            if hasUnmanaged then "unmanaged"
            if hasAllowsRefStruct then "allows ref struct"
            match nullabilityToken with
            | Some t -> t
            | None -> ()
            yield! typeConstraints
        |]
    box {| Declaration = declaration; Constraints = constraints |}

/// Look up the [`ILMethodDef`] for an IL-imported
/// [`FSharpMemberOrFunctionOrValue`] via the same reflection trick
/// [`tryReadIlMethodAccess`] uses to reach `RawMetadata`. Returns `None`
/// for non-IL methods (F#-defined, provided, etc.) where the caller falls
/// back to the FCS-public surface.
let private tryGetIlMethodDef (m: FSharpMemberOrFunctionOrValue) : obj option =
    let data = getProp m "Data"
    let isMethodLike =
        let isProp (name: string) : bool = getProp data name :?> bool
        isProp "IsM" || isProp "IsC"
    if not isMethodLike then None
    else
        let methInfo = getProp data "Item"
        let isIlMeth = getProp methInfo "IsILMeth" :?> bool
        if not isIlMeth then None
        else
            let ilMethInfo = getProp methInfo "ilMethInfo"
            Some (getProp ilMethInfo "RawMetadata")

/// Read the IL method-def rows on an [`ILTypeDef`]. Mirrors
/// [`ilTypeDefFields`]; `Methods.AsList()` is internal to FCS but the IL
/// stays public so reflection succeeds.
let private ilTypeDefMethods (ilTypeDef: obj) : obj list =
    let methods = getProp ilTypeDef "Methods"
    let asList =
        let t = methods.GetType()
        match t.GetMethod("AsList", reflectInstanceFlags) with
        | null ->
            failwithf "fcs-dump entities: reflection failed to locate ILMethodDefs.AsList on %s" t.FullName
        | mi -> nonNullObj (mi.Invoke(methods, [||]))
    asList :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// Same shape as [`ilTypeDefFields`], walking `ILTypeDef.Properties.AsList()`.
let private ilTypeDefProperties (ilTypeDef: obj) : obj list =
    let properties = getProp ilTypeDef "Properties"
    let asList =
        let t = properties.GetType()
        match t.GetMethod("AsList", reflectInstanceFlags) with
        | null ->
            failwithf "fcs-dump entities: reflection failed to locate ILPropertyDefs.AsList on %s" t.FullName
        | mi -> nonNullObj (mi.Invoke(properties, [||]))
    asList :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// Same shape as [`ilTypeDefProperties`], walking `ILTypeDef.Events.AsList()`.
/// See `il.fs:ILEventDefs` — the `AsList()` instance method materialises the
/// underlying `LazyOrderedMultiMap` into a flat `ILEventDef list`.
let private ilTypeDefEvents (ilTypeDef: obj) : obj list =
    let events = getProp ilTypeDef "Events"
    let asList =
        let t = events.GetType()
        match t.GetMethod("AsList", reflectInstanceFlags) with
        | null ->
            failwithf "fcs-dump entities: reflection failed to locate ILEventDefs.AsList on %s" t.FullName
        | mi -> nonNullObj (mi.Invoke(events, [||]))
    asList :?> System.Collections.IEnumerable
    |> Seq.cast<obj>
    |> Seq.toList

/// F# `option<'T>` for a reference type `T` compiles to a nullable
/// `FSharpOption<T>`: `None` is the actual `null` reference, `Some x` is
/// an `FSharpOption` instance with a public `Value : T` accessor. Reflect
/// the boxed value back into a plain `obj option` so we can pattern-match
/// in F# code without re-deriving the encoding at every call site.
let private unwrapOption (boxed: objnull) : obj option =
    match boxed with
    | null -> None
    | o -> Some (getProp o "Value")

/// Look up the [`ILMethodDef`] an [`ILMethodRef`] resolves to, against the
/// host [`ILTypeDef`]'s method-def list. The accessor name is usually
/// unique, but overloaded indexers (`this[int]` plus `this[string]`) compile
/// to accessors that share a name (`get_Item`) and differ only in their
/// parameter signature — so when the name alone is ambiguous, disambiguate by
/// the `ILMethodRef`'s `ArgTypes` against each candidate's IL parameter types.
/// This mirrors the Rust side, which binds each property to a specific
/// accessor token via ECMA-335 MethodSemantics rather than by name.
/// `numTypeTypars` is threaded only so both sides of the comparison render
/// generic positions consistently (the absolute rendering is irrelevant — we
/// compare ref-against-candidate, both rendered the same way).
let private lookupAccessor (numTypeTypars: int) (methods: obj list) (methodRef: obj) : obj =
    let refName = getProp methodRef "Name" :?> string
    let candidates =
        methods
        |> List.filter (fun m -> (getProp m "Name" :?> string) = refName)
    match candidates with
    | [ m ] -> m
    | [] ->
        failwithf "fcs-dump entities: property accessor `%s` not found in ILTypeDef.Methods" refName
    | many ->
        let refArgs =
            getProp methodRef "ArgTypes" :?> System.Collections.IEnumerable
            |> Seq.cast<obj>
            |> Seq.map (renderIlTypeInScope numTypeTypars)
            |> Seq.toList
        let paramTypes (m: obj) =
            getProp m "Parameters" :?> System.Collections.IEnumerable
            |> Seq.cast<obj>
            |> Seq.map (fun p -> renderIlTypeInScope numTypeTypars (getProp p "Type"))
            |> Seq.toList
        match many |> List.filter (fun m -> paramTypes m = refArgs) with
        | [ m ] -> m
        | [] ->
            failwithf
                "fcs-dump entities: %d methods named `%s` but none with the accessor signature [%s]"
                (List.length many) refName (String.concat ", " refArgs)
        | _ ->
            failwithf
                "fcs-dump entities: %d methods named `%s` with identical signatures — accessor lookup is ambiguous"
                (List.length many) refName

/// Validate that a property accessor's return-type signature carries
/// nothing the model can't represent. The motivating case is C# 9 `init`
/// accessors: the required `IsExternalInit` modreq lives on the *setter
/// method's* return type, not on the property's `PropertyType`, so the
/// existing `renderIlType propType` check doesn't see it. Mirrors the
/// Rust-side `project_method` walk that gets re-applied to accessors —
/// without this, an init setter would slip through and be modelled as a
/// regular `set` while the Rust side correctly rejected.
let private validateAccessorReturnType (propName: string) (kind: string) (ilMethodDef: obj) =
    let return_ = getProp ilMethodDef "Return"
    let retType = getProp return_ "Type"
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, fields = FSharpValue.GetUnionFields(retType, retType.GetType(), bindings)
    match case.Name with
    | "Modified" ->
        // `ILType.Modified (required, modifier: ILTypeRef, unmodified: ILType)`.
        // Two accessor shapes carry one legitimately, and the Rust side projects
        // exactly those two:
        //
        //   - a C# 9 `init` setter: `set_X` whose *void* return carries
        //     `modreq(System.Runtime.CompilerServices.IsExternalInit)`
        //     (a modifier run on a `void` return) — setter-only, `modreq`-only,
        //     `void`-only;
        //   - a `ref readonly` getter/indexer: `modreq(InAttribute)` over a
        //     `Byref` return, which `peelIlModifiers` recognises and the property
        //     type carries too.
        //
        // Anything else — the `init` marker on a getter, a `volatile` accessor
        // return, an unrecognised `modreq` — fails loud here exactly as the Rust
        // side refuses it; otherwise the oracle would emit a member Rust drops
        // and the differential would diverge. Keep the wording recognisable for a
        // cross-side grep.
        // Classify the whole chain rather than peeking at its head: `modopt`s may
        // sit anywhere in it, including *around* the `init` marker, and the Rust
        // `project_return` filters them out before looking at what is required.
        let mods, unmodified = splitIlModifiers retType
        let required = mods |> List.filter fst |> List.map snd
        let unmodCase, _ = FSharpValue.GetUnionFields(unmodified, unmodified.GetType(), bindings)
        let ok =
            match required with
            // Only ignorable `modopt`s (II.7.1.1): the return reads as if
            // unmodified, whatever it is — matching a `void` return whose run has
            // no required modifiers, and `classify_mods` on a non-void return.
            | [] -> true
            // The C# 9 `init` setter's marker — a setter's *void* return, only.
            | [ "System.Runtime.CompilerServices.IsExternalInit" ] ->
                kind = "setter" && unmodCase.Name = "Void"
            // A `ref readonly` getter/indexer — the marker over the byref.
            | [ "System.Runtime.InteropServices.InAttribute" ] -> unmodCase.Name = "Byref"
            | _ -> false
        if not ok then
            failwithf
                "fcs-dump entities: property `%s` %s return type carries custom modifier(s) other than an `init` setter's `modreq(IsExternalInit) void` or a `ref readonly` byref — unsupported"
                propName kind
    | _ -> ()

/// Join (least upper bound) on the C# accessibility lattice over masked
/// [`MethodAttributes`] values, mirroring the Rust-side `max_access` (which
/// also yields property accessibility — least-restrictive of the
/// accessors). The ECMA-335 lattice
/// (II.23.1.10/11) is *partial*: `Family` (protected) and `Assembly`
/// (internal) are incomparable, so a linear rank collapses them and the
/// result depends on argument order. The interesting cell is exactly the
/// case the diff harness pins on the property surface: `Family ∨ Assembly
/// = FamORAssem` (the C# "protected internal" union — accessible from
/// subclasses OR same-assembly), not whichever accessor was passed first.
///
/// Returns the masked access bits (other [`MethodAttributes`] flags are
/// dropped; callers downstream only look at the access mask anyway).
let private accessJoin
    (a: System.Reflection.MethodAttributes)
    (b: System.Reflection.MethodAttributes) : System.Reflection.MethodAttributes =
    let mask = System.Reflection.MethodAttributes.MemberAccessMask
    let ma = a &&& mask
    let mb = b &&& mask
    let validate (m: System.Reflection.MethodAttributes) =
        match m with
        | System.Reflection.MethodAttributes.Private
        | System.Reflection.MethodAttributes.FamANDAssem
        | System.Reflection.MethodAttributes.Assembly
        | System.Reflection.MethodAttributes.Family
        | System.Reflection.MethodAttributes.FamORAssem
        | System.Reflection.MethodAttributes.Public -> ()
        | System.Reflection.MethodAttributes.PrivateScope ->
            failwith "fcs-dump entities: MethodAttributes.PrivateScope (CompilerControlled) is unsupported"
        | other ->
            failwithf "fcs-dump entities: unrecognised masked MethodAttributes for accessor: %A" other
    validate ma
    validate mb
    let isP (x: System.Reflection.MethodAttributes) (target: System.Reflection.MethodAttributes) = x = target
    if isP ma System.Reflection.MethodAttributes.Public
       || isP mb System.Reflection.MethodAttributes.Public then
        System.Reflection.MethodAttributes.Public
    elif isP ma System.Reflection.MethodAttributes.FamORAssem
         || isP mb System.Reflection.MethodAttributes.FamORAssem then
        System.Reflection.MethodAttributes.FamORAssem
    elif (isP ma System.Reflection.MethodAttributes.Family
          && isP mb System.Reflection.MethodAttributes.Assembly)
         || (isP ma System.Reflection.MethodAttributes.Assembly
             && isP mb System.Reflection.MethodAttributes.Family) then
        // The partial-lattice case — Family ∨ Assembly widens to FamORAssem.
        System.Reflection.MethodAttributes.FamORAssem
    elif isP ma System.Reflection.MethodAttributes.Family
         || isP mb System.Reflection.MethodAttributes.Family then
        System.Reflection.MethodAttributes.Family
    elif isP ma System.Reflection.MethodAttributes.Assembly
         || isP mb System.Reflection.MethodAttributes.Assembly then
        System.Reflection.MethodAttributes.Assembly
    elif isP ma System.Reflection.MethodAttributes.FamANDAssem
         || isP mb System.Reflection.MethodAttributes.FamANDAssem then
        System.Reflection.MethodAttributes.FamANDAssem
    else
        System.Reflection.MethodAttributes.Private

/// `ILThisConvention` is an F# DU (`Instance | InstanceExplicit | Static`)
/// in `il.fs`. Walk it via `FSharpValue.GetUnionFields` to discriminate
/// the static case; ECMA-335 stores a property's static-ness as the
/// HASTHIS bit on the property signature blob, which FCS surfaces as
/// `ILThisConvention.Static`.
let private isStaticThisConvention (callingConv: obj) =
    let unionType = callingConv.GetType()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
    let case, _ = FSharpValue.GetUnionFields(callingConv, unionType, bindings)
    match case.Name with
    | "Static" -> true
    | "Instance"
    | "InstanceExplicit" -> false
    | other ->
        failwithf "fcs-dump entities: unhandled ILThisConvention case '%s'" other

/// Project an [`ILFieldDef`] (as an untyped `obj`) to the JSON shape
/// the Rust normaliser reads. Returns `None` for fields invisible to
/// F# code; the corresponding rows are filtered on the Rust side by
/// `accessible_from_some_fsharp_code` in `normalised_assembly.rs`.
///
/// `numTypeTypars` is the enclosing type's generic arity — threaded into
/// the [`ILType`] renderer so `T` in `Box<T>.item` projects to `!T0`.
///
/// Phase 4m.2: `typeNullableContext` is the enclosing TypeDef's
/// `[NullableContextAttribute]` byte (None when absent), used as the
/// scope-default fallback when the field carries no direct
/// `NullableAttribute`. Annotability of the field type gates the
/// fallback so a `int Foo` field stays Oblivious even under
/// `[NullableContext(1)]`.
let private projectIlField
    (numTypeTypars: int)
    (typeNullableContext: byte option)
    (ilFieldDef: obj)
    : objnull option =
    let attrs = getProp ilFieldDef "Attributes" :?> System.Reflection.FieldAttributes
    if not (ilFieldAccessibleFromSomeFSharpCode attrs) then
        None
    else
        let name = getProp ilFieldDef "Name" :?> string
        let isStatic = (attrs &&& System.Reflection.FieldAttributes.Static) <> enum 0
        let isLiteral = (attrs &&& System.Reflection.FieldAttributes.Literal) <> enum 0
        let isInitOnly = (attrs &&& System.Reflection.FieldAttributes.InitOnly) <> enum 0
        // The IL field `Type` for a `ref T` field (a `ref` field in a `ref
        // struct`) is `ILType.Byref`; `renderPositionTypeWithByref` keeps `T&`
        // with the referent's suffix after the `&`, matching the Rust model.
        let fieldType = getProp ilFieldDef "FieldType"
        // A `volatile` field's type carries `modreq(IsVolatile)` — the field type
        // is the one position that marker is meaningful, so the peel hands it back
        // here and it becomes a flag (mirroring `Field::is_volatile`), never a
        // silently-dropped modifier.
        let signature, isVolatile =
            renderPositionTypeWithByref
                numTypeTypars
                (sprintf "field `%s`" name)
                ilFieldDef
                fieldType
                typeNullableContext
        let isRequired = hasIlRequiredMemberAttribute ilFieldDef
        // The Rust side decodes `[CompilerFeatureRequired]` into the
        // field's gate set, but no MiniLib field carries one, so it projects
        // empty here. Fail loud rather than silently diverge if a future
        // fixture adds one: mirroring it would mean decoding the raw-IL
        // payload (feature string + `IsOptional`) the way the entity/method
        // paths read it from FCS's decoded `.Attributes`.
        if
            hasIlAttributeByFullName
                "System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute"
                ilFieldDef
        then
            failwithf
                "fcs-dump entities: CompilerFeatureRequiredAttribute on field `%s` is not yet \
                 mirrored (no fixture exercises it; see phase 4o)"
                name
        let flags =
            [|
                if isStatic then "static" else "instance"
                // Mirror `fieldFlags`: literals (C# `const`) carry Static +
                // Literal in ECMA-335 but not InitOnly, and the diff side
                // expects them to project as `static` only. The `not isLiteral`
                // guard is belt-and-braces for a hypothetical fixture that
                // sets both bits.
                if isInitOnly && not isLiteral then "init_only"
                if isVolatile then "volatile"
                if isRequired then "required"
            |]
        Some (box {| Kind = "Field"
                     Name = name
                     Signature = signature
                     Access = fieldAccessStringFromAttributes attrs
                     Flags = flags
                     GenericParameters = ([||]: obj array) |})

/// Project an [`ILPropertyDef`] (as untyped `obj`) to the JSON shape the
/// Rust normaliser reads. Returns `None` for properties invisible to F#
/// code (private; protected-and-internal); the corresponding rows are
/// filtered on the Rust side by `accessible_from_some_fsharp_code`
/// in `normalised_assembly.rs`.
///
/// `methods` is the list of [`ILMethodDef`]s on the enclosing
/// [`ILTypeDef`] — passed in rather than re-fetched per property so the
/// reflection cost stays linear in the property count.
///
/// `numTypeTypars` is the enclosing type's generic arity; threaded into
/// the [`ILType`] renderer so `T` in `Box<T>.Value` projects to `!T0`.
/// Properties themselves cannot be generic (ECMA-335 II.22.34) so we don't
/// thread a method-typar count here.
let private projectIlProperty
    (numTypeTypars: int)
    (typeNullableContext: byte option)
    (methods: obj list)
    (ilPropDef: obj)
    : objnull option
    =
    let name = getProp ilPropDef "Name" :?> string
    // Indexers carry their index-parameter types in `Args : ILTypes`
    // (types only — ECMA-335 keeps the parameter names / out / default on
    // the accessor methods). `Args` is used only to detect the indexer shape
    // and cross-check arity; the rendered types and their nullability come
    // from an accessor parameter instead (phase B3), because the property
    // signature carries neither the outer annotation nor composite inner
    // annotations (it flattens `List<string?>` to `List<string>`). The Rust
    // importer sources the same accessor parameter, so the diff stays
    // symmetric.
    let args =
        getProp ilPropDef "Args" :?> System.Collections.IEnumerable
        |> Seq.cast<obj>
        |> Seq.toList
    let propType = getProp ilPropDef "PropertyType"
    let getMethodOpt = unwrapOption (tryGetProp ilPropDef "GetMethod")
    let setMethodOpt = unwrapOption (tryGetProp ilPropDef "SetMethod")
    let getter =
        getMethodOpt
        |> Option.map (fun mref ->
            let m = lookupAccessor numTypeTypars methods mref
            validateAccessorReturnType name "getter" m
            m)
    let setter =
        setMethodOpt
        |> Option.map (fun mref ->
            let m = lookupAccessor numTypeTypars methods mref
            validateAccessorReturnType name "setter" m
            m)
    let accessorAttrs (m: obj) =
        getProp m "Attributes" :?> System.Reflection.MethodAttributes
    let maskedAccess =
        let mask = System.Reflection.MethodAttributes.MemberAccessMask
        match getter, setter with
        | Some g, Some s -> accessJoin (accessorAttrs g) (accessorAttrs s)
        | Some g, None -> accessorAttrs g &&& mask
        | None, Some s -> accessorAttrs s &&& mask
        | None, None ->
            failwithf "fcs-dump entities: property `%s` has neither getter nor setter" name
    // Mirror `ilFieldAccessibleFromSomeFSharpCode`: Public/Family/FamORAssem
    // pass the filter; everything else is filtered by the Rust normaliser
    // so we drop here to keep the diff symmetric.
    let visible =
        match maskedAccess with
        | System.Reflection.MethodAttributes.Public
        | System.Reflection.MethodAttributes.Family
        | System.Reflection.MethodAttributes.FamORAssem -> true
        | _ -> false
    if not visible then
        None
    else
        let isStatic = isStaticThisConvention (getProp ilPropDef "CallingConv")
        let isRequired = hasIlRequiredMemberAttribute ilPropDef
        // See `projectIlField`: no MiniLib property carries
        // `[CompilerFeatureRequired]`, so the Rust side projects an
        // empty gate set. Fail loud if a future fixture adds one.
        if
            hasIlAttributeByFullName
                "System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute"
                ilPropDef
        then
            failwithf
                "fcs-dump entities: CompilerFeatureRequiredAttribute on property `%s` is not yet \
                 mirrored (no fixture exercises it; see phase 4o)"
                name
        // A `ref`-returning property/indexer has an `ILType.Byref` `PropertyType`;
        // `renderPositionTypeWithByref` keeps `T&` with the referent's suffix
        // after the `&`, matching the Rust model (and the byref-return path).
        let propTypeRendered, propIsVolatile =
            renderPositionTypeWithByref
                numTypeTypars
                (sprintf "property `%s`" name)
                ilPropDef
                propType
                typeNullableContext
        // `volatile` is a field-only marker; the Rust `project_property` refuses
        // one on a property type, so the oracle must not emit the member either.
        if propIsVolatile then
            failwithf
                "fcs-dump entities: `volatile` modifier (`modreq(IsVolatile)`) on property `%s` — unsupported"
                name
        // Ordinary property: just the type. Indexer: bracketed
        // `[T1, T2] -> Ret`, mirroring `render_property_signature` on the
        // Rust side. Each index position's type AND nullability comes from an
        // accessor parameter (phase B3): the getter's parameters are exactly
        // the index dimension; the setter's are the index dimension plus a
        // trailing `value`. Render under the *accessor's* own NullableContext,
        // matching how the Rust importer projects the same accessor parameter.
        let signature =
            if List.isEmpty args then
                propTypeRendered
            else
                let accessorContext (m: obj) : byte option =
                    match
                        readNullableContextAttributeByte
                            (sprintf "property `%s` accessor" name)
                            m
                    with
                    | Some _ as direct -> direct
                    | None -> typeNullableContext
                let ilParamsOf (m: obj) =
                    getProp m "Parameters" :?> System.Collections.IEnumerable
                    |> Seq.cast<obj>
                    |> Seq.toList
                let accessorMethod, indexIlParams =
                    match getter, setter with
                    | Some g, _ -> g, ilParamsOf g
                    | None, Some s ->
                        match List.rev (ilParamsOf s) with
                        | _value :: revIdx -> s, List.rev revIdx
                        | [] ->
                            failwithf
                                "fcs-dump entities: setter for indexer `%s` has no value parameter"
                                name
                    | None, None ->
                        failwithf "fcs-dump entities: indexer `%s` has neither accessor" name
                if List.length indexIlParams <> List.length args then
                    failwithf
                        "fcs-dump entities: indexer `%s` signature carries %d index parameter(s) but its accessor carries %d"
                        name
                        (List.length args)
                        (List.length indexIlParams)
                let ctx = accessorContext accessorMethod
                let renderIndexParam (ilParam: obj) =
                    let ilType = getProp ilParam "Type"
                    // Strip a byref wrapper before the walker so the
                    // annotability gate sees the referent, mirroring
                    // `renderParameter` and the Rust-side byref-strip.
                    let ilInner =
                        let unionType = ilType.GetType()
                        let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
                        let case, fields = FSharpValue.GetUnionFields(ilType, unionType, bindings)
                        if case.Name = "Byref" then nonNullObj fields.[0] else ilType
                    renderIlTypeWithNullness
                        numTypeTypars
                        (sprintf "property `%s` index parameter" name)
                        ilParam
                        ilInner
                        ctx
                let argsRendered =
                    indexIlParams
                    |> List.map renderIndexParam
                    |> String.concat ", "
                sprintf "[%s] -> %s" argsRendered propTypeRendered
        let flags =
            [|
                if isStatic then "static" else "instance"
                if Option.isSome getMethodOpt then "get"
                if Option.isSome setMethodOpt then "set"
                if isRequired then "required"
            |]
        Some (box {| Kind = "Property"
                     Name = name
                     Signature = signature
                     Access = accessStringFromAttributes maskedAccess
                     Flags = flags
                     GenericParameters = ([||]: obj array) |})

/// Project an [`ILEventDef`] (as untyped `obj`) to the JSON shape the Rust
/// normaliser reads. Returns `None` for events invisible to F# code (private;
/// protected-and-internal). Mirrors `projectIlProperty`:
///
///   - Reject `OtherMethods` non-empty — the v1 model has no slot.
///   - Reject `EventType = None` — ECMA-335 II.22.13 permits it but no real
///     compiler emits one and the model carries the delegate type, not
///     `Option<TypeRef>`.
///   - Validate add/remove (and optional fire) accessor return-type
///     signatures the same way the property accessors are validated, so
///     exotic shapes (modreqs, etc.) fail loud rather than slipping through.
///   - Accessibility = least-restrictive of add and remove (fire is
///     observed-only and intentionally excluded — matching the Rust side).
///   - Static-ness = `MethodAttributes.Static` on the add accessor. No
///     top-level event flag exists; ECMA-335 doesn't formally require add
///     and remove to agree, but every real compiler emits a consistent pair.
let private projectIlEvent
    (numTypeTypars: int)
    (typeNullableContext: byte option)
    (methods: obj list)
    (ilEventDef: obj)
    : objnull option
    =
    let name = getProp ilEventDef "Name" :?> string
    let otherMethods =
        getProp ilEventDef "OtherMethods" :?> System.Collections.IEnumerable
        |> Seq.cast<obj>
        |> Seq.toList
    if not (List.isEmpty otherMethods) then
        failwithf
            "fcs-dump entities: event `%s` has %d non-standard accessor(s)"
            name (List.length otherMethods)
    let eventTypeOpt = unwrapOption (tryGetProp ilEventDef "EventType")
    let eventType =
        match eventTypeOpt with
        | Some t -> t
        | None ->
            failwithf
                "fcs-dump entities: event `%s` has no EventType (ECMA-335 II.22.13 permits but model rejects)"
                name
    let addMethod = lookupAccessor numTypeTypars methods (getProp ilEventDef "AddMethod")
    let removeMethod = lookupAccessor numTypeTypars methods (getProp ilEventDef "RemoveMethod")
    let fireMethodOpt =
        unwrapOption (tryGetProp ilEventDef "FireMethod")
        |> Option.map (lookupAccessor numTypeTypars methods)
    validateAccessorReturnType name "add accessor" addMethod
    validateAccessorReturnType name "remove accessor" removeMethod
    fireMethodOpt
    |> Option.iter (validateAccessorReturnType name "fire accessor")
    let accessorAttrs (m: obj) =
        getProp m "Attributes" :?> System.Reflection.MethodAttributes
    let maskedAccess = accessJoin (accessorAttrs addMethod) (accessorAttrs removeMethod)
    let visible =
        match maskedAccess with
        | System.Reflection.MethodAttributes.Public
        | System.Reflection.MethodAttributes.Family
        | System.Reflection.MethodAttributes.FamORAssem -> true
        | _ -> false
    if not visible then
        None
    else
        let isStaticOf (m: obj) =
            (accessorAttrs m) &&& System.Reflection.MethodAttributes.Static
            <> enum<_> 0
        let addStatic = isStaticOf addMethod
        let removeStatic = isStaticOf removeMethod
        if addStatic <> removeStatic then
            failwithf
                "fcs-dump entities: event `%s` has add/remove disagreeing on static-ness"
                name
        let flags =
            [|
                if addStatic then "static" else "instance"
                "add"
                "remove"
                if Option.isSome fireMethodOpt then "fire"
            |]
        let signature =
            renderIlTypeWithNullness
                numTypeTypars
                (sprintf "event `%s`" name)
                ilEventDef
                eventType
                typeNullableContext
        Some (box {| Kind = "Event"
                     Name = name
                     Signature = signature
                     Access = accessStringFromAttributes maskedAccess
                     Flags = flags
                     GenericParameters = ([||]: obj array) |})

/// `true` when the entity carries the named CLR attribute. FCS does
/// not expose typed `IsReadOnly` / `IsByRefLike` properties on
/// `FSharpEntity`, so the only public signal is to walk the raw
/// `Attributes` collection and match by the attribute type's
/// `TryFullName`. Mirrors the `detect_*_attribute` family on the Rust
/// side.
let private hasAttributeByFullName (fullName: string) (e: FSharpEntity) =
    e.Attributes
    |> Seq.exists (fun a -> a.AttributeType.TryFullName = Some fullName)

let private hasIsReadOnlyAttribute (e: FSharpEntity) =
    hasAttributeByFullName "System.Runtime.CompilerServices.IsReadOnlyAttribute" e

let private hasIsByRefLikeAttribute (e: FSharpEntity) =
    hasAttributeByFullName "System.Runtime.CompilerServices.IsByRefLikeAttribute" e

/// `true` when the entity carries `[Microsoft.FSharp.Core.AutoOpenAttribute]`.
/// F#-only marker on a module meaning "consumers don't need an explicit
/// `open <module>`". Mirrors `detect_autoopen_attribute` on the Rust side.
/// FCS exposes a typed `IsFSharpAbbreviation`-style predicate for many
/// F# attributes but not this one, so we walk the raw `Attributes`
/// collection by full name — same shape as the `IsReadOnly` /
/// `IsByRefLike` helpers above.
let private hasAutoOpenAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.AutoOpenAttribute" e

/// `true` when the entity carries
/// `[Microsoft.FSharp.Core.RequireQualifiedAccessAttribute]`. Marker
/// attribute on modules / DUs that forces callers to fully qualify
/// member references. Mirrors `detect_require_qualified_access_attribute`
/// on the Rust side. FCS has no typed predicate for this attribute, so
/// we read it from the raw `Attributes` list — same shape as the
/// `IsReadOnly` / `IsByRefLike` helpers above.
let private hasRequireQualifiedAccessAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.RequireQualifiedAccessAttribute" e

/// F# derived-impl policy cluster — `NoEquality`, `NoComparison`,
/// `StructuralEquality`, `StructuralComparison`. All four live in
/// `Microsoft.FSharp.Core` and are not catalogued in `WellKnownILAttributes`
/// (FCS tracks them on its TypedTree `WellKnownEntityAttributes` enum
/// instead). No typed predicate on `FSharpEntity` surfaces them, so we
/// read the raw `Attributes` list by full name — same shape as the
/// `AutoOpen` / `RequireQualifiedAccess` helpers above, and same shape
/// as `detect_equality_comparison_attributes` on the Rust side.
let private hasNoEqualityAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.NoEqualityAttribute" e

let private hasNoComparisonAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.NoComparisonAttribute" e

let private hasStructuralEqualityAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.StructuralEqualityAttribute" e

let private hasStructuralComparisonAttribute (e: FSharpEntity) =
    hasAttributeByFullName "Microsoft.FSharp.Core.StructuralComparisonAttribute" e

/// `true` when the entity carries
/// `[Microsoft.FSharp.Core.AllowNullLiteralAttribute]` *and the bool
/// ctor arg resolves to `true`*. F#-only marker on classes and
/// interfaces that opts the type out of F#'s default null-prohibition.
/// The two overloads behave like:
///
///   - parameterless (`[<AllowNullLiteral>]`)        → `true`
///   - `AllowNullLiteralAttribute(false)`            → `false`
///     (the deliberate *disable* form — opts out of an inherited `(true)`)
///   - `AllowNullLiteralAttribute(true)`             → `true`
///
/// Mirrors `detect_allow_null_literal_attribute` on the Rust side: both
/// halves emit the same bool so `[<AllowNullLiteral(false)>]` does not
/// surface the `allow_null_literal` token.
let private hasAllowNullLiteralAttribute (e: FSharpEntity) =
    e.Attributes
    |> Seq.tryFind (fun a ->
        a.AttributeType.TryFullName = Some "Microsoft.FSharp.Core.AllowNullLiteralAttribute")
    |> Option.map (fun a ->
        match a.ConstructorArguments |> Seq.toList with
        | [] -> true
        | [ (_, value) ] ->
            match value with
            | :? bool as b -> b
            // Defensive: the attribute's only typed ctor overload is `(bool)`,
            // so anything else means a future FSharp.Core shipped a new
            // overload. Fall back to "present == enabled" rather than crash.
            | _ -> true
        | _ ->
            // Same defensive fallback: a multi-arg overload would be a future
            // shape we haven't seen; treat presence as enabled.
            true)
    |> Option.defaultValue false

/// FCS classifies `SetsRequiredMembersAttribute` as the same
/// well-known marker under either of two namespaces — see
/// `TypedTreeOps.Attributes.fs` in the F# compiler:
///   * `System.Diagnostics.CodeAnalysis.SetsRequiredMembersAttribute`
///     (the canonical net10.0 home; what Roslyn always emits).
///   * `System.Runtime.CompilerServices.SetsRequiredMembersAttribute`
///     (accepted for polyfill / older-runtime scenarios).
/// Match both so the Rust-side mirror's widened check stays in sync.
let private hasSetsRequiredMembersAttribute (m: FSharpMemberOrFunctionOrValue) =
    m.Attributes
    |> Seq.exists (fun a ->
        match a.AttributeType.TryFullName with
        | Some "System.Diagnostics.CodeAnalysis.SetsRequiredMembersAttribute"
        | Some "System.Runtime.CompilerServices.SetsRequiredMembersAttribute" -> true
        | _ -> false)

/// `true` when `attrs` carries the specific Roslyn marker
/// `[CompilerFeatureRequired("RequiredMembers")]`. Other feature names
/// (e.g. `"RefStructs"`) are deliberately not matched: only the
/// `"RequiredMembers"` flavour comes paired with a synthetic Obsolete,
/// so widening the match would suppress legitimate user-authored
/// Obsoletes on unrelated feature-gated APIs.
///
/// Mirrors `has_required_members_feature_gate` in the Rust-side
/// reader. `projectMember` uses this in conjunction with a
/// constructor check to decide whether to drop a paired Obsolete —
/// Roslyn only emits the synthetic shape on non-`[SetsRequiredMembers]`
/// constructors, so both conditions must hold for the suppression to
/// fire. The Rust side enforces the same pair of conditions; this
/// keeps the MiniLib diff oracle balanced.
let private carriesRequiredMembersFeatureGate
    (attrs: System.Collections.Generic.IList<FSharpAttribute>) =
    attrs
    |> Seq.exists (fun a ->
        a.AttributeType.TryFullName = Some "System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute"
        && a.ConstructorArguments.Count = 1
        && (match a.ConstructorArguments.[0] with
            | (_, (:? string as s)) -> s = "RequiredMembers"
            | _ -> false))


/// Render the `[compiler-feature-required: <feature>]` strings the diff
/// harness expects, one per `[CompilerFeatureRequiredAttribute]` on
/// `attrs`. Mirrors `detect_compiler_feature_required_attributes` +
/// `format_compiler_feature_required` on the Rust side.
///
/// `CompilerFeatureRequiredAttribute` is `AllowMultiple = true`, so a
/// carrier can hold several gates (the diff side folds them into a set, so
/// order is irrelevant). Roslyn emits it on `ref struct` types
/// (`"RefStructs"`), on non-`[SetsRequiredMembers]` constructors of
/// required-member types (`"RequiredMembers"`), and a handful of other
/// feature gates.
///
/// Decode rules mirror the Rust refusals exactly — fail loud, never
/// degrade to presence-only, because the feature name *is* the entire
/// payload:
///   - exactly one constructor arg, a non-null string → the feature name;
///   - the only legal named arg is `IsOptional : bool`.
/// The feature string is decoded verbatim however long (the owned reader
/// decodes long CA strings correctly, and FCS decodes them faithfully).
let private formatCompilerFeatureRequiredList
    (attrs: System.Collections.Generic.IList<FSharpAttribute>)
    : string list
    =
    let compilerFeatureRequiredFullName =
        "System.Runtime.CompilerServices.CompilerFeatureRequiredAttribute"
    attrs
    |> Seq.filter (fun a -> a.AttributeType.TryFullName = Some compilerFeatureRequiredFullName)
    |> Seq.map (fun a ->
        let feature =
            match a.ConstructorArguments |> Seq.toList with
            | [ (_, (:? string as s)) ] -> s
            | [ (_, null) ] ->
                failwithf "fcs-dump entities: CompilerFeatureRequiredAttribute has null ctor arg"
            | other ->
                failwithf
                    "fcs-dump entities: CompilerFeatureRequiredAttribute has %d ctor args; \
                     expected 1"
                    other.Length
        let mutable isOptional = false
        for (_ty, name, _isField, value) in a.NamedArguments do
            match name, value with
            | "IsOptional", (:? bool as b) -> isOptional <- b
            | _ ->
                failwithf
                    "fcs-dump entities: CompilerFeatureRequiredAttribute has unexpected named \
                     arg `%s`"
                    name
        if isOptional then
            sprintf "[compiler-feature-required: %s (optional)]" feature
        else
            sprintf "[compiler-feature-required: %s]" feature)
    |> Seq.toList

/// Render the `[obsolete ...]` string the diff harness expects when `attrs`
/// contains a `System.ObsoleteAttribute`, otherwise `None`. Mirrors
/// `format_obsolete` in the Rust-side `test_support` and the decoding rules of
/// `Ecma335Assembly::detect_obsolete`: ctor `()`/`(string)`/`(string, bool)`;
/// `Message`/`IsError` named args overlay (last-write-wins); `DiagnosticId`/
/// `UrlFormat` ignored. Long strings are decoded verbatim — `Ecma335Assembly`'s
/// reader decodes long CA strings correctly, and FCS decodes them faithfully.
let private tryFormatObsolete (attrs: System.Collections.Generic.IList<FSharpAttribute>) : string option =
    attrs
    |> Seq.tryFind (fun a -> a.AttributeType.TryFullName = Some "System.ObsoleteAttribute")
    |> Option.map (fun a ->
        let mutable message : string option = None
        let mutable isError = false
        // The `(string)` and `(string, bool)` overloads accept a
        // null literal — `[Obsolete(null, true)]` is legal C# — and
        // FCS surfaces that as a literal `null` payload. Treat it the
        // same as an absent message; the Rust side does the equivalent
        // via `Option<Cow<str>>`.
        let typeNameOf (o: objnull) : string =
            match o with
            | null -> "<null>"
            | x ->
                match x.GetType().FullName with
                | null -> x.GetType().Name
                | n -> n
        let readMessageArg (v: objnull) =
            match v with
            | :? string as s -> message <- Some s
            | null -> message <- None
            | other ->
                failwithf
                    "fcs-dump entities: ObsoleteAttribute message arg has \
                     unexpected runtime type %s" (typeNameOf other)
        let readIsErrorArg (v: objnull) =
            match v with
            | :? bool as b -> isError <- b
            | other ->
                failwithf
                    "fcs-dump entities: ObsoleteAttribute IsError arg has \
                     unexpected runtime type %s" (typeNameOf other)
        let ctorArgs = a.ConstructorArguments
        match ctorArgs.Count with
        | 0 -> ()
        | 1 -> readMessageArg (snd ctorArgs.[0])
        | 2 ->
            readMessageArg (snd ctorArgs.[0])
            readIsErrorArg (snd ctorArgs.[1])
        | n ->
            failwithf
                "fcs-dump entities: ObsoleteAttribute has %d ctor args; \
                 expected 0/1/2" n
        for (_ty, name, _isField, value) in a.NamedArguments do
            match name with
            | "Message" -> readMessageArg value
            | "IsError" -> readIsErrorArg value
            | _ -> ()
        // `Ecma335Assembly`'s reader decodes long CA strings correctly, so
        // the message is emitted verbatim however long it is — FCS likewise
        // decodes it faithfully.
        match isError, message with
        | false, None -> "[obsolete]"
        | false, Some m -> sprintf "[obsolete: %s]" m
        | true, None -> "[obsolete error]"
        | true, Some m -> sprintf "[obsolete error: %s]" m)

/// Render the `[experimental ...]` string the diff harness expects
/// when `attrs` contains a `System.Diagnostics.CodeAnalysis.ExperimentalAttribute`,
/// otherwise `None`.
///
/// Mirrors `format_experimental` in the Rust-side `test_support`
/// module byte-for-byte. Decoding rules match the Rust-side
/// `detect_experimental_attribute`:
///
/// - constructor args: `(string diagnosticId)` (the single overload)
/// - named args: `UrlFormat` and `Message` (both strings); also
///   `DiagnosticId` as a named arg overrides the positional one
///   (last-write-wins — matches the Rust-side decoder)
///
/// Long strings are decoded verbatim, as in [`tryFormatObsolete`].
let private tryFormatExperimental (attrs: System.Collections.Generic.IList<FSharpAttribute>) : string option =
    attrs
    |> Seq.tryFind (fun a ->
        a.AttributeType.TryFullName = Some
            "System.Diagnostics.CodeAnalysis.ExperimentalAttribute")
    |> Option.map (fun a ->
        let typeNameOf (o: objnull) : string =
            match o with
            | null -> "<null>"
            | x ->
                match x.GetType().FullName with
                | null -> x.GetType().Name
                | n -> n
        let readString fieldName (v: objnull) : string option =
            match v with
            | :? string as s -> Some s
            | null -> None
            | other ->
                failwithf
                    "fcs-dump entities: ExperimentalAttribute %s arg has \
                     unexpected runtime type %s" fieldName (typeNameOf other)
        let ctorArgs = a.ConstructorArguments
        let mutable diagnosticId : string option =
            match ctorArgs.Count with
            | 1 -> readString "DiagnosticId" (snd ctorArgs.[0])
            | n ->
                failwithf
                    "fcs-dump entities: ExperimentalAttribute has %d ctor args; \
                     expected 1" n
        let mutable urlFormat : string option = None
        let mutable message : string option = None
        for (_ty, name, _isField, value) in a.NamedArguments do
            match name with
            | "DiagnosticId" -> diagnosticId <- readString "DiagnosticId" value
            | "UrlFormat" -> urlFormat <- readString "UrlFormat" value
            | "Message" -> message <- readString "Message" value
            | _ -> ()
        // Long strings are decoded verbatim (no SerString degradation) — see
        // `tryFormatObsolete`.
        let parts = System.Collections.Generic.List<string>()
        match diagnosticId with
        | Some id -> parts.Add(sprintf "id=%s" id)
        | None -> ()
        match urlFormat with
        | Some u -> parts.Add(sprintf "url=%s" u)
        | None -> ()
        match message with
        | Some m -> parts.Add(sprintf "message=%s" m)
        | None -> ()
        if parts.Count = 0 then "[experimental]"
        else sprintf "[experimental %s]" (System.String.Join(", ", parts)))

/// Render the `[default-member: ...]` string the diff harness expects
/// when `attrs` contains a `System.Reflection.DefaultMemberAttribute`,
/// otherwise `None`.
///
/// Mirrors `format_default_member` in the Rust-side `test_support`
/// module, and the refuse-loud policy of `Ecma335Assembly::
/// detect_default_member`. Concretely:
///
/// - Named args present → `failwithf`. Roslyn never emits them.
/// - The member name is decoded verbatim (long strings are fine — the reader
///   decodes long CA strings correctly, and FCS decodes them faithfully).
/// - Anything other than a single positional `string` ctor arg →
///   `failwithf`.
let private tryFormatDefaultMember (attrs: System.Collections.Generic.IList<FSharpAttribute>) : string option =
    attrs
    |> Seq.tryFind (fun a ->
        a.AttributeType.TryFullName = Some "System.Reflection.DefaultMemberAttribute")
    |> Option.map (fun a ->
        let typeNameOf (o: objnull) : string =
            match o with
            | null -> "<null>"
            | x ->
                match x.GetType().FullName with
                | null -> x.GetType().Name
                | n -> n
        let ctorArgs = a.ConstructorArguments
        let memberName =
            match ctorArgs.Count with
            | 1 ->
                match snd ctorArgs.[0] with
                | :? string as s -> s
                | null ->
                    failwithf
                        "fcs-dump entities: DefaultMemberAttribute has null ctor arg"
                | other ->
                    failwithf
                        "fcs-dump entities: DefaultMemberAttribute ctor arg has \
                         unexpected runtime type %s" (typeNameOf other)
            | n ->
                failwithf
                    "fcs-dump entities: DefaultMemberAttribute has %d ctor args; \
                     expected 1" n
        if a.NamedArguments.Count > 0 then
            failwithf
                "fcs-dump entities: DefaultMemberAttribute has %d named args; \
                 expected 0" a.NamedArguments.Count
        sprintf "[default-member: %s]" memberName)

/// Discriminate the entity kind into the same string set the Rust
/// projection emits. Order matters: F#-specific kinds (Module, Union,
/// Record, Abbreviation, Exception) take priority over the underlying
/// IL kind so an F# DU doesn't fall through to "Class".
///
/// Struct flavour markers (`readonly` / `ref` / `struct`) are prepended
/// to the base kind so the differential test sees them as part of the
/// same string. Order matches C# 11 surface syntax (`readonly ref
/// struct`). The `struct` marker only fires when the entity is a CLR
/// value type AND the base kind hides that — i.e. `[<Struct>] type R = {
/// ... }` projects as `Record` from the F# kind, but we want the diff
/// to see `struct Record`. Enums are already named `Enum` and the
/// base-kind `Struct` already names itself, so we suppress the
/// redundant prefix on those. Mirrors the [`Entity::is_struct`] flag
/// and its renderer on the Rust side.
let private entityKindString (e: FSharpEntity) =
    let baseKind =
        // `[<Measure>] type m` exposes as a class shape with
        // `IsMeasure = true`; the `IsMeasure` check must precede
        // `IsClass`, otherwise the kind round-trips as "Class".
        // The Rust merge upgrades the ECMA-derived `Class` to
        // `Measure` based on the pickle's `typar_kind`, so the
        // string here must match.
        if e.IsMeasure then "Measure"
        elif e.IsFSharpModule then "Module"
        elif e.IsFSharpUnion then "Union"
        elif e.IsFSharpRecord then "Record"
        elif e.IsFSharpAbbreviation then "Abbreviation"
        elif e.IsFSharpExceptionDeclaration then "Exception"
        elif e.IsInterface then "Interface"
        elif e.IsDelegate then "Delegate"
        elif e.IsEnum then "Enum"
        elif e.IsValueType then "Struct"
        elif e.IsClass then "Class"
        else
            failwithf "fcs-dump entities: unhandled FSharpEntity kind for %s" e.FullName
    let isStructPrefix =
        e.IsValueType && baseKind <> "Struct" && baseKind <> "Enum"
    let prefix =
        let parts = ResizeArray<string>()
        if hasIsReadOnlyAttribute e then parts.Add "readonly"
        if hasIsByRefLikeAttribute e then parts.Add "ref"
        if isStructPrefix then parts.Add "struct"
        if hasAutoOpenAttribute e then parts.Add "auto_open"
        if hasRequireQualifiedAccessAttribute e then parts.Add "require_qualified_access"
        if hasNoEqualityAttribute e then parts.Add "no_equality"
        if hasNoComparisonAttribute e then parts.Add "no_comparison"
        if hasStructuralEqualityAttribute e then parts.Add "structural_equality"
        if hasStructuralComparisonAttribute e then parts.Add "structural_comparison"
        if hasAllowNullLiteralAttribute e then parts.Add "allow_null_literal"
        if parts.Count = 0 then ""
        else (String.concat " " parts) + " "
    prefix + baseKind

/// FQN as the Rust normaliser computes it: `Namespace.DisplayName` (or
/// just `DisplayName` if the entity has no enclosing namespace, which is
/// the case for nested types — those are emitted inside their parent's
/// `NestedTypes`, where the diff harness doesn't look at the inner FQN).
let private entityFqn (e: FSharpEntity) =
    match e.Namespace with
    | Some ns when ns <> "" -> sprintf "%s.%s" ns e.DisplayName
    | _ -> e.DisplayName

/// FCS exposes property accessors, event add/remove handlers, and the
/// property/event symbol itself through `MembersFunctionsAndValues`.
/// Phase 3b adds fields (via `FSharpFields`) but not properties or events
/// — properties land in 3c — so filter the synthesised accessors out
/// here. The Rust side likewise skips property/event projection so
/// the diff agrees.
///
/// Phase 4g admits F# module-level `let` bindings (`let x = 42`,
/// `let foo x = …`) on Module entities. FCS surfaces these as
/// `IsModuleValueOrMember = true && IsMember = false`; the wider
/// gate uses `IsModuleValueOrMember` so it covers both module
/// values/functions AND `member` declarations on F# types.
/// Detect C# extension methods by their `[ExtensionAttribute]` marker.
///
/// `FSharpMemberOrFunctionOrValue.IsExtensionMember` is *not* a reliable
/// signal for C#-style extension methods — FCS reports it as `false`
/// for imported C# extensions, leaving the `[Extension]` attribute as
/// the authoritative discriminator (see `infos.fs` in the F# compiler,
/// which itself reads the attribute to recognise these).
let private isCSharpExtensionMethod (m: FSharpMemberOrFunctionOrValue) =
    m.Attributes
    |> Seq.exists (fun a ->
        a.AttributeType.TryFullName = Some "System.Runtime.CompilerServices.ExtensionAttribute")

let private isProjectableMethod (m: FSharpMemberOrFunctionOrValue) =
    m.IsModuleValueOrMember
    && not m.IsProperty
    && not m.IsPropertyGetterMethod
    && not m.IsPropertySetterMethod
    && not m.IsEvent
    && not m.IsEventAddMethod
    && not m.IsEventRemoveMethod
    // *Generic* F#-native extension member — a generic method extending a
    // builder, or a generic *target* type whose typars are lifted onto the
    // augmenting method. The receiver rendering cannot thread a generic
    // target's typars through, so the whole shape is elided; the Rust-side
    // mirror (`is_unmirrorable_generic_module_method` in `test_support.rs`)
    // drops exactly `Module ∧ extension-flagged ∧ generic` — the flag is
    // pickle-authoritative there since the member-list cutover. (Plain
    // generic module `let`s are NOT dropped on either side any more:
    // `projectMember` renders their generic parameters from the FCS
    // public surface.)
    && not (m.IsExtensionMember && m.GenericParameters.Count > 0)
    // A non-IL generic binding whose typar carries an **IL-visible**
    // constraint (`array2D`'s flexible `#seq` parameter compiles to a
    // coercion constraint row) — the FCS-surface rendering in
    // `projectMember` is name-only and cannot mirror the Rust side's
    // IL-derived constraint tokens, so elide the member rather than abort
    // the dump (previously `entities` crashed on the shipped FSharp.Core
    // here). Mirrored by the same predicate in
    // `is_unmirrorable_generic_module_method`; the IL-*erased* constraint
    // kinds (SRTP member constraints, comparison/equality) stay
    // projectable.
    && not (
        (tryGetIlMethodDef m).IsNone
        && m.GenericParameters
           |> Seq.exists (fun gp ->
               gp.Constraints
               |> Seq.exists (fun c ->
                   c.IsCoercesToConstraint
                   || c.IsNonNullableValueTypeConstraint
                   || c.IsReferenceTypeConstraint
                   || c.IsRequiresDefaultConstructorConstraint
                   || c.IsUnmanagedConstraint
                   || c.IsEnumConstraint
                   || c.IsDelegateConstraint)))
    // Module-level `[<Literal>] let X = …` constants compile to a static
    // literal field on the module class (no property, no method body).
    // FCS still surfaces them through `MembersFunctionsAndValues` as
    // zero-arg methods, but the Rust-side projector sees only the
    // field — projecting here would emit a `Method` against the other
    // side's nothing/field. Drop them on both sides for now; a future
    // slice can project literals symmetrically.
    && m.LiteralValue.IsNone

/// Render an [`FSharpParameter`] to the same string form `render_parameter`
/// uses in `crates/assembly/src/test_support.rs`:
///
/// - `out T`     for an `out` parameter (CLR byref + Param.is_out)
/// - `byref T`   for a plain byref parameter (no is_out)
/// - `T`         otherwise
///
/// FCS surfaces `out`/`byref` parameters with a `byref<T>` parameter type,
/// so we unwrap the byref before rendering the element — otherwise the
/// output would read `out T&` rather than `out T`.
let private renderParameter
    (numTypeTypars: int)
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (paramReturnContext: byte option)
    (ilParam: obj option)
    (p: FSharpParameter) =
    let pType = p.Type
    // A byref-*like* zero-arg intrinsic (`TypedReference` / `ArgIterator` /
    // `RuntimeArgumentHandle`) reports `IsByRef` but is passed by value, not by
    // reference — so it takes no `byref`/`out` prefix and its referent is
    // itself (`isRealByref` returns false; see its comment).
    let isByRef = isRealByref pType
    let inner =
        if isByRef then pType.GenericArguments.[0]
        else pType
    let paramLabel =
        match p.Name with
        | Some n -> sprintf "parameter `%s`" n
        | None -> "parameter <unnamed>"
    // Phase 4m.3: for IL-imported methods, the FCS public surface drops
    // `NullableAttribute` from `FSharpParameter.Attributes` and reports
    // per-position nullness on `FSharpType` as Oblivious — there is no
    // route from the FCS view back to the byte[] composite encoding for
    // method parameters. Read directly off the raw `ILParameter` instead
    // (carrier for the attribute, source of the IL `Type` to walk) so the
    // diff agrees with the Rust-side `walk_method_type`. F#-native methods
    // (no `ilMethodDef`) keep the FCS-side path — they don't carry the
    // composite encoding in practice and the existing scalar ladder
    // suffices.
    let typeWithSuffix =
        match ilParam with
        | Some il ->
            let ilType = getProp il "Type"
            // The IL parameter `Type` for `out`/`ref`/`byref` is
            // `ILType.Byref`. Mirror the Rust-side byref-strip before the
            // walker so the annotability gate sees the referent, not the
            // wrapper. The `byref`/`out` keyword prefix is still chosen
            // from the FCS-side `IsOutArg`/`isByRef` discovered above.
            let ilInner =
                // Peel any custom modifiers first (`in`/`ref readonly` carries
                // `modreq(InAttribute)` over the byref): the `readonly` bit rides
                // the `inref` prefix below, and the walker must see the referent.
                let _, isVolatile, unmodified = peelIlModifiers paramLabel ilType
                if isVolatile then
                    failwithf
                        "fcs-dump entities: `volatile` modifier (`modreq(IsVolatile)`) on %s — unsupported"
                        paramLabel
                let unionType = unmodified.GetType()
                let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
                let case, fields = FSharpValue.GetUnionFields(unmodified, unionType, bindings)
                if case.Name = "Byref" then nonNullObj fields.[0] else unmodified
            renderIlTypeWithNullness
                numTypeTypars
                paramLabel
                il
                ilInner
                paramReturnContext
        | None ->
            let rendered = renderTypeInScopeWithInnerNullness typeTypars methodTypars inner
            let nullable =
                resolveFcsPositionNullability paramLabel p.Attributes inner paramReturnContext
            sprintf "%s%s" rendered (nullabilitySuffix nullable)
    // A read-only byref — C# `in` / `ref readonly`, F# `inref<'T>` — is a
    // `modreq(System.Runtime.InteropServices.InAttribute)` over the byref. Read
    // it off the raw IL parameter type where we have one (the C#-fixture path);
    // for an F#-native member FCS's own `IsInArg` carries the same bit.
    let isReadonlyRef =
        isByRef
        && match ilParam with
           | Some il ->
               let modifierReadonly, _, _ = peelIlModifiers paramLabel (getProp il "Type")
               modifierReadonly || hasReadonlyRefAttribute il
           | None -> p.IsInArg
    let withPrefix =
        if p.IsOutArg then sprintf "out %s" typeWithSuffix
        elif isByRef && isReadonlyRef then sprintf "inref %s" typeWithSuffix
        elif isByRef then sprintf "byref %s" typeWithSuffix
        // C# `params T[]` (and F# `[<ParamArray>] T[]`) compile to a
        // value parameter carrying `[System.ParamArrayAttribute]`; the
        // IL type is unchanged. The Rust side reads the attribute
        // and surfaces `is_param_array`; FCS exposes the same bit via
        // `IsParamArrayArg`. Render `params T[]` so the diff agrees.
        // `params` is mutually exclusive with `out`/`byref` (the
        // attribute only sits on the trailing value parameter), so
        // these branches don't overlap.
        elif p.IsParamArrayArg then sprintf "params %s" typeWithSuffix
        else typeWithSuffix
    // The shared normaliser renders a parameter with `Param.has_default`
    // set as `T = ?`. The Rust side reads the parameter's default row;
    // FCS surfaces the equivalent through `IsOptionalArg`. Without this,
    // any API with a default argument would silently mis-render.
    if p.IsOptionalArg then sprintf "%s = ?" withPrefix else withPrefix

/// Render a return type. FCS surfaces a CLR `void` return as the F# `unit`
/// abbreviation (`Microsoft.FSharp.Core.Unit`) because F#'s type system has
/// no `void` — every function returns something. The Rust side reads
/// the raw IL and emits `System.Void`; collapse FCS's representation to
/// match so the diff agrees. Only applies in the return position: `unit`
/// can't appear as a parameter in IL (an `f : unit -> int` compiles down
/// to a parameterless method).
let private renderReturnType
    (numTypeTypars: int)
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (paramReturnContext: byte option)
    (ilReturn: obj option)
    (returnParam: FSharpParameter) =
    let t = returnParam.Type
    let resolved = if t.IsAbbreviation then t.AbbreviatedType else t
    let isUnit =
        resolved.HasTypeDefinition
        && (match resolved.TypeDefinition.TryFullName with
            | Some "Microsoft.FSharp.Core.Unit" -> true
            | _ -> false)
    if isUnit then
        "System.Void"
    else
        // Phase 4m.3 IL-side path: see the matching comment in
        // `renderParameter`. The Rust-side `project_return_type` reads
        // attributes off the return `ParameterMetadata` and walks the IL
        // return type — mirror by reading off `ILReturn.CustomAttrs` and
        // `Type`. F#-native methods (no `ilReturn`) fall back to FCS.
        match ilReturn with
        | Some il ->
            let ilType = getProp il "Type"
            // The IL return `Type` for a `ref T` return is `ILType.Byref`.
            // Strip the wrapper before walking so the position-level
            // attribute targets the referent (matches the Rust-side
            // `project_return_type` byref-strip); then reattach `&`
            // between the rendered body and the outer position suffix.
            //
            // The Rust side renders byref returns as `T&{suffix}`:
            // `render_type(ByRef(t))` produces `"{T}&"`, then
            // `render_signature` appends the position-level
            // `return_nullability` suffix at the end. Using
            // [`renderIlTypeWithNullnessSplit`] gives us the body and
            // outer suffix separately so we can compose
            // `body + "&" + suffix` to match. Without this, any
            // ref-returning API would diverge in the diff oracle.
            // A `ref readonly` return is `modreq(InAttribute)` over the byref;
            // peel it and render `readonly T&`, matching `render_type` on the
            // Rust side. (`volatile` has no meaning on a return — `peelIlModifiers`
            // hands it back and it is refused.)
            let modifierReadonly, retIsVolatile, ilUnmodified = peelIlModifiers "return type" ilType
            let readonlyRef = modifierReadonly || hasReadonlyRefAttribute il
            if retIsVolatile then
                failwith
                    "fcs-dump entities: `volatile` modifier (`modreq(IsVolatile)`) on a return type — unsupported"
            let isByref, ilInner =
                let unionType = ilUnmodified.GetType()
                let bindings = BindingFlags.Public ||| BindingFlags.NonPublic
                let case, fields = FSharpValue.GetUnionFields(ilUnmodified, unionType, bindings)
                if case.Name = "Byref" then true, nonNullObj fields.[0] else false, ilUnmodified
            if readonlyRef && not isByref then
                failwith
                    "fcs-dump entities: read-only-ref modifier (`modreq(InAttribute)`) not over a byref return"
            if isByref then
                let body, suffix =
                    renderIlTypeWithNullnessSplit
                        numTypeTypars
                        "return type"
                        il
                        ilInner
                        paramReturnContext
                let prefix = if readonlyRef then "readonly " else ""
                sprintf "%s%s&%s" prefix body suffix
            else
                renderIlTypeWithNullness
                    numTypeTypars
                    "return type"
                    il
                    ilInner
                    paramReturnContext
        | None ->
            // Only a real `byref<T>` return unwraps to its referent for the
            // nullability walk; a zero-arg byref-like intrinsic annotates
            // nothing and stays the whole type (see `isRealByref`).
            let annotable =
                if isRealByref t then t.GenericArguments.[0]
                else t
            let nullable =
                resolveFcsPositionNullability
                    "return type"
                    returnParam.Attributes
                    annotable
                    paramReturnContext
            sprintf
                "%s%s"
                (renderTypeInScopeWithInnerNullness typeTypars methodTypars t)
                (nullabilitySuffix nullable)

/// Return `true` if `t` is the F# `unit` type (`Microsoft.FSharp.Core.unit`,
/// the alias for the `Unit` class). Used to strip the synthetic `unit`
/// parameter FCS reports for nullary F#-native extension members.
let private isUnitType (t: FSharpType) =
    let resolved = if t.IsAbbreviation then t.AbbreviatedType else t
    resolved.HasTypeDefinition
    && (match resolved.TypeDefinition.TryFullName with
        | Some "Microsoft.FSharp.Core.Unit" -> true
        | _ -> false)

/// Render the receiver type of an F#-native instance extension member so
/// that it agrees with the Rust-side rendering of the first IL
/// parameter (the `this`-shaped receiver the F# compiler re-prepends to
/// the compiled MethodDef signature). For the cross-assembly extension
/// pattern used in the `MiniLibFsExt` fixture this is just the augmented
/// type's `entityFqn`. F#-native extensions on generic target types are
/// filtered out upstream in `isProjectableMethod` — the augmented type's
/// typars would need to thread through the receiver rendering, and the
/// Rust-side mirror drops the same shape. The assertions below
/// turn that contract into loud diagnostics if a future change to the
/// filter accidentally lets a generic-receiver member through.
let private renderExtensionReceiver (m: FSharpMemberOrFunctionOrValue) =
    let apparent =
        match m.ApparentEnclosingEntity with
        | Some e -> e
        | None ->
            failwithf
                "fcs-dump entities: F#-native extension member `%s` has no ApparentEnclosingEntity"
                m.LogicalName
    if apparent.GenericParameters.Count > 0 then
        failwithf
            "fcs-dump entities: F#-native extension on generic type `%s` reached \
             renderExtensionReceiver (member `%s`) — `isProjectableMethod` should \
             have dropped this; the receiver renderer cannot thread the augmented \
             type's typars."
            apparent.DisplayName
            m.LogicalName
    entityFqn apparent

/// Render a method's signature as `(p1, p2, ...) -> ret`. For CLR-imported
/// methods FCS always exposes a single parameter group, but F#-curried
/// functions can have multiple — flatten them since the diff contract is
/// the IL parameter list, not the F# currying shape.
///
/// Two FCS-vs-IL adjustments are needed to make the projected signature
/// match the raw MethodDef the Rust side reads:
///
/// 1. Synthetic `unit` parameter on nullary functions. FCS reports a
///    single `unit` parameter (`CurriedParameterGroups = [|[|unit|]|]`)
///    for two source shapes whose compiled IL has zero parameters:
///
///      a. F#-native extension members declared with explicit `()`,
///         e.g. `type T with member this.M() = …`.
///      b. Module-level `let` functions declared with explicit `()`,
///         e.g. `let ping () = 1` in a module. FCS surfaces these as
///         `IsModuleValueOrMember = true && IsMember = false`.
///
///    Critically, F# does NOT erase a *user-named* unit parameter —
///    `let f (u: unit) = …` and `member this.M(u: unit) = …` both
///    compile to a real `Microsoft.FSharp.Core.Unit` IL parameter, and
///    FCS faithfully surfaces it. The two shapes are distinguished by
///    `FSharpParameter.Name`: `None` for the synthetic `()`, `Some "u"`
///    for the user-named binding. Strip only when the name is absent.
///
///    KNOWN LIMITATION (deferred phase): F# *wildcard* unit parameters
///    — `let f (_: unit) = …` — also compile to a real Unit IL parameter
///    (named `_arg1` by the compiler), but FCS reports them with
///    `Name = None`, identical to the synthetic `()` shape. The strip
///    below would erroneously drop them, causing fcs-dump to emit
///    `() -> …` while the Rust side faithfully reads
///    `(Microsoft.FSharp.Core.Unit) -> …` and the diff diverges. No
///    fixture currently exercises this shape; a future phase needs to
///    bridge from the Rust side (e.g. detect IL parameter name
///    `_arg<N>` of type Unit on a method in an F# module entity and
///    drop it there too) since FCS itself does not surface a reliable
///    discriminator at the symbol level.
///
///    Regular F# instance members on a freshly-defined type
///    (`member this.M() = …`) report `CurriedParameterGroups = [|[||]|]`
///    (empty group) instead, so the strip below doesn't fire for them.
///
/// 2. Instance extension members strip the compiled `this` receiver
///    from `CurriedParameterGroups` (the F# surface presents the
///    member as an instance method, but the IL signature has the
///    receiver as the first parameter). Re-prepend it via
///    `renderExtensionReceiver` so the diff agrees.
let private renderMethodSignature
    (numTypeTypars: int)
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (methodNullableContext: byte option)
    (ilMethodDef: obj option)
    (m: FSharpMemberOrFunctionOrValue) =
    let methodTypars : System.Collections.Generic.IList<FSharpGenericParameter> =
        m.GenericParameters
    let parameters =
        m.CurriedParameterGroups
        |> Seq.collect id
        |> Seq.toArray
    // Strip the synthetic `unit` parameter FCS surfaces for nullary
    // F#-native extension members and nullary module-level `let`
    // functions (see (1) in the doc comment above). The `Name.IsNone`
    // check discriminates the synthetic `()` shape from a user-written
    // `(u: unit)` parameter — F# does NOT erase the latter; it emits a
    // real Unit IL parameter, which the Rust side projects. Both
    // kinds of source can carry either shape, so the kind gate AND the
    // anonymity check are both load-bearing.
    let parameters =
        if (m.IsExtensionMember || (m.IsModuleValueOrMember && not m.IsMember))
           && parameters.Length = 1
           && isUnitType parameters.[0].Type
           && parameters.[0].Name.IsNone then
            [||]
        else
            parameters
    // Phase 4m.3: fetch IL parameters / return so we can read
    // NullableAttribute off the raw IL carrier (FCS strips it from
    // `FSharpParameter.Attributes`). The IL parameter list is 1:1 with
    // FCS's `CurriedParameterGroups` except for F#-native instance
    // extension members, whose compiled IL has the receiver as
    // `Parameters[0]` while FCS strips it from the surface — slice it
    // off here so the indices align.
    let ilParameters : obj option [] =
        match ilMethodDef with
        | None -> Array.replicate parameters.Length None
        | Some d ->
            let allIlParams =
                getProp d "Parameters" :?> System.Collections.IEnumerable
                |> Seq.cast<obj>
                |> Seq.toArray
            let aligned =
                if m.IsExtensionMember && m.IsInstanceMember && allIlParams.Length = parameters.Length + 1 then
                    Array.sub allIlParams 1 parameters.Length
                else
                    allIlParams
            if aligned.Length <> parameters.Length then
                failwithf
                    "fcs-dump entities: IL parameter count %d does not match FCS parameter count %d for method `%s`"
                    aligned.Length
                    parameters.Length
                    m.LogicalName
            aligned |> Array.map Some
    let ilReturn : obj option =
        match ilMethodDef with
        | None -> None
        | Some d -> Some (getProp d "Return")
    let renderedParams =
        Array.map2
            (fun ilParam fcsParam ->
                renderParameter numTypeTypars typeTypars methodTypars methodNullableContext ilParam fcsParam)
            ilParameters
            parameters
    // Re-prepend the receiver for instance-style F#-native extension
    // members (see (2) in the doc comment above). Static extension
    // members (`type T with static member …`) skip this branch —
    // their compiled signature has no receiver to re-prepend.
    let paramStrs =
        if m.IsExtensionMember && m.IsInstanceMember then
            Array.append [| renderExtensionReceiver m |] renderedParams
        else
            renderedParams
    let paramStr = paramStrs |> String.concat ", "
    // Constructors return `void` in IL — `.ctor` MethodDef signatures
    // always carry a `void` return type per ECMA-335 §I.8.9.6.6. FCS
    // surfaces the *constructed* type (`() -> Foo`) on its symbol view
    // because that's how F#'s expression-level type system sees `new Foo()`,
    // but the diff oracle compares the IL surface. Collapse to `System.Void`
    // so the projection matches the Rust-side reading of the raw
    // MethodDef. This mirrors the unit-as-void collapse for normal methods
    // in `renderReturnType`: both halves should report the IL truth, not
    // the FCS surface artifact.
    let returnStr =
        if m.IsConstructor then "System.Void"
        else
            renderReturnType
                numTypeTypars
                typeTypars
                methodTypars
                methodNullableContext
                ilReturn
                m.ReturnParameter
    sprintf "(%s) -> %s" paramStr returnStr

/// Project the flag set the Rust-side `normalise_method` emits:
/// - `instance` / `static`
/// - `virtual` if the method participates in dispatch (declares a slot OR
///   overrides one); the Rust side reads ECMA-335's `virtual` bit
///   directly, which is set in both cases, so OR matches it.
/// - `abstract` if the IL method def's `IsAbstract` bit is set (interface
///   members and abstract-class members). Read off the raw `ILMethodDef`
///   to match the Rust-side `method.abstract_member`.
/// - `constructor` if `IsConstructor`
let private memberFlags (ilMethodDef: obj option) (m: FSharpMemberOrFunctionOrValue) =
    // Two adjustments to FCS's view of "instance":
    //
    // 1. Non-constructors: FCS's `IsInstanceMember` is the semantic view —
    //    an F# extension method reads as instance even though it compiles
    //    static. The Rust side reads the raw ECMA-335 calling-convention
    //    `instance` bit, so we want `IsInstanceMemberInCompiledCode`.
    //
    // 2. Constructors: FCS reports both `.ctor` and `.cctor` as not-instance
    //    (you don't "call a constructor on an instance" semantically), but
    //    in IL `.ctor` carries the `instance` calling-convention flag (it
    //    takes a `this` pointer) and `.cctor` does not. Override the
    //    compiled-code answer for those.
    let isInstance =
        if m.IsConstructor then m.LogicalName <> ".cctor"
        else m.IsInstanceMemberInCompiledCode
    let isAbstract =
        match ilMethodDef with
        | Some d -> getProp d "IsAbstract" :?> bool
        | None ->
            // F#-defined members have no raw ILMethodDef to read the bit
            // from (FCS reads its own assemblies through the pickled
            // signature, not IL). An interface member always compiles
            // with the IL `abstract` bit set, so recover it structurally.
            // (Abstract members on F# `[<AbstractClass>]` types would
            // need the same treatment via `not m.HasImplementation`; no
            // fixture exercises them yet, and the generative harness
            // doesn't emit them.)
            m.IsDispatchSlot
            && (match m.DeclaringEntity with
                | Some de -> de.IsInterface
                | None -> false)
    [|
        if isInstance then "instance" else "static"
        if m.IsDispatchSlot || m.IsOverrideOrExplicitInterfaceImplementation then
            "virtual"
        if isAbstract then
            "abstract"
        if m.IsConstructor then
            "constructor"
        // C#-style extension method (`static T M(this U)`): FCS surfaces
        // `[ExtensionAttribute]` through `m.Attributes`, and
        // `IsExtensionMember = false` (FCS reserves that flag for the
        // F#-native form). The Rust side picks up the IL
        // attribute via `detect_extension_attribute`.
        //
        // F#-native INSTANCE extension method (`type T with member
        // this.M …`): FCS sets `IsExtensionMember = true &&
        // IsInstanceMember = true`. The F# compiler does NOT emit
        // `[ExtensionAttribute]` on the compiled MethodDef for
        // augmentations — only on methods that are explicitly
        // decorated with `[<Extension>]`. The Rust side recovers
        // the flag structurally from the IL name mangling
        // (`Counter.M` on a Module-kind class), which is the only
        // signal available in pure IL.
        //
        // F#-native STATIC extensions (`type T with static member …`)
        // are deliberately NOT flagged on either side: FCS reports
        // `IsInstanceMember = false`, and the Rust-side heuristic
        // skips names ending in `.Static`.
        if isCSharpExtensionMethod m || (m.IsExtensionMember && m.IsInstanceMember) then
            "extension"
        // C# 11 `[SetsRequiredMembers]` is presence-only on the FCS side;
        // both the Rust and FCS halves of the diff oracle gate the
        // `sets_required_members` flag purely on the attribute being
        // present, with no further filtering (the model documents that
        // the bit is only *meaningful* on constructors, but the importer
        // surfaces the raw signal faithfully wherever it appears).
        if hasSetsRequiredMembersAttribute m then
            "sets_required_members"
    |]

/// Project an [`FSharpField`] to the JSON shape the Rust normaliser
/// reads. Mirrors `projectMember` for methods.
///
/// Only fires for fields surfaced through the public `FSharpFields`
/// accessor — i.e. F#-defined record/class/struct fields, plus the
/// `value__` row on IL enums. For plain IL-imported classes (the MiniLib
/// shape) FCS's `FSharpFields` returns `[]` and the projection runs
/// through [`projectIlField`] instead, which reads the raw `ILFieldDef`
/// rows via reflection.
///
/// Field-level details:
///
/// - **Flags.** `instance` / `static` and `init_only` for `readonly` —
///   matches the Rust-side normalisation in
///   `crates/assembly/src/test_support.rs`. `IsLiteral` is a C# `const`;
///   ECMA-335 packs it as `static + Literal + !InitOnly`, so the projection
///   drops `init_only` for literals to agree with the Rust side.
let private fieldFlags (f: FSharpField) =
    [|
        if f.IsStatic then "static" else "instance"
        // For C# `const`, FCS reports `IsLiteral = true` and `IsMutable = false`,
        // but ECMA-335's `InitOnly` is not set on a literal. We must NOT
        // emit `init_only` for literals — otherwise the diff would diverge
        // (the Rust side reads the raw bit and sees `init_only = false`).
        // `value__` on an F#-defined enum: FCS reports `IsMutable = false`,
        // but the compiled row carries no `initonly` bit — the Rust side
        // reads the raw bit and sees a plain mutable field. Match the IL.
        // Gate on `IsCompilerGenerated` so a *user* field that happens to
        // be named `value__` (legal in a record) keeps its real
        // `init_only`; only the synthesised enum slot is exempt.
        if not f.IsMutable
           && not f.IsLiteral
           && not (f.IsCompilerGenerated && f.Name = "value__") then
            "init_only"
    |]

let private projectField
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (f: FSharpField) : objnull =
    let emptyTypars : System.Collections.Generic.IList<FSharpGenericParameter> =
        upcast ResizeArray<_>()
    box {| Kind = "Field"
           Name = f.Name
           Signature = renderTypeInScope typeTypars emptyTypars f.FieldType
           Access = accessString f.Accessibility
           Flags = fieldFlags f
           GenericParameters = ([||]: obj array) |}

let private projectMember
    (typeTypars: System.Collections.Generic.IList<FSharpGenericParameter>)
    (numTypeTypars: int)
    (enclosingNullableContext: byte option)
    (m: FSharpMemberOrFunctionOrValue) : objnull =
    // Method generics live on the IL side: FCS's public surface gives us
    // typar *names* via `m.GenericParameters` (used for typar lookups
    // inside the method's signature), but variance and constraints are
    // only on `ILGenericParameterDef`. Walk the raw IL row for the
    // declarations; fall back to the public count for non-IL methods (no
    // such fixture exists yet, but the gap is loud rather than silent).
    let ilMethodDef = tryGetIlMethodDef m
    // Phase 4m.1: a typar with no direct `NullableAttribute` falls back to
    // a `[NullableContextAttribute(byte)]` at the closest enclosing
    // scope. Roslyn emits the attribute at the tightest scope where it
    // saves bytes — method first, then type — and an inner scope's
    // attribute shadows an outer one's. So: prefer the method-level
    // attribute when present; otherwise fall back to the enclosing
    // type's (passed in by `projectEntity`).
    let methodNullableContext : byte option =
        let direct =
            match ilMethodDef with
            | Some d -> readNullableContextAttributeByte (sprintf "method `%s`" m.LogicalName) d
            | None -> None
        match direct with
        | Some _ -> direct
        | None -> enclosingNullableContext
    let genericParameters =
        match ilMethodDef with
        | Some d ->
            ilMethodDefGenericParams d
            |> List.map (projectIlGenericParameter numTypeTypars methodNullableContext)
            |> List.toArray
        | None ->
            // FCS reads its own assemblies through the pickled signature
            // (module vals / FSMeth), so there is no raw ILMethodDef row to
            // walk. Render the typars from the FCS public surface instead:
            // method typars are invariant in IL, and the constraint kinds F#
            // module bindings carry in practice — SRTP member constraints
            // (`let inline addThem (x: ^a) …`), `comparison`/`equality` —
            // are *erased* from IL, so the Rust side (reading the raw rows)
            // sees an unconstrained typar; emit the same. A constraint that
            // WOULD compile into IL (coercion, struct/class, new(),
            // unmanaged, enum, delegate) has no reachable fixture through
            // this path; fail loudly rather than silently diverge from the
            // IL-derived Rust rendering.
            m.GenericParameters
            |> Seq.map (fun (gp: FSharpGenericParameter) ->
                let ilVisible =
                    gp.Constraints
                    |> Seq.filter (fun c ->
                        c.IsCoercesToConstraint
                        || c.IsNonNullableValueTypeConstraint
                        || c.IsReferenceTypeConstraint
                        || c.IsRequiresDefaultConstructorConstraint
                        || c.IsUnmanagedConstraint
                        || c.IsEnumConstraint
                        || c.IsDelegateConstraint)
                    |> Seq.length
                if ilVisible > 0 then
                    failwithf
                        "fcs-dump entities: non-IL generic method `%s` typar `%s` \
                         carries an IL-visible constraint; rendering it from the \
                         FCS public surface is not supported"
                        m.LogicalName
                        gp.Name
                box {| Declaration = gp.Name
                       Constraints = ([||]: string array) |})
            |> Seq.toArray
    // The Rust side carries the raw IL MethodDef name. `CompiledName`
    // always matches that — it's the F#-aware projection of the IL
    // name. `LogicalName` is the source-level identifier and diverges
    // from `CompiledName` for:
    //
    //   - F#-native extension members: `LogicalName = "Tripled"`,
    //     `CompiledName = "Counter.Tripled"` (qualified mangling).
    //
    //   - `[<CompiledName("Foo")>] let bar = …` (or `member …`):
    //     `LogicalName = "bar"`, `CompiledName = "Foo"`.
    //
    //   - F# operators: `LogicalName = "+"`,
    //     `CompiledName = "op_Addition"`.
    //
    // Use `CompiledName` unconditionally so all three cases agree with
    // the IL. The cases where they happen to match (regular F# members,
    // plain module-let bindings without `[<CompiledName>]`) are
    // unaffected.
    let name = m.CompiledName
    // Roslyn pairs `[Obsolete("...", error: true)]` with
    // `[CompilerFeatureRequired("RequiredMembers")]` on every non-
    // `[SetsRequiredMembers]` constructor of a type containing required
    // members. The Obsolete is a pre-C#-11 fallback; modern compilers
    // ignore it. Drop the Obsolete projection only when we see that
    // exact emission shape — a constructor carrying the
    // `"RequiredMembers"` feature gate — so user-authored Obsoletes on
    // ordinary methods or on APIs gated behind other features (e.g.
    // `"RefStructs"`) survive. Mirrors `project_method` in the Rust
    // backend.
    let obsolete : objnull =
        if m.IsConstructor && carriesRequiredMembersFeatureGate m.Attributes then
            null
        else
            match tryFormatObsolete m.Attributes with
            | Some s -> box s
            | None -> null
    let experimental : objnull =
        match tryFormatExperimental m.Attributes with
        | Some s -> box s
        | None -> null
    // Fold any `[CompilerFeatureRequiredAttribute]` gates into the flag set
    // (the Rust normaliser does the same for member kinds). On a
    // constructor the typical gate is `"RequiredMembers"`, paired with the
    // synthetic Obsolete that's dropped above.
    let flags =
        Array.append
            (memberFlags ilMethodDef m)
            (formatCompilerFeatureRequiredList m.Attributes |> List.toArray)
    box {| Kind = "Method"
           Name = name
           Signature = renderMethodSignature numTypeTypars typeTypars methodNullableContext ilMethodDef m
           Access = memberAccessString m
           Flags = flags
           GenericParameters = genericParameters
           Obsolete = obsolete
           Experimental = experimental |}

/// Project one non-namespace entity (a class, struct, interface, etc.) to
/// the JSON shape the Rust-side normaliser parses. Phase 3b emits methods
/// and fields alongside the type skeleton; properties and events arrive
/// in 3c.
let rec private projectEntity (e: FSharpEntity) : objnull =
    let typeTypars : System.Collections.Generic.IList<FSharpGenericParameter> =
        e.GenericParameters
    let emptyMethodTypars : System.Collections.Generic.IList<FSharpGenericParameter> =
        upcast ResizeArray<_>()
    // System.Text.Json's null encoding of `objnull` matches the
    // `"BaseType": null` shape the harness's `Option<String>` deserialiser
    // expects, so we can keep the field a single nullable cell rather than
    // wrapping the type-rendering in a discriminated option.
    let baseType : objnull =
        // Interfaces extend nothing in IL: the TypeDef row's `extends`
        // is null, which is what the Rust side reads. FCS's
        // pickle-backed view of an F#-*defined* interface reports
        // `BaseType = Some System.Object` (the TAST records `obj` as
        // every type's super); force the IL truth so the two sides
        // agree. IL-imported interfaces already report `None` here.
        if e.IsInterface then null
        else
            match e.BaseType with
            | Some t -> box (renderTypeInScope typeTypars emptyMethodTypars t)
            | None -> null

    let interfaces =
        e.DeclaredInterfaces
        |> Seq.map (renderTypeInScope typeTypars emptyMethodTypars)
        |> Seq.toArray

    let nestedTypes =
        e.NestedEntities
        |> Seq.filter (fun n -> not n.IsNamespace)
        |> Seq.map projectEntity
        |> Seq.toArray

    // Methods + fields, concatenated. Order doesn't matter for the diff
    // (the Rust-side normaliser sorts members by `kind`/`name`/`signature`),
    // but emitting both kinds in the same array keeps the JSON shape
    // single-array — there's no separate "Fields" key in the contract.
    //
    // Field source depends on backing:
    //
    // - IL-imported classes: walk `ILTyconRawMetadata.Fields.AsList()`
    //   directly. FCS's public `FSharpFields` only emits a `value__`
    //   row for IL enums; for everything else it returns the F#-side
    //   `AllFieldsAsList`, which is empty for plain IL imports. The
    //   raw walk uses the same `ILFieldDef` rows the Rust side
    //   reads, so the diff stays honest.
    //
    // - F#-defined entities (records, F# classes/structs, modules with
    //   `val` slots): fall back to `FSharpFields`. The
    //   `IsCompilerGenerated` filter drops auto-property backing
    //   fields (`<Count>k__BackingField`) — those are private and
    //   would be filtered by the diff normaliser anyway, but skipping
    //   them here keeps the JSON output cleaner.
    let ilTypeDef = tryGetIlTypeDef e
    // Fire accessors on an event live in `ILTypeDef.Methods` under their
    // own name (no `add_`/`remove_` prefix), and FCS exposes no
    // `IsEventRaiseMethod` predicate — `isProjectableMethod` above will
    // happily emit them as regular methods. The Rust side pulls
    // raise accessors *into* the `Event` via `MethodSemantics`, so the
    // diff would either duplicate them (Method + Event.fire) or, more
    // commonly, surface a method on the FCS side that doesn't exist on
    // the Rust side. C# never emits a `FireMethod`, but ILAsm and
    // managed-C++ do.
    let fireMethodNames : Set<string> =
        match ilTypeDef with
        | Some ilTypeDef ->
            ilTypeDefEvents ilTypeDef
            |> List.choose (fun ev ->
                unwrapOption (tryGetProp ev "FireMethod")
                |> Option.map (fun mref -> getProp mref "Name" :?> string))
            |> Set.ofList
        | None -> Set.empty
    // `numTypeTypars` is the enclosing type's generic arity — IL signatures
    // disambiguate class typars from method typars by index (class typars
    // occupy `[0, numTypeTypars)`; method typars come after). Threaded
    // through every IL-level signature renderer so a `TypeVar n` inside a
    // field/property/event/method signature projects to the right
    // `!T<n>` / `!!M<n>` string. Read from the raw IL row for IL-imported
    // types; the FCS-public `e.GenericParameters.Count` agrees for those
    // but we use the IL count as the authoritative source.
    let numTypeTypars =
        match ilTypeDef with
        | Some il -> List.length (ilTypeDefGenericParams il)
        | None -> e.GenericParameters.Count
    // Phase 4m.1: read the type-level `[NullableContextAttribute(byte)]`
    // up-front so it can serve as the scope-default fallback both for the
    // type's own typars (below) AND for any method on the type whose
    // typars carry no closer attribute. Roslyn elides the per-method
    // attribute when all of the method's typar positions agree with the
    // type-level default, so without this thread-through such typars
    // project as `Oblivious`.
    let typeNullableContext : byte option =
        match ilTypeDef with
        | Some il -> readNullableContextAttributeByte (sprintf "type `%s`" (entityFqn e)) il
        | None -> None
    // Mirror the Rust normaliser's `accessible_from_some_fsharp_code` on
    // the F#-*native* paths. For IL-imported entities FCS itself already
    // applies `AccessibleFromSomeFSharpCode` when enumerating
    // `MembersFunctionsAndValues` (see `GetImmediateIntrinsicMethInfosOfType`),
    // so private/internal members never reach us — but the pickle view of
    // an F#-defined assembly is unfiltered, and a `member private` /
    // `let private` would otherwise project here while the Rust side
    // drops it. FCS's 4-way approximation buckets the kept IL shapes as
    // `IsPublic`/`IsProtected` (protected-internal collapses to
    // Protected, protected ctors to Public — both kept), so the same
    // predicate is a no-op on the IL path; still, gate it to F#-native
    // entities so IL projection provably keeps its raw-bits behaviour.
    let fcsMemberAccessible (m: FSharpMemberOrFunctionOrValue) =
        ilTypeDef.IsSome || m.Accessibility.IsPublic || m.Accessibility.IsProtected
    let methods =
        e.MembersFunctionsAndValues
        |> Seq.filter isProjectableMethod
        |> Seq.filter fcsMemberAccessible
        |> Seq.filter (fun m -> not (Set.contains m.LogicalName fireMethodNames))
        |> Seq.map (projectMember typeTypars numTypeTypars typeNullableContext)
    let fields =
        match ilTypeDef with
        | Some ilTypeDef ->
            ilTypeDefFields ilTypeDef
            |> List.choose (projectIlField numTypeTypars typeNullableContext)
            :> seq<_>
        | None ->
            // `value__` (the enum backing slot every IL enum carries) is
            // `IsCompilerGenerated` on the F#-defined-enum path, but the
            // Rust side reads it off the raw `Field` table like any
            // other row — keep it so the diff agrees. (IL-imported enums
            // take the `Some` branch above, where the raw walk already
            // includes it.)
            e.FSharpFields
            |> Seq.filter (fun f -> not f.IsCompilerGenerated || f.Name = "value__")
            |> Seq.map (projectField typeTypars)
    let properties =
        // IL-imported types walk the raw `ILPropertyDef` rows — the
        // public surface's accessor-accessibility collapse for IL
        // properties (see `Infos.fs:GetterAccessibility`) is exactly the
        // bug the raw walk avoids. F#-*defined* types have no raw rows
        // to walk (FCS reads them through the pickle), so those project
        // property symbols from `MembersFunctionsAndValues` instead;
        // accessor accessibility is not collapsed on that path (the
        // pickle stores the property's own accessibility).
        match ilTypeDef with
        | Some ilTypeDef ->
            let methodDefs = ilTypeDefMethods ilTypeDef
            ilTypeDefProperties ilTypeDef
            |> List.choose (projectIlProperty numTypeTypars typeNullableContext methodDefs)
            :> seq<_>
        | None ->
            e.MembersFunctionsAndValues
            |> Seq.filter (fun m -> m.IsProperty)
            // Same accessibility mirror as `fcsMemberAccessible` above:
            // the pickle view is unfiltered, and the Rust normaliser
            // drops private/internal members.
            |> Seq.filter (fun m -> m.Accessibility.IsPublic || m.Accessibility.IsProtected)
            // FCS synthesises an `IsC0`/`IsC1`/… case-tester property
            // symbol per union case; the Rust side's F#-member filter
            // hides the whole compiler-generated union surface, so drop
            // the testers here too.
            |> Seq.filter (fun m -> not m.IsUnionCaseTester)
            |> Seq.map (fun m ->
                // An indexed property (an F# `Item` indexer) carries its
                // index dimension in `CurriedParameterGroups`; rendering
                // it needs the bracketed `[T] -> Ret` shape plus
                // index-type threading. No fixture exercises one yet —
                // fail loud rather than project a zero-arg shape the
                // Rust side would contradict.
                let hasIndex =
                    m.CurriedParameterGroups
                    |> Seq.exists (fun g -> g.Count > 0)
                if hasIndex then
                    failwithf
                        "fcs-dump entities: F#-defined indexed property `%s` not supported"
                        m.DisplayName
                let flags =
                    [|
                        if m.IsInstanceMemberInCompiledCode then "instance" else "static"
                        if m.HasGetterMethod then "get"
                        if m.HasSetterMethod then "set"
                    |]
                // Render nullability the same way the F#-native method
                // return path does (`renderReturnType`'s no-ILMethodDef
                // branch): nullness-aware inner descent plus the
                // precedence-ladder outer suffix, so a nullness-enabled
                // F# property (`string | null`) carries the same `?` the
                // Rust side reads off the IL `NullableAttribute`.
                let propType = m.ReturnParameter.Type
                let outerNullable =
                    resolveFcsPositionNullability
                        (sprintf "property `%s`" m.DisplayName)
                        m.ReturnParameter.Attributes
                        propType
                        None
                let signature =
                    sprintf
                        "%s%s"
                        (renderTypeInScopeWithInnerNullness typeTypars emptyMethodTypars propType)
                        (nullabilitySuffix outerNullable)
                box {| Kind = "Property"
                       Name = m.DisplayName
                       Signature = signature
                       Access = accessString m.Accessibility
                       Flags = flags
                       GenericParameters = ([||]: obj array) |})
    let events =
        // Phase 3d: same constraint as properties — only the raw IL path.
        // FCS's `ILEvent`-derived `EventInfo` surface hard-codes accessor
        // accessibility to Public (parallel to the ILProp gap), so reading
        // the raw IL is what keeps the diff honest against the Rust side.
        match ilTypeDef with
        | Some ilTypeDef ->
            let methodDefs = ilTypeDefMethods ilTypeDef
            ilTypeDefEvents ilTypeDef
            |> List.choose (projectIlEvent numTypeTypars typeNullableContext methodDefs)
            :> seq<_>
        | None -> Seq.empty
    let members =
        Seq.concat [ methods; fields; properties; events ]
        |> Seq.toArray

    // Generic parameters: phase 3e emits these on every entity (empty
    // array for non-generic types). IL-imported types carry variance +
    // constraints on `ILGenericParameterDef`, which FCS's public
    // `FSharpGenericParameter` surface doesn't expose; reach for the raw
    // IL row through reflection so we can emit the same shape the Rust
    // side does.
    // Phase 4m.1: the type-level `[NullableContextAttribute(byte)]`
    // (computed above for method-typar fallback) is also the scope
    // default for the type's *own* typars when they carry no direct
    // attribute.
    let genericParameters =
        match ilTypeDef with
        | Some il ->
            ilTypeDefGenericParams il
            |> List.map (projectIlGenericParameter numTypeTypars typeNullableContext)
            |> List.toArray
        | None ->
            if e.GenericParameters.Count > 0 then
                failwithf
                    "fcs-dump entities: non-IL generic entity `%s` (arity %d) \
                     not supported — variance/constraints unavailable through \
                     the FCS public surface"
                    (entityFqn e)
                    e.GenericParameters.Count
            [||]

    let obsolete : objnull =
        match tryFormatObsolete e.Attributes with
        | Some s -> box s
        | None -> null
    let experimental : objnull =
        match tryFormatExperimental e.Attributes with
        | Some s -> box s
        | None -> null
    let defaultMember : objnull =
        match tryFormatDefaultMember e.Attributes with
        | Some s -> box s
        | None -> null
    box {| Fqn = entityFqn e
           Kind = entityKindString e
           Access = accessString e.Accessibility
           GenericParameters = genericParameters
           BaseType = baseType
           Interfaces = interfaces
           Members = members
           NestedTypes = nestedTypes
           Obsolete = obsolete
           Experimental = experimental
           DefaultMember = defaultMember
           CompilerFeatureRequired = formatCompilerFeatureRequiredList e.Attributes |> List.toArray |}

/// Walk the FSharpAssemblySignature tree, flattening namespace nodes and
/// emitting one JSON object per real type definition at the top level. A
/// "real type" is any `FSharpEntity` for which `IsNamespace = false`,
/// excluding the ECMA-335 `<Module>` pseudo-class (II.22.37) which holds
/// module-level pseudo-members and isn't a type the LSP would surface.
/// Mirrors the Rust side's `<Module>` skip (`is_skipped_type`); without this
/// both projections would diverge by one extra entity.
let rec private collectTopLevelEntities (entities: System.Collections.Generic.IList<FSharpEntity>) : seq<objnull> =
    seq {
        for e in entities do
            if e.IsNamespace then
                yield! collectTopLevelEntities e.NestedEntities
            elif e.LogicalName = "<Module>" then
                ()
            else
                yield projectEntity e
    }

let private dumpEntities (dllAbsolute: string) =
    // Build minimal FSharpProjectOptions by feeding a synthetic .fsx that
    // does `#r "<dll>"`. ParseAndCheckProject forces FCS to import the
    // referenced assembly; we then pull the matching FSharpAssembly out
    // of GetReferencedAssemblies and walk its Entities tree.
    let checker = FSharpChecker.Create()
    let scriptPath = Path.Combine(Path.GetTempPath(), sprintf "fcs-dump-entities-%s.fsx" (System.Guid.NewGuid().ToString("N")))
    // If the target DLL has sibling `.dll`s in its directory they are
    // (almost certainly) its dependencies that MSBuild's CopyLocal pass
    // dropped next to it — e.g. the `MiniLibFsExt` fixture sits next to
    // `MiniLib.dll` because the augmentations target `MiniLib.Counter`.
    // Without referencing them explicitly FCS can't resolve cross-DLL
    // symbols and member enumeration throws
    // `entity ... is in an unresolved assembly`. `#r` each sibling DLL
    // up front; FCS de-duplicates against the target.
    //
    // Skip files that aren't managed assemblies (native interop DLLs
    // commonly ship in the same output directory on Windows). FCS
    // surfaces a native DLL via `#r` as a fatal script diagnostic, which
    // would mask the real assembly load below. `AssemblyName.GetAssemblyName`
    // probes the metadata header without holding a file lock; it throws
    // `BadImageFormatException` on non-PE files and `FileLoadException`
    // on PE files without managed metadata, both of which we treat as
    // "not a managed assembly".
    let isManagedAssembly (path: string) =
        try
            System.Reflection.AssemblyName.GetAssemblyName(path) |> ignore
            true
        with _ -> false
    let dir = Path.GetDirectoryName dllAbsolute |> Option.ofObj
    let extraRefs =
        match dir with
        | None -> [||]
        | Some d when not (Directory.Exists d) -> [||]
        | Some d ->
            Directory.GetFiles(d, "*.dll")
            |> Array.filter (fun p ->
                not (String.Equals(Path.GetFullPath p, Path.GetFullPath dllAbsolute, System.StringComparison.OrdinalIgnoreCase)))
            |> Array.filter isManagedAssembly
    let refLines =
        extraRefs
        |> Array.map (fun p -> sprintf "#r @\"%s\"\n" p)
        |> String.concat ""
    let source = SourceText.ofString (sprintf "%s#r @\"%s\"\n()\n" refLines dllAbsolute)

    let opts, scriptDiags =
        checker.GetProjectOptionsFromScript(
            scriptPath,
            source,
            assumeDotNetFramework = false,
            useSdkRefs = true)
        |> Async.RunSynchronously

    let blocking =
        scriptDiags
        |> List.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
    if not (List.isEmpty blocking) then
        let summary =
            blocking
            |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
            |> String.concat "; "
        failwithf "fcs-dump entities: script resolution failed: %s" summary

    let projResults =
        checker.ParseAndCheckProject(opts, userOpName = "fcs-dump-entities")
        |> Async.RunSynchronously

    // Match referenced assemblies by file name. FCS normalises paths
    // internally but case-insensitive compare on the absolute path is the
    // robust choice on all three host platforms we care about.
    let target =
        projResults.ProjectContext.GetReferencedAssemblies()
        |> List.tryFind (fun a ->
            match a.FileName with
            | Some f ->
                String.Equals(
                    Path.GetFullPath f,
                    Path.GetFullPath dllAbsolute,
                    System.StringComparison.OrdinalIgnoreCase)
            | None -> false)

    match target with
    | None ->
        failwithf "fcs-dump entities: %s not found in FCS-resolved assembly list" dllAbsolute
    | Some asm ->
        let entities =
            collectTopLevelEntities asm.Contents.Entities
            |> Seq.toArray

        let payload =
            {| Assembly = asm.SimpleName
               Entities = entities |}

        let json = JsonSerializer.Serialize(payload, buildOptions ())
        Console.Out.Write(json)
        Console.Out.WriteLine()

// ============================================================================
// Symbol-uses dump (sema name-resolution differential test oracle)
// ============================================================================

/// Extra `-r:` reference arguments from `BORZOI_FCS_EXTRA_REFS`
/// (`;`- or newline-separated absolute `.dll` paths). Lets a `uses` /
/// `uses-project` caller make a fixture assembly resolvable without putting an
/// offset-shifting `#r` line in the source the differential also feeds to the
/// Rust parser.
let private extraRefArgs () : string[] =
    match Option.ofObj (Environment.GetEnvironmentVariable("BORZOI_FCS_EXTRA_REFS")) with
    | None -> [||]
    | Some paths ->
        paths.Split([| ';'; '\n' |], StringSplitOptions.RemoveEmptyEntries)
        |> Array.map (fun p -> "-r:" + p.Trim())

/// `-r:` reference arguments from an explicit list — the resident-batch sibling
/// of [`extraRefArgs`], which reads them from `BORZOI_FCS_EXTRA_REFS`. A resident
/// child is spawned once and serves callers whose fixture refs differ, so the
/// refs must ride in each *request* (the env channel is fixed at spawn); this
/// turns one request's `refs` list into the same `-r:` switches `extraRefArgs`
/// would have produced from the env var.
let private extraRefArgsOf (refs: string list) : string[] =
    refs
    |> List.map (fun p -> "-r:" + p.Trim())
    |> List.filter (fun s -> s <> "-r:")
    |> List.toArray

/// Normalise one symbol use to the JSON shape the sema differential consumes:
/// the symbol's display name, the use range, whether it is the defining
/// occurrence, the declaration range (`null` for an out-of-any-file symbol),
/// and — for resolving into referenced assemblies — the declaring assembly's
/// simple name and the symbol's full name (`null` when FCS cannot produce
/// them). The latter two let the consumer match an assembly resolution without
/// a usable in-file declaration range.
let private projectSymbolUse (u: FSharpSymbolUse) =
    let declRange : objnull =
        match u.Symbol.DeclarationLocation with
        | Some r -> box r
        | None -> null
    let assemblyName : objnull =
        try box u.Symbol.Assembly.SimpleName with _ -> null
    let fullName : objnull =
        try box u.Symbol.FullName with _ -> null
    {| SymbolName = u.Symbol.DisplayName
       Range = u.Range
       IsFromDefinition = u.IsFromDefinition
       DeclRange = declRange
       Assembly = assemblyName
       FullName = fullName |}

let private projectDiagnostic (d: FSharp.Compiler.Diagnostics.FSharpDiagnostic) =
    {| Severity = d.Severity.ToString()
       Message = d.Message
       ErrorNumber = d.ErrorNumber
       Range = d.Range |}

/// Classify one symbol use for the *bucket census* (the Phase-3 scoping
/// measurement, `crates/sema/tests/uses_census.rs`). Emits the FCS facts that
/// determine *what machinery* a resolver needs to resolve this use — the symbol
/// category plus the member / instance / extension / overload flags — and lets
/// the Rust analyser bucket them (lexical / shallow-inference / hard-pile).
///
/// Every field read is guarded: `GetAllUsesOfAllSymbolsInFile` on a
/// partially-erroring file (the common case when a corpus file is checked in
/// isolation, with no siblings) can return symbols whose property accessors
/// throw. `IsOverloaded` is computed structurally — does the declaring entity
/// hold more than one member of this logical name — so it reflects the *symbol*,
/// not the use site, and stays meaningful even when cross-file context is
/// missing.
let private projectSymbolUseCensus (u: FSharpSymbolUse) =
    let sym = u.Symbol
    let mfv =
        match sym with
        | :? FSharpMemberOrFunctionOrValue as m -> Some m
        | _ -> None
    let ent =
        match sym with
        | :? FSharpEntity as e -> Some e
        | _ -> None
    let cls =
        match sym with
        | :? FSharpMemberOrFunctionOrValue -> "Mfv"
        | :? FSharpEntity -> "Entity"
        | :? FSharpField -> "Field"
        | :? FSharpUnionCase -> "UnionCase"
        | :? FSharpActivePatternCase -> "ActivePatternCase"
        | :? FSharpGenericParameter -> "GenericParameter"
        | :? FSharpParameter -> "Parameter"
        | _ -> "Other"
    let mb (f: FSharpMemberOrFunctionOrValue -> bool) =
        match mfv with
        | Some m -> (try f m with _ -> false)
        | None -> false
    let isOverloaded =
        match mfv with
        | Some m ->
            try
                if m.IsMember then
                    match m.DeclaringEntity with
                    | Some e ->
                        let nm = m.LogicalName
                        let count =
                            e.MembersFunctionsAndValues
                            |> Seq.filter (fun x -> (try x.LogicalName = nm with _ -> false))
                            |> Seq.length
                        count > 1
                    | None -> false
                else
                    false
            with _ ->
                false
        | None -> false
    // Carry the use range and (in-file-or-not) declaration range too — the
    // census proper buckets by *machinery* and ignores them, but the
    // resolution corpus-diff (`crates/sema/tests/resolve_corpus_diff.rs`) needs
    // them to check *which binder* our resolver points at. Same shape and
    // boxing as `projectSymbolUse` (a `range` carries its `File`, so the Rust
    // side tells an in-file declaration from a referenced-assembly one).
    let declRange : objnull =
        match u.Symbol.DeclarationLocation with
        | Some r -> box r
        | None -> null
    {| SymbolName = u.Symbol.DisplayName
       Range = u.Range
       DeclRange = declRange
       IsFromDefinition = u.IsFromDefinition
       Class = cls
       IsMember = mb (fun m -> m.IsMember)
       IsInstance = mb (fun m -> m.IsInstanceMember)
       IsExtension = mb (fun m -> m.IsExtensionMember)
       IsProperty = mb (fun m -> m.IsProperty)
       IsConstructor = mb (fun m -> m.IsConstructor)
       IsModuleValueOrMember = mb (fun m -> m.IsModuleValueOrMember)
       IsValue = mb (fun m -> m.IsValue)
       IsActivePattern = mb (fun m -> m.IsActivePattern)
       IsFunction =
        (match mfv with
         | Some m -> (try m.CurriedParameterGroups.Count > 0 with _ -> false)
         | None -> false)
       IsOverloaded = isOverloaded
       IsNamespace =
        (match ent with
         | Some e -> (try e.IsNamespace with _ -> false)
         | None -> false)
       IsModule =
        (match ent with
         | Some e -> (try e.IsFSharpModule with _ -> false)
         | None -> false) |}

/// Dump every symbol use FCS records for a single file, as the oracle for
/// the `sema` name resolver. Per use we emit `{ SymbolName, Range,
/// IsFromDefinition, DeclRange, Assembly, FullName }`, where `DeclRange` is the
/// symbol's declaration location (`null` when the symbol declares outside any
/// file, e.g. a compiler-intrinsic) and `Assembly`/`FullName` identify a
/// referenced-assembly symbol. The `range`s carry their `File`, so the
/// consumer can tell an in-file declaration from a referenced-assembly one.
let private dumpUses (absolute: string) =
    let text = File.ReadAllText(absolute)
    let sourceText = SourceText.ofString text
    let checker = FSharpChecker.Create()

    // Resolve options as for a single-file script: this pulls the SDK
    // references (FSharp.Core, the runtime) without an .fsproj, so the file
    // actually type-checks and name resolution runs. The parser subset under
    // test is a subset of what scripts accept, so resolution of *in-file*
    // binders is unaffected by script mode; any use FCS resolves into an
    // implicit open or a referenced assembly is out of this slice's scope and
    // the Stage C consumer is permitted to leave it `Deferred`.
    let opts0, scriptDiags =
        checker.GetProjectOptionsFromScript(
            absolute,
            sourceText,
            assumeDotNetFramework = false,
            useSdkRefs = true)
        |> Async.RunSynchronously

    // Append any fixture references (`BORZOI_FCS_EXTRA_REFS`) so a snippet
    // can reference a test assembly's types.
    let opts =
        { opts0 with
            OtherOptions = Array.append opts0.OtherOptions (extraRefArgs ()) }

    let blocking =
        scriptDiags
        |> List.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
    if not (List.isEmpty blocking) then
        let summary =
            blocking
            |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
            |> String.concat "; "
        failwithf "fcs-dump uses: script resolution failed: %s" summary

    let _parseResults, checkAnswer =
        checker.ParseAndCheckFileInProject(absolute, 0, sourceText, opts, userOpName = "fcs-dump-uses")
        |> Async.RunSynchronously

    let checkResults =
        match checkAnswer with
        | FSharpCheckFileAnswer.Succeeded r -> r
        | FSharpCheckFileAnswer.Aborted -> failwith "fcs-dump uses: type-check aborted"

    let uses =
        checkResults.GetAllUsesOfAllSymbolsInFile()
        |> Seq.map projectSymbolUse
        |> Seq.toArray

    let payload = {| Uses = uses |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

// ============================================================================
// Attribute-resolution dump (EX-3 §2(d) attribute-resolution oracle)
// ============================================================================

/// Chase a type-abbreviation chain to its terminal (non-abbreviation) entity.
/// `None` — never an intermediate abbreviation — when there is no terminal to
/// report: the chain is opaque to the symbols API (an abbreviation of a
/// non-nominal type, a property read that throws — including
/// `IsFSharpAbbreviation` itself, whose failure means we cannot even tell
/// whether the chain has ended) or longer than `fuel` (a bound so a
/// pathological chain cannot hang the oracle). A consumer keys decisions on
/// the terminal, so an unfinished chase must decline loudly rather than lie.
let rec private chaseAbbreviation (fuel: int) (e: FSharpEntity) : FSharpEntity option =
    let isAbbreviation =
        try
            Some e.IsFSharpAbbreviation
        with _ ->
            None

    match isAbbreviation with
    | None -> None
    | Some false -> Some e
    | Some true ->
        if fuel = 0 then
            None
        else
            let target =
                try
                    let t = e.AbbreviatedType
                    if t.HasTypeDefinition then Some t.TypeDefinition else None
                with _ ->
                    None

            match target with
            | Some t -> chaseAbbreviation (fuel - 1) t
            | None -> None

/// Every syntactic attribute-name range in the parse tree, collected by a
/// generic structural walk over the `ParsedInput` object graph (unions,
/// records, tuples, and sequences reach every `SynAttribute`; the dozens of
/// attribute-bearing syntactic positions — modules, types, members, bindings,
/// parameters, union cases, `[<assembly: …>]` targets — are enumerated by the
/// type structure itself, not by hand). The name-resolution sink alone cannot
/// identify attributes: `TcNameOfExpr` resolves a type argument of `nameof`
/// with the same `ItemOccurrence.UseInAttribute`, so `dumpAttrs` intersects
/// the sink records with these syntactic ranges.
let private collectAttributeNameRanges (parsed: ParsedInput) =
    let found = System.Collections.Generic.HashSet<range>()
    let bindings = BindingFlags.Public ||| BindingFlags.NonPublic

    let rec walk (o: objnull) =
        match o with
        // `None` is represented as `null`, as are genuinely-null fields.
        | null -> ()
        | :? SynAttribute as attr -> found.Add attr.TypeName.Range |> ignore
        // Leaves the structural walk must not descend into: a `string` is an
        // `IEnumerable` of chars, and ranges/positions carry nothing below.
        | :? string
        | :? range
        | :? pos -> ()
        | o ->
            let t = o.GetType()

            if t.IsPrimitive || t.IsEnum then
                ()
            elif FSharpType.IsUnion(t, bindings) then
                FSharpValue.GetUnionFields(o, t, bindings) |> snd |> Array.iter walk
            elif FSharpType.IsRecord(t, bindings) then
                FSharpValue.GetRecordFields(o, bindings) |> Array.iter walk
            elif FSharpType.IsTuple t then
                FSharpValue.GetTupleFields o |> Array.iter walk
            else
                match o with
                | :? System.Collections.IEnumerable as xs ->
                    for x in xs do
                        walk x
                // Any other class (an `Ident`, an `XmlDoc`) cannot contain a
                // `SynAttribute` below it.
                | _ -> ()

    walk (box parsed)
    found

/// Normalise one attribute-type resolution — an `ItemOccurrence.UseInAttribute`
/// entity use at a syntactic attribute-name range — to the JSON shape the
/// attribute differential consumes. The range is the *written* attribute name
/// (FCS's synthesized `…Attribute` suffix candidate reuses the written ident's
/// range); `Assembly`/`FullName` identify the entity the name resolved to, and
/// `TargetAssembly`/`TargetFullName` the terminal entity after chasing a
/// type-abbreviation chain — the currency for recognising
/// `type MyExt = ExtensionAttribute` aliases. The `Target*` fields equal
/// `Assembly`/`FullName` when the resolution is not an abbreviation, and are
/// `null` when no terminal is available (an opaque or over-long chain, see
/// [`chaseAbbreviation`]) — a consumer must read `null` as unknowable, never
/// as "not an attribute of interest".
let private attrUse (u: FSharpSymbolUse) (e: FSharpEntity) =
    // The resolved entity's declaration range, `null` when it declares outside
    // any file — same shape and purpose as `projectSymbolUse`'s: an *in-file*
    // attribute type (a project-declared `type MyExtAttribute = …`) has no
    // referenced-assembly identity to match on, so the differential compares
    // the declaration site instead.
    let declRange: objnull =
        match u.Symbol.DeclarationLocation with
        | Some r -> box r
        | None -> null

    let assemblyName: objnull =
        try
            box e.Assembly.SimpleName
        with _ ->
            null

    let fullName: objnull =
        try
            box e.FullName
        with _ ->
            null

    let terminal = chaseAbbreviation 1024 e

    let targetAssembly: objnull =
        match terminal with
        | Some t ->
            try
                box t.Assembly.SimpleName
            with _ ->
                null
        | None -> null

    let targetFullName: objnull =
        match terminal with
        | Some t ->
            try
                box t.FullName
            with _ ->
                null
        | None -> null

    {| SymbolName = e.DisplayName
       Range = u.Range
       DeclRange = declRange
       Assembly = assemblyName
       FullName = fullName
       TargetAssembly = targetAssembly
       TargetFullName = targetFullName
       // `true` when FCS legitimately sank DISTINCT entities at this range —
       // e.g. an attribute on a type parameter (`type R<[<Literal>] 'T>`)
       // records both the built-in special attribute and a same-named local.
       // The oracle then names neither: the record marks the range as an
       // attribute with NO claim about its target, so a differential can
       // neither confirm nor refute a commitment there.
       Ambiguous = false |}

/// The deduped, source-ordered attribute records of one checked file — the
/// shared projection behind [`dumpAttrs`] and [`dumpAttrsBatchCore`]; see
/// [`dumpAttrs`] for the full contract (the syntactic-range intersection, the
/// ctor filter, the speculative-pass collapse, the loud ambiguity failure).
let private collectAttrRecords (parsed: ParsedInput) (checkResults: FSharpCheckFileResults) =
    let attrNameRanges = collectAttributeNameRanges parsed

    checkResults.GetAllUsesOfAllSymbolsInFile()
    |> Seq.filter (fun u -> u.IsFromAttribute && attrNameRanges.Contains u.Range)
    |> Seq.choose (fun u ->
        match u.Symbol with
        | :? FSharpEntity as e -> Some(u, e)
        | _ -> None)
    |> Seq.toArray
    |> Array.groupBy (fun (u, _) -> u.Range)
    |> Array.map (fun (range, group) ->
        let rendered = group |> Array.map (fun (u, e) -> attrUse u e) |> Array.distinct

        match rendered with
        | [| one |] -> one
        | several ->
            // Distinct entities at one range is a real FCS shape: an
            // attribute on a type parameter records both the built-in
            // special attribute and a same-named local. Emit an
            // `Ambiguous` no-claim record rather than aborting the dump —
            // see the field's doc on [`attrUse`].
            {| several.[0] with
                DeclRange = (null: objnull)
                Assembly = (null: objnull)
                FullName = (null: objnull)
                TargetAssembly = (null: objnull)
                TargetFullName = (null: objnull)
                Ambiguous = true |})
    |> Array.sortBy (fun r -> r.Range.StartLine, r.Range.StartColumn)

/// Dump the type each written attribute resolved to, as the oracle for the
/// sema attribute-resolution differential (EX-3 §2(d)). FCS resolves an
/// attribute in `ResolveAttributeType` — the written last segment with the
/// `Attribute` suffix appended is tried first, then the name as written,
/// through the general `ResolveTypeLongIdent` — and records the winning
/// entity to the sink as an `ItemOccurrence.UseInAttribute` use at the
/// written name's range. This op projects exactly those records out of
/// `GetAllUsesOfAllSymbolsInFile`:
///
/// - the occurrence alone is not enough — `TcNameOfExpr` resolves a type
///   argument of `nameof` with the same occurrence — so the records are also
///   intersected with the parse tree's syntactic attribute-name ranges
///   ([`collectAttributeNameRanges`]);
/// - each attribute *also* records its constructor at the last segment's
///   range as a plain `Use`; the `IsFromAttribute` + entity filter drops it;
/// - an attribute FCS cannot resolve sinks nothing (decline by absence), so
///   `Errors` (every `Severity=Error` diagnostic) is emitted alongside to let
///   a consumer distinguish a clean "no attributes" from a failed check;
/// - the file's own module/namespace attributes are checked speculatively
///   (`TcAttributesCanFail`) before the real pass and both sink; identical
///   re-records are collapsed, and two *different* resolutions at one range
///   fail loudly — an ambiguous oracle is worse than none.
let private dumpAttrs (absolute: string) =
    let text = File.ReadAllText(absolute)
    let sourceText = SourceText.ofString text
    let checker = FSharpChecker.Create()

    // Script-mode options, exactly as `dumpUses`: SDK references without an
    // .fsproj, plus any fixture references from `BORZOI_FCS_EXTRA_REFS`.
    let opts0, scriptDiags =
        checker.GetProjectOptionsFromScript(
            absolute,
            sourceText,
            assumeDotNetFramework = false,
            useSdkRefs = true)
        |> Async.RunSynchronously

    let opts =
        { opts0 with
            OtherOptions = Array.append opts0.OtherOptions (extraRefArgs ()) }

    let blocking =
        scriptDiags
        |> List.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)

    if not (List.isEmpty blocking) then
        let summary =
            blocking
            |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
            |> String.concat "; "

        failwithf "fcs-dump attrs: script resolution failed: %s" summary

    let parseResults, checkAnswer =
        checker.ParseAndCheckFileInProject(absolute, 0, sourceText, opts, userOpName = "fcs-dump-attrs")
        |> Async.RunSynchronously

    let checkResults =
        match checkAnswer with
        | FSharpCheckFileAnswer.Succeeded r -> r
        | FSharpCheckFileAnswer.Aborted -> failwith "fcs-dump attrs: type-check aborted"

    let attrs = collectAttrRecords parseResults.ParseTree checkResults

    let errors =
        checkResults.Diagnostics
        |> Array.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
        |> Array.map (fun d ->
            {| Line = d.StartLine
               Code = d.ErrorNumber
               Message = d.Message |})

    let payload = {| Attrs = attrs; Errors = errors |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Tolerant batch sibling of [`dumpAttrs`], for the generative and corpus
/// attribute differentials (one resident child, many snippets — .NET startup
/// paid once). Reads source paths from stdin, one per line; for each,
/// type-checks it in isolation with the SDK reference set harvested once (by
/// script resolution on the first usable path, plus `BORZOI_FCS_EXTRA_REFS`)
/// and emits one compact JSON line `{ Path, Ok, Error, Attrs, Errors }` with
/// the same per-attribute shape as `attrs`. Never aborts on a type error —
/// `Errors` carries them, exactly as the single-file op does.
let private dumpAttrsBatchCore () =
    let options = buildOptionsCompact ()
    let checker = FSharpChecker.Create()

    let mutable baseOpts: FSharpProjectOptions option = None
    let mutable switches: string[] = [||]

    let ensureRefs (absolute: string) =
        if baseOpts.IsNone then
            try
                let t = SourceText.ofString (File.ReadAllText absolute)

                let so, _ =
                    checker.GetProjectOptionsFromScript(
                        absolute, t, assumeDotNetFramework = false, useSdkRefs = true)
                    |> Async.RunSynchronously

                baseOpts <- Some so

                switches <-
                    so.OtherOptions
                    |> Array.filter (fun o -> o.StartsWith("-") || o.StartsWith("/"))
                    |> fun s -> Array.append s (extraRefArgs ())
            with _ ->
                ()

    let mutable line = Console.In.ReadLine()

    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()

        if path <> "" then
            let absolute = Path.GetFullPath path
            ensureRefs absolute

            let json =
                try
                    match baseOpts with
                    | None ->
                        JsonSerializer.Serialize(
                            {| Path = path
                               Ok = false
                               Error = "reference resolution failed"
                               Attrs = ([||]: obj[])
                               Errors = ([||]: obj[]) |},
                            options
                        )
                    | Some opts0 ->
                        let sourceText = SourceText.ofString (File.ReadAllText absolute)

                        let dir =
                            Option.ofObj (Path.GetDirectoryName absolute) |> Option.defaultValue "."

                        let projOpts =
                            { opts0 with
                                ProjectFileName = Path.Combine(dir, "fcs-dump-attrs.fsproj")
                                SourceFiles = [| absolute |]
                                OtherOptions = switches
                                UseScriptResolutionRules = false }

                        let parseResults, checkAnswer =
                            checker.ParseAndCheckFileInProject(
                                absolute, 0, sourceText, projOpts, userOpName = "fcs-dump-attrs-batch")
                            |> Async.RunSynchronously

                        match checkAnswer with
                        | FSharpCheckFileAnswer.Succeeded r ->
                            let attrs = collectAttrRecords parseResults.ParseTree r

                            let errors =
                                r.Diagnostics
                                |> Array.filter (fun d ->
                                    d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
                                |> Array.map (fun d ->
                                    {| Line = d.StartLine
                                       Code = d.ErrorNumber
                                       Message = d.Message |})

                            JsonSerializer.Serialize(
                                {| Path = path
                                   Ok = true
                                   Error = ""
                                   Attrs = attrs
                                   Errors = errors |},
                                options
                            )
                        | FSharpCheckFileAnswer.Aborted ->
                            JsonSerializer.Serialize(
                                {| Path = path
                                   Ok = false
                                   Error = "type-check aborted"
                                   Attrs = ([||]: obj[])
                                   Errors = ([||]: obj[]) |},
                                options
                            )
                with ex ->
                    JsonSerializer.Serialize(
                        {| Path = path
                           Ok = false
                           Error = ex.Message
                           Attrs = ([||]: obj[])
                           Errors = ([||]: obj[]) |},
                        options
                    )

            Console.Out.WriteLine(json)
            Console.Out.Flush()

        line <- Console.In.ReadLine()

/// Batch attrs driver on a large-stack thread, mirroring [`dumpUsesCensusBatch`]:
/// type-checking a deeply-nested corpus file can overflow the default 1 MB
/// stack and kill the whole batch, so give it 512 MB of headroom.
let private dumpAttrsBatch () =
    let mutable captured: exn option = None

    let worker =
        System.Threading.Thread(
            (fun () ->
                try
                    dumpAttrsBatchCore ()
                with ex ->
                    captured <- Some ex),
            512 * 1024 * 1024
        )

    worker.Start()
    worker.Join()

    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Project-aware sibling of `dumpUses`: type-check an ordered set of source
/// files as a *single project* (not isolated scripts), so name resolution sees
/// cross-file declarations. Reads absolute source paths from stdin, one per
/// line, in Compile order (F# cross-file order is load-bearing). Emits, per
/// file, the same per-use shape as `uses` — but a use's `DeclRange.File` now
/// faithfully names the *declaring* file, which may differ from the use's file.
///
/// The reference set (FSharp.Core + the runtime) is harvested the way the
/// single-file `uses` path does — via script resolution on the first file —
/// then those `-r:` switches are reused for a real multi-file project so the
/// source-file set is exactly the Compile-ordered list given on stdin.
/// Shared core of the `uses-project` oracle: type-check `paths` (Compile order)
/// as ONE project against the given `checker`, folding `refArgs` / `defineArgs`
/// / `langVersionArgs` into the compiler switches, and return the per-file
/// `{ Path, Diagnostics, Uses }` records. Fails hard on a reference-resolution
/// error or a per-file abort — the one-shot [`dumpUsesProject`] lets that exit
/// the process nonzero; the resident [`usesProjectBatchCore`] catches it into a
/// `BatchError` line so one bad project cannot wedge the resident child.
///
/// The switch handling (verbatim from the pre-refactor one-shot): keep only
/// compiler switches from the script options (the `-r:` references and flags such
/// as `--targetprofile`); drop any source-file entries so the explicit,
/// Compile-ordered `SourceFiles` is the sole file set. Append the fixture refs,
/// the caller's `#if` symbols (`defineArgs`) so FCS parses the same conditional
/// branches the caller's own parser did, and the pinned `--langversion`
/// (`langVersionArgs`) so both sides take the same version-gated syntax branches.
/// `--define`/`--langversion` are additive to FCS's implicit symbols (e.g.
/// `COMPILED`); any `--langversion` script resolution injected is filtered out
/// first so the pinned one is the sole (last-wins-safe) version switch.
let private usesProjectFiles
    (checker: FSharpChecker)
    (paths: string[])
    (refArgs: string[])
    (defineArgs: string[])
    (langVersionArgs: string[])
    =
    let firstText = SourceText.ofString (File.ReadAllText(paths.[0]))
    let scriptOpts, scriptDiags =
        checker.GetProjectOptionsFromScript(
            paths.[0],
            firstText,
            assumeDotNetFramework = false,
            useSdkRefs = true,
            otherFlags = Array.append defineArgs langVersionArgs)
        |> Async.RunSynchronously

    let blocking =
        scriptDiags
        |> List.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
    if not (List.isEmpty blocking) then
        let summary =
            blocking
            |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
            |> String.concat "; "
        failwithf "fcs-dump uses-project: reference resolution failed: %s" summary

    let otherOptions =
        scriptOpts.OtherOptions
        |> Array.filter (fun o -> o.StartsWith("-") || o.StartsWith("/"))
        |> Array.filter (fun o ->
            not (o.StartsWith("--langversion:") || o.StartsWith("/langversion:")))
        |> fun switches -> Array.concat [ switches; refArgs; defineArgs; langVersionArgs ]

    let projOpts =
        { scriptOpts with
            ProjectFileName =
                let dir = Option.ofObj (Path.GetDirectoryName(paths.[0])) |> Option.defaultValue "."
                Path.Combine(dir, "fcs-dump-uses-project.fsproj")
            SourceFiles = paths
            OtherOptions = otherOptions
            UseScriptResolutionRules = false }

    paths
    |> Array.map (fun absolute ->
        let sourceText = SourceText.ofString (File.ReadAllText(absolute))
        let parseResults, checkAnswer =
            checker.ParseAndCheckFileInProject(
                absolute,
                0,
                sourceText,
                projOpts,
                userOpName = "fcs-dump-uses-project")
            |> Async.RunSynchronously

        let checkResults =
            match checkAnswer with
            | FSharpCheckFileAnswer.Succeeded r -> r
            | FSharpCheckFileAnswer.Aborted ->
                failwithf "fcs-dump uses-project: type-check aborted for %s" absolute

        let diagnostics =
            Array.append parseResults.Diagnostics checkResults.Diagnostics
            |> Array.map projectDiagnostic
        let uses =
            checkResults.GetAllUsesOfAllSymbolsInFile()
            |> Seq.map projectSymbolUse
            |> Seq.toArray

        {| Path = absolute
           Diagnostics = diagnostics
           Uses = uses |})

let private dumpUsesProject () =
    let paths =
        let acc = ResizeArray<string>()
        let mutable line = Console.In.ReadLine()
        while not (isNull line) do
            let p = (Option.ofObj line |> Option.defaultValue "").Trim()
            if p <> "" then acc.Add(Path.GetFullPath p)
            line <- Console.In.ReadLine()
        acc.ToArray()

    if Array.isEmpty paths then
        failwith "fcs-dump uses-project: no source files on stdin"

    // `#if` symbols the caller wants defined (`BORZOI_FCS_DEFINES`,
    // `;`/newline-separated) as `--define:` flags; the pinned `<LangVersion>`
    // (`BORZOI_FCS_LANGVERSION`) as a `--langversion:` flag. Both are threaded
    // through [`usesProjectFiles`] to *both* script-option discovery and the
    // per-file check — see its doc-comment. (Unset langversion means the SDK
    // default, which FCS already agrees with, so no flag.)
    let defineArgs : string[] =
        match Option.ofObj (Environment.GetEnvironmentVariable "BORZOI_FCS_DEFINES") with
        | None -> [||]
        | Some s ->
            s.Split([| ';'; '\n' |], StringSplitOptions.RemoveEmptyEntries)
            |> Array.map (fun d -> "--define:" + d.Trim())

    let langVersionArgs : string[] =
        match Option.ofObj (Environment.GetEnvironmentVariable "BORZOI_FCS_LANGVERSION") with
        | None -> [||]
        | Some s when s.Trim() = "" -> [||]
        | Some s -> [| "--langversion:" + s.Trim() |]

    let checker = FSharpChecker.Create()
    let files = usesProjectFiles checker paths (extraRefArgs ()) defineArgs langVersionArgs
    let payload = {| Files = files |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Resident sibling of [`dumpUsesProject`]: the multi-file `uses-project`
/// projection driven as a [`fileBatchCore`]-style oracle for `sema`'s per-project
/// differential loops (`resolve_project_diff`, `resolve_straddle_gen_diff`,
/// `resolve_project_assembly_diff`, the fold matrices), which otherwise spawn one
/// `dotnet fcs-dump uses-project` per project and pay the .NET + FCS cold-start
/// every time. One JSON request per stdin line —
/// `{ "paths": [<abs .fs>…], "refs": [<dll>…], "defines": [<sym>…],
/// "langversion": <token|null> }`, Compile order preserved — and one compact
/// `{ "Files": [ { Path, Diagnostics, Uses } … ] }` response line: the SAME
/// payload the one-shot emits (compact rather than indented; the `serde_json`
/// consumer is whitespace-insensitive). Per-request failures become
/// `{ "BatchError": <msg> }`, exactly as in `file-batch`.
let private usesProjectBatchCore () =
    let checker = FSharpChecker.Create()
    let compact = buildOptionsCompact ()

    let respond (line: string) : string =
        try
            use doc = JsonDocument.Parse(line: string)
            let root = doc.RootElement
            let strArray (name: string) : string[] =
                match root.TryGetProperty(name) with
                | true, arr when arr.ValueKind = JsonValueKind.Array ->
                    arr.EnumerateArray()
                    |> Seq.choose (fun e -> Option.ofObj (e.GetString()))
                    |> Seq.toArray
                | _ -> [||]
            let paths = strArray "paths" |> Array.map Path.GetFullPath
            if Array.isEmpty paths then
                failwith "no paths in request"
            let refArgs = strArray "refs" |> Array.toList |> extraRefArgsOf
            let defineArgs = strArray "defines" |> Array.map (fun d -> "--define:" + d.Trim())
            let langVersionArgs =
                match root.TryGetProperty("langversion") with
                | true, v when v.ValueKind = JsonValueKind.String ->
                    match Option.ofObj (v.GetString()) with
                    | Some s when s.Trim() <> "" -> [| "--langversion:" + s.Trim() |]
                    | _ -> [||]
                | _ -> [||]
            let files = usesProjectFiles checker paths refArgs defineArgs langVersionArgs
            JsonSerializer.Serialize({| Files = files |}, compact)
        with ex ->
            JsonSerializer.Serialize({| BatchError = ex.Message |}, compact)

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let trimmed = (Option.ofObj line |> Option.defaultValue "").Trim()
        if trimmed <> "" then
            Console.Out.WriteLine(respond trimmed)
            Console.Out.Flush()
        line <- Console.In.ReadLine()

/// [`usesProjectBatchCore`] on a large-stack thread, mirroring [`fileBatch`]:
/// a deeply-nested project's typed tree can recurse past the default 1 MB stack
/// and take the resident child down with an uncatchable `StackOverflowException`.
let private usesProjectBatch () =
    let mutable captured: exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try usesProjectBatchCore ()
                with ex -> captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Tolerant, per-file census driver for the Phase-3 scoping measurement
/// (`crates/sema/tests/uses_census.rs`). Reads source paths from stdin, one per
/// line, and for each type-checks it *in isolation* as a single-file project —
/// reusing the SDK reference set harvested once (by script resolution on the
/// first usable path) — then emits one compact JSON line
/// `{ Path, Ok, Error, Uses: [classified-use] }`.
///
/// Unlike `uses` / `uses-project`, this NEVER aborts on a type error. A corpus
/// file checked without its siblings cannot resolve cross-file names, but
/// `GetAllUsesOfAllSymbolsInFile` still returns every use FCS *could* resolve
/// (locals, parameters, same-file definitions, FSharp.Core / BCL members) —
/// exactly the population the census measures. The member-needing fraction is
/// therefore a *lower bound* (cross-file member targets on unresolved sibling
/// types drop out), but the hardness split *among resolved members* (overloaded
/// / extension vs simple) is intrinsic to each symbol and so unbiased.
let private dumpUsesCensusBatchCore () =
    let options = buildOptionsCompact ()
    let checker = FSharpChecker.Create()

    // Harvested lazily from the first path that resolves: the SDK `-r:` switches
    // (FSharp.Core + the runtime) plus any `BORZOI_FCS_EXTRA_REFS`. Reused
    // for every file so the expensive script resolution runs at most once.
    let mutable baseOpts: FSharpProjectOptions option = None
    let mutable switches: string[] = [||]
    let ensureRefs (absolute: string) =
        if baseOpts.IsNone then
            try
                let t = SourceText.ofString (File.ReadAllText absolute)
                let so, _ =
                    checker.GetProjectOptionsFromScript(
                        absolute, t, assumeDotNetFramework = false, useSdkRefs = true)
                    |> Async.RunSynchronously
                baseOpts <- Some so
                switches <-
                    so.OtherOptions
                    |> Array.filter (fun o -> o.StartsWith("-") || o.StartsWith("/"))
                    |> fun s -> Array.append s (extraRefArgs ())
            with _ ->
                ()

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()
        if path <> "" then
            let absolute = Path.GetFullPath path
            ensureRefs absolute
            let json =
                try
                    match baseOpts with
                    | None ->
                        JsonSerializer.Serialize(
                            {| Path = path
                               Ok = false
                               Error = "reference resolution failed"
                               Uses = ([||]: obj[]) |}, options)
                    | Some opts0 ->
                        let sourceText = SourceText.ofString (File.ReadAllText absolute)
                        let dir =
                            Option.ofObj (Path.GetDirectoryName absolute)
                            |> Option.defaultValue "."
                        let projOpts =
                            { opts0 with
                                ProjectFileName = Path.Combine(dir, "fcs-dump-census.fsproj")
                                SourceFiles = [| absolute |]
                                OtherOptions = switches
                                UseScriptResolutionRules = false }
                        let _parse, checkAnswer =
                            checker.ParseAndCheckFileInProject(
                                absolute, 0, sourceText, projOpts, userOpName = "fcs-dump-census")
                            |> Async.RunSynchronously
                        match checkAnswer with
                        | FSharpCheckFileAnswer.Succeeded r ->
                            let uses =
                                r.GetAllUsesOfAllSymbolsInFile()
                                |> Seq.map projectSymbolUseCensus
                                |> Seq.toArray
                            JsonSerializer.Serialize(
                                {| Path = path; Ok = true; Error = ""; Uses = uses |}, options)
                        | FSharpCheckFileAnswer.Aborted ->
                            JsonSerializer.Serialize(
                                {| Path = path
                                   Ok = false
                                   Error = "type-check aborted"
                                   Uses = ([||]: obj[]) |}, options)
                with ex ->
                    JsonSerializer.Serialize(
                        {| Path = path; Ok = false; Error = ex.Message; Uses = ([||]: obj[]) |}, options)
            Console.Out.WriteLine(json)
            Console.Out.Flush()
        line <- Console.In.ReadLine()

/// Batch census driver on a large-stack thread, mirroring [`dumpAstBatch`]:
/// type-checking a deeply-nested file can recurse far enough to overflow the
/// default 1 MB stack and kill the whole batch, so give it 512 MB of headroom.
let private dumpUsesCensusBatch () =
    let mutable captured: exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try
                    dumpUsesCensusBatchCore ()
                with ex ->
                    captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Project-mode sibling of [`dumpUsesCensusBatchCore`]: type-check the
/// Compile-ordered paths from stdin as a *single project* (cross-file names
/// resolve), then emit one `{ Path, Ok, Uses }` JSON line per file with the same
/// classified-use shape. The unbiased counterpart for the Phase-3 census's
/// isolation-bias probe (`crates/sema/tests/uses_census_project.rs`): running a
/// real project's files this way *and* standalone, on the same set, exposes how
/// many member accesses the standalone batch loses to unresolved siblings.
///
/// References come from script resolution on the first file (FSharp.Core + the
/// runtime ref pack) plus `BORZOI_FCS_EXTRA_REFS`, exactly as
/// [`dumpUsesProject`]. A genuinely missing dependency only *depresses* the
/// in-project count, so the measured bias stays a conservative lower bound.
let private dumpUsesCensusProjectCore () =
    let options = buildOptionsCompact ()

    let paths =
        let acc = ResizeArray<string>()
        let mutable line = Console.In.ReadLine()
        while not (isNull line) do
            let p = (Option.ofObj line |> Option.defaultValue "").Trim()
            if p <> "" then acc.Add(Path.GetFullPath p)
            line <- Console.In.ReadLine()
        acc.ToArray()

    if Array.isEmpty paths then
        failwith "fcs-dump uses-census-project: no source files on stdin"

    let checker = FSharpChecker.Create()

    let firstText = SourceText.ofString (File.ReadAllText(paths.[0]))
    let scriptOpts, _scriptDiags =
        checker.GetProjectOptionsFromScript(
            paths.[0], firstText, assumeDotNetFramework = false, useSdkRefs = true)
        |> Async.RunSynchronously

    let otherOptions =
        scriptOpts.OtherOptions
        |> Array.filter (fun o -> o.StartsWith("-") || o.StartsWith("/"))
        |> fun switches -> Array.append switches (extraRefArgs ())

    let projOpts =
        { scriptOpts with
            ProjectFileName =
                let dir = Option.ofObj (Path.GetDirectoryName(paths.[0])) |> Option.defaultValue "."
                Path.Combine(dir, "fcs-dump-census-project.fsproj")
            SourceFiles = paths
            OtherOptions = otherOptions
            UseScriptResolutionRules = false }

    for absolute in paths do
        let json =
            try
                let sourceText = SourceText.ofString (File.ReadAllText absolute)
                let _parse, checkAnswer =
                    checker.ParseAndCheckFileInProject(
                        absolute, 0, sourceText, projOpts, userOpName = "fcs-dump-census-project")
                    |> Async.RunSynchronously
                match checkAnswer with
                | FSharpCheckFileAnswer.Succeeded r ->
                    let uses =
                        r.GetAllUsesOfAllSymbolsInFile()
                        |> Seq.map projectSymbolUseCensus
                        |> Seq.toArray
                    JsonSerializer.Serialize(
                        {| Path = absolute; Ok = true; Error = ""; Uses = uses |}, options)
                | FSharpCheckFileAnswer.Aborted ->
                    JsonSerializer.Serialize(
                        {| Path = absolute
                           Ok = false
                           Error = "type-check aborted"
                           Uses = ([||]: obj[]) |}, options)
            with ex ->
                JsonSerializer.Serialize(
                    {| Path = absolute; Ok = false; Error = ex.Message; Uses = ([||]: obj[]) |}, options)
        Console.Out.WriteLine(json)
        Console.Out.Flush()

let private dumpUsesCensusProject () =
    let mutable captured: exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try
                    dumpUsesCensusProjectCore ()
                with ex ->
                    captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

// ============================================================================
// Expression-type dump (sema type-inference oracle / Phase-3 type census)
// ============================================================================

/// Whether a member is overloaded: its declaring entity holds more than one
/// member of the same logical name. Computed structurally — mirroring
/// `projectSymbolUseCensus`'s `isOverloaded` — so it reflects the *symbol*, not
/// the use site, and stays meaningful when a corpus file is checked without its
/// siblings.
let private isOverloadedMember (m: FSharpMemberOrFunctionOrValue) : bool =
    try
        match m.DeclaringEntity with
        | Some e ->
            let nm = m.LogicalName
            (e.MembersFunctionsAndValues
             |> Seq.filter (fun x -> (try x.LogicalName = nm with _ -> false))
             |> Seq.length) > 1
        | None -> false
    with _ -> false

/// Classify one *elaborated* expression node by the machinery a resolver needs
/// to assign it a type — the type-side analogue of `projectSymbolUseCensus`'s
/// name-resolution taxonomy. The Rust census (`crates/sema/tests/types_census.rs`)
/// folds these tags into literal / HM-spine / member-lookup / hard-pile buckets.
///
/// The tree is FCS's *reduced* typed tree (pattern matches lowered to decision
/// trees, pipelines/CEs/optional-args desugared), so the population is the
/// elaborated-node set, not the source-syntax set — stated as a bias on the
/// census, exactly as the uses census states its isolation/corpus biases.
let private classifyExpr (e: FSharpExpr) : string =
    let g (m: FSharpMemberOrFunctionOrValue) f = try f m with _ -> false
    let callTag (m: FSharpMemberOrFunctionOrValue) =
        if not (g m (fun m -> m.IsMember)) then
            // A module-bound `let` value compiles to a property getter, so a
            // *reference* to one is reified as `Call(None, getter, …)` with no
            // parameter groups — a lexical leaf, not a function application.
            // Split it out so `call:function` means a genuine call.
            let isFunc = try m.CurriedParameterGroups.Count > 0 with _ -> false
            if isFunc then "call:function" else "value:module"
        elif g m (fun m -> m.IsExtensionMember) then "call:extension"
        elif g m (fun m -> m.IsInstanceMember) then
            if isOverloadedMember m then "call:instance-overloaded" else "call:instance"
        elif isOverloadedMember m then "call:static-overloaded"
        else "call:static"
    match e with
    | Const _ -> "const"
    | Value _ -> "value"
    | TraitCall _ -> "trait-call"
    | Call(_, m, _, _, _) -> callTag m
    | NewObject(m, _, _) -> if isOverloadedMember m then "new-object-overloaded" else "new-object"
    | Application _ -> "application"
    | Lambda _ -> "lambda"
    | TypeLambda _ -> "type-lambda"
    | FSharpFieldGet(objOpt, _, _) ->
        match objOpt with
        | Some _ -> "field-get:instance"
        | None -> "field-get:static"
    | AnonRecordGet _ -> "anon-record-get"
    | UnionCaseGet _ -> "union-case-get"
    | IfThenElse _ -> "if"
    | Let _ -> "let"
    | LetRec _ -> "let-rec"
    | NewTuple _ -> "new-tuple"
    | NewRecord _ -> "new-record"
    | NewAnonRecord _ -> "new-anon-record"
    | NewUnionCase _ -> "new-union-case"
    | NewArray _ -> "new-array"
    | NewDelegate _ -> "new-delegate"
    | Coerce _ -> "coerce"
    | TypeTest _ -> "type-test"
    | Sequential _ -> "sequential"
    | TupleGet _ -> "tuple-get"
    | DecisionTree _ -> "decision-tree"
    | DecisionTreeSuccess _ -> "decision-tree-success"
    | TryWith _ -> "try-with"
    | TryFinally _ -> "try-finally"
    | WhileLoop _ -> "while"
    | FastIntegerForLoop _ -> "for"
    | ObjectExpr _ -> "object-expr"
    | Quote _ -> "quote"
    | ThisValue _ -> "this-value"
    | BaseValue _ -> "base-value"
    | DefaultValue _ -> "default-value"
    | ValueSet _ -> "value-set"
    | AddressOf _ -> "address-of"
    | AddressSet _ -> "address-set"
    | FSharpFieldSet _ -> "field-set"
    | UnionCaseTag _ -> "union-case-tag"
    | UnionCaseTest _ -> "union-case-test"
    | UnionCaseSet _ -> "union-case-set"
    | WitnessArg _ -> "witness-arg"
    | ILFieldGet(objOpt, _, _) ->
        // A .NET field read: an *instance* read needs the receiver type to find
        // the field (member lookup), exactly like `FSharpFieldGet`.
        match objOpt with
        | Some _ -> "il-field-get:instance"
        | None -> "il-field-get:static"
    | ILFieldSet _ -> "il-field-set"
    | ILAsm _ -> "il-asm"
    | _ -> "other"

/// Render an expression's inferred type for the oracle. Uses FCS's own
/// pretty-printer (`FSharpType.Format`), which — unlike the entity-metadata
/// `renderType` — handles function / tuple / measure types and never throws;
/// the *canonical* renderer that the Rust `Ty` will diff against is the Phase-3.1
/// "Ty representation" open question, deferred until that lands.
let private renderExprType (t: FSharpType) : string =
    try t.Format(FSharpDisplayContext.Empty) with _ -> "?"

/// The canonical name of the type parameter at position `i` — the **shared
/// convention** with the Rust side's `Ty::typar_name` (`crates/sema/src/ty.rs`):
/// `'a`, `'b`, …, `'z` for the first 26, then a fixed overflow tail `'t26`,
/// `'t27`, … past `'z`. We control both sides of this oracle, so the scheme is
/// chosen once and mirrored — a generalised scheme then compares by string
/// equality regardless of FCS's arbitrary internal typar names.
let private canonicalTyparName (i: int) : string =
    if i < 26 then sprintf "'%c" (char (int 'a' + i)) else sprintf "'t%d" i

/// Canonical type rendering for the inference differential
/// (`infer_literals_diff` / `infer_binder_types_diff`): abbreviation-resolved BCL
/// FQNs (`System.Int32`, `System.Byte[]`) — the *same* convention the
/// entity-metadata [`renderType`] emits, so the Rust `Ty` renderer can match it
/// byte-for-byte. A **reference tuple** renders `a * b` with FQN elements (nested
/// tuples parenthesised), a **function** `dom -> ran`, and a **generic parameter**
/// renders canonically by first appearance (`'a`, `'b`, …) — the same numbering
/// `Ty::render` emits for a generalised scheme's `Ty::Param`s (Stage 3.2c-2c).
///
/// This lives *here*, not in the shared [`renderType`], so it stays confined to
/// the inference `TypeCanon` path and does not change the assembly-metadata dump's
/// tuple/typar currency (which keeps the IL `System.Tuple<…>` / positional-typar
/// forms the Rust assembly normaliser mirrors). A **measure** or **SRTP**
/// (`^`-static-req) typar renders *distinctively* — `<measure:name>` /
/// `<srtp:name>` — a shape the Rust side never emits, so a wrong emission on our
/// side fails loudly instead of matching by accident. Falls back to FCS's `Format`
/// for shapes [`renderType`] still does not model (struct tuples, measures on
/// named types), which inference never produces.
let private renderTypeCanonical (t: FSharpType) : string =
    // A rename table threaded through one top-level render call, mapping each
    // generic-parameter *name* to its canonical index by first appearance. Fresh
    // per call, so distinct binders each start their numbering at `'a`.
    let rename = System.Collections.Generic.Dictionary<string, int>()
    let canonTypar (name: string) : string =
        match rename.TryGetValue name with
        | true, i -> canonicalTyparName i
        | false, _ ->
            let i = rename.Count
            rename.[name] <- i
            canonicalTyparName i
    let rec go (t: FSharpType) : string =
        if t.IsTupleType && not t.IsStructTupleType then
            t.GenericArguments
            |> Seq.map (fun a ->
                let s = go a
                // Parenthesise a tuple or function element so the flat ` * ` join
                // stays unambiguous, matching `Ty::render`'s `render_tuple`
                // (`* ` binds tighter than `->`, so a bare function element would
                // be mis-grouped as `a -> (b * c)`).
                if (a.IsTupleType && not a.IsStructTupleType) || a.IsFunctionType then
                    sprintf "(%s)" s
                else
                    s)
            |> String.concat " * "
        elif t.IsFunctionType then
            // `dom -> ran`, matching `Ty::render`'s `render_fun`: `->` is right-
            // associative and looser than `*`, so the range is never parenthesised
            // (a curried `a -> b -> c` reads flat via recursion on the range) and
            // the domain is parenthesised only when it is itself a function
            // (`(a -> b) -> c`); a tuple domain (`a * b -> c`) needs none.
            let args = t.GenericArguments |> Seq.toArray
            if args.Length = 2 then
                let dom = go args.[0]
                let dom = if args.[0].IsFunctionType then sprintf "(%s)" dom else dom
                sprintf "%s -> %s" dom (go args.[1])
            else
                try renderType t with _ -> renderExprType t
        elif t.IsGenericParameter then
            let tp = t.GenericParameter
            // A measure or SRTP typar renders distinctively — a shape the Rust
            // side never produces, so any accidental match is impossible.
            if tp.IsMeasure then sprintf "<measure:%s>" tp.Name
            elif tp.IsSolveAtCompileTime then sprintf "<srtp:%s>" tp.Name
            else canonTypar tp.Name
        else
            try renderType t with _ -> renderExprType t
    go t

/// Walk one implementation file's typed tree, emitting `{ Range, Kind, Type,
/// TypeCanon }` for every expression node carrying a real source location in
/// *this* file. `canonical` controls whether the (exception-prone) `TypeCanon`
/// field is computed: the single-file `types` oracle needs it for the inference
/// differential; the whole-corpus `types-census-batch` does not and passes
/// `false` to avoid the per-node exception cost.
///
/// Two filters keep the population hover-faithful rather than tree-faithful:
///   * Compiler-synthesised nodes (range in another file, or zero-width) are
///     dropped — they are not hover targets.
///   * Nodes sharing an *identical* source range are de-duplicated, keeping the
///     first seen. The walk is pre-order, so that is the outermost node — the
///     one a hover at that span resolves to. This collapses the elaboration
///     fan-out of `inline` operators (e.g. `a + b` reifies `op_Addition` as a
///     stack of same-range lambdas/applications) down to the single source
///     expression, so the census counts source spans, not IL-shaped artifacts.
let private collectExprTypes (canonical: bool) (impl: FSharpImplementationFileContents) =
    let acc = ResizeArray<_>()
    let seen = System.Collections.Generic.HashSet<struct (int * int * int * int)>()
    let emit (e: FSharpExpr) =
        let r = e.Range
        let zeroWidth = r.StartLine = r.EndLine && r.StartColumn = r.EndColumn
        if r.FileName = impl.FileName && not zeroWidth then
            let key = struct (r.StartLine, r.StartColumn, r.EndLine, r.EndColumn)
            if seen.Add(key) then
                let canon = if canonical then renderTypeCanonical e.Type else ""
                acc.Add(
                    {| Range = r
                       Kind = classifyExpr e
                       Type = renderExprType e.Type
                       TypeCanon = canon |})
    let rec walkExpr (e: FSharpExpr) =
        emit e
        for sub in e.ImmediateSubExpressions do
            walkExpr sub
    let rec walkDecl (d: FSharpImplementationFileDeclaration) =
        match d with
        | FSharpImplementationFileDeclaration.Entity(_, sub) -> List.iter walkDecl sub
        | FSharpImplementationFileDeclaration.MemberOrFunctionOrValue(_, _, body) -> walkExpr body
        | FSharpImplementationFileDeclaration.InitAction e -> walkExpr e
    List.iter walkDecl impl.Declarations
    acc.ToArray()

/// Type-check a single file *as a script* with `keepAssemblyContents = true`
/// (so the elaborated typed tree is retained) and return its
/// [`FSharpImplementationFileContents`] **together with the check's own
/// diagnostics**. `label` names the caller in error messages. Fails hard (as a
/// single-file oracle should) if *script resolution* errors, the check aborts,
/// or no implementation file comes back — but **type** errors are returned, not
/// thrown: a file FCS could not fully check still yields a (partial) elaborated
/// tree, which is exactly the population the oracles measure.
///
/// The diagnostics matter to the OV-9 differential
/// (`crates/sema/tests/overload_corpus_diff.rs`) and they are load-bearing, not
/// decoration: **an elaborated `Call` node does not mean FCS *resolved* the
/// call.** FCS's single-`IsCandidate` shortcut
/// (`docs/overload-resolution-plan.md` §2.2) commits the lone arity-surviving
/// candidate with **no applicability test**, so `M("x")` against a sole
/// `M(int)` still elaborates a `Call` to `M(int)` — while raising an argument
/// type error. A consumer that reads "FCS chose this overload, therefore FCS
/// found it applicable" is wrong on exactly those sites; the error lines are how
/// it tells the two apart.
let private checkScriptImplFileWithDiags
    (label: string)
    (absolute: string)
    : FSharpImplementationFileContents * FSharp.Compiler.Diagnostics.FSharpDiagnostic[] =
    let text = File.ReadAllText(absolute)
    let sourceText = SourceText.ofString text
    let checker = FSharpChecker.Create(keepAssemblyContents = true)

    let opts0, scriptDiags =
        checker.GetProjectOptionsFromScript(
            absolute,
            sourceText,
            assumeDotNetFramework = false,
            useSdkRefs = true)
        |> Async.RunSynchronously

    let opts =
        { opts0 with
            OtherOptions = Array.append opts0.OtherOptions (extraRefArgs ()) }

    let blocking =
        scriptDiags
        |> List.filter (fun d -> d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
    if not (List.isEmpty blocking) then
        let summary =
            blocking
            |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
            |> String.concat "; "
        failwithf "fcs-dump %s: script resolution failed: %s" label summary

    let _parseResults, checkAnswer =
        checker.ParseAndCheckFileInProject(
            absolute, 0, sourceText, opts, userOpName = "fcs-dump-" + label)
        |> Async.RunSynchronously

    let checkResults =
        match checkAnswer with
        | FSharpCheckFileAnswer.Succeeded r -> r
        | FSharpCheckFileAnswer.Aborted -> failwithf "fcs-dump %s: type-check aborted" label

    match checkResults.ImplementationFile with
    | Some impl -> impl, checkResults.Diagnostics
    | None ->
        failwithf "fcs-dump %s: no implementation file (keepAssemblyContents not honoured?)" label

/// [`checkScriptImplFileWithDiags`] without the diagnostics — the shape the
/// `types` / `binder-types` oracles want.
let private checkScriptImplFile (label: string) (absolute: string) : FSharpImplementationFileContents =
    fst (checkScriptImplFileWithDiags label absolute)

/// Dump every typed expression node FCS produces for a single file, as the
/// oracle for the `sema` type-inference layer (Phase 3) and the type census.
let private dumpTypes (absolute: string) =
    let impl = checkScriptImplFile "types" absolute
    let exprs = collectExprTypes true impl
    let payload = {| File = absolute; Exprs = exprs |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Walk one implementation file's typed tree, emitting `{ Range, Name,
/// TypeCanon }` for every **binder** — a value / function / member declaration
/// and each of its curried parameters — carrying a real source location in this
/// file. The oracle for Phase-3 *binder-type* inference
/// (`crates/sema/tests/infer_binder_types_diff.rs`): a function value has no
/// expression node of its own (its type lives on the binder), so the expression
/// [`collectExprTypes`] oracle cannot reach it — this dumps the binder side
/// directly. `Range` is the binder's declaration location, matching the
/// `sema` resolver's `Def::range`, so the two are keyed the same way.
let private collectBinderTypes (impl: FSharpImplementationFileContents) =
    let acc = ResizeArray<_>()
    let emit (mfv: FSharpMemberOrFunctionOrValue) =
        // `.DeclarationLocation` / `.FullType` can throw on a broken symbol;
        // stay tolerant (skip it) rather than fail the whole dump.
        try
            let r = mfv.DeclarationLocation
            let zeroWidth = r.StartLine = r.EndLine && r.StartColumn = r.EndColumn
            if r.FileName = impl.FileName && not zeroWidth then
                let canon = try renderTypeCanonical mfv.FullType with _ -> ""
                acc.Add(
                    {| Range = r
                       Name = mfv.LogicalName
                       TypeCanon = canon |})
        with _ -> ()
    let rec walkDecl (d: FSharpImplementationFileDeclaration) =
        match d with
        | FSharpImplementationFileDeclaration.Entity(_, sub) -> List.iter walkDecl sub
        | FSharpImplementationFileDeclaration.MemberOrFunctionOrValue(mfv, curriedArgs, _body) ->
            emit mfv
            // The curried parameter groups (`[[a]; [b]]` for `let f a b`,
            // `[[a; b]]` for a tupled `let f (a, b)`): each parameter is itself a
            // binder with its own declaration range and type.
            for group in curriedArgs do
                for p in group do
                    emit p
        | FSharpImplementationFileDeclaration.InitAction _ -> ()
    List.iter walkDecl impl.Declarations
    acc.ToArray()

/// Dump every binder's inferred type for a single file — the Phase-3
/// binder-type oracle (see [`collectBinderTypes`]).
let private dumpBinderTypes (absolute: string) =
    let impl = checkScriptImplFile "binder-types" absolute
    let binders = collectBinderTypes impl
    let payload = {| File = absolute; Binders = binders |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Render the fully-qualified name of the entity that declares `m`, for the
/// overloads oracle's `DeclaringType` field. Tolerant: an abbreviation entity
/// (or a symbol whose declaring entity is unavailable) yields `""`. Per §3.1 of
/// `docs/overload-resolution-plan.md` this field is *informational* — the
/// elaborated `mfv` can be override-retargeted to a base declaration, so the
/// Rust differential compares by signature (`XmlDocSig` + params), tolerating a
/// declaring entity that is a base of the one it records.
let private declaringTypeName (m: FSharpMemberOrFunctionOrValue) : string =
    try
        match m.DeclaringEntity with
        | Some e ->
            match e.TryFullName with
            | Some n -> n
            | None -> e.DisplayName
        | None -> ""
    with _ -> ""

/// Walk one implementation file's typed tree, emitting one record per
/// **invocation node** (`Call` / `NewObject`) carrying a real source location in
/// this file — the oracle for overload resolution (Stage OV-1 of
/// `docs/overload-resolution-plan.md`). Each record identifies the overload FCS
/// *chose*: its `XmlDocSig` names it in one string, and `Params`/`Return`
/// (canonical rendering, the same currency as the `types` / `binder-types`
/// oracles) give the resolved signature the Rust engine will later diff against.
///
/// **Contract — range-keyed, like the `types` oracle.** The output is the set of
/// *invocation* nodes FCS elaborated, keyed by range; a consumer selects the
/// node at the **range of the source call it is resolving** (exactly how
/// `parse_fcs_types` is consumed). It is deliberately **not** a curated list of
/// user-written call sites: the elaborated tree contains compiler-**synthesized
/// invocations** — an implicit base constructor (`type C() = …` emits a
/// `System.Object..ctor` `NewObject` on the type-name range), an inserted
/// widening/`op_Implicit` conversion (`c.M(3)` where the parameter is `float`
/// emits a `float`/`op_Implicit` call), an **eta-expanded bare function
/// reference** (`let g = f` becomes `fun x -> f x`, emitting a `Call` to `f` on
/// the bare-identifier range), etc. These are genuine invocations, so they are
/// kept; a range-keyed consumer never queries their ranges, so they are
/// harmless. Distinguishing "user-written" from "synthesized" is not something
/// FCS exposes cleanly (the synthesized base ctor calls `Object`'s *real*,
/// non-compiler-generated ctor), so the line drawn here is *invocation vs
/// non-invocation*, not *source vs synthesized*.
///
/// Population/emission notes, all from §3.1 of the plan (verified while
/// probing):
///   * The node's `mfv` **is** the chosen overload — no separate resolution
///     step needed on this side.
///   * `Kind` is emitted for diagnostics but the differential must **not** gate
///     on it: `isOverloadedMember` undercounts (a call classified `call:instance`
///     may still be an overloaded name), so the engine runs on every call node.
///   * Out-arg / tuple-return folding can make a source call emit **no** `Call`
///     node at all (P12) — a missing record is a legitimate outcome the Rust
///     side must tolerate, not an assertion failure.
///   * Same range-dedup and synthesised-node filters as [`collectExprTypes`],
///     so a hover at the call span maps to the emitted record.
///   * **Non-invocation reads are excluded** (`isInvocation`): a *plain*
///     property/event accessor read (`s.Length` ⇒ `get_Length`) and a
///     module-bound `let` **value** read (`let y = x`) are reified as `Call`
///     nodes but are not invocations, so they are dropped. An **indexer**
///     accessor (`h.[i]` ⇒ `get_Item(i)`, distinguished from a plain property by
///     its index parameters), a module *function* call, and a **constructor**
///     (`NewObject`) — including the compiler-synthesized implicit base ctor —
///     ARE invocations and are kept (see the range-keyed contract above).
///   * **`Params`/`Return` are best-effort for a *generic* overload.**
///     [`renderTypeCanonical`] canonicalises a *bare* generic parameter (`'a`,
///     `'b`, …) but has no enclosing typar scope for one buried inside a generic
///     *instantiation* (`'T list`), so such a type falls back to FCS display
///     text (`'T Microsoft.FSharp.Collections.list`) — non-canonical and
///     sensitive to the source typar name. Canonical generic-instantiation
///     rendering is the deferred "Ty generic args" work
///     (`docs/overload-resolution-plan.md` §7), and the engine defers generic
///     winners in v1 (§5), so this is not blocking. **Consumers must key a
///     generic overload on `XmlDocSig`** — which *is* canonical and stable
///     (``M:C.M`1(…FSharpList{`0})``, independent of `'T`/`'U`).
let private collectOverloads (impl: FSharpImplementationFileContents) =
    let acc = ResizeArray<_>()
    let seen = System.Collections.Generic.HashSet<struct (int * int * int * int)>()
    let emit (e: FSharpExpr) (m: FSharpMemberOrFunctionOrValue) =
        let r = e.Range
        let zeroWidth = r.StartLine = r.EndLine && r.StartColumn = r.EndColumn
        if r.FileName = impl.FileName && not zeroWidth then
            let key = struct (r.StartLine, r.StartColumn, r.EndLine, r.EndColumn)
            if seen.Add(key) then
                // Curried parameter groups, each parameter's type in canonical
                // form (`System.Int32`, `'a`, …). A .NET method has one group; a
                // curried F# member has several.
                let paramGroups =
                    try
                        m.CurriedParameterGroups
                        |> Seq.map (fun group ->
                            group
                            |> Seq.map (fun p -> try renderTypeCanonical p.Type with _ -> "?")
                            |> Seq.toArray)
                        |> Seq.toArray
                    with _ -> [||]
                let ret = try renderTypeCanonical m.ReturnParameter.Type with _ -> "?"
                let xmlDocSig = try m.XmlDocSig with _ -> ""
                acc.Add(
                    {| Range = r
                       Name = (try m.LogicalName with _ -> "")
                       Kind = classifyExpr e
                       DeclaringType = declaringTypeName m
                       Params = paramGroups
                       Return = ret
                       XmlDocSig = xmlDocSig |})
    // `isInvocation` drops the *non-invocation reads* FCS reifies as `Call`s: a
    // property/event accessor (`s.Length` ⇒ `get_Length`) and a module-bound
    // `let` value read (`let y = x`). This *population* filter is distinct from
    // gating the differential on `Kind` (which consumers must not do). It does NOT
    // (and cannot cleanly) filter *synthesized invocations* — an implicit base
    // ctor, an inserted conversion, or an eta-expanded bare function reference
    // (`let g = f`); those are covered by the range-keyed contract instead (a
    // consumer keys on the source call's range and never queries theirs).
    let g (m: FSharpMemberOrFunctionOrValue) f = try f m with _ -> false
    let paramCount (m: FSharpMemberOrFunctionOrValue) =
        try m.CurriedParameterGroups |> Seq.sumBy (fun grp -> grp.Count) with _ -> 0
    let isInvocation (m: FSharpMemberOrFunctionOrValue) =
        // A property/event accessor read/write is a member access, not an
        // overload site — EXCEPT an *indexer* accessor (`h.[i]` ⇒ `get_Item`,
        // `h.[i] <- v` ⇒ `set_Item`), which carries index parameters and IS a
        // real (possibly overloaded) call site FCS classifies `call:instance*`.
        // Distinguish by accessor arity: a plain getter has 0 params (an indexer
        // getter ≥ 1); a plain setter has 1 param — the value — (an indexer
        // setter ≥ 2, indices + value). Keep the indexer forms, drop the plain
        // reads/writes and events.
        let isPlainGetter = g m (fun m -> m.IsPropertyGetterMethod) && paramCount m = 0
        let isPlainSetter = g m (fun m -> m.IsPropertySetterMethod) && paramCount m <= 1
        let isEventAccessor =
            g m (fun m -> m.IsEventAddMethod) || g m (fun m -> m.IsEventRemoveMethod)
        // A module-bound `let` **value** read (`let y = x`) reifies as an
        // argument-less getter `Call` with no parameter groups — a lexical leaf,
        // not an invocation. (A bare module *function* reference `let g = f` is
        // NOT caught here — FCS eta-expands it to `fun x -> f x`, a *synthesized*
        // invocation with real args; it is a range-keyed synthesized node like the
        // implicit base ctor, harmless to a consumer keying on source call ranges.)
        let isModuleValueRead =
            not (g m (fun m -> m.IsMember)) && not (g m (fun m -> m.CurriedParameterGroups.Count > 0))
        not (isPlainGetter || isPlainSetter || isEventAccessor || isModuleValueRead)
    let rec walkExpr (e: FSharpExpr) =
        (match e with
         | Call(_, m, _, _, _) -> if isInvocation m then emit e m
         | NewObject(m, _, _) -> emit e m
         | _ -> ())
        for sub in e.ImmediateSubExpressions do
            walkExpr sub
    let rec walkDecl (d: FSharpImplementationFileDeclaration) =
        match d with
        | FSharpImplementationFileDeclaration.Entity(_, sub) -> List.iter walkDecl sub
        | FSharpImplementationFileDeclaration.MemberOrFunctionOrValue(_, _, body) -> walkExpr body
        | FSharpImplementationFileDeclaration.InitAction e -> walkExpr e
    List.iter walkDecl impl.Declarations
    acc.ToArray()

/// Dump the chosen overload at every call node in a single file — the Stage
/// OV-1 overload-resolution oracle (see [`collectOverloads`]) — **plus the
/// lines FCS reported an error on**.
///
/// The error lines are not a diagnostics feature; they are what makes the
/// oracle's `Calls` interpretable. An elaborated `Call` node means "this is the
/// member the typed tree names here", NOT "FCS found this member applicable":
/// the single-`IsCandidate` shortcut (§2.2) elaborates the lone arity-surviving
/// candidate without any applicability test, and error recovery can elaborate a
/// candidate for a call that failed outright. A consumer asserting anything
/// about FCS's *applicability* judgment (the OV-9 differential's
/// over-approximation check) must first exclude the sites FCS errored on — a
/// distinction invisible in `Calls` alone.
let private dumpOverloads (absolute: string) =
    let impl, diags = checkScriptImplFileWithDiags "overloads" absolute
    let calls = collectOverloads impl
    let errorLines =
        diags
        |> Array.filter (fun d ->
            d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
        |> Array.map (fun d -> {| Line = d.StartLine; Code = d.ErrorNumber; Message = d.Message |})
    let payload = {| File = absolute; Calls = calls; Errors = errorLines |}
    let json = JsonSerializer.Serialize(payload, buildOptions ())
    Console.Out.Write(json)
    Console.Out.WriteLine()

/// Resident, multi-projection single-file oracle: the amortised-startup engine
/// behind `sema`'s per-case differential loops (`resolve_diff`, `infer_*_diff`,
/// `attr_resolution_diff`, `overloads_oracle`, …). Each of those otherwise spawns
/// one `dotnet fcs-dump <kind> <file>` per snippet and pays the ~1.6 s .NET + FCS
/// cold-start *every* time; driven as a [`BatchChild`]-style resident child it is
/// paid once per pool slot for the whole test binary — the same amortisation
/// `dumpAstBatch` gives the `cst` parser diffs, extended to carry per-request
/// fixture references and to fan several projections through one warm checker.
///
/// Protocol: one JSON request per stdin line,
/// `{ "kind": <projection>, "path": <abs .fs>, "refs": [<dll>…] }`, and exactly
/// one compact JSON response line — the SAME payload the matching one-shot
/// `dumpXxx` emits. The one-shots serialise with `buildOptions ()`
/// (`WriteIndented = true`); this uses `buildOptionsCompact ()` so the response
/// is a single line (the transport is line-delimited), which is invisible to the
/// Rust `serde_json` consumers (whitespace-insensitive) — so no consumer changes.
///
/// A per-request failure is reported as `{ "BatchError": <msg> }` rather than
/// thrown: throwing would kill the resident child mid-batch, so one bad snippet
/// would respawn-storm and eventually panic every *later* request too. The Rust
/// driver turns the sentinel back into the loud panic the one-shot's `failwith`
/// would have raised, so a genuinely broken case still fails its own test, alone.
let private fileBatchCore () =
    // `keepAssemblyContents` so the `types` / `binder-types` / `overloads`
    // projections can read the elaborated implementation file; harmless to the
    // `uses` / `attrs` projections (they never touch it). One checker, reused for
    // every request, is what makes the referenced-assembly reads warm.
    let checker = FSharpChecker.Create(keepAssemblyContents = true)
    let compact = buildOptionsCompact ()

    // A required JSON string field, coerced to non-null: a missing or `null`
    // field is a malformed request, reported (via the enclosing `try`) as a
    // `BatchError` rather than silently defaulting.
    let reqString (root: JsonElement) (name: string) : string =
        match root.GetProperty(name).GetString() with
        | null -> failwithf "request field %s is null" name
        | s -> s

    let respond (line: string) : string =
        try
            use doc = JsonDocument.Parse(line: string)
            let root = doc.RootElement
            let kind = reqString root "kind"
            let path = reqString root "path"
            let refs =
                match root.TryGetProperty("refs") with
                | true, arr when arr.ValueKind = JsonValueKind.Array ->
                    arr.EnumerateArray()
                    |> Seq.choose (fun e -> Option.ofObj (e.GetString()))
                    |> Seq.toList
                | _ -> []
            let absolute = Path.GetFullPath path
            let refArgs = extraRefArgsOf refs

            let text = File.ReadAllText absolute
            let sourceText = SourceText.ofString text

            // Mirror the one-shot handlers exactly: script-mode options (SDK
            // refs, no .fsproj) plus this request's fixture refs, fail hard on a
            // *reference-resolution* error, then parse-and-check.
            let opts0, scriptDiags =
                checker.GetProjectOptionsFromScript(
                    absolute, sourceText, assumeDotNetFramework = false, useSdkRefs = true)
                |> Async.RunSynchronously
            let opts =
                { opts0 with OtherOptions = Array.append opts0.OtherOptions refArgs }
            let blocking =
                scriptDiags
                |> List.filter (fun d ->
                    d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
            if not (List.isEmpty blocking) then
                let summary =
                    blocking
                    |> List.map (fun d -> sprintf "FS%04d: %s" d.ErrorNumber d.Message)
                    |> String.concat "; "
                failwithf "script resolution failed: %s" summary

            let parseResults, checkAnswer =
                checker.ParseAndCheckFileInProject(
                    absolute, 0, sourceText, opts, userOpName = "fcs-dump-file-batch")
                |> Async.RunSynchronously
            let checkResults =
                match checkAnswer with
                | FSharpCheckFileAnswer.Succeeded r -> r
                | FSharpCheckFileAnswer.Aborted -> failwith "type-check aborted"

            let implFile () =
                match checkResults.ImplementationFile with
                | Some impl -> impl
                | None -> failwith "no implementation file (keepAssemblyContents not honoured?)"

            let errorLines () =
                checkResults.Diagnostics
                |> Array.filter (fun d ->
                    d.Severity = FSharp.Compiler.Diagnostics.FSharpDiagnosticSeverity.Error)
                |> Array.map (fun d ->
                    {| Line = d.StartLine; Code = d.ErrorNumber; Message = d.Message |})

            match kind with
            | "uses" ->
                let uses =
                    checkResults.GetAllUsesOfAllSymbolsInFile()
                    |> Seq.map projectSymbolUse
                    |> Seq.toArray
                JsonSerializer.Serialize({| Uses = uses |}, compact)
            | "attrs" ->
                let attrs = collectAttrRecords parseResults.ParseTree checkResults
                JsonSerializer.Serialize({| Attrs = attrs; Errors = errorLines () |}, compact)
            | "types" ->
                let exprs = collectExprTypes true (implFile ())
                JsonSerializer.Serialize({| File = absolute; Exprs = exprs |}, compact)
            | "binder-types" ->
                let binders = collectBinderTypes (implFile ())
                JsonSerializer.Serialize({| File = absolute; Binders = binders |}, compact)
            | "overloads" ->
                let calls = collectOverloads (implFile ())
                JsonSerializer.Serialize({| File = absolute; Calls = calls; Errors = errorLines () |}, compact)
            | other ->
                failwithf "unknown kind %s" other
        with ex ->
            JsonSerializer.Serialize({| BatchError = ex.Message |}, compact)

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let trimmed = (Option.ofObj line |> Option.defaultValue "").Trim()
        if trimmed <> "" then
            Console.Out.WriteLine(respond trimmed)
            Console.Out.Flush()
        line <- Console.In.ReadLine()

/// [`fileBatchCore`] on a large-stack thread, mirroring [`dumpAstBatch`]: the
/// `types` / `binder-types` projections serialise a deeply-nested typed tree,
/// which can recurse past the default 1 MB stack and take the whole resident
/// child down with an uncatchable `StackOverflowException`. 512 MB clears it.
let private fileBatch () =
    let mutable captured: exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try fileBatchCore ()
                with ex -> captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

/// Tolerant, per-file census driver for the Phase-3 *type* scoping measurement
/// (`crates/sema/tests/types_census.rs`) — the type-side sibling of
/// [`dumpUsesCensusBatchCore`]. Reads source paths from stdin, one per line, and
/// for each type-checks it *in isolation* (reusing the SDK reference set
/// harvested once) with `keepAssemblyContents`, then emits one compact JSON line
/// `{ Path, Ok, Error, Exprs: [{ Range, Kind, Type }] }`.
///
/// NEVER aborts on a type error: a file checked without its siblings still yields
/// a (partial) elaborated tree for the parts FCS could check, exactly the
/// population the census measures. A file FCS cannot elaborate at all surfaces as
/// `Ok = false` with no exprs (the type-side analogue of an unresolved use simply
/// not appearing), and the Rust side reports that fraction.
let private dumpTypesCensusBatchCore () =
    let options = buildOptionsCompact ()
    let checker = FSharpChecker.Create(keepAssemblyContents = true)

    let mutable baseOpts: FSharpProjectOptions option = None
    let mutable switches: string[] = [||]
    let ensureRefs (absolute: string) =
        if baseOpts.IsNone then
            try
                let t = SourceText.ofString (File.ReadAllText absolute)
                let so, _ =
                    checker.GetProjectOptionsFromScript(
                        absolute, t, assumeDotNetFramework = false, useSdkRefs = true)
                    |> Async.RunSynchronously
                baseOpts <- Some so
                switches <-
                    so.OtherOptions
                    |> Array.filter (fun o -> o.StartsWith("-") || o.StartsWith("/"))
                    |> fun s -> Array.append s (extraRefArgs ())
            with _ ->
                ()

    let mutable line = Console.In.ReadLine()
    while not (isNull line) do
        let path = (Option.ofObj line |> Option.defaultValue "").Trim()
        if path <> "" then
            let absolute = Path.GetFullPath path
            ensureRefs absolute
            let json =
                try
                    match baseOpts with
                    | None ->
                        JsonSerializer.Serialize(
                            {| Path = path
                               Ok = false
                               Error = "reference resolution failed"
                               Exprs = ([||]: obj[]) |}, options)
                    | Some opts0 ->
                        let sourceText = SourceText.ofString (File.ReadAllText absolute)
                        let dir =
                            Option.ofObj (Path.GetDirectoryName absolute)
                            |> Option.defaultValue "."
                        let projOpts =
                            { opts0 with
                                ProjectFileName = Path.Combine(dir, "fcs-dump-types-census.fsproj")
                                SourceFiles = [| absolute |]
                                OtherOptions = switches
                                UseScriptResolutionRules = false }
                        let _parse, checkAnswer =
                            checker.ParseAndCheckFileInProject(
                                absolute, 0, sourceText, projOpts, userOpName = "fcs-dump-types-census")
                            |> Async.RunSynchronously
                        match checkAnswer with
                        | FSharpCheckFileAnswer.Succeeded r ->
                            match r.ImplementationFile with
                            | Some impl ->
                                let exprs = collectExprTypes false impl
                                JsonSerializer.Serialize(
                                    {| Path = path; Ok = true; Error = ""; Exprs = exprs |}, options)
                            | None ->
                                JsonSerializer.Serialize(
                                    {| Path = path
                                       Ok = false
                                       Error = "no implementation file"
                                       Exprs = ([||]: obj[]) |}, options)
                        | FSharpCheckFileAnswer.Aborted ->
                            JsonSerializer.Serialize(
                                {| Path = path
                                   Ok = false
                                   Error = "type-check aborted"
                                   Exprs = ([||]: obj[]) |}, options)
                with ex ->
                    JsonSerializer.Serialize(
                        {| Path = path; Ok = false; Error = ex.Message; Exprs = ([||]: obj[]) |}, options)
            Console.Out.WriteLine(json)
            Console.Out.Flush()
        line <- Console.In.ReadLine()

/// Batch type-census driver on a large-stack thread, mirroring
/// [`dumpUsesCensusBatch`]: elaborating a deeply-nested file can recurse far
/// enough to overflow the default 1 MB stack, so give it 512 MB of headroom.
let private dumpTypesCensusBatch () =
    let mutable captured: exn option = None
    let worker =
        System.Threading.Thread(
            (fun () ->
                try
                    dumpTypesCensusBatchCore ()
                with ex ->
                    captured <- Some ex),
            512 * 1024 * 1024)
    worker.Start()
    worker.Join()
    match captured with
    | Some ex -> raise ex
    | None -> ()

let private usage () =
    eprintfn "usage: fcs-dump <command> [<source-path>]"
    eprintfn "  ast                      dump ParsedInput as JSON"
    eprintfn "  ast-batch                read paths from stdin, emit ParsedInput JSONL"
    eprintfn "  parse-bench [N]          read paths from stdin, parse-only throughput bench (N iters, cache off)"
    eprintfn "  parse-bench-cached [N]   as parse-bench but with the parse cache sized to the whole input"
    eprintfn "  tokens-raw [--compact]      dump pre-LexFilter token stream"
    eprintfn "  tokens-filtered [--compact] dump post-LexFilter token stream"
    eprintfn "  tokens-filtered-batch    read paths from stdin, emit JSONL"
    eprintfn "  tokens-raw-batch         read paths from stdin, emit JSONL"
    eprintfn "  entities <dll-path>      dump entity skeletons of a managed DLL"
    eprintfn "  uses <source-path>       dump symbol uses (name-resolution oracle)"
    eprintfn "  attrs <source-path>      dump each attribute's resolved type (attribute-resolution oracle)"
    eprintfn "  attrs-batch              read paths from stdin; one tolerant attrs JSON line per file"
    eprintfn "  uses-project             read Compile-ordered paths from stdin; dump per-file uses"
    eprintfn "  uses-census-batch        read paths from stdin; emit per-file classified uses (Phase-3 census)"
    eprintfn "  uses-census-project      read Compile-ordered paths from stdin; classified uses, cross-file resolved"
    eprintfn "  types <source-path>      dump per-expression inferred types (Phase-3 type oracle)"
    eprintfn "  types-census-batch       read paths from stdin; emit per-file classified expr types (Phase-3 census)"
    eprintfn "  binder-types <source-path>  dump per-binder inferred types (Phase-3 binder-type oracle)"
    eprintfn "  overloads <source-path>  dump the chosen overload at each call node (overload-resolution oracle)"
    eprintfn "  file-batch               resident single-file oracle: one JSON request/line {kind,path,refs}, one compact JSON response/line"
    eprintfn "  uses-project-batch       resident project oracle: one JSON request/line {paths,refs,defines,langversion}, one compact {Files} response/line"
    2

[<EntryPoint>]
let main argv =
    match argv with
    | [| "ast"; sourcePath |] ->
        dumpAst (Path.GetFullPath sourcePath) []
        0
    // `ast <file> SYM…` — define each trailing SYM for conditional
    // compilation, so `#if SYM` selects the *then* branch.
    | _ when argv.Length > 2 && argv.[0] = "ast" ->
        let defines = argv.[2..] |> Array.toList
        dumpAst (Path.GetFullPath argv.[1]) defines
        0
    | [| "ast-batch" |] ->
        dumpAstBatch ()
        0
    | [| "parse-bench" |] ->
        parseBench false 1
        0
    | [| "parse-bench"; iterations |] ->
        parseBench false (int iterations)
        0
    | [| "parse-bench-cached" |] ->
        parseBench true 1
        0
    | [| "parse-bench-cached"; iterations |] ->
        parseBench true (int iterations)
        0
    | [| "tokens-raw"; sourcePath |] ->
        dumpTokens (Path.GetFullPath sourcePath) false false
        0
    | [| "tokens-raw"; sourcePath; "--compact" |] ->
        dumpTokens (Path.GetFullPath sourcePath) false true
        0
    | [| "tokens-filtered"; sourcePath |] ->
        dumpTokens (Path.GetFullPath sourcePath) true false
        0
    | [| "tokens-filtered"; sourcePath; "--compact" |] ->
        dumpTokens (Path.GetFullPath sourcePath) true true
        0
    | [| "tokens-filtered-batch" |] ->
        dumpTokensBatch true
        0
    | [| "tokens-raw-batch" |] ->
        dumpTokensBatch false
        0
    | [| "entities"; dllPath |] ->
        dumpEntities (Path.GetFullPath dllPath)
        0
    | [| "uses"; sourcePath |] ->
        dumpUses (Path.GetFullPath sourcePath)
        0
    | [| "attrs"; sourcePath |] ->
        dumpAttrs (Path.GetFullPath sourcePath)
        0
    | [| "attrs-batch" |] ->
        dumpAttrsBatch ()
        0
    | [| "uses-project" |] ->
        dumpUsesProject ()
        0
    | [| "uses-project-batch" |] ->
        usesProjectBatch ()
        0
    | [| "uses-census-batch" |] ->
        dumpUsesCensusBatch ()
        0
    | [| "uses-census-project" |] ->
        dumpUsesCensusProject ()
        0
    | [| "types"; sourcePath |] ->
        dumpTypes (Path.GetFullPath sourcePath)
        0
    | [| "types-census-batch" |] ->
        dumpTypesCensusBatch ()
        0
    | [| "binder-types"; sourcePath |] ->
        dumpBinderTypes (Path.GetFullPath sourcePath)
        0
    | [| "overloads"; sourcePath |] ->
        dumpOverloads (Path.GetFullPath sourcePath)
        0
    | [| "file-batch" |] ->
        fileBatch ()
        0
    | _ -> usage ()

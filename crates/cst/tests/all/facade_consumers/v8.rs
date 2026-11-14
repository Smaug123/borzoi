//! FROZEN consumer fixture of the published `v8` typed facade
//! (`docs/ast-versioning-plan.md` D6). `tools/astgen` wrote it when the facade
//! was published and never regenerates it; `tools/ci/facade-lock-check.sh`
//! refuses any edit to it. It is a consumer of the facade as it was frozen: it
//! matches every enum exhaustively, with no wildcard arm, and calls every
//! accessor with its result's type spelled out, so it stops compiling if the
//! facade loses or changes anything it had. Nothing in it runs.
//!
//! freeze-epoch: 1

#![allow(dead_code)]

use borzoi_cst::syntax::v8 as f;

/// What every node type and dispatch enum is.
fn handle<
    T: borzoi_cst::syntax::AstNode + std::fmt::Debug + Clone + PartialEq + Eq + std::hash::Hash,
>() {
}

/// The facade's root, read from a parse.
fn view(
    parse: &borzoi_cst::parser::Parse,
) -> Result<f::ParsedInput, borzoi_cst::syntax::ViewError> {
    parse.view::<f::ParsedInput>()
}

fn handles() {
    handle::<f::AbstractSlot>();
    handle::<f::ActivePatName>();
    handle::<f::AddressOfExpr>();
    handle::<f::AndsPat>();
    handle::<f::AnonRecdExpr>();
    handle::<f::AnonRecdType>();
    handle::<f::AnonRecdTypeField>();
    handle::<f::AnonType>();
    handle::<f::AppExpr>();
    handle::<f::AppType>();
    handle::<f::ArrayOrListExpr>();
    handle::<f::ArrayOrListPat>();
    handle::<f::ArrayType>();
    handle::<f::AsPat>();
    handle::<f::AssertExpr>();
    handle::<f::AssignExpr>();
    handle::<f::AttribPat>();
    handle::<f::Attribute>();
    handle::<f::AttributeList>();
    handle::<f::AttributesDecl>();
    handle::<f::AutoProperty>();
    handle::<f::Binding>();
    handle::<f::BindingReturnInfo>();
    handle::<f::ComputationExpr>();
    handle::<f::ConsExpr>();
    handle::<f::ConstExpr>();
    handle::<f::ConstPat>();
    handle::<f::ConstrainedType>();
    handle::<f::DelegateRepr>();
    handle::<f::DoBangExpr>();
    handle::<f::DoExpr>();
    handle::<f::DotGetExpr>();
    handle::<f::DotIndexedGetExpr>();
    handle::<f::DotLambdaExpr>();
    handle::<f::DotMissingExpr>();
    handle::<f::DowncastExpr>();
    handle::<f::DynamicExpr>();
    handle::<f::EnumCase>();
    handle::<f::EnumRepr>();
    handle::<f::ExceptionDefnDecl>();
    handle::<f::ExprDecl>();
    handle::<f::ExternArg>();
    handle::<f::ExternDecl>();
    handle::<f::ExternRet>();
    handle::<f::FixedExpr>();
    handle::<f::ForEachExpr>();
    handle::<f::ForExpr>();
    handle::<f::FunExpr>();
    handle::<f::FunType>();
    handle::<f::GetSetAccessor>();
    handle::<f::GetSetMember>();
    handle::<f::HashConstraintType>();
    handle::<f::HashDirectiveDecl>();
    handle::<f::IdentExpr>();
    handle::<f::IfThenElseExpr>();
    handle::<f::ImplFile>();
    handle::<f::ImplicitCtor>();
    handle::<f::IndexRangeExpr>();
    handle::<f::InferredDowncastExpr>();
    handle::<f::InferredUpcastExpr>();
    handle::<f::InheritMember>();
    handle::<f::InlineIlExpr>();
    handle::<f::InlineIlRepr>();
    handle::<f::InterfaceImpl>();
    handle::<f::InterpStringExpr>();
    handle::<f::IntersectionType>();
    handle::<f::IsInstPat>();
    handle::<f::JoinInExpr>();
    handle::<f::LazyExpr>();
    handle::<f::LetDecl>();
    handle::<f::LetOrUseExpr>();
    handle::<f::LibraryOnlyFieldGetExpr>();
    handle::<f::ListConsPat>();
    handle::<f::LongIdent>();
    handle::<f::LongIdentAppType>();
    handle::<f::LongIdentExpr>();
    handle::<f::LongIdentPat>();
    handle::<f::LongIdentType>();
    handle::<f::MatchBangExpr>();
    handle::<f::MatchClause>();
    handle::<f::MatchExpr>();
    handle::<f::MatchLambdaExpr>();
    handle::<f::MeasureAnon>();
    handle::<f::MeasureDivide>();
    handle::<f::MeasureLitExpr>();
    handle::<f::MeasureLitPat>();
    handle::<f::MeasureNamed>();
    handle::<f::MeasureOne>();
    handle::<f::MeasureParen>();
    handle::<f::MeasurePower>();
    handle::<f::MeasurePowerType>();
    handle::<f::MeasureProduct>();
    handle::<f::MeasureSeq>();
    handle::<f::MeasureVar>();
    handle::<f::MemberDo>();
    handle::<f::MemberLetBindings>();
    handle::<f::MemberMethod>();
    handle::<f::MemberSig>();
    handle::<f::ModuleAbbrevDecl>();
    handle::<f::ModuleOrNamespace>();
    handle::<f::NamePatPair>();
    handle::<f::NamePatPairs>();
    handle::<f::NamedPat>();
    handle::<f::NestedModuleDecl>();
    handle::<f::NewExpr>();
    handle::<f::NullExpr>();
    handle::<f::NullPat>();
    handle::<f::ObjExpr>();
    handle::<f::ObjectModelRepr>();
    handle::<f::OpenDecl>();
    handle::<f::OptionalValPat>();
    handle::<f::OrPat>();
    handle::<f::ParenExpr>();
    handle::<f::ParenPat>();
    handle::<f::ParenType>();
    handle::<f::QuoteExpr>();
    handle::<f::QuotePat>();
    handle::<f::RationalConstInteger>();
    handle::<f::RationalConstNegate>();
    handle::<f::RationalConstParen>();
    handle::<f::RationalConstRational>();
    handle::<f::RecordExpr>();
    handle::<f::RecordField>();
    handle::<f::RecordFieldDecl>();
    handle::<f::RecordPat>();
    handle::<f::RecordPatField>();
    handle::<f::RecordRepr>();
    handle::<f::SequentialExpr>();
    handle::<f::SigFile>();
    handle::<f::SignatureParameterType>();
    handle::<f::StaticConstExprType>();
    handle::<f::StaticConstNamedType>();
    handle::<f::StaticConstNullType>();
    handle::<f::StaticConstType>();
    handle::<f::StaticOptCondition>();
    handle::<f::StaticOptWhenClause>();
    handle::<f::StaticOptimizationExpr>();
    handle::<f::TraitCallExpr>();
    handle::<f::TryExpr>();
    handle::<f::TupleExpr>();
    handle::<f::TuplePat>();
    handle::<f::TupleType>();
    handle::<f::TyparConstraint>();
    handle::<f::TyparConstraints>();
    handle::<f::TyparDecl>();
    handle::<f::TyparDecls>();
    handle::<f::TyparExpr>();
    handle::<f::TypeAbbrev>();
    handle::<f::TypeAppExpr>();
    handle::<f::TypeDefn>();
    handle::<f::TypeDefnsDecl>();
    handle::<f::TypeTestExpr>();
    handle::<f::TypedExpr>();
    handle::<f::TypedPat>();
    handle::<f::UnionCase>();
    handle::<f::UnionCaseField>();
    handle::<f::UnionRepr>();
    handle::<f::UpcastExpr>();
    handle::<f::ValDecl>();
    handle::<f::ValField>();
    handle::<f::ValSig>();
    handle::<f::VarType>();
    handle::<f::WhileBangExpr>();
    handle::<f::WhileExpr>();
    handle::<f::WildcardPat>();
    handle::<f::YieldExpr>();
    handle::<f::Expr>();
    handle::<f::Measure>();
    handle::<f::MemberDefn>();
    handle::<f::ModuleDecl>();
    handle::<f::ParsedInput>();
    handle::<f::Pat>();
    handle::<f::RationalConst>();
    handle::<f::SigDecl>();
    handle::<f::Type>();
    handle::<f::TypeDefnRepr>();
}

fn enum_expr(value: f::Expr) {
    match value {
        f::Expr::AddressOf(node) => {
            let _: f::AddressOfExpr = node;
        }
        f::Expr::AnonRecd(node) => {
            let _: f::AnonRecdExpr = node;
        }
        f::Expr::App(node) => {
            let _: f::AppExpr = node;
        }
        f::Expr::ArrayOrList(node) => {
            let _: f::ArrayOrListExpr = node;
        }
        f::Expr::Assert(node) => {
            let _: f::AssertExpr = node;
        }
        f::Expr::Assign(node) => {
            let _: f::AssignExpr = node;
        }
        f::Expr::Computation(node) => {
            let _: f::ComputationExpr = node;
        }
        f::Expr::Cons(node) => {
            let _: f::ConsExpr = node;
        }
        f::Expr::Const(node) => {
            let _: f::ConstExpr = node;
        }
        f::Expr::Do(node) => {
            let _: f::DoExpr = node;
        }
        f::Expr::DoBang(node) => {
            let _: f::DoBangExpr = node;
        }
        f::Expr::DotGet(node) => {
            let _: f::DotGetExpr = node;
        }
        f::Expr::DotIndexedGet(node) => {
            let _: f::DotIndexedGetExpr = node;
        }
        f::Expr::DotLambda(node) => {
            let _: f::DotLambdaExpr = node;
        }
        f::Expr::DotMissing(node) => {
            let _: f::DotMissingExpr = node;
        }
        f::Expr::Downcast(node) => {
            let _: f::DowncastExpr = node;
        }
        f::Expr::Dynamic(node) => {
            let _: f::DynamicExpr = node;
        }
        f::Expr::Fixed(node) => {
            let _: f::FixedExpr = node;
        }
        f::Expr::For(node) => {
            let _: f::ForExpr = node;
        }
        f::Expr::ForEach(node) => {
            let _: f::ForEachExpr = node;
        }
        f::Expr::Fun(node) => {
            let _: f::FunExpr = node;
        }
        f::Expr::Ident(node) => {
            let _: f::IdentExpr = node;
        }
        f::Expr::IfThenElse(node) => {
            let _: f::IfThenElseExpr = node;
        }
        f::Expr::IndexRange(node) => {
            let _: f::IndexRangeExpr = node;
        }
        f::Expr::InferredDowncast(node) => {
            let _: f::InferredDowncastExpr = node;
        }
        f::Expr::InferredUpcast(node) => {
            let _: f::InferredUpcastExpr = node;
        }
        f::Expr::InlineIl(node) => {
            let _: f::InlineIlExpr = node;
        }
        f::Expr::InterpString(node) => {
            let _: f::InterpStringExpr = node;
        }
        f::Expr::JoinIn(node) => {
            let _: f::JoinInExpr = node;
        }
        f::Expr::Lazy(node) => {
            let _: f::LazyExpr = node;
        }
        f::Expr::LetOrUse(node) => {
            let _: f::LetOrUseExpr = node;
        }
        f::Expr::LibraryOnlyFieldGet(node) => {
            let _: f::LibraryOnlyFieldGetExpr = node;
        }
        f::Expr::LongIdent(node) => {
            let _: f::LongIdentExpr = node;
        }
        f::Expr::Match(node) => {
            let _: f::MatchExpr = node;
        }
        f::Expr::MatchBang(node) => {
            let _: f::MatchBangExpr = node;
        }
        f::Expr::MatchLambda(node) => {
            let _: f::MatchLambdaExpr = node;
        }
        f::Expr::MeasureLit(node) => {
            let _: f::MeasureLitExpr = node;
        }
        f::Expr::New(node) => {
            let _: f::NewExpr = node;
        }
        f::Expr::Null(node) => {
            let _: f::NullExpr = node;
        }
        f::Expr::ObjExpr(node) => {
            let _: f::ObjExpr = node;
        }
        f::Expr::Paren(node) => {
            let _: f::ParenExpr = node;
        }
        f::Expr::Quote(node) => {
            let _: f::QuoteExpr = node;
        }
        f::Expr::Record(node) => {
            let _: f::RecordExpr = node;
        }
        f::Expr::Sequential(node) => {
            let _: f::SequentialExpr = node;
        }
        f::Expr::StaticOptimization(node) => {
            let _: f::StaticOptimizationExpr = node;
        }
        f::Expr::TraitCall(node) => {
            let _: f::TraitCallExpr = node;
        }
        f::Expr::Try(node) => {
            let _: f::TryExpr = node;
        }
        f::Expr::Tuple(node) => {
            let _: f::TupleExpr = node;
        }
        f::Expr::Typar(node) => {
            let _: f::TyparExpr = node;
        }
        f::Expr::TypeApp(node) => {
            let _: f::TypeAppExpr = node;
        }
        f::Expr::TypeTest(node) => {
            let _: f::TypeTestExpr = node;
        }
        f::Expr::Typed(node) => {
            let _: f::TypedExpr = node;
        }
        f::Expr::Upcast(node) => {
            let _: f::UpcastExpr = node;
        }
        f::Expr::While(node) => {
            let _: f::WhileExpr = node;
        }
        f::Expr::WhileBang(node) => {
            let _: f::WhileBangExpr = node;
        }
        f::Expr::Yield(node) => {
            let _: f::YieldExpr = node;
        }
    }
}

fn enum_measure(value: f::Measure) {
    match value {
        f::Measure::Anon(node) => {
            let _: f::MeasureAnon = node;
        }
        f::Measure::Divide(node) => {
            let _: f::MeasureDivide = node;
        }
        f::Measure::Named(node) => {
            let _: f::MeasureNamed = node;
        }
        f::Measure::One(node) => {
            let _: f::MeasureOne = node;
        }
        f::Measure::Paren(node) => {
            let _: f::MeasureParen = node;
        }
        f::Measure::Power(node) => {
            let _: f::MeasurePower = node;
        }
        f::Measure::Product(node) => {
            let _: f::MeasureProduct = node;
        }
        f::Measure::Seq(node) => {
            let _: f::MeasureSeq = node;
        }
        f::Measure::Var(node) => {
            let _: f::MeasureVar = node;
        }
    }
}

fn enum_member_defn(value: f::MemberDefn) {
    match value {
        f::MemberDefn::AbstractSlot(node) => {
            let _: f::AbstractSlot = node;
        }
        f::MemberDefn::AutoProperty(node) => {
            let _: f::AutoProperty = node;
        }
        f::MemberDefn::Do(node) => {
            let _: f::MemberDo = node;
        }
        f::MemberDefn::GetSetMember(node) => {
            let _: f::GetSetMember = node;
        }
        f::MemberDefn::Inherit(node) => {
            let _: f::InheritMember = node;
        }
        f::MemberDefn::Interface(node) => {
            let _: f::InterfaceImpl = node;
        }
        f::MemberDefn::LetBindings(node) => {
            let _: f::MemberLetBindings = node;
        }
        f::MemberDefn::Member(node) => {
            let _: f::MemberMethod = node;
        }
        f::MemberDefn::MemberSig(node) => {
            let _: f::MemberSig = node;
        }
        f::MemberDefn::ValField(node) => {
            let _: f::ValField = node;
        }
    }
}

fn enum_module_decl(value: f::ModuleDecl) {
    match value {
        f::ModuleDecl::Attributes(node) => {
            let _: f::AttributesDecl = node;
        }
        f::ModuleDecl::Exception(node) => {
            let _: f::ExceptionDefnDecl = node;
        }
        f::ModuleDecl::Expr(node) => {
            let _: f::ExprDecl = node;
        }
        f::ModuleDecl::Extern(node) => {
            let _: f::ExternDecl = node;
        }
        f::ModuleDecl::HashDirective(node) => {
            let _: f::HashDirectiveDecl = node;
        }
        f::ModuleDecl::Let(node) => {
            let _: f::LetDecl = node;
        }
        f::ModuleDecl::ModuleAbbrev(node) => {
            let _: f::ModuleAbbrevDecl = node;
        }
        f::ModuleDecl::NestedModule(node) => {
            let _: f::NestedModuleDecl = node;
        }
        f::ModuleDecl::Open(node) => {
            let _: f::OpenDecl = node;
        }
        f::ModuleDecl::Types(node) => {
            let _: f::TypeDefnsDecl = node;
        }
    }
}

fn enum_parsed_input(value: f::ParsedInput) {
    match value {
        f::ParsedInput::ImplFile(node) => {
            let _: f::ImplFile = node;
        }
        f::ParsedInput::SigFile(node) => {
            let _: f::SigFile = node;
        }
    }
}

fn enum_pat(value: f::Pat) {
    match value {
        f::Pat::Ands(node) => {
            let _: f::AndsPat = node;
        }
        f::Pat::ArrayOrList(node) => {
            let _: f::ArrayOrListPat = node;
        }
        f::Pat::As(node) => {
            let _: f::AsPat = node;
        }
        f::Pat::Attrib(node) => {
            let _: f::AttribPat = node;
        }
        f::Pat::Const(node) => {
            let _: f::ConstPat = node;
        }
        f::Pat::IsInst(node) => {
            let _: f::IsInstPat = node;
        }
        f::Pat::ListCons(node) => {
            let _: f::ListConsPat = node;
        }
        f::Pat::LongIdent(node) => {
            let _: f::LongIdentPat = node;
        }
        f::Pat::MeasureLit(node) => {
            let _: f::MeasureLitPat = node;
        }
        f::Pat::Named(node) => {
            let _: f::NamedPat = node;
        }
        f::Pat::Null(node) => {
            let _: f::NullPat = node;
        }
        f::Pat::OptionalVal(node) => {
            let _: f::OptionalValPat = node;
        }
        f::Pat::Or(node) => {
            let _: f::OrPat = node;
        }
        f::Pat::Paren(node) => {
            let _: f::ParenPat = node;
        }
        f::Pat::Quote(node) => {
            let _: f::QuotePat = node;
        }
        f::Pat::Record(node) => {
            let _: f::RecordPat = node;
        }
        f::Pat::Tuple(node) => {
            let _: f::TuplePat = node;
        }
        f::Pat::Typed(node) => {
            let _: f::TypedPat = node;
        }
        f::Pat::Wildcard(node) => {
            let _: f::WildcardPat = node;
        }
    }
}

fn enum_rational_const(value: f::RationalConst) {
    match value {
        f::RationalConst::Integer(node) => {
            let _: f::RationalConstInteger = node;
        }
        f::RationalConst::Negate(node) => {
            let _: f::RationalConstNegate = node;
        }
        f::RationalConst::Paren(node) => {
            let _: f::RationalConstParen = node;
        }
        f::RationalConst::Rational(node) => {
            let _: f::RationalConstRational = node;
        }
    }
}

fn enum_sig_decl(value: f::SigDecl) {
    match value {
        f::SigDecl::Exception(node) => {
            let _: f::ExceptionDefnDecl = node;
        }
        f::SigDecl::HashDirective(node) => {
            let _: f::HashDirectiveDecl = node;
        }
        f::SigDecl::ModuleAbbrev(node) => {
            let _: f::ModuleAbbrevDecl = node;
        }
        f::SigDecl::NestedModule(node) => {
            let _: f::NestedModuleDecl = node;
        }
        f::SigDecl::Open(node) => {
            let _: f::OpenDecl = node;
        }
        f::SigDecl::Types(node) => {
            let _: f::TypeDefnsDecl = node;
        }
        f::SigDecl::Val(node) => {
            let _: f::ValDecl = node;
        }
    }
}

fn enum_type(value: f::Type) {
    match value {
        f::Type::Anon(node) => {
            let _: f::AnonType = node;
        }
        f::Type::AnonRecd(node) => {
            let _: f::AnonRecdType = node;
        }
        f::Type::App(node) => {
            let _: f::AppType = node;
        }
        f::Type::Array(node) => {
            let _: f::ArrayType = node;
        }
        f::Type::Constrained(node) => {
            let _: f::ConstrainedType = node;
        }
        f::Type::Fun(node) => {
            let _: f::FunType = node;
        }
        f::Type::Hash(node) => {
            let _: f::HashConstraintType = node;
        }
        f::Type::Intersection(node) => {
            let _: f::IntersectionType = node;
        }
        f::Type::LongIdent(node) => {
            let _: f::LongIdentType = node;
        }
        f::Type::LongIdentApp(node) => {
            let _: f::LongIdentAppType = node;
        }
        f::Type::MeasurePower(node) => {
            let _: f::MeasurePowerType = node;
        }
        f::Type::Paren(node) => {
            let _: f::ParenType = node;
        }
        f::Type::SignatureParameter(node) => {
            let _: f::SignatureParameterType = node;
        }
        f::Type::StaticConst(node) => {
            let _: f::StaticConstType = node;
        }
        f::Type::StaticConstExpr(node) => {
            let _: f::StaticConstExprType = node;
        }
        f::Type::StaticConstNamed(node) => {
            let _: f::StaticConstNamedType = node;
        }
        f::Type::StaticConstNull(node) => {
            let _: f::StaticConstNullType = node;
        }
        f::Type::Tuple(node) => {
            let _: f::TupleType = node;
        }
        f::Type::Var(node) => {
            let _: f::VarType = node;
        }
    }
}

fn enum_type_defn_repr(value: f::TypeDefnRepr) {
    match value {
        f::TypeDefnRepr::Abbrev(node) => {
            let _: f::TypeAbbrev = node;
        }
        f::TypeDefnRepr::Delegate(node) => {
            let _: f::DelegateRepr = node;
        }
        f::TypeDefnRepr::Enum(node) => {
            let _: f::EnumRepr = node;
        }
        f::TypeDefnRepr::InlineIl(node) => {
            let _: f::InlineIlRepr = node;
        }
        f::TypeDefnRepr::ObjectModel(node) => {
            let _: f::ObjectModelRepr = node;
        }
        f::TypeDefnRepr::Record(node) => {
            let _: f::RecordRepr = node;
        }
        f::TypeDefnRepr::Union(node) => {
            let _: f::UnionRepr = node;
        }
    }
}

fn value_auto_property_kind(value: f::AutoPropertyKind) {
    match value {
        f::AutoPropertyKind::Member => {}
        f::AutoPropertyKind::PropertyGet => {}
        f::AutoPropertyKind::PropertyGetSet => {}
        f::AutoPropertyKind::PropertySet => {}
    }
}

fn value_extern_c_type_base(value: f::ExternCTypeBase) {
    match value {
        f::ExternCTypeBase::Path(x0) => {
            let _: f::LongIdent = x0;
        }
        f::ExternCTypeBase::Void(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
    }
}

fn value_extern_c_type_suffix(value: f::ExternCTypeSuffix) {
    match value {
        f::ExternCTypeSuffix::Array(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
        f::ExternCTypeSuffix::Byref(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
        f::ExternCTypeSuffix::Pointer(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
    }
}

fn value_interp_string_part(value: f::InterpStringPart) {
    match value {
        f::InterpStringPart::Fill { expr, qualifier } => {
            let _: f::Expr = expr;
            let _: Option<borzoi_cst::syntax::SyntaxToken> = qualifier;
        }
        f::InterpStringPart::Fragment(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
    }
}

fn value_member_leading(value: f::MemberLeading) {
    match value {
        f::MemberLeading::Default => {}
        f::MemberLeading::Member => {}
        f::MemberLeading::New => {}
        f::MemberLeading::Override => {}
        f::MemberLeading::StaticMember => {}
    }
}

fn value_member_sig_leading(value: f::MemberSigLeading) {
    match value {
        f::MemberSigLeading::Abstract => {}
        f::MemberSigLeading::AbstractMember => {}
        f::MemberSigLeading::Default => {}
        f::MemberSigLeading::Member => {}
        f::MemberSigLeading::New => {}
        f::MemberSigLeading::Override => {}
        f::MemberSigLeading::Static => {}
        f::MemberSigLeading::StaticAbstract => {}
        f::MemberSigLeading::StaticAbstractMember => {}
        f::MemberSigLeading::StaticMember => {}
    }
}

fn value_module_or_namespace_kind(value: f::ModuleOrNamespaceKind) {
    match value {
        f::ModuleOrNamespaceKind::Anon => {}
        f::ModuleOrNamespaceKind::DeclaredNamespace => {}
        f::ModuleOrNamespaceKind::GlobalNamespace => {}
        f::ModuleOrNamespaceKind::NamedModule => {}
    }
}

fn value_tuple_segment(value: f::TupleSegment) {
    match value {
        f::TupleSegment::Slash(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
        f::TupleSegment::Star(x0) => {
            let _: borzoi_cst::syntax::SyntaxToken = x0;
        }
        f::TupleSegment::Type(x0) => {
            let _: f::Type = x0;
        }
    }
}

fn value_typar_constraint_kind(value: f::TyparConstraintKind) {
    match value {
        f::TyparConstraintKind::Comparable => {}
        f::TyparConstraintKind::Delegate => {}
        f::TyparConstraintKind::Enum => {}
        f::TyparConstraintKind::Equatable => {}
        f::TyparConstraintKind::NotSupportsNull => {}
        f::TyparConstraintKind::ReferenceType => {}
        f::TyparConstraintKind::SubtypeOf => {}
        f::TyparConstraintKind::SupportsNull => {}
        f::TyparConstraintKind::Unmanaged => {}
        f::TyparConstraintKind::ValueType => {}
    }
}

fn value_typar_decls_form(value: f::TyparDeclsForm) {
    match value {
        f::TyparDeclsForm::Postfix => {}
        f::TyparDeclsForm::PrefixList => {}
        f::TyparDeclsForm::SinglePrefix => {}
    }
}

fn derives_auto_property_kind() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + std::hash::Hash + PartialEq>() {}
    check::<f::AutoPropertyKind>();
}

fn derives_extern_c_type_base() {
    fn check<T: Clone + std::fmt::Debug + Eq + std::hash::Hash + PartialEq>() {}
    check::<f::ExternCTypeBase>();
}

fn derives_extern_c_type_suffix() {
    fn check<T: Clone + std::fmt::Debug + Eq + std::hash::Hash + PartialEq>() {}
    check::<f::ExternCTypeSuffix>();
}

fn derives_interp_string_part() {
    fn check<T: Clone + std::fmt::Debug>() {}
    check::<f::InterpStringPart>();
}

fn derives_member_leading() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + PartialEq>() {}
    check::<f::MemberLeading>();
}

fn derives_member_sig_leading() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + PartialEq>() {}
    check::<f::MemberSigLeading>();
}

fn derives_module_or_namespace_kind() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + std::hash::Hash + PartialEq>() {}
    check::<f::ModuleOrNamespaceKind>();
}

fn derives_tuple_segment() {
    fn check<T: Clone + std::fmt::Debug + Eq + std::hash::Hash + PartialEq>() {}
    check::<f::TupleSegment>();
}

fn derives_typar_constraint_kind() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + PartialEq>() {}
    check::<f::TyparConstraintKind>();
}

fn derives_typar_decls_form() {
    fn check<T: Clone + Copy + std::fmt::Debug + Eq + PartialEq>() {}
    check::<f::TyparDeclsForm>();
}

fn node_abstract_slot(node: &f::AbstractSlot) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: bool = node.is_abstract_member();
    let _: bool = node.is_static();
    let _: Option<f::ValSig> = node.val_sig();
}

fn node_active_pat_name(node: &f::ActivePatName) {
    let _: Vec<borzoi_cst::syntax::SyntaxToken> = node.case_tokens().collect();
    let _: Option<borzoi_cst::syntax::TextRange> = node.name_range();
}

fn node_address_of_expr(node: &f::AddressOfExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: bool = node.is_byref();
}

fn node_ands_pat(node: &f::AndsPat) {
    let _: Vec<f::Pat> = node.operands().collect();
}

fn node_anon_recd_expr(node: &f::AnonRecdExpr) {
    let _: Option<f::Expr> = node.copy_source();
    let _: Vec<f::RecordField> = node.fields().collect();
    let _: bool = node.is_struct();
}

fn node_anon_recd_type(node: &f::AnonRecdType) {
    let _: Vec<f::AnonRecdTypeField> = node.fields().collect();
    let _: bool = node.is_struct();
}

fn node_anon_recd_type_field(node: &f::AnonRecdTypeField) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Option<f::Type> = node.ty();
}

fn node_anon_type(node: &f::AnonType) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.underscore();
}

fn node_app_expr(node: &f::AppExpr) {
    let _: Option<f::Expr> = node.arg();
    let _: Option<f::Expr> = node.func();
    let _: bool = node.is_atomic();
    let _: bool = node.is_bracket_indexer();
    let _: bool = node.is_infix();
}

fn node_app_type(node: &f::AppType) {
    let _: bool = node.is_postfix();
    let _: Vec<f::Type> = node.type_args();
    let _: Option<f::Type> = node.type_name();
}

fn node_array_or_list_expr(node: &f::ArrayOrListExpr) {
    let _: Option<f::Expr> = node.inner();
    let _: bool = node.is_array();
}

fn node_array_or_list_pat(node: &f::ArrayOrListPat) {
    let _: Vec<f::Pat> = node.elements().collect();
    let _: bool = node.is_array();
}

fn node_array_type(node: &f::ArrayType) {
    let _: Option<f::Type> = node.element_type();
    let _: usize = node.rank();
}

fn node_as_pat(node: &f::AsPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.as_token();
    let _: Option<f::Pat> = node.lhs();
    let _: Option<f::Pat> = node.rhs();
}

fn node_assert_expr(node: &f::AssertExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_assign_expr(node: &f::AssignExpr) {
    let _: Option<f::Expr> = node.target();
    let _: Option<f::Expr> = node.value();
}

fn node_attrib_pat(node: &f::AttribPat) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::Pat> = node.pat();
}

fn node_attribute(node: &f::Attribute) {
    let _: Option<f::Expr> = node.arg();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.target();
    let _: Option<f::LongIdent> = node.type_name();
}

fn node_attribute_list(node: &f::AttributeList) {
    let _: Vec<f::Attribute> = node.attributes().collect();
}

fn node_attributes_decl(node: &f::AttributesDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
}

fn node_auto_property(node: &f::AutoProperty) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::Expr> = node.expr();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: bool = node.is_static();
    let _: f::AutoPropertyKind = node.prop_kind();
    let _: Option<f::Type> = node.ty();
}

fn node_binding(node: &f::Binding) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ctor_self_id();
    let _: Option<f::Expr> = node.expr();
    let _: bool = node.is_inline();
    let _: bool = node.is_mutable();
    let _: Option<f::Pat> = node.pat();
    let _: Option<f::Type> = node.return_type();
}

fn node_binding_return_info(node: &f::BindingReturnInfo) {
    let _: Option<f::Type> = node.ty();
}

fn node_computation_expr(node: &f::ComputationExpr) {
    let _: Option<f::Expr> = node.inner();
}

fn node_cons_expr(node: &f::ConsExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.cons_token();
    let _: Option<f::Expr> = node.lhs();
    let _: Option<f::Expr> = node.rhs();
}

fn node_const_expr(node: &f::ConstExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.literal();
}

fn node_const_pat(node: &f::ConstPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.literal();
}

fn node_constrained_type(node: &f::ConstrainedType) {
    let _: Option<f::Type> = node.base();
    let _: Option<f::TyparConstraints> = node.constraints();
    let _: Option<f::Type> = node.subtype();
}

fn node_delegate_repr(node: &f::DelegateRepr) {
    let _: Option<f::Type> = node.ty();
}

fn node_do_bang_expr(node: &f::DoBangExpr) {
    let _: Option<f::Expr> = node.inner();
}

fn node_do_expr(node: &f::DoExpr) {
    let _: Option<f::Expr> = node.inner();
}

fn node_dot_get_expr(node: &f::DotGetExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Option<f::LongIdent> = node.long_ident();
}

fn node_dot_indexed_get_expr(node: &f::DotIndexedGetExpr) {
    let _: Option<f::Expr> = node.index();
    let _: Option<f::Expr> = node.object();
}

fn node_dot_lambda_expr(node: &f::DotLambdaExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_dot_missing_expr(node: &f::DotMissingExpr) {
    let _: Option<f::Expr> = node.receiver();
}

fn node_downcast_expr(node: &f::DowncastExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Option<f::Type> = node.ty();
}

fn node_dynamic_expr(node: &f::DynamicExpr) {
    let _: Option<f::Expr> = node.arg();
    let _: Option<f::Expr> = node.lhs();
}

fn node_enum_case(node: &f::EnumCase) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Option<(&'static str, borzoi_cst::syntax::TextRange)> = node.operator_name();
    let _: Option<f::Expr> = node.value();
}

fn node_enum_repr(node: &f::EnumRepr) {
    let _: Vec<f::EnumCase> = node.cases().collect();
}

fn node_exception_defn_decl(node: &f::ExceptionDefnDecl) {
    let _: Option<f::LongIdent> = node.abbrev_path();
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::MemberDefn> = node.members().collect();
    let _: Option<f::UnionCase> = node.union_case();
}

fn node_expr_decl(node: &f::ExprDecl) {
    let _: Option<f::Expr> = node.expr();
}

fn node_extern_arg(node: &f::ExternArg) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::ExternCTypeBase> = node.c_type_base();
    let _: Vec<f::ExternCTypeSuffix> = node.c_type_suffixes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.name();
    let _: Option<f::LongIdent> = node.ty();
}

fn node_extern_decl(node: &f::ExternDecl) {
    let _: Vec<f::ExternArg> = node.args().collect();
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::LongIdent> = node.name();
    let _: Option<f::ExternRet> = node.return_info();
}

fn node_extern_ret(node: &f::ExternRet) {
    let _: Option<f::ExternCTypeBase> = node.c_type_base();
    let _: Vec<f::ExternCTypeSuffix> = node.c_type_suffixes().collect();
    let _: bool = node.is_void();
    let _: Option<f::LongIdent> = node.ty();
}

fn node_fixed_expr(node: &f::FixedExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_for_each_expr(node: &f::ForEachExpr) {
    let _: Option<f::Expr> = node.body();
    let _: Option<f::Expr> = node.enum_expr();
    let _: Option<f::Pat> = node.pat();
}

fn node_for_expr(node: &f::ForExpr) {
    let _: Option<f::Expr> = node.body();
    let _: Option<f::Expr> = node.from_expr();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: bool = node.is_ascending();
    let _: Option<f::Expr> = node.to_expr();
}

fn node_fun_expr(node: &f::FunExpr) {
    let _: Vec<f::Pat> = node.args().collect();
    let _: Option<f::Expr> = node.body();
}

fn node_fun_type(node: &f::FunType) {
    let _: Option<f::Type> = node.arg();
    let _: Option<f::Type> = node.ret();
}

fn node_get_set_accessor(node: &f::GetSetAccessor) {
    let _: Vec<f::Pat> = node.args().collect();
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::Expr> = node.body();
    let _: bool = node.is_get();
    let _: Option<f::Type> = node.return_type();
}

fn node_get_set_member(node: &f::GetSetMember) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::GetSetAccessor> = node.getter();
    let _: Option<f::LongIdentPat> = node.head_pat();
    let _: bool = node.is_static();
    let _: Option<f::LongIdent> = node.name();
    let _: Option<f::GetSetAccessor> = node.setter();
}

fn node_hash_constraint_type(node: &f::HashConstraintType) {
    let _: Option<f::Type> = node.inner();
}

fn node_ident_expr(node: &f::IdentExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
}

fn node_if_then_else_expr(node: &f::IfThenElseExpr) {
    let _: Option<f::Expr> = node.condition();
    let _: Option<f::Expr> = node.else_branch();
    let _: bool = node.has_else();
    let _: Option<f::Expr> = node.then_branch();
}

fn node_impl_file(node: &f::ImplFile) {
    let _: Vec<f::ModuleOrNamespace> = node.modules().collect();
}

fn node_implicit_ctor(node: &f::ImplicitCtor) {
    let _: Option<f::Pat> = node.args();
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.self_id();
}

fn node_index_range_expr(node: &f::IndexRangeExpr) {
    let _: Option<f::Expr> = node.lower();
    let _: Option<f::Expr> = node.upper();
}

fn node_inferred_downcast_expr(node: &f::InferredDowncastExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_inferred_upcast_expr(node: &f::InferredUpcastExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_inherit_member(node: &f::InheritMember) {
    let _: Option<f::Expr> = node.args();
    let _: Option<f::Type> = node.base_type();
    let _: bool = node.is_implicit();
}

fn node_inline_il_expr(node: &f::InlineIlExpr) {
    let _: Vec<f::Expr> = node.args().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.instruction();
    let _: Vec<f::Type> = node.types().collect();
}

fn node_interface_impl(node: &f::InterfaceImpl) {
    let _: bool = node.has_with();
    let _: Option<f::Type> = node.interface_type();
    let _: Vec<f::MemberDefn> = node.members().collect();
}

fn node_interp_string_expr(node: &f::InterpStringExpr) {
    let _: Vec<f::InterpStringPart> = node.parts();
}

fn node_intersection_type(node: &f::IntersectionType) {
    let _: Option<f::VarType> = node.typar();
    let _: Vec<f::Type> = node.types().collect();
}

fn node_is_inst_pat(node: &f::IsInstPat) {
    let _: Option<f::Type> = node.ty();
}

fn node_join_in_expr(node: &f::JoinInExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.in_token();
    let _: Option<f::Expr> = node.lhs();
    let _: Option<f::Expr> = node.rhs();
}

fn node_lazy_expr(node: &f::LazyExpr) {
    let _: Option<f::Expr> = node.expr();
}

fn node_let_decl(node: &f::LetDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::Binding> = node.bindings().collect();
    let _: bool = node.is_rec();
    let _: bool = node.is_use();
}

fn node_let_or_use_expr(node: &f::LetOrUseExpr) {
    let _: Vec<f::Binding> = node.bindings().collect();
    let _: Option<f::Expr> = node.body();
    let _: bool = node.is_bang();
    let _: bool = node.is_rec();
    let _: bool = node.is_use();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.keyword();
}

fn node_library_only_field_get_expr(node: &f::LibraryOnlyFieldGetExpr) {
    let _: Option<i32> = node.field_num();
    let _: Option<f::Expr> = node.object();
}

fn node_list_cons_pat(node: &f::ListConsPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.cons_token();
    let _: Option<f::Pat> = node.lhs();
    let _: Option<f::Pat> = node.rhs();
}

fn node_long_ident(node: &f::LongIdent) {
    let _: Vec<f::ActivePatName> = node.active_pat_names().collect();
    let _: Vec<borzoi_cst::syntax::SyntaxToken> = node.idents().collect();
}

fn node_long_ident_app_type(node: &f::LongIdentAppType) {
    let _: Option<f::LongIdent> = node.path();
    let _: Option<f::Type> = node.root();
    let _: Vec<f::Type> = node.type_args();
}

fn node_long_ident_expr(node: &f::LongIdentExpr) {
    let _: Option<f::LongIdent> = node.long_ident();
}

fn node_long_ident_pat(node: &f::LongIdentPat) {
    let _: Option<f::ActivePatName> = node.active_pat_name();
    let _: Vec<f::Pat> = node.args().collect();
    let _: Option<f::LongIdent> = node.head();
    let _: Option<f::NamePatPairs> = node.name_pat_pairs();
    let _: Option<f::TyparDecls> = node.typar_decls();
}

fn node_long_ident_type(node: &f::LongIdentType) {
    let _: Option<f::LongIdent> = node.long_ident();
}

fn node_match_bang_expr(node: &f::MatchBangExpr) {
    let _: Vec<f::MatchClause> = node.clauses().collect();
    let _: Option<f::Expr> = node.scrutinee();
}

fn node_match_clause(node: &f::MatchClause) {
    let _: Option<f::Expr> = node.guard();
    let _: Option<f::Pat> = node.pat();
    let _: Option<f::Expr> = node.result();
}

fn node_match_expr(node: &f::MatchExpr) {
    let _: Vec<f::MatchClause> = node.clauses().collect();
    let _: Option<f::Expr> = node.scrutinee();
}

fn node_match_lambda_expr(node: &f::MatchLambdaExpr) {
    let _: Vec<f::MatchClause> = node.clauses().collect();
}

fn node_measure_divide(node: &f::MeasureDivide) {
    let _: Option<f::Measure> = node.denominator();
    let _: Option<f::Measure> = node.numerator();
}

fn node_measure_lit_expr(node: &f::MeasureLitExpr) {
    let _: Option<f::ConstExpr> = node.const_expr();
    let _: Option<f::Measure> = node.measure();
}

fn node_measure_lit_pat(node: &f::MeasureLitPat) {
    let _: Option<f::ConstPat> = node.const_pat();
    let _: Option<f::Measure> = node.measure();
}

fn node_measure_named(node: &f::MeasureNamed) {
    let _: Option<f::LongIdent> = node.path();
}

fn node_measure_paren(node: &f::MeasureParen) {
    let _: Option<f::Measure> = node.inner();
}

fn node_measure_power(node: &f::MeasurePower) {
    let _: Option<f::Measure> = node.base();
    let _: Option<f::RationalConst> = node.exponent();
    let _: bool = node.is_negated();
}

fn node_measure_power_type(node: &f::MeasurePowerType) {
    let _: Option<f::Type> = node.base();
    let _: Option<f::RationalConst> = node.exponent();
    let _: bool = node.is_negated();
}

fn node_measure_product(node: &f::MeasureProduct) {
    let _: Option<f::Measure> = node.lhs();
    let _: Option<f::Measure> = node.rhs();
}

fn node_measure_seq(node: &f::MeasureSeq) {
    let _: Vec<f::Measure> = node.measures().collect();
}

fn node_measure_var(node: &f::MeasureVar) {
    let _: bool = node.is_head_type();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.name();
}

fn node_member_do(node: &f::MemberDo) {
    let _: Option<f::Expr> = node.expr();
    let _: bool = node.is_static();
}

fn node_member_let_bindings(node: &f::MemberLetBindings) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::Binding> = node.bindings().collect();
    let _: bool = node.is_rec();
    let _: bool = node.is_static();
    let _: bool = node.is_use();
}

fn node_member_method(node: &f::MemberMethod) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::Binding> = node.binding();
    let _: bool = node.is_static();
    let _: f::MemberLeading = node.leading_keyword();
}

fn node_member_sig(node: &f::MemberSig) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: f::MemberSigLeading = node.leading_keyword();
    let _: Option<f::ValSig> = node.val_sig();
}

fn node_module_abbrev_decl(node: &f::ModuleAbbrevDecl) {
    let _: Option<f::LongIdent> = node.ident();
    let _: Option<f::LongIdent> = node.long_id();
}

fn node_module_or_namespace(node: &f::ModuleOrNamespace) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::ModuleDecl> = node.decls().collect();
    let _: bool = node.is_anon();
    let _: bool = node.is_rec();
    let _: f::ModuleOrNamespaceKind = node.kind();
    let _: Option<f::LongIdent> = node.long_id();
    let _: Vec<f::SigDecl> = node.sig_decls().collect();
}

fn node_name_pat_pair(node: &f::NamePatPair) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.name();
    let _: Option<f::Pat> = node.pat();
}

fn node_name_pat_pairs(node: &f::NamePatPairs) {
    let _: Vec<f::NamePatPair> = node.pairs().collect();
}

fn node_named_pat(node: &f::NamedPat) {
    let _: Option<f::ActivePatName> = node.active_pat_name();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
}

fn node_nested_module_decl(node: &f::NestedModuleDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::ModuleDecl> = node.decls().collect();
    let _: bool = node.is_rec();
    let _: Option<f::LongIdent> = node.long_id();
    let _: Vec<f::SigDecl> = node.sig_decls().collect();
}

fn node_new_expr(node: &f::NewExpr) {
    let _: Option<f::Expr> = node.arg();
    let _: Option<f::Type> = node.target_type();
}

fn node_null_expr(node: &f::NullExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.keyword();
}

fn node_null_pat(node: &f::NullPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.keyword();
}

fn node_obj_expr(node: &f::ObjExpr) {
    let _: Option<f::Expr> = node.arg();
    let _: Vec<f::Binding> = node.bindings().collect();
    let _: Vec<f::MemberDefn> = node.extra_impls().collect();
    let _: Vec<f::MemberDefn> = node.members().collect();
    let _: Option<f::Type> = node.obj_type();
}

fn node_object_model_repr(node: &f::ObjectModelRepr) {
    let _: bool = node.is_augmentation();
    let _: bool = node.is_class();
    let _: bool = node.is_interface();
    let _: bool = node.is_struct();
    let _: Vec<f::MemberDefn> = node.members().collect();
}

fn node_open_decl(node: &f::OpenDecl) {
    let _: bool = node.is_type();
    let _: Option<f::LongIdent> = node.long_ident();
    let _: Option<f::Type> = node.ty();
}

fn node_optional_val_pat(node: &f::OptionalValPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
}

fn node_or_pat(node: &f::OrPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.bar_token();
    let _: Option<f::Pat> = node.lhs();
    let _: Option<f::Pat> = node.rhs();
}

fn node_paren_expr(node: &f::ParenExpr) {
    let _: Option<f::Expr> = node.inner();
}

fn node_paren_pat(node: &f::ParenPat) {
    let _: Option<f::Pat> = node.inner();
}

fn node_paren_type(node: &f::ParenType) {
    let _: Option<f::Type> = node.inner();
}

fn node_quote_expr(node: &f::QuoteExpr) {
    let _: Option<f::Expr> = node.inner();
    let _: bool = node.is_raw();
}

fn node_quote_pat(node: &f::QuotePat) {
    let _: Option<f::Expr> = node.inner();
}

fn node_rational_const_integer(node: &f::RationalConstInteger) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.value_token();
}

fn node_rational_const_negate(node: &f::RationalConstNegate) {
    let _: Option<f::RationalConst> = node.inner();
}

fn node_rational_const_paren(node: &f::RationalConstParen) {
    let _: Option<f::RationalConst> = node.inner();
}

fn node_rational_const_rational(node: &f::RationalConstRational) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.denominator();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.numerator();
}

fn node_record_expr(node: &f::RecordExpr) {
    let _: Option<f::Expr> = node.copy_source();
    let _: Vec<f::RecordField> = node.fields().collect();
    let _: Option<f::InheritMember> = node.inherit();
}

fn node_record_field(node: &f::RecordField) {
    let _: Option<f::LongIdent> = node.field_name();
    let _: Option<f::Expr> = node.value();
}

fn node_record_field_decl(node: &f::RecordFieldDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: bool = node.is_mutable();
    let _: Option<f::Type> = node.ty();
}

fn node_record_pat(node: &f::RecordPat) {
    let _: Vec<f::RecordPatField> = node.fields().collect();
}

fn node_record_pat_field(node: &f::RecordPatField) {
    let _: Option<f::LongIdent> = node.name();
    let _: Option<f::Pat> = node.pat();
}

fn node_record_repr(node: &f::RecordRepr) {
    let _: Vec<f::RecordFieldDecl> = node.fields().collect();
}

fn node_sequential_expr(node: &f::SequentialExpr) {
    let _: Vec<f::Expr> = node.statements().collect();
}

fn node_sig_file(node: &f::SigFile) {
    let _: Vec<f::ModuleOrNamespace> = node.modules().collect();
}

fn node_signature_parameter_type(node: &f::SignatureParameterType) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: bool = node.is_optional();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.name();
    let _: Option<f::Type> = node.value_type();
}

fn node_static_const_expr_type(node: &f::StaticConstExprType) {
    let _: Option<f::Expr> = node.expr();
}

fn node_static_const_named_type(node: &f::StaticConstNamedType) {
    let _: Option<f::Type> = node.ident();
    let _: Option<f::Type> = node.value();
}

fn node_static_const_type(node: &f::StaticConstType) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.literal();
}

fn node_static_opt_condition(node: &f::StaticOptCondition) {
    let _: bool = node.is_struct();
    let _: Option<f::Type> = node.ty();
    let _: Option<f::TyparDecl> = node.typar();
}

fn node_static_opt_when_clause(node: &f::StaticOptWhenClause) {
    let _: Option<f::Expr> = node.branch();
    let _: Vec<f::StaticOptCondition> = node.conditions().collect();
}

fn node_static_optimization_expr(node: &f::StaticOptimizationExpr) {
    let _: Vec<f::StaticOptWhenClause> = node.clauses().collect();
    let _: Option<f::Expr> = node.main_expr();
}

fn node_trait_call_expr(node: &f::TraitCallExpr) {
    let _: Option<f::Expr> = node.arg();
    let _: Option<f::MemberSig> = node.member_sig();
    let _: Option<f::Type> = node.support_type();
    let _: Vec<f::Type> = node.support_types().collect();
}

fn node_try_expr(node: &f::TryExpr) {
    let _: Option<f::Expr> = node.finally_expr();
    let _: bool = node.is_try_finally();
    let _: Option<f::Expr> = node.try_expr();
    let _: Vec<f::MatchClause> = node.with_clauses().collect();
}

fn node_tuple_expr(node: &f::TupleExpr) {
    let _: Vec<f::Expr> = node.elements().collect();
    let _: bool = node.is_struct();
}

fn node_tuple_pat(node: &f::TuplePat) {
    let _: Vec<f::Pat> = node.elements().collect();
    let _: bool = node.is_struct();
}

fn node_tuple_type(node: &f::TupleType) {
    let _: bool = node.is_struct();
    let _: Vec<f::TupleSegment> = node.segments();
}

fn node_typar_constraint(node: &f::TyparConstraint) {
    let _: Option<f::TyparConstraintKind> = node.kind();
    let _: Option<f::MemberSig> = node.member_sig();
    let _: Option<f::Type> = node.self_constraint();
    let _: Vec<f::Type> = node.support_types().collect();
    let _: Option<f::Type> = node.ty();
    let _: Option<f::TyparDecl> = node.typar();
    let _: Vec<f::Type> = node.type_args().collect();
}

fn node_typar_constraints(node: &f::TyparConstraints) {
    let _: Vec<f::TyparConstraint> = node.constraints().collect();
}

fn node_typar_decl(node: &f::TyparDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Vec<f::Type> = node.intersection_constraints().collect();
    let _: bool = node.is_head_type();
}

fn node_typar_decls(node: &f::TyparDecls) {
    let _: Option<f::TyparConstraints> = node.constraint_clause();
    let _: f::TyparDeclsForm = node.form();
    let _: Vec<f::TyparDecl> = node.typars().collect();
}

fn node_typar_expr(node: &f::TyparExpr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
}

fn node_type_abbrev(node: &f::TypeAbbrev) {
    let _: Option<f::Type> = node.ty();
}

fn node_type_app_expr(node: &f::TypeAppExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Vec<f::Type> = node.type_args();
}

fn node_type_defn(node: &f::TypeDefn) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::TyparConstraint> = node.constraints().collect();
    let _: Option<f::ImplicitCtor> = node.implicit_ctor();
    let _: Option<f::LongIdent> = node.long_id();
    let _: Vec<f::MemberDefn> = node.members().collect();
    let _: Option<f::TypeDefnRepr> = node.repr();
    let _: Option<f::TyparDecls> = node.typar_decls();
}

fn node_type_defns_decl(node: &f::TypeDefnsDecl) {
    let _: Vec<f::TypeDefn> = node.defns().collect();
}

fn node_type_test_expr(node: &f::TypeTestExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Option<f::Type> = node.ty();
}

fn node_typed_expr(node: &f::TypedExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Option<f::Type> = node.ty();
}

fn node_typed_pat(node: &f::TypedPat) {
    let _: Option<f::Pat> = node.pat();
    let _: Option<f::Type> = node.ty();
}

fn node_union_case(node: &f::UnionCase) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Vec<f::UnionCaseField> = node.fields().collect();
    let _: Option<f::Type> = node.full_type();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Option<(&'static str, borzoi_cst::syntax::TextRange)> = node.operator_name();
}

fn node_union_case_field(node: &f::UnionCaseField) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Option<f::Type> = node.ty();
}

fn node_union_repr(node: &f::UnionRepr) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.access();
    let _: Vec<f::UnionCase> = node.cases().collect();
}

fn node_upcast_expr(node: &f::UpcastExpr) {
    let _: Option<f::Expr> = node.expr();
    let _: Option<f::Type> = node.ty();
}

fn node_val_decl(node: &f::ValDecl) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<f::ValSig> = node.val_sig();
}

fn node_val_field(node: &f::ValField) {
    let _: Vec<f::AttributeList> = node.attributes().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: bool = node.is_mutable();
    let _: bool = node.is_static();
    let _: Option<f::Type> = node.ty();
}

fn node_val_sig(node: &f::ValSig) {
    let _: Option<f::ActivePatName> = node.active_pat_name();
    let _: Vec<f::TyparConstraint> = node.constraints().collect();
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: Option<f::Expr> = node.literal_value();
    let _: Option<f::Type> = node.ty();
    let _: Option<f::TyparDecls> = node.typar_decls();
}

fn node_var_type(node: &f::VarType) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.ident();
    let _: bool = node.is_head_type();
}

fn node_while_bang_expr(node: &f::WhileBangExpr) {
    let _: Option<f::Expr> = node.body();
    let _: Option<f::Expr> = node.cond();
}

fn node_while_expr(node: &f::WhileExpr) {
    let _: Option<f::Expr> = node.body();
    let _: Option<f::Expr> = node.cond();
}

fn node_wildcard_pat(node: &f::WildcardPat) {
    let _: Option<borzoi_cst::syntax::SyntaxToken> = node.underscore();
}

fn node_yield_expr(node: &f::YieldExpr) {
    let _: Option<f::Expr> = node.inner();
    let _: bool = node.is_from();
    let _: bool = node.is_yield();
}

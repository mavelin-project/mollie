mod array;
mod as_expr;
mod binary;
mod block;
mod call;
mod closure_expr;
mod for_in_expr;
mod ident;
mod if_else;
mod index;
mod literal;
mod loop_expr;
mod match_expr;
mod node;
mod template;
mod type_index;
mod unary_expr;
mod while_expr;

use mollie_lexer::Token;
use mollie_shared::{Operator, Positioned};
use mollie_typing::PrimitiveType;

pub use self::{
    array::ArrayExpr,
    as_expr::{IsExpr, IsPattern, NameValuePattern, TypePattern},
    binary::{BinaryExpr, RangeExpr},
    block::{BlockExpr, parse_statements_until},
    call::{CallArg, FuncCallExpr},
    closure_expr::ClosureExpr,
    for_in_expr::ForInExpr,
    ident::Ident,
    if_else::IfElseExpr,
    index::{IndexExpr, IndexTarget},
    literal::{LiteralExpr, Number, SizeType},
    loop_expr::{BreakExpr, ContinueExpr, LoopExpr},
    match_expr::{MatchArm, MatchExpr},
    node::{NameValue, NodeExpr},
    template::{TemplateExpr, TemplatePart},
    type_index::{TypePathExpr, TypePathSegment},
    unary_expr::UnaryExpr,
    while_expr::WhileExpr,
};
use crate::{Parse, ParseError, ParseResult, Parser};

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Hash)]
pub enum Precedence {
    PLowest,
    PAssign,
    POr,
    PAnd,
    Cmp,
    /// `start..end`, between comparisons and operators on numbers.
    PRange,
    PBitOr,
    PBitAnd,
    PSum,
    PProduct,
    PCast,
    PCheck,
    PUnary,
    PCall,
    PIndex,
    /// `value?`, applied before anything else.
    PTry,
}

impl Precedence {
    const fn from_ref(token: &Token) -> (Self, Option<Operator>) {
        match &token {
            Token::AndAnd => (Self::PAnd, Some(Operator::And)),
            Token::OrOr => (Self::POr, Some(Operator::Or)),
            Token::And => (Self::PBitAnd, Some(Operator::BitAnd)),
            Token::AndEq => (Self::PAssign, Some(Operator::BitAndAssign)),
            Token::Or => (Self::PBitOr, Some(Operator::BitOr)),
            Token::OrEq => (Self::PAssign, Some(Operator::BitOrAssign)),
            Token::Eq => (Self::PAssign, Some(Operator::Assign)),
            Token::EqEq => (Self::Cmp, Some(Operator::Equal)),
            Token::NotEq => (Self::Cmp, Some(Operator::NotEqual)),
            Token::Less => (Self::Cmp, Some(Operator::LessThan)),
            Token::LessEq => (Self::Cmp, Some(Operator::LessThanEqual)),
            Token::Greater => (Self::Cmp, Some(Operator::GreaterThan)),
            Token::GreaterEq => (Self::Cmp, Some(Operator::GreaterThanEqual)),
            Token::Plus => (Self::PSum, Some(Operator::Add)),
            Token::PlusEq => (Self::PAssign, Some(Operator::AddAssign)),
            Token::Minus => (Self::PSum, Some(Operator::Sub)),
            Token::MinusEq => (Self::PAssign, Some(Operator::SubAssign)),
            Token::Star => (Self::PProduct, Some(Operator::Mul)),
            Token::StarEq => (Self::PAssign, Some(Operator::MulAssign)),
            Token::Slash => (Self::PProduct, Some(Operator::Div)),
            Token::SlashEq => (Self::PAssign, Some(Operator::DivAssign)),
            Token::Percent => (Self::PProduct, Some(Operator::Rem)),
            Token::PercentEq => (Self::PAssign, Some(Operator::RemAssign)),
            Token::ParenOpen => (Self::PCall, None),
            Token::BracketOpen | Token::Dot => (Self::PIndex, None),
            Token::Question => (Self::PTry, None),
            Token::DotDot | Token::DotDotEq => (Self::PRange, None),
            Token::Is => (Self::PCheck, None),
            Token::As => (Self::PCast, None),
            _ => (Self::PLowest, None),
        }
    }
}

/// Applies operators of a higher precedence than `precedence` to `left`, one
/// after another: a loop, so long chains like `a + b + c + ...` don't nest
/// calls. They nest expressions though (`(a + b) + c`), which later passes
/// handle recursively, so every operator counts as a level of nesting.
fn go_parse_pratt_expr(parser: &mut Parser, precedence: Precedence, mut left: Positioned<Expr>, is_limited_expr: bool) -> ParseResult<Positioned<Expr>> {
    let mut chain = 0;

    while let Some(value) = parser.peek() {
        let (p, _) = Precedence::from_ref(&value.value);

        // The operator applies (every arm below but the last).
        if precedence < p {
            chain += 1;
            parser.check_depth(chain)?;
        }

        left = match p {
            Precedence::PCheck if precedence < Precedence::PCheck => {
                parser.consume(&Token::Is)?;

                // Only conditions of `if` and `while` (limited expressions) can
                // have a block right after the pattern: elsewhere, braces after
                // it are always its fields (`(x is Some { value: 1 })`).
                let pattern = parser.nested(|parser| IsPattern::parse_with(parser, !is_limited_expr))?;

                left.between(&pattern).wrap(Expr::Is(IsExpr {
                    target: Box::new(left),
                    pattern,
                }))
            }
            Precedence::PCast if precedence < Precedence::PCast => {
                parser.consume(&Token::As)?;

                let primitive = PrimitiveType::parse(parser)?;

                left.between(&primitive).wrap(Expr::Cast(Box::new(left), primitive))
            }
            Precedence::PCall if precedence < Precedence::PCall => FuncCallExpr::parse(parser, left)?.map(Expr::FunctionCall),
            Precedence::PIndex if precedence < Precedence::PIndex => IndexExpr::parse(parser, left)?.map(Expr::Index),
            Precedence::PRange if precedence < Precedence::PRange => RangeExpr::parse(parser, left, is_limited_expr)?.map(Expr::Range),
            Precedence::PTry if precedence < Precedence::PTry => {
                let question = parser.consume(&Token::Question)?;

                left.between(&question).wrap(Expr::Try(Box::new(left)))
            }
            ref peek_precedence if precedence < *peek_precedence => BinaryExpr::parse(parser, left, is_limited_expr)?.map(Expr::Binary),
            _ => return Ok(left),
        };
    }

    Ok(left)
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum Expr {
    Literal(LiteralExpr),
    Template(TemplateExpr),
    FunctionCall(FuncCallExpr),
    Node(NodeExpr),
    Index(IndexExpr),
    Binary(BinaryExpr),
    Range(RangeExpr),
    Unary(UnaryExpr),
    TypeIndex(TypePathExpr),
    Array(ArrayExpr),
    IfElse(IfElseExpr),
    Match(MatchExpr),
    Loop(LoopExpr),
    Break(BreakExpr),
    Continue(ContinueExpr),
    /// `value?`: the value of `Ok` or `Some`, otherwise the error or `None`
    /// is returned.
    Try(Box<Positioned<Self>>),
    /// `return value`, or `return` alone.
    Return(Option<Box<Positioned<Self>>>),
    While(WhileExpr),
    Block(BlockExpr),
    ForIn(ForInExpr),
    Is(IsExpr),
    Cast(Box<Positioned<Self>>, Positioned<PrimitiveType>),
    Closure(ClosureExpr),
    Ident(Ident),
    This,
    Nothing,
}

impl Expr {
    fn parse_atom(parser: &mut Parser, is_limited_expr: bool) -> ParseResult<Positioned<Self>> {
        if parser.check(&Token::ParenOpen) {
            let ([start, end], mut parser) = parser.split(&Token::ParenOpen, &Token::ParenClose)?;

            if parser.is_empty() {
                Ok(start.between(&end).wrap(Self::Nothing))
            } else {
                Self::parse(&mut parser)
            }
        } else if is_limited_expr {
            LiteralExpr::parse(parser)
                .map(|v| v.map(Self::Literal))
                .or_else(|_| TemplateExpr::parse(parser).map(|v| v.map(Self::Template)))
                .or_else(|_| BlockExpr::parse(parser).map(|v| v.map(Self::Block)))
                .or_else(|_| IfElseExpr::parse(parser).map(|v| v.map(Self::IfElse)))
                .or_else(|_| MatchExpr::parse(parser).map(|v| v.map(Self::Match)))
                .or_else(|_| Self::parse_return(parser))
                .or_else(|_| LoopExpr::parse(parser).map(|v| v.map(Self::Loop)))
                .or_else(|_| BreakExpr::parse(parser).map(|v| v.map(Self::Break)))
                .or_else(|_| ContinueExpr::parse(parser).map(|v| v.map(Self::Continue)))
                .or_else(|_| loop_expr::parse_labeled_loop(parser))
                .or_else(|_| WhileExpr::parse(parser).map(|v| v.map(Self::While)))
                .or_else(|_| ArrayExpr::parse(parser).map(|v| v.map(Self::Array)))
                .or_else(|_| ForInExpr::parse(parser).map(|v| v.map(Self::ForIn)))
                .or_else(|_| ClosureExpr::parse(parser).map(|v| v.map(Self::Closure)))
                .or_else(|_| UnaryExpr::parse(parser).map(|v| v.map(Self::Unary)))
                .or_else(|_| {
                    Ident::parse(parser)
                        .or_else(|_| parser.consume_map(|token| if matches!(token, Token::Super) { Some(Ident::new("super")) } else { None }))
                        .and_then(|name| {
                            Ok(if parser.check(&Token::PathSep) {
                                TypePathExpr::parse(name.span.wrap(TypePathSegment { name, args: None }), parser, true)?.map(Self::TypeIndex)
                            } else if name.value.0 == "super" && !parser.check(&Token::Dot) {
                                // `super.name(...)` calls the default of a
                                // trait function.
                                return Err(ParseError::unexpected_token(Some(&name.span.wrap(Token::Super))));
                            } else {
                                name.map(Self::Ident)
                            })
                        })
                })
                .or_else(|_| parser.consume(&Token::This).map(|v| v.wrap(Self::This)))
        } else {
            LiteralExpr::parse(parser)
                .map(|v| v.map(Self::Literal))
                .or_else(|_| TemplateExpr::parse(parser).map(|v| v.map(Self::Template)))
                .or_else(|_| BlockExpr::parse(parser).map(|v| v.map(Self::Block)))
                .or_else(|_| IfElseExpr::parse(parser).map(|v| v.map(Self::IfElse)))
                .or_else(|_| MatchExpr::parse(parser).map(|v| v.map(Self::Match)))
                .or_else(|_| Self::parse_return(parser))
                .or_else(|_| LoopExpr::parse(parser).map(|v| v.map(Self::Loop)))
                .or_else(|_| BreakExpr::parse(parser).map(|v| v.map(Self::Break)))
                .or_else(|_| ContinueExpr::parse(parser).map(|v| v.map(Self::Continue)))
                .or_else(|_| loop_expr::parse_labeled_loop(parser))
                .or_else(|_| WhileExpr::parse(parser).map(|v| v.map(Self::While)))
                .or_else(|_| ArrayExpr::parse(parser).map(|v| v.map(Self::Array)))
                .or_else(|_| ForInExpr::parse(parser).map(|v| v.map(Self::ForIn)))
                .or_else(|_| ClosureExpr::parse(parser).map(|v| v.map(Self::Closure)))
                .or_else(|_| UnaryExpr::parse(parser).map(|v| v.map(Self::Unary)))
                .or_else(|_| {
                    Ident::parse(parser)
                        .or_else(|_| parser.consume_map(|token| if matches!(token, Token::Super) { Some(Ident::new("super")) } else { None }))
                        .and_then(|name| {
                            Ok(if parser.check(&Token::PathSep) {
                                let path = TypePathExpr::parse(name.span.wrap(TypePathSegment { name, args: None }), parser, true)?;

                                if parser.check_one_of(&[Token::BraceOpen, Token::From]) {
                                    NodeExpr::parse(path, parser)?.map(Self::Node)
                                } else {
                                    path.map(Self::TypeIndex)
                                }
                            } else {
                                // `super.name(...)` calls the default of a
                                // trait function.
                                if name.value.0 == "super" && !parser.check(&Token::Dot) {
                                    return Err(ParseError::unexpected_token(Some(&name.span.wrap(Token::Super))));
                                }

                                if parser.check_one_of(&[Token::BraceOpen, Token::From]) {
                                    NodeExpr::parse(
                                        name.span.wrap(TypePathExpr {
                                            segments: vec![name.span.wrap(TypePathSegment { name, args: None })],
                                        }),
                                        parser,
                                    )?
                                    .map(Self::Node)
                                } else {
                                    name.map(Self::Ident)
                                }
                            })
                        })
                })
                .or_else(|_| parser.consume(&Token::This).map(|v| v.wrap(Self::This)))
        }
    }

    /// `return value` or `return`, which has no value before `;`, `}` or `,`.
    fn parse_return(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Return)?;

        if parser.check_one_of(&[Token::Semi, Token::BraceClose, Token::Comma, Token::EOF]) {
            Ok(start.wrap(Self::Return(None)))
        } else {
            let value = Self::parse(parser)?;

            Ok(start.between(&value).wrap(Self::Return(Some(Box::new(value)))))
        }
    }

    fn parse_pratt_expr(parser: &mut Parser, precedence: Precedence, is_limited_expr: bool) -> ParseResult<Positioned<Self>> {
        // Every nested expression is parsed through here.
        parser.nested(|parser| {
            let left = Self::parse_atom(parser, is_limited_expr)?;

            go_parse_pratt_expr(parser, precedence, left, is_limited_expr)
        })
    }
}

impl Parse for Expr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        Self::parse_pratt_expr(parser, Precedence::PLowest, false)
    }
}

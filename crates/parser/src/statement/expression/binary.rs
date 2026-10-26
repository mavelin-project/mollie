
use mollie_lexer::Token;
use mollie_shared::{Operator, Positioned};

use crate::{Expr, ParseError, ParseResult, Parser, Precedence};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct BinaryExpr {
    pub lhs: Box<Positioned<Expr>>,
    pub rhs: Box<Positioned<Expr>>,
    pub operator: Positioned<Operator>,
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct RangeExpr {
    pub start: Box<Positioned<Expr>>,
    pub end: Box<Positioned<Expr>>,
    pub inclusive: bool,
}

impl RangeExpr {
    pub fn parse(parser: &mut Parser, start: Positioned<Expr>, is_limited_expr: bool) -> ParseResult<Positioned<Self>> {
        let inclusive = parser
            .consume_map(|token| match token {
                Token::DotDot => Some(false),
                Token::DotDotEq => Some(true),
                _ => None,
            })?
            .value;
        let end = Expr::parse_pratt_expr(parser, Precedence::PRange, is_limited_expr)?;

        Ok(start.between(&end).wrap(Self {
            start: Box::new(start),
            end: Box::new(end),
            inclusive,
        }))
    }
}

impl BinaryExpr {
    pub fn parse(parser: &mut Parser, lhs: Positioned<Expr>, is_limited_expr: bool) -> ParseResult<Positioned<Self>> {
        let (precedence, operator, tokens) = Precedence::peek(parser);
        let first = parser.current();
        let operator = operator.ok_or_else(|| ParseError::expected_tokens(&[Token::Plus, Token::Minus, Token::Star, Token::Slash], first))?;
        let mut span = first.span;

        for _ in 0..tokens {
            if let Some(token) = parser.next() {
                span = span.between(token.span);
            }
        }

        let operator = span.wrap(operator);
        let rhs = Expr::parse_pratt_expr(parser, precedence, is_limited_expr)?;

        Ok(lhs.between(&rhs).wrap(Self {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            operator,
        }))
    }
}

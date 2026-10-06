// use std::fmt;

use mollie_lexer::Token;
use mollie_shared::{Operator, Positioned};

use crate::{Expr, ParseError, ParseResult, Parser, Precedence};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct BinaryExpr {
    pub lhs: Box<Positioned<Expr>>,
    pub rhs: Box<Positioned<Expr>>,
    pub operator: Positioned<Operator>,
}

// impl fmt::Display for BinaryExpression {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         write!(f, "{} {} {}", self.lhs, self.operator, self.rhs)
//     }
// }

/// `start..end` or `start..=end` (`inclusive`).
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct RangeExpr {
    pub start: Box<Positioned<Expr>>,
    pub end: Box<Positioned<Expr>>,
    pub inclusive: bool,
}

impl RangeExpr {
    /// Parses the operator and the end of a range starting with `start`.
    ///
    /// # Errors
    ///
    /// Returns `ParseError` if parsing failed
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
    /// # Errors
    ///
    /// Returns `ParseError` if parsing failed
    pub fn parse(parser: &mut Parser, lhs: Positioned<Expr>, is_limited_expr: bool) -> ParseResult<Positioned<Self>> {
        let peeked = parser.peek().ok_or_else(|| ParseError::new("expected operator", None))?;

        let (precedence, operator) = Precedence::from_ref(&peeked.value);

        let operator = peeked
            .span
            .wrap(operator.ok_or_else(|| ParseError::expected_tokens(&[Token::Plus, Token::Minus, Token::Star, Token::Slash], Some(peeked)))?);

        parser.next();

        let rhs = Expr::parse_pratt_expr(parser, precedence, is_limited_expr)?;

        Ok(lhs.between(&rhs).wrap(Self {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            operator,
        }))
    }
}

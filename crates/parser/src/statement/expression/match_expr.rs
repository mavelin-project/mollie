use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, IsPattern, Parse, ParseResult, Parser, Precedence};

/// `pattern if guard => body`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct MatchArm {
    pub pattern: Positioned<IsPattern>,
    pub guard: Option<Positioned<Expr>>,
    pub body: Positioned<Expr>,
}

/// `match target { arms }`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct MatchExpr {
    pub target: Box<Positioned<Expr>>,
    pub arms: Positioned<Vec<Positioned<MatchArm>>>,
}

impl Parse for MatchExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Match)?;
        // The target can't be a node expression, its braces are the arms.
        let target = Expr::parse_pratt_expr(parser, Precedence::PLowest, true)?;
        let arms_start = parser.consume(&Token::BraceOpen)?;
        let mut arms = Vec::new();

        while !parser.check(&Token::BraceClose) {
            let pattern = IsPattern::parse(parser)?;
            let guard = if parser.try_consume(&Token::If) {
                Some(Expr::parse_pratt_expr(parser, Precedence::PLowest, true)?)
            } else {
                None
            };

            parser.consume(&Token::FatArrow)?;

            let body = Expr::parse(parser)?;
            // Arms ending with a block don't need a comma.
            let ends_with_block = matches!(body.value, Expr::Block(_) | Expr::IfElse(_) | Expr::While(_) | Expr::ForIn(_) | Expr::Match(_));

            arms.push(pattern.between(&body).wrap(MatchArm { pattern, guard, body }));

            if !parser.try_consume(&Token::Comma) && !ends_with_block && !parser.check(&Token::BraceClose) {
                parser.consume(&Token::Comma)?;
            }
        }

        let end = parser.consume(&Token::BraceClose)?;

        Ok(start.between(&end).wrap(Self {
            target: Box::new(target),
            arms: arms_start.between(&end).wrap(arms),
        }))
    }
}

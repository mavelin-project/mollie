use mollie_lexer::Token;
use mollie_shared::{Positioned, UnaryOperator};

use crate::{Expr, Parse, ParseResult, Parser, Precedence};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct UnaryExpr {
    pub operator: Positioned<UnaryOperator>,
    pub expr: Box<Positioned<Expr>>,
}

impl Parse for UnaryExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let operator = match parser.consume_if(|t| matches!(t, Token::Not | Token::Minus))? {
            Positioned { value: Token::Minus, span } => span.wrap(UnaryOperator::Neg),
            Positioned { value: Token::Not, span } => span.wrap(UnaryOperator::Not),
            _ => unreachable!(),
        };

        let expr = Box::new(Expr::parse_pratt_expr(parser, Precedence::PUnary, false)?);

        Ok(operator.span.between(expr.span).wrap(Self { operator, expr }))
    }
}

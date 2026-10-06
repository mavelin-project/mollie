use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{BlockExpr, Expr, Parse, ParseResult, Parser, Precedence};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct WhileExpr {
    pub label: Option<Positioned<crate::Ident>>,
    pub condition: Box<Positioned<Expr>>,
    pub block: Positioned<BlockExpr>,
}

impl Parse for WhileExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::While)?;

        let condition = Expr::parse_pratt_expr(parser, Precedence::PLowest, true)?;
        let block = BlockExpr::parse(parser)?;

        Ok(start.between(&block).wrap(Self {
            label: None,
            condition: Box::new(condition),
            block,
        }))
    }
}

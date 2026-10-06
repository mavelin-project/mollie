use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{BlockExpr, Expr, Parse, ParseResult, Parser, Precedence};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct IfElseExpr {
    pub condition: Box<Positioned<Expr>>,
    pub block: Positioned<BlockExpr>,
    pub else_block: Option<Box<Positioned<Expr>>>,
}

impl Parse for IfElseExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::If)?;

        let condition = Expr::parse_pratt_expr(parser, Precedence::PLowest, true)?;
        let block = BlockExpr::parse(parser)?;

        let else_block = if parser.try_consume(&Token::Else) {
            if parser.check(&Token::If) {
                // `else if` chains nest.
                Some(parser.nested(Self::parse)?.map(Expr::IfElse))
            } else {
                Some(BlockExpr::parse(parser)?.map(Expr::Block))
            }
        } else {
            None
        }
        .map(Box::new);

        Ok(else_block
            .as_ref()
            .map_or_else(|| start.between(&block), |else_block| start.between(else_block))
            .wrap(Self {
                condition: Box::new(condition),
                block,
                else_block,
            }))
    }
}

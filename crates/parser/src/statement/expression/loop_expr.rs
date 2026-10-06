use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{BlockExpr, Expr, ForInExpr, Ident, Parse, ParseError, ParseResult, Parser, WhileExpr};

/// `loop { ... }`, repeated until `break`, which can give it a value.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct LoopExpr {
    pub label: Option<Positioned<Ident>>,
    pub block: Positioned<BlockExpr>,
}

impl Parse for LoopExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Loop)?;
        let block = BlockExpr::parse(parser)?;

        Ok(start.between(&block).wrap(Self { label: None, block }))
    }
}

/// `break`, `break 'label`, `break value` or `break 'label value`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct BreakExpr {
    pub label: Option<Positioned<Ident>>,
    pub value: Option<Box<Positioned<Expr>>>,
}

/// `continue` or `continue 'label`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct ContinueExpr {
    pub label: Option<Positioned<Ident>>,
}

/// Parses a label of `break` or `continue`, if there's one.
fn parse_label(parser: &mut Parser) -> Option<Positioned<Ident>> {
    parser
        .consume_map(|token| {
            if let Token::Label(name) = token {
                Some(Ident::new(name.clone()))
            } else {
                None
            }
        })
        .ok()
}

impl Parse for BreakExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Break)?;
        let label = parse_label(parser);

        if parser.check_one_of(&[Token::Semi, Token::BraceClose, Token::Comma, Token::EOF]) {
            let span = label.as_ref().map_or(start.span, |label| start.between(label));

            Ok(span.wrap(Self { label, value: None }))
        } else {
            let value = Expr::parse(parser)?;

            Ok(start.between(&value).wrap(Self {
                label,
                value: Some(Box::new(value)),
            }))
        }
    }
}

impl Parse for ContinueExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Continue)?;
        let label = parse_label(parser);
        let span = label.as_ref().map_or(start.span, |label| start.between(label));

        Ok(span.wrap(Self { label }))
    }
}

/// Parses `'label: loop { ... }`, `'label: while ...` or `'label: for ...`.
pub fn parse_labeled_loop(parser: &mut Parser) -> ParseResult<Positioned<Expr>> {
    let label = parse_label(parser).ok_or_else(|| ParseError::unexpected_token(parser.peek()))?;

    parser.consume(&Token::Colon)?;

    let expr = if parser.check(&Token::Loop) {
        LoopExpr::parse(parser)?.map(|mut expr| {
            expr.label = Some(label.clone());

            Expr::Loop(expr)
        })
    } else if parser.check(&Token::While) {
        WhileExpr::parse(parser)?.map(|mut expr| {
            expr.label = Some(label.clone());

            Expr::While(expr)
        })
    } else if parser.check(&Token::For) {
        ForInExpr::parse(parser)?.map(|mut expr| {
            expr.label = Some(label.clone());

            Expr::ForIn(expr)
        })
    } else {
        return Err(ParseError::unexpected_token(parser.peek()));
    };

    Ok(label.between(&expr).wrap(expr.value))
}

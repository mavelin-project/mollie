use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, IfElseExpr, Parse, ParseError, ParseResult, Parser, Stmt};

pub type BlockStmts = (Vec<Positioned<Stmt>>, Option<Positioned<Stmt>>);

/// # Errors
///
/// Returns error if parsing failed
pub fn parse_statements_until(parser: &mut Parser, token: &Token) -> ParseResult<BlockStmts> {
    let mut statements = Vec::new();
    let mut return_statement: Option<Positioned<Stmt>> = None;

    while !parser.check(token) {
        // Statements nest through declarations (a function in a block).
        let statement = parser.nested(Stmt::parse)?;

        // A block, a `match` or a `loop` used as a statement doesn't need a
        // semicolon; it's the value of the block only at its end (like in
        // Rust: `{ { 1 } }` is 1).
        if matches!(statement.value, Stmt::Expression(Expr::Match(_) | Expr::Loop(_) | Expr::Block(_))) && return_statement.is_none() {
            if parser.try_consume(&Token::Semi) || !parser.check(token) {
                statements.push(statement);
            } else {
                return_statement.replace(statement);
            }

            continue;
        }

        if matches!(statement.value, Stmt::Expression(_))
            && !matches!(
                statement.value,
                Stmt::Expression(
                    Expr::IfElse(IfElseExpr {
                        block: Positioned {
                            value: BlockExpr { final_stmt: None, .. },
                            ..
                        },
                        ..
                    }) | Expr::Block(_)
                        | Expr::While(_)
                        | Expr::ForIn(_)
                )
            )
        {
            if parser.try_consume(&Token::Semi) {
                statements.push(statement);
            } else if let Some(statement) = &return_statement {
                return Err(ParseError::new("missing ;", Some(statement.span)));
            } else {
                return_statement.replace(statement);
            }
        } else if return_statement.is_none() {
            statements.push(statement);
        } else {
            return Err(ParseError::new(
                format!("return value already exists: {return_statement:?}"),
                Some(statement.span),
            ));
        }
    }

    Ok((statements, return_statement))
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct BlockExpr {
    pub stmts: Vec<Positioned<Stmt>>,
    pub final_stmt: Option<Box<Positioned<Stmt>>>,
}

impl Parse for BlockExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::BraceOpen)?;

        let (statements, final_statement) = parse_statements_until(parser, &Token::BraceClose)?;

        let final_statement = final_statement.map(Box::new);
        let end = parser.consume(&Token::BraceClose)?;

        Ok(start.between(&end).wrap(Self {
            stmts: statements,
            final_stmt: final_statement,
        }))
    }
}

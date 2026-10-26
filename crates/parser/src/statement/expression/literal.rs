use std::{
    hash::{Hash, Hasher},
    mem::discriminant,
};

use mollie_lexer::{NumberToken, Token};
use mollie_shared::Positioned;

use crate::{Parse, ParseResult, Parser};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum Number {
    I64(i64),
    Float(f64),
}

impl Hash for Number {
    fn hash<H: Hasher>(&self, state: &mut H) {
        discriminant(self).hash(state);

        match self {
            Self::I64(value) => value.hash(state),
            Self::Float(value) => value.to_bits().hash(state),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub enum SizeType {
    Pixel,
    Percent,
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum LiteralExpr {
    Number(Positioned<Number>, Option<Positioned<String>>),
    Bool(bool),
    String(String),
}

impl Parse for LiteralExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser
            .consume_if(|token| matches!(token, Token::Bool(_) | Token::Number(..) | Token::String(_)))
            .map(|token| {
                token.span.wrap(match token.value {
                    Token::Bool(value) => Self::Bool(value),
                    Token::Number(value, postfix) => Self::Number(
                        value.map(|value| match value {
                            NumberToken::Float(value) => Number::Float(value),
                            NumberToken::I64(value) => Number::I64(value),
                        }),
                        postfix,
                    ),
                    Token::String(value) => Self::String(value),
                    _ => unreachable!(),
                })
            })
    }
}

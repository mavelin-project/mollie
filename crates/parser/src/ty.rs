use mollie_lexer::Token;
use mollie_shared::Positioned;
use mollie_typing::{IntType, PrimitiveType, UIntType};

use crate::{Ident, Parse, ParseResult, Parser, TypePathExpr, TypePathSegment};

// #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
// pub enum PrimitiveType {
//     ISize,
//     I64,
//     I32,
//     I16,
//     I8,
//     USize,
//     U64,
//     U32,
//     U16,
//     U8,
//     F32,
//     Bool,
//     String,
//     Void,
// }

impl Parse for PrimitiveType {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.consume_map(|token| match token {
            Token::Ident(value) => match value.as_str() {
                "isize" => Some(Self::Int(IntType::ISize)),
                "i64" => Some(Self::Int(IntType::I64)),
                "i32" => Some(Self::Int(IntType::I32)),
                "i16" => Some(Self::Int(IntType::I16)),
                "i8" => Some(Self::Int(IntType::I8)),
                "usize" => Some(Self::UInt(UIntType::USize)),
                "u64" => Some(Self::UInt(UIntType::U64)),
                "u32" => Some(Self::UInt(UIntType::U32)),
                "u16" => Some(Self::UInt(UIntType::U16)),
                "u8" => Some(Self::UInt(UIntType::U8)),
                "f32" => Some(Self::F32),
                "bool" => Some(Self::Bool),
                "string" => Some(Self::String),
                "void" => Some(Self::Void),
                _ => None,
            },
            _ => None,
        })
    }
}

/// A generic parameter of a function or an impl block, with its bounds:
/// `T`, `T: Shape` or `T: Shape + Named`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct GenericParam {
    pub name: Positioned<Ident>,
    pub bounds: Vec<Positioned<TypePathExpr>>,
}

impl Parse for GenericParam {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = Ident::parse(parser)?;
        let mut bounds = Vec::new();

        if parser.try_consume(&Token::Colon) {
            loop {
                bounds.push(TypePathExpr::parse(
                    TypePathSegment::parse_from(Ident::parse(parser)?, parser, false)?,
                    parser,
                    false,
                )?);

                if !parser.try_consume(&Token::Plus) {
                    break;
                }
            }
        }

        let span = bounds.last().map_or(name.span, |bound| name.between(bound));

        Ok(span.wrap(Self { name, bounds }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct NameWithGenerics {
    pub name: Positioned<Ident>,
    pub generics: Vec<Positioned<Ident>>,
}

impl Parse for NameWithGenerics {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = Ident::parse(parser)?;

        let (generics, end) = if parser.try_consume(&Token::Less) {
            let generics = parser.consume_separated_until(&Token::Comma, &Token::Greater)?;
            let end = parser.consume(&Token::Greater)?;

            (generics, end.span)
        } else {
            (Vec::new(), name.span)
        };

        Ok(name.span.between(end).wrap(Self { name, generics }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct TypeArgs(pub Vec<Positioned<Type>>);

impl Parse for TypeArgs {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        Ok(parser.consume_separated_in(&Token::Comma, &Token::Less, &Token::Greater)?.map(Self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub enum Type {
    Primitive(PrimitiveType),
    Array(Box<Positioned<Self>>, Option<Positioned<usize>>),
    Func(Vec<Positioned<Self>>, Option<Box<Positioned<Self>>>),
    Path(TypePathExpr),
}

impl Type {
    fn parse_simple(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        PrimitiveType::parse(parser)
            .map(|v| v.map(Self::Primitive))
            .or_else(|_| {
                let start = parser.consume(&Token::Func)?;
                let args = parser.consume_separated_in(&Token::Comma, &Token::ParenOpen, &Token::ParenClose)?;
                let returns = if parser.try_consume(&Token::Arrow) {
                    Some(Box::new(Self::parse(parser)?))
                } else {
                    None
                };

                ParseResult::Ok(
                    returns
                        .as_ref()
                        .map_or_else(|| start.between(&args), |returns| start.between(returns))
                        .wrap(Self::Func(args.value, returns)),
                )
            })
            .or_else(|_| {
                let name = Ident::parse(parser)?;

                TypePathExpr::parse(TypePathSegment::parse_from(name, parser, false)?, parser, false).map(|path| path.map(Self::Path))
            })
    }
}

impl Parse for Type {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        // Types nest in type arguments and function types.
        parser.nested(Self::parse_nested)
    }
}

impl Type {
    fn parse_nested(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let value = if parser.try_consume(&Token::ParenOpen) {
            let value = Self::parse_simple(parser)?;

            parser.consume(&Token::ParenClose)?;

            value
        } else {
            Self::parse_simple(parser)?
        };

        let value = if parser.try_consume(&Token::BracketOpen) {
            let size = parser
                .consume_if(Token::is_i64)
                .map(|v| v.map(Token::unwrap_integer))
                .map_or(None, |size| size.value.0.value.try_into().ok().map(|v| size.span.wrap(v)));

            let end = parser.consume(&Token::BracketClose)?;

            value.span.between(end.span).wrap(Self::Array(Box::new(value), size))
        } else {
            value
        };

        Ok(value)
    }
}

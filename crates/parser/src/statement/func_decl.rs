use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{BlockExpr, Expr, Ident, Parse, ParseResult, Parser, ty::Type};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub enum FuncModifier {
    Public,
    Postfix,
}

impl Parse for FuncModifier {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser
            .consume(&Token::Public)
            .map(|t| t.wrap(Self::Public))
            .or_else(|_| parser.consume(&Token::Postfix).map(|t| t.wrap(Self::Postfix)))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct Argument {
    pub name: Positioned<Ident>,
    pub ty: Positioned<Type>,
    /// `= value`, used when a call doesn't pass the argument. It's evaluated
    /// at the call and may use earlier parameters.
    pub default: Option<Positioned<Expr>>,
}

impl Parse for Argument {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.verify_if(Token::is_ident)?;
        parser.verify2(&Token::Colon)?;

        let name = Ident::parse(parser)?;

        parser.consume(&Token::Colon)?;

        let ty = Type::parse(parser)?;
        let default = if parser.try_consume(&Token::Eq) { Some(Expr::parse(parser)?) } else { None };
        let span = default.as_ref().map_or_else(|| name.between(&ty), |default| name.between(default));

        Ok(span.wrap(Self { name, ty, default }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct FuncDecl {
    pub modifiers: Vec<Positioned<FuncModifier>>,
    pub name: Positioned<Ident>,
    /// Generic parameters: `T` in `func id<T>(x: T) -> T`.
    pub generics: Vec<Positioned<crate::GenericParam>>,
    pub args: Positioned<Vec<Positioned<Argument>>>,
    pub returns: Option<Positioned<Type>>,
    pub body: Positioned<BlockExpr>,
    /// `extern func f(...) -> T;`: declared only (in stubs of the host's
    /// API), with an empty body.
    pub external: bool,
}

impl Parse for FuncDecl {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        Self::parse_with(parser, false)
    }
}

impl FuncDecl {
    /// Parses a function, or the declaration of an external one (ended with
    /// `;` instead of a body).
    ///
    /// # Errors
    ///
    /// Returns an error if the code isn't a function.
    pub fn parse_with(parser: &mut Parser, external: bool) -> ParseResult<Positioned<Self>> {
        let modifiers = parser.consume_while(|parser| parser.check_one_of(&[Token::Public, Token::Postfix]))?;
        let start = parser.consume(&Token::Func)?;
        let name = Ident::parse(parser)?;
        let generics = if parser.try_consume(&Token::Less) {
            let generics = parser.consume_separated_until(&Token::Comma, &Token::Greater)?;

            parser.consume(&Token::Greater)?;

            generics
        } else {
            Vec::new()
        };

        let args_start = parser.consume(&Token::ParenOpen)?;

        let mut args = Vec::new();

        while !parser.check(&Token::ParenClose) {
            if !args.is_empty() {
                parser.consume(&Token::Comma)?;
            }

            if parser.check(&Token::ParenClose) {
                break;
            }

            args.push(Argument::parse(parser)?);
        }

        let args_end = parser.consume(&Token::ParenClose)?;

        let returns = if parser.try_consume(&Token::Arrow) {
            Some(Type::parse(parser)?)
        } else {
            None
        };

        let body = if external {
            parser.consume(&Token::Semi)?.wrap(BlockExpr {
                stmts: Vec::new(),
                final_stmt: None,
            })
        } else {
            BlockExpr::parse(parser)?
        };

        Ok(start.between(&body).wrap(Self {
            modifiers,
            name,
            generics,
            args: args_start.between(&args_end).wrap(args),
            returns,
            body,
            external,
        }))
    }
}

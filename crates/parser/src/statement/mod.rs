mod enum_decl;
mod expression;
mod func_decl;
mod implementation;
mod import;
mod module_decl;
mod struct_decl;
mod trait_decl;
mod variable_decl;
mod view_decl;

use mollie_lexer::Token;
use mollie_shared::{LangItem, Positioned};

pub use self::{
    enum_decl::EnumDecl,
    expression::*,
    func_decl::{Argument, FuncDecl, FuncModifier},
    implementation::{Impl, ImplFunction},
    import::{Import, ImportKind},
    module_decl::ModuleDecl,
    struct_decl::{Property, StructDecl},
    trait_decl::{TraitDecl, TraitFuncArgument, TraitFunction},
    variable_decl::{ConstDecl, VariableDecl},
    view_decl::{ViewDecl, ViewProperty},
};
use super::{ParseResult, Parser};
use crate::{Parse, ParseError};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum AttributeValue {
    Literal(LiteralExpr),
    LangItem(LangItem),
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct Attribute {
    pub name: Positioned<Ident>,
    pub value: Option<Positioned<AttributeValue>>,
}

impl Parse for Attribute {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Attr)?;

        parser.consume(&Token::BracketOpen)?;

        let name = Ident::parse(parser)?;
        let value = if parser.try_consume(&Token::Eq) {
            let value = LiteralExpr::parse(parser)?;
            let value = match &value.value {
                LiteralExpr::String(lit) => match (name.value.0.as_str(), lit.as_str()) {
                    ("lang_item", "IntoIterator") => value.span.wrap(AttributeValue::LangItem(LangItem::IntoIterator)),
                    ("lang_item", "IntoIterator::into_iter") => value.span.wrap(AttributeValue::LangItem(LangItem::IntoIteratorIntoIter)),
                    ("lang_item", "Iterator") => value.span.wrap(AttributeValue::LangItem(LangItem::Iterator)),
                    ("lang_item", "Iterator::next") => value.span.wrap(AttributeValue::LangItem(LangItem::IteratorNext)),
                    ("lang_item", "Option") => value.span.wrap(AttributeValue::LangItem(LangItem::Option)),
                    ("lang_item", "Option::Some") => value.span.wrap(AttributeValue::LangItem(LangItem::OptionSome)),
                    ("lang_item", "Option::None") => value.span.wrap(AttributeValue::LangItem(LangItem::OptionNone)),
                    ("lang_item", "Result") => value.span.wrap(AttributeValue::LangItem(LangItem::Result)),
                    ("lang_item", "Container") => value.span.wrap(AttributeValue::LangItem(LangItem::Container)),
                    ("lang_item", "Container::children") => value.span.wrap(AttributeValue::LangItem(LangItem::ContainerChildren)),
                    ("lang_item", "Container::set_children") => value.span.wrap(AttributeValue::LangItem(LangItem::ContainerSetChildren)),
                    ("lang_item", "Range") => value.span.wrap(AttributeValue::LangItem(LangItem::Range)),
                    _ => value.span.wrap(AttributeValue::Literal(value.value)),
                },
                _ => value.span.wrap(AttributeValue::Literal(value.value)),
            };

            Some(value)
        } else {
            None
        };

        let end = parser.consume(&Token::BracketClose)?;

        Ok(start.between(&end).wrap(Self { name, value }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum Stmt {
    Expression(Expr),
    VariableDecl(VariableDecl),
    ConstDecl(ConstDecl),
    StructDecl(StructDecl),
    ViewDecl(ViewDecl),
    TraitDecl(TraitDecl),
    EnumDecl(EnumDecl),
    FuncDecl(FuncDecl),
    Impl(Impl),
    Import(Import),
    Module(ModuleDecl),
}

impl Parse for Stmt {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let attributes = {
            let mut items = Vec::new();

            while parser.check(&Token::Attr) {
                items.push(Attribute::parse(parser)?);
            }

            items
        };

        // `extern` declarations of stubs of the host's API: `extern func`
        // (without a body), `extern struct`, `extern value struct` and
        // `extern trait`. `extern` is a keyword only there.
        let external = parser.check_if(|token| token.is_ident_and(|name| name == "extern"))
            && (parser.check2_one_of(&[Token::Func, Token::Struct, Token::Enum, Token::Trait])
                || parser.peek_nth(1).is_some_and(|token| token.value.is_ident_and(|name| name == "value")));
        let extern_start = if external { parser.next().map(|token| token.span) } else { None };

        if external && parser.check(&Token::Func) {
            let decl = FuncDecl::parse_with(parser, true)?;
            let span = extern_start.map_or(decl.span, |start| start.between(decl.span));

            return Ok(span.wrap(Self::FuncDecl(decl.value)));
        }

        // `value struct` and `value enum`: `value` is a keyword only there, it
        // stays a name everywhere else.
        let value = parser.check_if(|token| token.is_ident_and(|name| name == "value")) && parser.check2_one_of(&[Token::Struct, Token::Enum]);
        let start = if value { parser.next().map(|token| token.span) } else { None };

        match parser.peek().map(|v| &v.value) {
            Some(Token::View) => Ok(ViewDecl::parse(parser, attributes)?.map(Self::ViewDecl)),
            Some(Token::Struct) => {
                let decl = StructDecl::parse(parser, attributes)?;
                let span = start.map_or(decl.span, |start| start.between(decl.span));

                Ok(span.wrap(Self::StructDecl(StructDecl { value, ..decl.value })))
            }
            Some(Token::Enum) => {
                let decl = EnumDecl::parse(parser, attributes)?;
                let span = start.map_or(decl.span, |start| start.between(decl.span));

                Ok(span.wrap(Self::EnumDecl(EnumDecl { value, ..decl.value })))
            }
            Some(Token::Let) => Ok(VariableDecl::parse(parser)?.map(Self::VariableDecl)),
            Some(Token::Const) => Ok(ConstDecl::parse(parser)?.map(Self::ConstDecl)),
            Some(Token::Impl) => Ok(Impl::parse(parser)?.map(Self::Impl)),
            Some(Token::Func | Token::Postfix | Token::Public) => Ok(FuncDecl::parse(parser)?.map(Self::FuncDecl)),
            Some(Token::Trait) => Ok(TraitDecl::parse(parser, attributes)?.map(Self::TraitDecl)),
            Some(Token::Import) => Ok(Import::parse(parser)?.map(Self::Import)),
            Some(Token::Module) => Ok(ModuleDecl::parse(parser)?.map(Self::Module)),
            Some(_) => Ok(Expr::parse(parser)?.map(Self::Expression)),
            None => Err(ParseError::new("unexpected <EOF>", None)),
        }
    }
}

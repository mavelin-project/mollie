use mollie_lexer::{TemplatePart as TokenTemplatePart, Token};
use mollie_shared::{FormatSpec, Positioned};

use crate::{Expr, Parse, ParseError, ParseResult, Parser};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum TemplatePart {
    Text(String),
    /// `${value}`, or `${value:spec}` with a format specifier.
    Expr(Box<Positioned<Expr>>, Option<FormatSpec>),
}

/// A string with interpolated expressions: `"Hi, ${name}!"`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct TemplateExpr(pub Vec<TemplatePart>);

impl Parse for TemplateExpr {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let token = parser.consume_if(|token| matches!(token, Token::Template(_)))?;
        let Token::Template(parts) = token.value else {
            unreachable!("only templates are consumed")
        };

        let parts = parts
            .into_iter()
            .map(|part| match part {
                TokenTemplatePart::Text(text) => Ok(TemplatePart::Text(text)),
                TokenTemplatePart::Expr(mut tokens, spec) => {
                    let mut parser = parser.sub(&mut tokens);
                    let expr = Expr::parse(&mut parser)?;

                    // The whole interpolated part must be one expression.
                    if !parser.check(&Token::EOF) {
                        return Err(ParseError::unexpected_token(parser.peek()));
                    }

                    let spec = match spec {
                        Some(spec) => {
                            Some(FormatSpec::parse(&spec).ok_or_else(|| ParseError::new(format!("invalid format specifier `{spec}`"), Some(expr.span)))?)
                        }
                        None => None,
                    };

                    Ok(TemplatePart::Expr(Box::new(expr), spec))
                }
            })
            .collect::<ParseResult<Vec<_>>>()?;

        Ok(token.span.wrap(Self(parts)))
    }
}

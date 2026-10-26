#![allow(missing_docs)]

mod error;
mod statement;
mod ty;

use std::{fmt, mem};

use mollie_lexer::{Lexer, Token};
use mollie_shared::{
    Positioned,
    limits::{MAX_NESTING, grow_stack},
};

pub use self::{
    error::{ParseError, ParseResult},
    statement::*,
    ty::*,
};

#[derive(Debug)]
pub struct Parser<'t> {
    tokens: &'t mut [Positioned<Token>],
    end: Positioned<Token>,
    depth: usize,
}

const fn placeholder() -> Positioned<Token> {
    Positioned {
        value: Token::EOF,
        span: mollie_shared::Span::new(0, 0, mollie_shared::SpanRange::new(0, 0, 0, 0)),
    }
}

impl<'t> Parser<'t> {
    #[must_use]
    pub fn new(tokens: &'t mut [Positioned<Token>]) -> Self {
        // Lexed tokens end with `EOF`; only an empty source has none, and its
        // errors are at its start.
        let end = tokens.last().cloned().unwrap_or_else(placeholder);

        Self { tokens, end, depth: 0 }
    }

    #[must_use]
    pub fn sub<'s>(&self, tokens: &'s mut [Positioned<Token>]) -> Parser<'s> {
        let end = tokens.last().cloned().unwrap_or_else(|| self.end.clone());

        Parser { tokens, end, depth: self.depth }
    }

    #[must_use]
    pub fn current(&self) -> &Positioned<Token> {
        self.peek().unwrap_or(&self.end)
    }

    pub fn check_depth(&mut self, extra: usize) -> ParseResult<()> {
        if self.depth + extra > MAX_NESTING {
            Err(ParseError::new(
                format!("the code is nested too deeply (more than {MAX_NESTING} levels)"),
                self.current().span,
            ))
        } else {
            Ok(())
        }
    }

    pub fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> ParseResult<T>) -> ParseResult<T> {
        self.check_depth(1)?;
        self.depth += 1;

        let result = grow_stack(|| parse(self));

        self.depth -= 1;

        result
    }

    pub fn split(&mut self, start: &Token, end: &Token) -> ParseResult<([Positioned<Token>; 2], Self)> {
        let start_position = self.consume(start)?;
        let mut skip = 0usize;
        let mut end_index = None;

        for (index, token) in self.tokens.iter().enumerate() {
            if &token.value == start {
                skip += 1;
            } else if &token.value == end {
                if skip == 0 {
                    end_index = Some(index);

                    break;
                }

                skip -= 1;
            }
        }

        let Some(end_index) = end_index else {
            return Err(ParseError::expected_token(end, self.tokens.last().unwrap_or(&self.end)));
        };

        let (inside, rest) = mem::take(&mut self.tokens).split_at_mut(end_index);
        let Some((end_position, rest)) = rest.split_first_mut() else {
            unreachable!("the end token was found");
        };

        self.tokens = rest;

        let end_position = mem::replace(end_position, placeholder());

        Ok(([start_position, end_position.clone()], Parser {
            tokens: inside,
            end: end_position,
            depth: self.depth,
        }))
    }

    pub fn try_consume(&mut self, value: &Token) -> bool {
        self.next_if(|token| token == value).is_some()
    }

    pub fn try_consume_then<T, F: FnOnce(&mut Self) -> ParseResult<T>>(&mut self, value: &Token, func: F) -> ParseResult<Option<T>> {
        if self.try_consume(value) { func(self).map(Some) } else { Ok(None) }
    }

    fn verify_nth(&self, index: usize, token: &Token) -> ParseResult<()> {
        if self.peek_nth(index).is_some_and(|value| &value.value == token) {
            Ok(())
        } else {
            Err(ParseError::expected_token(token, self.peek_nth(index).unwrap_or(&self.end)))
        }
    }

    fn verify_nth_if<F: Fn(&Token) -> bool>(&self, index: usize, func: F) -> ParseResult<()> {
        if self.peek_nth(index).is_some_and(|value| func(&value.value)) {
            Ok(())
        } else {
            Err(ParseError::unexpected_token(self.peek_nth(index).unwrap_or(&self.end)))
        }
    }

    pub const fn len(&self) -> usize {
        self.tokens.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn verify(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(0, token)
    }

    pub fn verify2(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(1, token)
    }

    pub fn verify3(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(2, token)
    }

    pub fn verify_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(0, func)
    }

    pub fn verify2_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(1, func)
    }

    pub fn verify3_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(2, func)
    }

    pub fn check_one_of(&mut self, values: &[Token]) -> bool {
        self.check_if(|v| values.contains(v))
    }

    pub fn check2_one_of(&mut self, values: &[Token]) -> bool {
        self.check2_if(|v| values.contains(v))
    }

    pub fn check3_one_of(&mut self, values: &[Token]) -> bool {
        self.check3_if(|v| values.contains(v))
    }

    pub fn check(&mut self, value: &Token) -> bool {
        self.check_if(|v| v == value)
    }

    pub fn check2(&mut self, value: &Token) -> bool {
        self.check2_if(|v| v == value)
    }

    pub fn check3(&mut self, value: &Token) -> bool {
        self.check3_if(|v| v == value)
    }

    pub fn check_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(0).is_some_and(|value| func(&value.value))
    }

    pub fn check2_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(1).is_some_and(|value| func(&value.value))
    }

    pub fn check3_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(2).is_some_and(|value| func(&value.value))
    }

    pub fn consume_separated<T: Parse>(&mut self, separator: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = vec![T::parse(self)?];

        while self.try_consume(separator) {
            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    pub fn consume_separated_until<T: Parse>(&mut self, separator: &Token, until: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = vec![T::parse(self)?];

        while !self.check(until) {
            self.consume(separator)?;

            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    pub fn consume_separated_in<T: Parse>(&mut self, separator: &Token, from: &Token, to: &Token) -> ParseResult<Positioned<Vec<Positioned<T>>>> {
        let from = self.consume(from)?;

        let mut values = vec![];

        while !self.check(to) {
            if !values.is_empty() {
                self.consume(separator)?;
            }

            if self.check(to) {
                break;
            }

            values.push(T::parse(self)?);
        }

        let to = self.consume(to)?;

        Ok(from.between(&to).wrap(values))
    }

    pub fn consume_in<T: Parse>(&mut self, from: &Token, to: &Token) -> ParseResult<Positioned<Vec<Positioned<T>>>> {
        let from = self.consume(from)?;

        let mut values = Vec::new();

        while !self.check(to) {
            values.push(T::parse(self)?);
        }

        let to = self.consume(to)?;

        Ok(from.between(&to).wrap(values))
    }

    pub fn consume_until<T: Parse>(&mut self, value: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = Vec::new();

        while !self.check(value) {
            values.push(T::parse(self)?);
        }

        self.consume(value)?;

        Ok(values)
    }

    pub fn consume_while<T: Parse, F: Fn(&mut Self) -> bool>(&mut self, func: F) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = Vec::new();

        while func(self) {
            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    pub fn consume(&mut self, value: &Token) -> ParseResult<Positioned<Token>> {
        self.next_if(|current| current == value)
            .map_or_else(|| Err(ParseError::expected_token(value, self.current())), Ok)
    }

    pub fn consume_one_of(&mut self, values: &[Token]) -> ParseResult<Positioned<Token>> {
        self.next_if(|value| values.contains(value))
            .map_or_else(|| Err(ParseError::expected_tokens(values, self.current())), Ok)
    }

    pub fn consume_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<Positioned<Token>> {
        self.next_if(func).map_or_else(|| Err(ParseError::unexpected_token(self.current())), Ok)
    }

    pub fn consume_map<T, F: Fn(&Token) -> Option<T>>(&mut self, func: F) -> ParseResult<Positioned<T>> {
        if let Some(value) = self.peek().and_then(|value| func(&value.value).map(|result| value.span.wrap(result))) {
            self.next();

            Ok(value)
        } else {
            Err(ParseError::unexpected_token(self.current()))
        }
    }

    #[allow(clippy::should_implement_trait, clippy::mem_replace_with_default)]
    pub const fn next(&mut self) -> Option<Positioned<Token>> {
        let Some((first, rest)) = mem::replace(&mut self.tokens, &mut []).split_first_mut() else {
            return None;
        };

        self.tokens = rest;

        Some(mem::replace(first, placeholder()))
    }

    pub const fn peek(&self) -> Option<&Positioned<Token>> {
        self.tokens.first()
    }

    pub fn peek_nth(&self, n: usize) -> Option<&Positioned<Token>> {
        self.tokens.get(n)
    }

    pub fn next_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> Option<Positioned<Token>> {
        if self.peek().is_some_and(|token| func(&token.value)) {
            self.next()
        } else {
            None
        }
    }

    #[must_use]
    pub fn collect(self) -> Vec<Positioned<Token>> {
        self.tokens.iter_mut().map(|token| mem::replace(token, placeholder())).collect()
    }

    pub fn expected_token<T: fmt::Display>(&self, expected: T) -> ParseError {
        let found = self.current();

        ParseError(format!("Expected {expected}, found {}", found.value), found.span)
    }
}

pub trait Parse: Sized {
    fn parse_value<T: AsRef<str>>(value: T) -> ParseResult<Positioned<Self>> {
        let mut tokens = Lexer::lex(value);
        let mut parser = Parser::new(&mut tokens);

        let value = Self::parse(&mut parser)?;

        parser.try_consume(&Token::EOF);

        Ok(value)
    }

    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>>;
}

#[cfg(test)]
mod tests {
    use mollie_lexer::{Lexer, Token};

    use crate::{Expr, Parse, Parser};

    #[test]
    fn test_node_parsing() {
        let mut tokens = Lexer::lex("Option::Some(value)");
        let mut parser = Parser::new(&mut tokens);
        let expr = Expr::parse(&mut parser);

        println!("{expr:#?}");
    }

    fn parse_expr(source: &str) -> Expr {
        let mut tokens = Lexer::lex(source);
        let mut parser = Parser::new(&mut tokens);

        Expr::parse(&mut parser).expect("the expression must parse").value
    }

    #[test]
    fn negated_conditions_keep_their_block() {
        // `!done { ... }` isn't a node: the braces are the `if`'s block.
        for source in ["if !done { 1 } else { 2 }", "if !self.ready { 1 } else { 2 }", "while -x { 1 }"] {
            let parsed = parse_expr(source);

            assert!(matches!(parsed, Expr::IfElse(_) | Expr::While(_)), "{source}: {parsed:?}");
        }
    }

    #[test]
    fn parentheses_hold_one_expression() {
        let Expr::Binary(product) = parse_expr("((a + b) * (c))") else {
            panic!("expected a product");
        };

        assert!(matches!(product.lhs.value, Expr::Binary(_)));
        assert!(matches!(parse_expr("()"), Expr::Nothing));
        // The parser goes on after the parentheses.
        assert!(matches!(parse_expr("(a) + (b)"), Expr::Binary(_)));

        // Tokens after the expression are an error, not dropped.
        for source in ["(1 2)", "(a + b c d)", "((a) b)"] {
            let mut tokens = Lexer::lex(source);

            assert!(Expr::parse(&mut Parser::new(&mut tokens)).is_err(), "{source}");
        }
    }

    #[test]
    fn ranges_bind_looser_than_arithmetic() {
        let Expr::Range(range) = parse_expr("a + 1..b * 2") else {
            panic!("expected a range");
        };

        assert!(!range.inclusive);
        assert!(matches!(range.start.value, Expr::Binary(_)));
        assert!(matches!(range.end.value, Expr::Binary(_)));

        let Expr::Range(range) = parse_expr("0..=n") else {
            panic!("expected a range");
        };

        assert!(range.inclusive);
    }

    #[test]
    fn labeled_loops() {
        let Expr::Loop(loop_expr) = parse_expr("'outer: loop { break 'outer 1; }") else {
            panic!("expected a loop");
        };

        assert_eq!(loop_expr.label.map(|label| label.value.0), Some(String::from("outer")));
        assert!(matches!(parse_expr("'rows: for row in rows { continue 'rows; }"), Expr::ForIn(for_in) if for_in.label.is_some()));
        assert!(matches!(parse_expr("'wait: while ready { break; }"), Expr::While(while_expr) if while_expr.label.is_some()));
    }

    fn parse_node(source: &str) -> crate::NodeExpr {
        match parse_expr(source) {
            Expr::Node(node) => node,
            other => panic!("expected a node: {other:?}"),
        }
    }

    #[test]
    fn closure_fields() {
        let node = parse_node("Button { on_click => { play(); }, on_change(value) => { value }, on_drag[start = x, game](dx, dy) => { dx } }");

        assert_eq!(node.properties.len(), 3);

        let value = |index: usize| node.properties[index].value.value.as_ref().map(|value| &value.value);

        assert!(matches!(value(0), Some(Expr::Closure(closure)) if closure.args.value.is_empty()));
        assert!(matches!(value(1), Some(Expr::Closure(closure)) if closure.args.value.len() == 1));

        // Bindings are `let`s of a block whose value is the closure.
        let Some(Expr::Block(block)) = value(2) else {
            panic!("expected a block: {:?}", value(2));
        };

        assert_eq!(block.stmts.len(), 2);
        assert!(block.stmts.iter().all(|stmt| matches!(stmt.value, crate::Stmt::VariableDecl(_))));
        assert!(
            matches!(&block.final_stmt.as_ref().map(|stmt| &stmt.value), Some(crate::Stmt::Expression(Expr::Closure(closure))) if closure.args.value.len() == 2)
        );
        assert!(node.children.value.is_empty());
    }

    #[test]
    fn closure_fields_before_children() {
        let node = parse_node("Button { on_click => { play(); } Label { text: \"Play\" } }");

        assert_eq!(node.properties.len(), 1);
        assert!(matches!(&node.children.value[..], [child] if matches!(child.value, crate::NodeChild::Node(_))));
    }

    #[test]
    fn control_flow_in_children() {
        use crate::NodeChild;

        let node = parse_node(
            "Column { spacing: 2.0, Title {} if !loaded { Spinner {} } else if failed { Error {} } else { Play {} Quit {} } for item in items { Row { Text { content: item } } } { footer(game) } }",
        );

        assert_eq!(node.properties.len(), 1);

        let children = &node.children.value;

        assert_eq!(children.len(), 4);
        assert!(matches!(children[0].value, NodeChild::Node(_)));

        let NodeChild::If { condition, then, otherwise } = &children[1].value else {
            panic!("expected `if`: {:?}", children[1]);
        };

        assert!(matches!(condition.value, Expr::Unary(_)));
        assert_eq!(then.value.len(), 1);

        // `else if` is an `else` with a single `if`.
        let otherwise = otherwise.as_ref().expect("`else`");

        assert!(matches!(&otherwise.value[..], [child] if matches!(&child.value, NodeChild::If { otherwise: Some(last), .. } if last.value.len() == 2)));
        assert!(matches!(&children[2].value, NodeChild::For { body, .. } if body.value.len() == 1));
        assert!(matches!(children[3].value, NodeChild::Expr(Expr::Block(_))));
    }

    fn parse_stmts(source: &str) -> crate::ParseResult<Vec<crate::Stmt>> {
        let mut tokens = Lexer::lex(source);
        let mut parser = Parser::new(&mut tokens);
        let mut stmts = Vec::new();

        // The lexer ends the tokens with `EOF`.
        while parser.peek().is_some_and(|token| token.value != Token::EOF) {
            stmts.push(crate::Stmt::parse(&mut parser)?.value);
        }

        Ok(stmts)
    }

    #[test]
    fn pub_marks_items_and_fields() {
        use crate::Stmt;

        let stmts = parse_stmts(
            "pub struct Point { pub x: f32, y: f32 }
            pub value enum Shape { Circle { radius: f32 } }
            pub view Label { pub text: string, size: f32 = 12.0, pub color?: string }
            pub trait Draw { func draw(self); }
            pub func area() -> f32 { 1.0 }
            pub postfix func px(value: f32) -> f32 { value }
            pub extern func reveal() -> i32;
            pub const LIMIT: i32 = 4;
            pub import { Point } from shapes;
            pub module shapes;
            @[lang_item = \"Option\"]
            pub enum Maybe { Nothing }
            impl Point { pub func length(self) -> f32 { 0.0 } func hidden(self) {} }
            struct Hidden { x: f32 }",
        )
        .expect("the items must parse");

        let Stmt::StructDecl(point) = &stmts[0] else { panic!("expected a struct") };

        assert!(point.public);
        assert_eq!(point.properties.value.iter().map(|field| field.value.public).collect::<Vec<_>>(), [true, false]);
        assert!(matches!(&stmts[1], Stmt::EnumDecl(decl) if decl.public && decl.value));

        let Stmt::ViewDecl(label) = &stmts[2] else { panic!("expected a view") };

        assert!(label.public);
        assert_eq!(label.properties.iter().map(|field| field.value.public).collect::<Vec<_>>(), [true, false, true]);
        assert!(matches!(&stmts[3], Stmt::TraitDecl(decl) if decl.public));
        assert!(matches!(&stmts[4], Stmt::FuncDecl(decl) if decl.public));
        assert!(matches!(&stmts[5], Stmt::FuncDecl(decl) if decl.public && decl.modifiers.len() == 1));
        assert!(matches!(&stmts[6], Stmt::FuncDecl(decl) if decl.public && decl.external));
        assert!(matches!(&stmts[7], Stmt::ConstDecl(decl) if decl.public));
        assert!(matches!(&stmts[8], Stmt::Import(decl) if decl.public));
        assert!(matches!(&stmts[9], Stmt::Module(decl) if decl.public));
        assert!(matches!(&stmts[10], Stmt::EnumDecl(decl) if decl.public));

        let Stmt::Impl(implementation) = &stmts[11] else { panic!("expected an impl") };

        assert_eq!(implementation.functions.value.iter().map(|func| func.value.public).collect::<Vec<_>>(), [true, false]);
        assert!(matches!(&stmts[12], Stmt::StructDecl(decl) if !decl.public && !decl.properties.value[0].value.public));
    }

    #[test]
    fn variables_are_declared_without_values() {
        use crate::Stmt;

        let stmts = parse_stmts("extern let mut data: Data;\nlet x: i32;\nlet y;").expect("the declarations must parse");

        assert!(matches!(&stmts[0], Stmt::VariableDecl(decl) if decl.external && decl.mutable.is_some() && decl.ty.is_some() && decl.value.is_none()));
        assert!(matches!(&stmts[1], Stmt::VariableDecl(decl) if !decl.external && decl.ty.is_some() && decl.value.is_none()));
        assert!(matches!(&stmts[2], Stmt::VariableDecl(decl) if decl.ty.is_none() && decl.value.is_none()));
    }

    #[test]
    fn views_end_before_the_next_pub_item() {
        use crate::Stmt;

        // The `pub` after the view's `}` isn't one of its properties.
        let stmts = parse_stmts("pub view Button { pub width: f32 = 0.0, inner: f32 = 1.0 }\npub func button() {}").expect("the items must parse");

        assert!(matches!(&stmts[0], Stmt::ViewDecl(decl) if decl.properties.len() == 2));
        assert!(matches!(&stmts[1], Stmt::FuncDecl(decl) if decl.public));
    }

    #[test]
    fn pub_only_marks_items() {
        for source in ["pub let x = 1;", "pub x + 1;", "pub impl Point {}", "pub pub func f() {}"] {
            assert!(parse_stmts(source).is_err(), "{source}");
        }

        // `public` isn't a keyword.
        assert!(parse_stmts("let public = 1;").is_ok());
    }

    #[test]
    fn array_types() {
        fn kind_of(source: &str) -> crate::ArrayKind {
            let mut tokens = Lexer::lex(source);
            let mut parser = Parser::new(&mut tokens);

            match crate::Type::parse(&mut parser).expect("the type must parse").value {
                crate::Type::Array(_, kind) => kind,
                ty => panic!("expected an array type, found {ty:?}"),
            }
        }

        assert_eq!(kind_of("i32[]"), crate::ArrayKind::Growable);
        assert!(matches!(kind_of("i32[3]"), crate::ArrayKind::Fixed(size) if size.value == 3));
        assert_eq!(kind_of("f32[..]"), crate::ArrayKind::View);
        assert_eq!(kind_of("string[_]"), crate::ArrayKind::AnySize);
        // The outer array holds arrays: four rows of two.
        assert!(matches!(kind_of("f32[2][4]"), crate::ArrayKind::Fixed(size) if size.value == 4));
    }

    #[test]
    fn mutable_and_variadic_parameters() {
        let stmts = parse_stmts("func f(mut values: f32[..], ..rest: string[..]) {}").expect("the function must parse");
        let crate::Stmt::FuncDecl(func) = &stmts[0] else { panic!("expected a function") };
        let args = &func.args.value;

        assert!(args[0].value.mutable && !args[0].value.variadic);
        assert!(args[1].value.variadic && !args[1].value.mutable);
        assert_eq!(args[1].value.name.value.0, "rest");
    }
}

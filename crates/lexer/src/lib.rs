mod token;

use std::{
    cell::Cell,
    iter::{self, Peekable},
    mem,
    ops::Neg,
    str::Chars,
};

use mollie_shared::{Positioned, Span, SpanRange, limits::MAX_INTERPOLATION_NESTING};

pub use crate::token::{NumberToken, TemplatePart, Token};

pub struct Lexer;

thread_local! {
    /// How deeply the interpolation being lexed is nested in others.
    static INTERPOLATION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

static KEYWORDS: phf::Map<&'static str, Token> = phf::phf_map! {
    "true" => Token::Bool(true),
    "false" => Token::Bool(false),
    "self" => Token::This,
    "view" => Token::View,
    "inherits" => Token::Inherits,
    "as" => Token::As,
    "from" => Token::From,
    "struct" => Token::Struct,
    "enum" => Token::Enum,
    "import" => Token::Import,
    "module" => Token::Module,
    "super" => Token::Super,
    "switch" => Token::Switch,
    "match" => Token::Match,
    "return" => Token::Return,
    "func" => Token::Func,
    "trait" => Token::Trait,
    "impl" => Token::Impl,
    "const" => Token::Const,
    "let" => Token::Let,
    "mut" => Token::Mut,
    "while" => Token::While,
    "for" => Token::For,
    "public" => Token::Public,
    "postfix" => Token::Postfix,
    "in" => Token::In,
    "loop" => Token::Loop,
    "break" => Token::Break,
    "continue" => Token::Continue,
    "if" => Token::If,
    "else" => Token::Else,
    "is" => Token::Is,
};

impl Lexer {
    fn lex_reserved(ident: String) -> Token {
        KEYWORDS.get(&ident).cloned().unwrap_or(Token::Ident(ident))
    }

    fn lex_other(chars: &mut Peekable<Chars>, span: &mut Span, character: char) -> Option<Token> {
        match character {
            '[' => Some(Token::BracketOpen),
            ']' => Some(Token::BracketClose),
            '{' => Some(Token::BraceOpen),
            '}' => Some(Token::BraceClose),
            '(' => Some(Token::ParenOpen),
            ')' => Some(Token::ParenClose),
            ':' => {
                if chars.next_if_eq(&':').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::PathSep)
                } else {
                    Some(Token::Colon)
                }
            }
            ';' => Some(Token::Semi),
            '+' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::PlusEq)
                } else {
                    Some(Token::Plus)
                }
            }
            '*' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::StarEq)
                } else {
                    Some(Token::Star)
                }
            }
            '/' => {
                if chars.next_if_eq(&'/').is_some() {
                    let mut utf8size = 0;

                    while let Some(character) = chars.next_if(|character| character != &'\n') {
                        utf8size += character.len_utf8();
                    }

                    chars.next_if_eq(&'\n');

                    span.start = span.end;
                    span.end += utf8size + 2;
                    span.range.add_lines(1);
                    span.range.set_column(0);

                    None
                } else if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::SlashEq)
                } else {
                    Some(Token::Slash)
                }
            }
            '=' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::EqEq)
                } else if chars.next_if_eq(&'>').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::FatArrow)
                } else {
                    Some(Token::Eq)
                }
            }
            '&' => {
                if chars.next_if_eq(&'&').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::AndAnd)
                } else if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::AndEq)
                } else {
                    Some(Token::And)
                }
            }
            '|' => {
                if chars.next_if_eq(&'|').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::OrOr)
                } else if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::OrEq)
                } else {
                    Some(Token::Or)
                }
            }
            '%' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::PercentEq)
                } else {
                    Some(Token::Percent)
                }
            }
            '.' => {
                if chars.next_if_eq(&'.').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    if chars.next_if_eq(&'=').is_some() {
                        span.end += 1;
                        span.range.add_columns(1);

                        Some(Token::DotDotEq)
                    } else {
                        Some(Token::DotDot)
                    }
                } else {
                    Some(Token::Dot)
                }
            }
            ',' => Some(Token::Comma),
            '!' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::NotEq)
                } else {
                    Some(Token::Not)
                }
            }
            // '#' => Token::Pound,
            '@' => Some(Token::Attr),
            '?' => Some(Token::Question),
            '>' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::GreaterEq)
                } else {
                    Some(Token::Greater)
                }
            }
            '<' => {
                if chars.next_if_eq(&'=').is_some() {
                    span.end += 1;
                    span.range.add_columns(1);

                    Some(Token::LessEq)
                } else {
                    Some(Token::Less)
                }
            }
            character => Some(Token::Unknown(character)),
        }
    }

    const fn len_utf8(char: char) -> u32 {
        const MAX_ONE_B: u32 = 0x80;
        const MAX_TWO_B: u32 = 0x800;
        const MAX_THREE_B: u32 = 0x10000;

        match char as u32 {
            ..MAX_ONE_B => 1,
            MAX_ONE_B..MAX_TWO_B => 2,
            MAX_TWO_B..MAX_THREE_B => 3,
            _ => 4,
        }
    }

    /// Lexes a string literal after its opening quote, which is at the byte
    /// offset `start` and the 0-based `line` and `column`. Returns the token
    /// with the number of characters and bytes consumed (without quotes).
    ///
    /// Supports escapes (`\n`, `\t`, `\r`, `\0`, and `\` before any other
    /// character to keep it as is) and interpolation: `"a ${b} c"` produces a
    /// [`Token::Template`].
    /// Splits `value:spec` of an interpolation at its last `:` outside of
    /// strings and brackets that isn't a part of `::`, if what follows it
    /// looks like a format specifier (which the parser checks).
    fn split_format_spec(source: &str) -> (&str, Option<&str>) {
        let bytes = source.as_bytes();
        let mut depth = 0isize;
        let mut in_string = false;
        let mut escaped = false;
        let mut split = None;

        for (index, character) in source.char_indices() {
            if in_string {
                match character {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => (),
                }

                continue;
            }

            match character {
                '"' => in_string = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ':' if depth == 0 && bytes.get(index + 1) != Some(&b':') && (index == 0 || bytes[index - 1] != b':') => split = Some(index),
                _ => (),
            }
        }

        match split {
            Some(index)
                if {
                    let spec = source[index + 1..].trim();

                    !spec.is_empty()
                        && spec
                            .chars()
                            .all(|character| matches!(character, '<' | '>' | '^' | '.' | 'x' | 'X' | 'b' | '0'..='9'))
                } =>
            {
                (&source[..index], Some(source[index + 1..].trim()))
            }
            _ => (source, None),
        }
    }

    fn parse_string(chars: &mut Peekable<Chars>, start: usize, line: u32, column: u32) -> (Token, u32, u32) {
        let mut size = 0;
        let mut utf8size = 0;
        let mut text = String::new();
        let mut parts = Vec::new();
        let mut is_template = false;

        while let Some(character) = chars.next_if(|character| character != &'"') {
            size += 1;
            utf8size += Self::len_utf8(character);

            match character {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        size += 1;
                        utf8size += Self::len_utf8(escaped);

                        text.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '0' => '\0',
                            escaped => escaped,
                        });
                    }
                }
                '$' if chars.peek() == Some(&'{') => {
                    chars.next();
                    size += 1;
                    utf8size += 1;

                    // Position of the expression's first character (after the
                    // opening quote).
                    let expr_start = start + 1 + utf8size as usize;
                    let expr_column = column + 1 + size;
                    let mut source = String::new();
                    let mut depth = 0usize;
                    let mut in_string = false;

                    while let Some(character) = chars.next() {
                        size += 1;
                        utf8size += Self::len_utf8(character);

                        if in_string {
                            match character {
                                '\\' => {
                                    if let Some(escaped) = chars.next() {
                                        size += 1;
                                        utf8size += Self::len_utf8(escaped);
                                        source.push(character);
                                        source.push(escaped);

                                        continue;
                                    }
                                }
                                '"' => in_string = false,
                                _ => (),
                            }
                        } else {
                            match character {
                                '"' => in_string = true,
                                '{' => depth += 1,
                                '}' if depth == 0 => break,
                                '}' => depth -= 1,
                                _ => (),
                            }
                        }

                        source.push(character);
                    }

                    if !text.is_empty() {
                        parts.push(TemplatePart::Text(mem::take(&mut text)));
                    }

                    let (source, spec) = Self::split_format_spec(&source);
                    // Interpolations in strings in interpolations are lexed
                    // recursively, up to a limit.
                    let nested = INTERPOLATION_DEPTH.with(|depth| {
                        let nested = depth.get() < MAX_INTERPOLATION_NESTING;

                        if nested {
                            depth.set(depth.get() + 1);
                        }

                        nested
                    });
                    let mut tokens = if nested {
                        let tokens = Self::lex(source);

                        INTERPOLATION_DEPTH.with(|depth| depth.set(depth.get() - 1));

                        tokens
                    } else {
                        let length = u32::try_from(source.len()).unwrap_or(u32::MAX);

                        vec![
                            Span::new(0, source.len(), SpanRange::new(0, 0, 0, length)).wrap(Token::Invalid(format!(
                                "strings are interpolated in each other too deeply (more than {MAX_INTERPOLATION_NESTING} levels)"
                            ))),
                            Span::new(source.len(), source.len(), SpanRange::new(0, length, 0, length)).wrap(Token::EOF),
                        ]
                    };

                    // Tokens of the expression are positioned in the string.
                    for token in &mut tokens {
                        token.span.start += expr_start;
                        token.span.end += expr_start;

                        if token.span.range.start_line == 0 {
                            token.span.range.start_column += expr_column;
                        }

                        if token.span.range.end_line == 0 {
                            token.span.range.end_column += expr_column;
                        }

                        token.span.range.add_lines(line);
                    }

                    parts.push(TemplatePart::Expr(tokens, spec.map(str::to_owned)));
                    is_template = true;
                }
                character => text.push(character),
            }
        }

        chars.next_if_eq(&'"');

        let token = if is_template {
            if !text.is_empty() {
                parts.push(TemplatePart::Text(text));
            }

            Token::Template(parts)
        } else {
            Token::String(text)
        };

        (token, size, utf8size)
    }

    fn lex_number(chars: &mut Peekable<Chars>, tokens: &mut Vec<Positioned<Token>>, span: &mut Span, character: char, neg: bool) {
        if character == '0' && chars.next_if_eq(&'x').is_some() {
            let hex = iter::from_fn(|| chars.by_ref().next_if(char::is_ascii_hexdigit)).collect::<String>();

            span.start = span.end;
            span.end += hex.len() + 2;

            let token = i64::from_str_radix(&hex, 16).map_or_else(
                |_| Token::InvalidNumber(format!("0x{hex}")),
                |value| Token::Number(span.wrap(NumberToken::I64(if neg { value.neg() } else { value })), None),
            );

            tokens.push(span.wrap(token));
        } else {
            let mut size = 1;
            let mut value = String::from(character);

            // A dot is part of the number only if a digit follows it, so
            // `1..2` and `1.len()` aren't numbers with dots.
            let dot_starts_fraction = |chars: &Peekable<Chars>| {
                let mut ahead = chars.clone();

                ahead.next();
                ahead.peek().is_some_and(char::is_ascii_digit)
            };

            while let Some(c) = chars.next_if(|c| c.is_ascii_digit() || *c == '_').or_else(|| {
                if chars.peek() == Some(&'.') && !value.contains('.') && dot_starts_fraction(chars) {
                    chars.next()
                } else {
                    None
                }
            }) {
                size += 1;

                if c != '_' {
                    value.push(c);
                }
            }

            span.start = span.end;
            // Digits, dots and underscores are ASCII, so `size` is the length
            // in bytes too (`value` has no underscores).
            span.end += size as usize;
            span.range.start_column = span.range.end_column;
            span.range.end_column += size;

            let number = if value.contains('.') {
                value.parse().map(NumberToken::F32).ok()
            } else {
                value.parse().map(NumberToken::I64).ok()
            };

            let Some(number) = number else {
                tokens.push(span.wrap(Token::InvalidNumber(value)));

                return;
            };

            let mut number = span.wrap(number);

            if neg {
                match &mut number.value {
                    NumberToken::F32(value) => *value = value.neg(),
                    NumberToken::I64(value) => *value = value.neg(),
                }
            }

            let postfix = if let Some(c) = chars.next_if(char::is_ascii_alphabetic) {
                let mut size = 1;
                let mut postfix = String::from(c);

                while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || c == &'_') {
                    size += 1;
                    postfix.push(c);
                }

                span.start = span.end;
                span.end += postfix.len();
                span.range.start_column = span.range.end_column;
                span.range.end_column += size;

                let postfix = span.wrap(postfix);

                Some(postfix)
            } else {
                None
            };

            tokens.push(
                number
                    .span
                    .between(postfix.as_ref().map_or(number.span, |postfix| postfix.span))
                    .wrap(Token::Number(number, postfix)),
            );
        }
    }

    /// # Panics
    ///
    /// Can panic if number failed to parse
    pub fn lex<T: AsRef<str>>(data: T) -> Vec<Positioned<Token>> {
        let mut tokens = vec![];
        let mut chars = data.as_ref().chars().peekable();
        let mut span = Span::new(0, 0, SpanRange::from_single(0, 0));

        while let Some(character) = chars.next() {
            match character {
                'A'..='Z' | 'a'..='z' | '_' => {
                    span.start = span.end;
                    span.range.start_column = span.range.end_column;
                    span.end += 1;
                    span.range.end_column += 1;

                    let mut ident = String::from(character);

                    while let Some(c) = chars.by_ref().next_if(|s| s.is_ascii_alphanumeric() || s == &'_') {
                        span.end += 1;
                        span.range.end_column += 1;

                        ident.push(c);
                    }

                    tokens.push(span.wrap(Self::lex_reserved(ident)));
                }
                '0'..='9' => {
                    Self::lex_number(&mut chars, &mut tokens, &mut span, character, false);
                }
                '-' => {
                    span.start = span.end;

                    if chars.next_if_eq(&'>').is_some() {
                        span.end += 2;

                        tokens.push(span.wrap(Token::Arrow));

                        span.range.add_columns(2);
                    } else if chars.next_if_eq(&'=').is_some() {
                        span.end += 2;

                        tokens.push(span.wrap(Token::MinusEq));

                        span.range.add_columns(2);
                    } else {
                        span.end += 1;

                        tokens.push(span.wrap(Token::Minus));

                        span.range.add_columns(1);
                    }
                }
                // A label of a loop: `'outer`.
                '\'' if chars.peek().is_some_and(|character| character.is_ascii_alphabetic() || *character == '_') => {
                    span.start = span.end;
                    span.range.start_column = span.range.end_column;

                    let mut name = String::new();

                    while let Some(character) = chars.next_if(|character| character.is_ascii_alphanumeric() || *character == '_') {
                        name.push(character);
                    }

                    span.end += name.len() + 1;
                    span.range.end_column += u32::try_from(name.len()).unwrap_or(u32::MAX) + 1;

                    tokens.push(span.wrap(Token::Label(name)));
                }
                '"' => {
                    span.start = span.end;

                    let (value, size, utf8size) = Self::parse_string(&mut chars, span.end, span.range.end_line, span.range.end_column);

                    span.start = span.end;
                    span.end += utf8size as usize + 2;
                    span.range.end_column += size + 2;

                    tokens.push(span.wrap(value));

                    span.range.start_column += size + 2;
                }
                character => {
                    span.start = span.end;
                    // Spans are in bytes: unknown characters may be longer.
                    span.end += character.len_utf8();

                    if character.is_ascii_whitespace() {
                        if character == '\n' {
                            span.range.add_lines(1);
                            span.range.set_column(0);
                        } else {
                            span.range.add_columns(1);
                        }

                        continue;
                    }

                    if let Some(token) = Self::lex_other(&mut chars, &mut span, character) {
                        tokens.push(span.wrap(token));

                        span.range.add_columns(1);
                    }
                }
            }
        }

        span.start = span.end;
        span.range.start_column = span.range.end_column;

        tokens.push(span.wrap(Token::EOF));

        tokens
    }
}

#[cfg(test)]
mod tests {
    use mollie_shared::{Positioned, Span, SpanRange, limits::MAX_INTERPOLATION_NESTING};

    use crate::{Lexer, NumberToken, TemplatePart, Token};

    fn assert_lex_single_eq(input: &str, output: &Token) {
        let lexed = Lexer::lex(input);

        assert_ne!(lexed, []);
        assert_eq!(&lexed[0].value, output);
    }

    fn assert_lex_eq<T: IntoIterator<Item = (Token, Span)>>(input: &str, output: T) {
        let lexed = Lexer::lex(input);

        assert_ne!(lexed, []);
        assert_eq!(lexed, output.into_iter().map(|(token, span)| span.wrap(token)).collect::<Vec<_>>());
    }

    #[test]
    fn test_number_parsing() {
        assert_lex_eq("bruh", [
            (Token::ident("bruh"), Span::new(0, 4, SpanRange::new(0, 0, 0, 4))),
            (Token::EOF, Span::new(4, 4, SpanRange::new(0, 4, 0, 4))),
        ]);

        assert_lex_eq("\"bruh\"", [
            (Token::String(String::from("bruh")), Span::new(0, 6, SpanRange::new(0, 0, 0, 6))),
            (Token::EOF, Span::new(6, 6, SpanRange::new(0, 6, 0, 6))),
        ]);

        assert_lex_eq("\"bruh\" \"bruh\"", [
            (Token::String(String::from("bruh")), Span::new(0, 6, SpanRange::new(0, 0, 0, 6))),
            (Token::String(String::from("bruh")), Span::new(7, 13, SpanRange::new(0, 7, 0, 13))),
            (Token::EOF, Span::new(13, 13, SpanRange::new(0, 13, 0, 13))),
        ]);

        assert_lex_single_eq(
            "123",
            &Token::Number(Span::new(0, 3, SpanRange::new(0, 0, 0, 3)).wrap(NumberToken::I64(123)), None),
        );

        assert_lex_single_eq(
            "123.0",
            &Token::Number(Span::new(0, 5, SpanRange::new(0, 0, 0, 5)).wrap(NumberToken::F32(123.0)), None),
        );

        assert_lex_single_eq(
            "123f32",
            &Token::Number(
                Span::new(0, 3, SpanRange::new(0, 0, 0, 3)).wrap(NumberToken::I64(123)),
                Some(Span::new(3, 6, SpanRange::new(0, 3, 0, 6)).wrap(String::from("f32"))),
            ),
        );

        assert_lex_single_eq(
            "123.0f32",
            &Token::Number(
                Span::new(0, 5, SpanRange::new(0, 0, 0, 5)).wrap(NumberToken::F32(123.0)),
                Some(Span::new(5, 8, SpanRange::new(0, 5, 0, 8)).wrap(String::from("f32"))),
            ),
        );
    }

    /// Values of tokens, without positions.
    fn values(input: &str) -> Vec<Token> {
        Lexer::lex(input).into_iter().map(|token| token.value).collect()
    }

    #[test]
    fn string_escapes() {
        assert_lex_single_eq(r#""a\"b""#, &Token::String(String::from("a\"b")));
        assert_lex_single_eq(r#""line\nnext\ttab""#, &Token::String(String::from("line\nnext\ttab")));
        assert_lex_single_eq(r#""back\\slash""#, &Token::String(String::from("back\\slash")));
        assert_lex_single_eq(r#""\${not interpolated}""#, &Token::String(String::from("${not interpolated}")));
        assert_lex_single_eq(r#""costs $5""#, &Token::String(String::from("costs $5")));
    }

    #[test]
    fn interpolations_nest_up_to_a_limit() {
        let nested = |depth: usize| format!("{}x{}", "\"${".repeat(depth), "}\"".repeat(depth));
        let contains_invalid = |tokens: &[Positioned<Token>]| {
            fn walk(tokens: &[Positioned<Token>]) -> bool {
                tokens.iter().any(|token| match &token.value {
                    Token::Invalid(_) => true,
                    Token::Template(parts) => parts.iter().any(|part| matches!(part, TemplatePart::Expr(tokens, _) if walk(tokens))),
                    _ => false,
                })
            }

            walk(tokens)
        };

        assert!(!contains_invalid(&Lexer::lex(nested(MAX_INTERPOLATION_NESTING))));
        assert!(contains_invalid(&Lexer::lex(nested(MAX_INTERPOLATION_NESTING + 1))));
        // Deeper strings don't overflow the stack.
        assert!(contains_invalid(&Lexer::lex(nested(10_000))));
    }

    #[test]
    fn unknown_characters_take_their_bytes() {
        let lexed = Lexer::lex("é a");
        let bytes = lexed.iter().map(|token| (token.span.start, token.span.end)).collect::<Vec<_>>();

        assert_eq!(lexed[0].value, Token::Unknown('é'));
        assert_eq!(lexed[1].value, Token::ident("a"));
        // `é` is 2 bytes long.
        assert_eq!(bytes[..2], [(0, 2), (3, 4)]);
    }

    #[test]
    fn string_escapes_keep_following_tokens() {
        assert_eq!(values(r#""\"" x"#), [Token::String(String::from("\"")), Token::ident("x"), Token::EOF]);
    }

    #[test]
    fn template_parts() {
        let [Token::Template(parts), Token::EOF] = <[Token; 2]>::try_from(values(r#""Hi, ${name}!""#)).unwrap() else {
            panic!("expected a template");
        };

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], TemplatePart::Text(String::from("Hi, ")));
        assert_eq!(parts[2], TemplatePart::Text(String::from("!")));

        let TemplatePart::Expr(tokens, None) = &parts[1] else {
            panic!("expected an expression");
        };

        assert_eq!(tokens.iter().map(|token| token.value.clone()).collect::<Vec<_>>(), [
            Token::ident("name"),
            Token::EOF
        ]);
        // `name` starts after `"Hi, ${`.
        assert_eq!(tokens[0].span.start, 7);
        assert_eq!(tokens[0].span.range.start_column, 7);
    }

    #[test]
    fn template_expression_with_braces_and_strings() {
        let [Token::Template(parts), Token::EOF] = <[Token; 2]>::try_from(values(r#""${f({a: "}"})}""#)).unwrap() else {
            panic!("expected a template");
        };

        let [TemplatePart::Expr(tokens, None)] = parts.as_slice() else {
            panic!("expected only an expression, got {parts:?}");
        };

        assert_eq!(tokens.iter().map(|token| token.value.clone()).collect::<Vec<_>>(), [
            Token::ident("f"),
            Token::ParenOpen,
            Token::BraceOpen,
            Token::ident("a"),
            Token::Colon,
            Token::String(String::from("}")),
            Token::BraceClose,
            Token::ParenClose,
            Token::EOF
        ]);
    }

    #[test]
    fn spans_of_numbers_with_underscores() {
        let tokens = Lexer::lex("1_000 x");

        assert_eq!((tokens[0].span.start, tokens[0].span.end), (0, 5));
        assert_eq!(tokens[1].span.start, 6);
    }

    #[test]
    fn format_specifiers_of_templates() {
        let spec_of = |source: &str| {
            let [Token::Template(parts), Token::EOF] = <[Token; 2]>::try_from(values(source)).unwrap() else {
                panic!("expected a template");
            };

            let [TemplatePart::Expr(tokens, spec)] = parts.as_slice() else {
                panic!("expected only an expression, got {parts:?}");
            };

            (tokens.len(), spec.clone())
        };

        assert_eq!(spec_of(r#""${value:>8.2}""#), (2, Some(String::from(">8.2"))));
        assert_eq!(spec_of(r#""${n:x}""#), (2, Some(String::from("x"))));
        // Paths and colons inside brackets aren't specifiers.
        assert_eq!(spec_of(r#""${a::b}""#), (4, None));
        assert_eq!(spec_of(r#""${a::b:04}""#), (4, Some(String::from("04"))));
        assert_eq!(spec_of(r#""${f(x: 1)}""#), (7, None));
        assert_eq!(spec_of(r#""${s:"a:1"}""#).1, None);
    }

    #[test]
    fn templates_with_several_expressions() {
        let [Token::Template(parts), Token::EOF] = <[Token; 2]>::try_from(values(r#""${a}${b} and ${c}""#)).unwrap() else {
            panic!("expected a template");
        };

        assert_eq!(parts.len(), 4);
        assert!(matches!(parts[2], TemplatePart::Text(ref text) if text == " and "));
    }

    #[test]
    fn invalid_numbers_are_tokens() {
        assert_eq!(values("0x"), [Token::InvalidNumber(String::from("0x")), Token::EOF]);
        assert_eq!(values("0xFFFFFFFFFFFFFFFFFF"), [
            Token::InvalidNumber(String::from("0xFFFFFFFFFFFFFFFFFF")),
            Token::EOF
        ]);
        assert_eq!(values("99999999999999999999"), [
            Token::InvalidNumber(String::from("99999999999999999999")),
            Token::EOF
        ]);
    }

    #[test]
    fn dots_after_numbers() {
        assert!(matches!(values("1..2").as_slice(), [
            Token::Number(..),
            Token::DotDot,
            Token::Number(..),
            Token::EOF
        ]));
        assert!(matches!(values("1..=2").as_slice(), [
            Token::Number(..),
            Token::DotDotEq,
            Token::Number(..),
            Token::EOF
        ]));
        assert!(matches!(values("a ..= b").as_slice(), [
            Token::Ident(_),
            Token::DotDotEq,
            Token::Ident(_),
            Token::EOF
        ]));
        assert!(matches!(values("1.len").as_slice(), [
            Token::Number(..),
            Token::Dot,
            Token::Ident(_),
            Token::EOF
        ]));
        assert!(matches!(values("1.5").as_slice(), [Token::Number(number, None), Token::EOF] if number.value == NumberToken::F32(1.5)));
        // A second dot ends the number.
        assert!(matches!(values("1.5.2").as_slice(), [
            Token::Number(..),
            Token::Dot,
            Token::Number(..),
            Token::EOF
        ]));
    }
}

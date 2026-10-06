use derive_more::derive::{Display, IsVariant, Unwrap};
use mollie_shared::Positioned;

#[derive(Debug, Display, IsVariant, Unwrap, PartialEq, Clone, PartialOrd)]
pub enum NumberToken {
    #[display("{_0}")]
    #[is_variant]
    #[unwrap]
    F32(f32),
    #[display("{_0}")]
    #[is_variant]
    #[unwrap]
    I64(i64),
}

/// A part of a string template.
#[derive(Debug, PartialEq, Clone, PartialOrd)]
pub enum TemplatePart {
    Text(String),
    /// Tokens of an interpolated expression, ending with [`Token::EOF`],
    /// and its format specifier (after `:`), if any.
    Expr(Vec<Positioned<Token>>, Option<String>),
}

#[derive(Debug, Display, IsVariant, Unwrap, PartialEq, Clone, PartialOrd)]
pub enum Token {
    #[is_variant]
    #[unwrap]
    Ident(String),
    // #[display("\"{}\"", _0.iter().map(ToString::to_string).collect::<String>())]
    // #[is_variant]
    // FormattedString(Vec<StringPart>),
    // #[display("\"{_0}\"")]
    #[display("{_0:?}")]
    #[is_variant]
    #[unwrap]
    String(String),
    /// A string with interpolated expressions: `"Hi, ${name}!"`.
    #[display("<template>")]
    #[is_variant]
    #[unwrap]
    Template(Vec<TemplatePart>),
    #[display("{_0}")]
    #[is_variant]
    #[unwrap]
    Number(Positioned<NumberToken>, Option<Positioned<String>>),
    #[display("{_0}")]
    #[is_variant]
    #[unwrap]
    Bool(bool),
    #[display("import")]
    Import,
    #[display("module")]
    Module,
    #[display("public")]
    Public,
    #[display("<super>")]
    Super,
    #[display("self")]
    This,
    #[display("view")]
    View,
    #[display("inherits")]
    Inherits,
    #[display("as")]
    As,
    #[display("is")]
    Is,
    #[display("from")]
    From,
    #[display("..")]
    DotDot,
    #[display("..=")]
    DotDotEq,
    #[display("enum")]
    Enum,
    #[display("struct")]
    Struct,
    #[display("const")]
    Const,
    #[display("let")]
    Let,
    #[display("mut")]
    Mut,
    #[display("while")]
    While,
    #[display("for")]
    For,
    #[display("in")]
    In,
    #[display("if")]
    If,
    #[display("else")]
    Else,
    #[display("loop")]
    Loop,
    #[display("postfix")]
    Postfix,
    #[display("fn")]
    Func,
    #[display("@")]
    Attr,
    #[display("switch")]
    Switch,
    #[display("match")]
    Match,
    #[display("return")]
    Return,
    #[display("break")]
    Break,
    #[display("continue")]
    Continue,
    /// A label of a loop: `'outer`.
    #[display("'{_0}")]
    Label(String),
    #[display("[")]
    BracketOpen,
    #[display("]")]
    BracketClose,
    #[display("{{")]
    BraceOpen,
    #[display("}}")]
    BraceClose,
    #[display("(")]
    ParenOpen,
    #[display(")")]
    ParenClose,
    #[display("::")]
    PathSep,
    #[display(":")]
    Colon,
    #[display(";")]
    Semi,
    #[display("-")]
    Minus,
    #[display("-=")]
    MinusEq,
    #[display("+")]
    Plus,
    #[display("+=")]
    PlusEq,
    #[display("/")]
    Slash,
    #[display("/=")]
    SlashEq,
    #[display("*")]
    Star,
    #[display("*=")]
    StarEq,
    #[display("=>")]
    FatArrow,
    #[display("->")]
    Arrow,
    #[display("=")]
    Eq,
    #[display("==")]
    EqEq,
    #[display("!=")]
    NotEq,
    #[display("!")]
    Not,
    #[display("|")]
    Or,
    #[display("|=")]
    OrEq,
    #[display("||")]
    OrOr,
    #[display("&")]
    And,
    #[display("&=")]
    AndEq,
    #[display("&&")]
    AndAnd,
    #[display("?")]
    Question,
    #[display("%")]
    Percent,
    #[display("%=")]
    PercentEq,
    #[display(",")]
    Comma,
    #[display("<")]
    Less,
    #[display(">")]
    Greater,
    #[display("<=")]
    LessEq,
    #[display(">=")]
    GreaterEq,
    #[display(".")]
    Dot,
    #[display("impl")]
    Impl,
    #[display("trait")]
    Trait,
    #[display("{_0}")]
    Unknown(char),
    /// A number that can't be represented, like `1.2.3` or a hex number out
    /// of the range of `i64`.
    #[display("invalid number `{_0}`")]
    InvalidNumber(String),
    /// Code the lexer can't accept, with the reason.
    #[display("{_0}")]
    Invalid(String),
    #[display("<EOF>")]
    #[is_variant]
    EOF,
}

impl Token {
    pub fn ident<T: Into<String>>(value: T) -> Self {
        Self::Ident(value.into())
    }

    #[must_use]
    pub fn is_ident_and(&self, func: impl FnOnce(&String) -> bool) -> bool {
        if let Self::Ident(value) = self { func(value) } else { false }
    }

    pub const fn is_i64(&self) -> bool {
        if let Self::Number(value, _) = self { value.value.is_i_64() } else { false }
    }

    pub const fn is_f32(&self) -> bool {
        if let Self::Number(value, _) = self { value.value.is_f_32() } else { false }
    }

    pub fn unwrap_integer(self) -> (Positioned<i64>, Option<Positioned<String>>) {
        let (number, postfix) = self.unwrap_number();

        (number.span.wrap(number.value.unwrap_i_64()), postfix)
    }

    // #[must_use]
    // pub const fn is_number(&self) -> bool {
    //     self.is_float() || self.is_integer()
    // }

    // #[must_use]
    // pub const fn is_usize(&self) -> bool {
    //     if let Self::Integer(value, _) = self {
    //         *value >= 0
    //     } else {
    //         false
    //     }
    // }

    // #[must_use]
    // pub fn into_usize(self) -> usize {
    //     if let Self::Integer(value, _) = self {
    //         let Ok(value) = value.try_into() else {
    //             unreachable!()
    //         };

    //         value
    //     } else {
    //         unreachable!()
    //     }
    // }
}

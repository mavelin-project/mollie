#![allow(clippy::missing_errors_doc)]

use core::{
    fmt::{self, Display},
    mem,
};

pub struct Formatter<'a, 'b> {
    track_newlines: bool,
    newlines: usize,
    indent_level: usize,
    f: &'a mut fmt::Formatter<'b>,
}

impl<'a, 'b> Formatter<'a, 'b> {
    pub const fn new(f: &'a mut fmt::Formatter<'b>) -> Self {
        Self {
            track_newlines: false,
            newlines: 1,
            indent_level: 0,
            f,
        }
    }

    pub fn tracked_lines<F: FnOnce(&mut Self) -> fmt::Result>(&mut self, func: F) -> Result<usize, fmt::Error> {
        self.track_newlines = true;

        let newlines = mem::replace(&mut self.newlines, 1);

        func(self)?;

        self.track_newlines = false;

        Ok(mem::replace(&mut self.newlines, newlines))
    }

    pub fn newline(&mut self) -> fmt::Result {
        if self.track_newlines {
            self.newlines += 1;
        }

        self.f.write_str("\n")
    }

    pub fn write_indent(&mut self) -> fmt::Result {
        for _ in 0..self.indent_level {
            self.f.write_str("  ")?;
        }

        Ok(())
    }

    pub const fn inc_indent(&mut self) {
        self.indent_level += 1;
    }

    pub const fn dec_indent(&mut self) {
        self.indent_level -= 1;
    }

    pub fn in_scope<F: FnOnce(&mut Self) -> fmt::Result>(&mut self, func: F) -> fmt::Result {
        self.inc_indent();

        func(self)?;

        self.dec_indent();

        Ok(())
    }

    pub fn in_block<S: Display, E: Display, F: FnOnce(&mut Self) -> fmt::Result>(&mut self, start: S, end: E, compact: bool, func: F) -> fmt::Result {
        start.fmt(self.f)?;

        if compact {
            self.f.write_str(" ")?;
        } else {
            self.inc_indent();
            self.newline()?;
        }

        func(self)?;

        if compact {
            self.f.write_str(" ")?;
        } else {
            self.newline()?;
            self.dec_indent();
            self.write_indent()?;
        }

        end.fmt(self.f)?;

        Ok(())
    }

    pub fn fmt_separated_by<T: Display, V, F: Fn(&mut Self, &V) -> fmt::Result>(&mut self, items: &[V], separator: T, pretty: bool, fmt: F) -> fmt::Result {
        let mut first = true;

        for item in items {
            if first {
                first = false;

                if pretty {
                    self.write_indent()?;
                }
            } else {
                separator.fmt(self.f)?;

                if pretty {
                    self.newline()?;
                    self.write_indent()?;
                }
            }

            fmt(self, item)?;
        }

        Ok(())
    }

    pub fn fmt_literal(&mut self, literal_expr: &mollie_parser::LiteralExpr) -> fmt::Result {
        use mollie_parser::{
            LiteralExpr::{Bool, Number, String},
            Number::{F32, I64},
        };

        match literal_expr {
            Number(number, suffix) => {
                match number.value {
                    I64(value) => value.fmt(self.f)?,
                    // `Debug` keeps the point of whole numbers (`1.0`).
                    F32(value) => write!(self.f, "{value:?}")?,
                }

                match suffix {
                    Some(suffix) => self.f.write_str(&suffix.value),
                    None => Ok(()),
                }
            }
            Bool(value) => value.fmt(self.f),
            String(value) => {
                self.f.write_str("\"")?;
                self.fmt_string_text(value)?;
                self.f.write_str("\"")
            }
        }
    }

    /// Text of a string literal, escaped.
    fn fmt_string_text(&mut self, text: &str) -> fmt::Result {
        let mut chars = text.chars().peekable();

        while let Some(character) = chars.next() {
            match character {
                '"' => self.f.write_str("\\\"")?,
                '\\' => self.f.write_str("\\\\")?,
                '\n' => self.f.write_str("\\n")?,
                '\t' => self.f.write_str("\\t")?,
                '\r' => self.f.write_str("\\r")?,
                '\0' => self.f.write_str("\\0")?,
                // `${` would start an interpolation.
                '$' if chars.peek() == Some(&'{') => self.f.write_str("\\$")?,
                character => write!(self.f, "{character}")?,
            }
        }

        Ok(())
    }

    pub fn fmt_type(&mut self, ty: &mollie_parser::Type) -> fmt::Result {
        match ty {
            mollie_parser::Type::Primitive(primitive_type) => primitive_type.fmt(self.f),
            mollie_parser::Type::Array(element, size) => {
                self.fmt_type(&element.value)?;

                if let Some(size) = size {
                    self.f.write_str("[")?;

                    size.value.fmt(self.f)?;

                    self.f.write_str("]")
                } else {
                    self.f.write_str("[]")
                }
            }
            mollie_parser::Type::Func(args, returns) => {
                self.f.write_str("func(")?;
                self.fmt_separated_by(args, ", ", false, |me, arg| me.fmt_type(&arg.value))?;
                self.f.write_str(")")?;

                match returns {
                    Some(returns) => {
                        self.f.write_str(" -> ")?;
                        self.fmt_type(&returns.value)
                    }
                    None => Ok(()),
                }
            }
            mollie_parser::Type::Path(type_path_expr) => self.fmt_type_path(type_path_expr),
        }
    }

    pub fn fmt_type_path(&mut self, type_path_expr: &mollie_parser::TypePathExpr) -> fmt::Result {
        self.fmt_separated_by(&type_path_expr.segments, "::", false, |me, segment| {
            me.f.write_str(segment.value.name.value.0.as_str())?;

            if let Some(args) = &segment.value.args {
                me.f.write_str("<")?;
                me.fmt_separated_by(&args.value.0, ", ", false, |me, arg| me.fmt_type(&arg.value))?;
                me.f.write_str(">")
            } else {
                Ok(())
            }
        })
    }

    pub fn fmt_node_expr(&mut self, node_expr: &mollie_parser::NodeExpr) -> fmt::Result {
        self.fmt_type_path(&node_expr.name.value)?;
        self.f.write_str(" ")?;
        self.in_block('{', '}', node_expr.properties.is_empty() && node_expr.children.value.is_empty(), |me| {
            me.fmt_separated_by(&node_expr.properties, ",", true, |me, prop| {
                prop.value.name.value.0.fmt(me.f)?;

                if let Some(value) = &prop.value.value {
                    me.f.write_str(": ")?;
                    me.fmt_expr(&value.value)
                } else {
                    Ok(())
                }
            })?;

            if !node_expr.children.value.is_empty() {
                me.f.write_str(",")?;
                me.newline()?;

                let mut write_sep = false;

                for item in &node_expr.children.value {
                    me.newline()?;

                    if write_sep {
                        me.newline()?;
                    }

                    me.write_indent()?;

                    write_sep = me.tracked_lines(|me| me.fmt_node_expr(&item.value))? > 1;
                }
            }

            Ok(())
        })
    }

    /// Formats an expression. Statements other than expressions and `let`
    /// can't be formatted yet: they fail with [`fmt::Error`].
    pub fn fmt_expr(&mut self, expr: &mollie_parser::Expr) -> fmt::Result {
        use mollie_parser::Expr;

        match expr {
            Expr::Literal(literal_expr) => self.fmt_literal(literal_expr),
            Expr::Template(template) => self.fmt_template(template),
            Expr::FunctionCall(call) => {
                self.fmt_operand(&call.function.value)?;
                self.f.write_str("(")?;
                self.fmt_separated_by(&call.args.value, ", ", false, |me, arg| {
                    if let Some(name) = &arg.value.name {
                        write!(me.f, "{}: ", name.value.0)?;
                    }

                    me.fmt_expr(&arg.value.value.value)
                })?;
                self.f.write_str(")")
            }
            Expr::Node(node_expr) => self.fmt_node_expr(node_expr),
            Expr::Index(index) => {
                self.fmt_operand(&index.target.value)?;

                match &index.index.value {
                    mollie_parser::IndexTarget::Named(name) => write!(self.f, ".{}", name.0),
                    mollie_parser::IndexTarget::Expression(element) => {
                        self.f.write_str("[")?;
                        self.fmt_expr(element)?;
                        self.f.write_str("]")
                    }
                }
            }
            Expr::Binary(binary) => {
                self.fmt_operand(&binary.lhs.value)?;
                write!(self.f, " {} ", binary.operator.value)?;
                self.fmt_operand(&binary.rhs.value)
            }
            Expr::Range(range) => {
                self.fmt_operand(&range.start.value)?;
                self.f.write_str(if range.inclusive { "..=" } else { ".." })?;
                self.fmt_operand(&range.end.value)
            }
            Expr::Unary(unary) => {
                unary.operator.value.fmt(self.f)?;
                self.fmt_operand(&unary.expr.value)
            }
            Expr::TypeIndex(path) => self.fmt_type_path(path),
            Expr::Array(array) => {
                self.f.write_str("[")?;
                self.fmt_separated_by(&array.elements, ", ", false, |me, element| me.fmt_expr(&element.value))?;
                self.f.write_str("]")
            }
            Expr::IfElse(if_else) => {
                self.f.write_str("if ")?;
                self.fmt_expr(&if_else.condition.value)?;
                self.f.write_str(" ")?;
                self.fmt_block(&if_else.block.value)?;

                match &if_else.else_block {
                    Some(otherwise) => {
                        self.f.write_str(" else ")?;
                        self.fmt_expr(&otherwise.value)
                    }
                    None => Ok(()),
                }
            }
            Expr::Match(match_expr) => {
                self.f.write_str("match ")?;
                self.fmt_expr(&match_expr.target.value)?;
                self.f.write_str(" ")?;
                self.in_block('{', '}', match_expr.arms.value.is_empty(), |me| {
                    me.fmt_separated_by(&match_expr.arms.value, ",", true, |me, arm| {
                        me.fmt_pattern(&arm.value.pattern.value)?;

                        if let Some(guard) = &arm.value.guard {
                            me.f.write_str(" if ")?;
                            me.fmt_expr(&guard.value)?;
                        }

                        me.f.write_str(" => ")?;
                        me.fmt_expr(&arm.value.body.value)
                    })
                })
            }
            Expr::Loop(loop_expr) => {
                self.fmt_label(loop_expr.label.as_ref().map(|label| label.value.0.as_str()))?;
                self.f.write_str("loop ")?;
                self.fmt_block(&loop_expr.block.value)
            }
            Expr::Break(break_expr) => {
                self.f.write_str("break")?;

                if let Some(label) = &break_expr.label {
                    write!(self.f, " '{}", label.value.0)?;
                }

                match &break_expr.value {
                    Some(value) => {
                        self.f.write_str(" ")?;
                        self.fmt_expr(&value.value)
                    }
                    None => Ok(()),
                }
            }
            Expr::Continue(continue_expr) => {
                self.f.write_str("continue")?;

                match &continue_expr.label {
                    Some(label) => write!(self.f, " '{}", label.value.0),
                    None => Ok(()),
                }
            }
            Expr::Try(value) => {
                self.fmt_operand(&value.value)?;
                self.f.write_str("?")
            }
            Expr::Return(value) => {
                self.f.write_str("return")?;

                match value {
                    Some(value) => {
                        self.f.write_str(" ")?;
                        self.fmt_expr(&value.value)
                    }
                    None => Ok(()),
                }
            }
            Expr::While(while_expr) => {
                self.fmt_label(while_expr.label.as_ref().map(|label| label.value.0.as_str()))?;
                self.f.write_str("while ")?;
                self.fmt_expr(&while_expr.condition.value)?;
                self.f.write_str(" ")?;
                self.fmt_block(&while_expr.block.value)
            }
            Expr::Block(block) => self.fmt_block(block),
            Expr::ForIn(for_in) => {
                self.fmt_label(for_in.label.as_ref().map(|label| label.value.0.as_str()))?;
                write!(self.f, "for {} in ", for_in.name.value.0)?;
                self.fmt_expr(&for_in.target.value)?;
                self.f.write_str(" ")?;
                self.fmt_block(&for_in.block.value)
            }
            Expr::Is(is_expr) => {
                self.fmt_operand(&is_expr.target.value)?;
                self.f.write_str(" is ")?;
                self.fmt_pattern(&is_expr.pattern.value)
            }
            Expr::Cast(value, ty) => {
                self.fmt_operand(&value.value)?;
                write!(self.f, " as {}", ty.value)
            }
            Expr::Closure(closure) => {
                self.f.write_str("|")?;
                self.fmt_separated_by(&closure.args.value, ", ", false, |me, arg| me.f.write_str(&arg.value.0))?;
                self.f.write_str("| ")?;
                self.fmt_block(&closure.body.value)
            }
            Expr::Ident(ident) => ident.0.fmt(self.f),
            Expr::This => self.f.write_str("self"),
            Expr::Nothing => self.f.write_str("()"),
        }
    }

    /// An operand of an operator: in parentheses unless it's a single item,
    /// so the meaning doesn't depend on precedence.
    fn fmt_operand(&mut self, expr: &mollie_parser::Expr) -> fmt::Result {
        use mollie_parser::Expr;

        if matches!(
            expr,
            Expr::Literal(_)
                | Expr::Template(_)
                | Expr::FunctionCall(_)
                | Expr::Index(_)
                | Expr::TypeIndex(_)
                | Expr::Array(_)
                | Expr::Ident(_)
                | Expr::This
                | Expr::Nothing
                | Expr::Try(_)
        ) {
            self.fmt_expr(expr)
        } else {
            self.f.write_str("(")?;
            self.fmt_expr(expr)?;
            self.f.write_str(")")
        }
    }

    fn fmt_label(&mut self, label: Option<&str>) -> fmt::Result {
        match label {
            Some(label) => write!(self.f, "'{label}: "),
            None => Ok(()),
        }
    }

    fn fmt_template(&mut self, template: &mollie_parser::TemplateExpr) -> fmt::Result {
        self.f.write_str("\"")?;

        for part in &template.0 {
            match part {
                mollie_parser::TemplatePart::Text(text) => self.fmt_string_text(text)?,
                mollie_parser::TemplatePart::Expr(value, spec) => {
                    self.f.write_str("${")?;
                    self.fmt_expr(&value.value)?;

                    if let Some(spec) = spec {
                        write!(self.f, ":{spec}")?;
                    }

                    self.f.write_str("}")?;
                }
            }
        }

        self.f.write_str("\"")
    }

    pub fn fmt_pattern(&mut self, pattern: &mollie_parser::IsPattern) -> fmt::Result {
        match pattern {
            mollie_parser::IsPattern::Literal(literal) => self.fmt_literal(literal),
            mollie_parser::IsPattern::Wildcard => self.f.write_str("_"),
            mollie_parser::IsPattern::Type { ty, pattern } => {
                self.fmt_type_path(&ty.value)?;

                match pattern.as_ref().map(|pattern| &pattern.value) {
                    Some(mollie_parser::TypePattern::Name(name)) => write!(self.f, " {}", name.0),
                    Some(mollie_parser::TypePattern::Values(values)) => {
                        self.f.write_str(" { ")?;
                        self.fmt_separated_by(values, ", ", false, |me, value| {
                            me.f.write_str(&value.value.name.value.0)?;

                            match &value.value.value {
                                Some(pattern) => {
                                    me.f.write_str(": ")?;
                                    me.fmt_pattern(&pattern.value)
                                }
                                None => Ok(()),
                            }
                        })?;
                        self.f.write_str(" }")
                    }
                    None => Ok(()),
                }
            }
        }
    }

    pub fn fmt_block(&mut self, block: &mollie_parser::BlockExpr) -> fmt::Result {
        let empty = block.stmts.is_empty() && block.final_stmt.is_none();

        self.in_block('{', '}', empty, |me| {
            let mut first = true;

            for stmt in &block.stmts {
                if !first {
                    me.newline()?;
                }

                first = false;
                me.write_indent()?;
                me.fmt_stmt(&stmt.value, true)?;
            }

            if let Some(stmt) = &block.final_stmt {
                if !first {
                    me.newline()?;
                }

                me.write_indent()?;
                me.fmt_stmt(&stmt.value, false)?;
            }

            Ok(())
        })
    }

    /// Formats a statement of a block, ended with `;` if `ended` (and it isn't
    /// a block-like expression). Only expressions and `let` are supported.
    pub fn fmt_stmt(&mut self, stmt: &mollie_parser::Stmt, ended: bool) -> fmt::Result {
        use mollie_parser::{Expr, Stmt};

        match stmt {
            Stmt::Expression(expr) => {
                self.fmt_expr(expr)?;

                let block_like = matches!(
                    expr,
                    Expr::IfElse(_) | Expr::Match(_) | Expr::Loop(_) | Expr::While(_) | Expr::ForIn(_) | Expr::Block(_)
                );

                if ended && !block_like { self.f.write_str(";") } else { Ok(()) }
            }
            Stmt::VariableDecl(decl) => {
                self.f.write_str("let ")?;

                if decl.mutable.is_some() {
                    self.f.write_str("mut ")?;
                }

                self.f.write_str(&decl.name.value.0)?;

                if let Some(ty) = &decl.ty {
                    self.f.write_str(": ")?;
                    self.fmt_type(&ty.value)?;
                }

                self.f.write_str(" = ")?;
                self.fmt_expr(&decl.value.value)?;
                self.f.write_str(";")
            }
            _ => Err(fmt::Error),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::fmt;

    use mollie_parser::Parse;

    use crate::Formatter;

    struct Formatted<'a>(&'a mollie_parser::Expr);

    impl fmt::Display for Formatted<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            Formatter::new(f).fmt_expr(self.0)
        }
    }

    fn format(source: &str) -> String {
        let expr = mollie_parser::Expr::parse_value(source).unwrap_or_else(|error| panic!("`{source}` must parse: {error}"));

        Formatted(&expr.value).to_string()
    }

    #[test]
    #[allow(clippy::literal_string_with_formatting_args)]
    fn formatting_is_stable() {
        for source in [
            "a + b * c",
            "(a + b) * c",
            "-x + !y",
            "f(1, name: \"a\\\"b\", [1.0, 2.5])",
            "items[i].value?",
            "x as f32 + 1u8",
            "value is Option::Some { value: 1 } && other is None",
            "if a { b } else if c { d } else { e }",
            "match x { Some { value } if value > 1 => value, _ => 0 }",
            "'outer: for i in 0..=10 { let mut y: i32 = i; while y > 0 { y -= 1; } break 'outer y; }",
            "|a, b| { a + b }",
            "\"text ${x:>4} \\${not} ${y}\"",
            "loop { continue; }",
            "()",
        ] {
            let once = format(source);

            assert_eq!(format(&once), once, "formatting `{source}` isn't stable");
        }
    }
}

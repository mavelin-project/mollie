use std::collections::HashMap;

use lsp_types::{SemanticToken, SemanticTokens, SemanticTokensLegend};
use mollie_lexer::{Lexer, Token};
use mollie_shared::{Positioned, Span};
use mollie_typing::{AdtKind, ModuleId, ModuleItem};

use crate::analysis::Analysis;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Namespace,
    Type,
    Struct,
    Enum,
    Interface,
    TypeParameter,
    Parameter,
    Variable,
    Property,
    EnumMember,
    Function,
    Method,
    Label,
}

const TYPES: [&str; 13] = [
    "namespace",
    "type",
    "struct",
    "enum",
    "interface",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
    "label",
];

const MODIFIERS: [&str; 3] = ["declaration", "readonly", "defaultLibrary"];
const DECLARATION: u32 = 1;
const READONLY: u32 = 2;
const DEFAULT_LIBRARY: u32 = 4;

const PRIMITIVES: [&str; 15] = [
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool", "string", "void",
];

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend::new(TYPES.map(String::from).to_vec(), MODIFIERS.map(String::from).to_vec())
}

pub fn tokens(analysis: &Analysis, module: ModuleId) -> Option<SemanticTokens> {
    let file = analysis.files.get(&module)?;
    let mut data = Vec::new();
    let mut last = (0, 0);

    for (span, kind, modifiers) in classify(analysis, module)? {
        let (start, end) = (file.index.position(span.start), file.index.position(span.end));

        // Identifiers are on one line.
        if start.line != end.line {
            continue;
        }

        let delta_line = start.line - last.0;
        let delta_start = if delta_line == 0 { start.character - last.1 } else { start.character };

        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: end.character - start.character,
            token_type: kind as u32,
            token_modifiers_bitset: modifiers,
        });
        last = (start.line, start.character);
    }

    Some(SemanticTokens::new(None, data))
}

fn classify(analysis: &Analysis, module: ModuleId) -> Option<Vec<(Span, Kind, u32)>> {
    let tokens = Lexer::lex(analysis.files.get(&module)?.index.text());
    // Variables (not those made up by desugaring), by where their names are
    // declared, and their uses, by where they are.
    let mut declarations = HashMap::new();
    let mut uses = HashMap::new();

    for ast in analysis.asts(module) {
        for variable in ast.variables.iter().filter(|variable| !variable.name.starts_with('<')) {
            declarations.insert(variable.span.start, Kind::Variable);
        }

        uses.extend(ast.var_uses.iter().map(|&(usage, declaration)| (usage.start, declaration.start)));
    }

    // Parameters: declared in `(`, `,` or `|` lists.
    for (index, token) in tokens.iter().enumerate() {
        if let Some(kind) = declarations.get_mut(&token.span.start)
            && matches!(previous(&tokens, index), Some(Token::ParenOpen | Token::Comma | Token::Or))
        {
            *kind = Kind::Parameter;
        }
    }

    let classifier = Classifier {
        analysis,
        module,
        tokens: &tokens,
        declarations: &declarations,
        uses: &uses,
    };

    Some(
        (0..tokens.len())
            .filter_map(|index| classifier.classify(index).map(|(kind, modifiers)| (tokens[index].span, kind, modifiers)))
            .collect(),
    )
}

fn previous(tokens: &[Positioned<Token>], index: usize) -> Option<&Token> {
    index.checked_sub(1).map(|index| &tokens[index].value)
}

fn next(tokens: &[Positioned<Token>], index: usize) -> Option<&Token> {
    tokens.get(index + 1).map(|token| &token.value)
}

struct Classifier<'a> {
    analysis: &'a Analysis,
    module: ModuleId,
    tokens: &'a [Positioned<Token>],
    declarations: &'a HashMap<usize, Kind>,
    uses: &'a HashMap<usize, usize>,
}

impl Classifier<'_> {
    fn classify(&self, index: usize) -> Option<(Kind, u32)> {
        let token = &self.tokens[index];
        let name = match &token.value {
            Token::Ident(name) => name.as_str(),
            Token::Label(_) => return Some((Kind::Label, 0)),
            _ => return None,
        };
        let start = token.span.start;

        if let Some(&kind) = self.declarations.get(&start) {
            return Some((kind, DECLARATION));
        }

        if let Some(declaration) = self.uses.get(&start) {
            return Some((self.declarations.get(declaration).copied().unwrap_or(Kind::Variable), 0));
        }

        let previous = previous(self.tokens, index);
        let next = next(self.tokens, index);

        // Members: `a.b()` and `a.b`.
        if matches!(previous, Some(Token::Dot)) {
            return Some((if matches!(next, Some(Token::ParenOpen)) { Kind::Method } else { Kind::Property }, 0));
        }

        // A segment of a path: in the module (or the type) before `::`.
        if matches!(previous, Some(Token::PathSep)) {
            return self.path_segment(index, name);
        }

        if PRIMITIVES.contains(&name) {
            return Some((Kind::Type, DEFAULT_LIBRARY));
        }

        if name == "Self" {
            return Some((Kind::Type, 0));
        }

        let declaration = if matches!(
            previous,
            Some(Token::Func | Token::Struct | Token::Enum | Token::Trait | Token::Const | Token::Module | Token::View)
        ) {
            DECLARATION
        } else {
            0
        };

        if let Some(item) = self.analysis.context.tcx.def_registry.lookup(self.module, name) {
            let (kind, modifiers) = self.item_kind(item);

            return Some((kind, modifiers | declaration));
        }

        // Functions of impls and traits aren't items of modules.
        if matches!(previous, Some(Token::Func)) {
            return Some((Kind::Method, DECLARATION));
        }

        // Fields: declared (`x: f32`) or given (`Point { x: 1.0 }`).
        if matches!(next, Some(Token::Colon)) {
            return Some((Kind::Property, 0));
        }

        // Generic parameters (`T`, `K`).
        let mut chars = name.chars();

        if chars.next().is_some_and(|first| first.is_ascii_uppercase()) && chars.next().is_none() {
            return Some((Kind::TypeParameter, 0));
        }

        None
    }

    fn path_segment(&self, index: usize, name: &str) -> Option<(Kind, u32)> {
        let registry = &self.analysis.context.tcx.def_registry;
        // The segment before: a module, or a type.
        let before = match index.checked_sub(2).map(|index| &self.tokens[index].value) {
            Some(Token::Ident(before)) => registry.lookup(self.module, before),
            _ => None,
        };
        let starts_upper = name.chars().next().is_some_and(|first| first.is_ascii_uppercase());

        match before {
            Some(ModuleItem::SubModule(module)) => registry.lookup(module, name).map(|item| self.item_kind(item)),
            Some(ModuleItem::Adt(adt)) if registry.adt_types[adt].kind == AdtKind::Enum && starts_upper => Some((Kind::EnumMember, 0)),
            // Functions of types: `Point::new`.
            Some(ModuleItem::Adt(_) | ModuleItem::Trait(_)) => Some((Kind::Function, 0)),
            _ if matches!(next(self.tokens, index), Some(Token::ParenOpen)) => Some((Kind::Function, 0)),
            _ if starts_upper => Some((Kind::Type, 0)),
            _ => None,
        }
    }

    fn item_kind(&self, item: ModuleItem) -> (Kind, u32) {
        let registry = &self.analysis.context.tcx.def_registry;

        match item {
            ModuleItem::SubModule(_) => (Kind::Namespace, 0),
            ModuleItem::Adt(adt) if registry.adt_types[adt].kind == AdtKind::Enum => (Kind::Enum, 0),
            ModuleItem::Adt(_) => (Kind::Struct, 0),
            ModuleItem::Trait(_) => (Kind::Interface, 0),
            ModuleItem::Func(_) | ModuleItem::Intrinsic(..) => (Kind::Function, 0),
            ModuleItem::Const(_) => (Kind::Variable, READONLY),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use lsp_types::Uri;

    use super::*;
    use crate::analysis::{Documents, Encoding};

    fn kinds(source: &str) -> HashMap<String, (Kind, u32)> {
        let dir = env::temp_dir().join(format!("mollie-semantic-{}", std::process::id()));

        fs::create_dir_all(&dir).expect("the folder can be made");

        let uri = Uri::from_file_path(dir.join("main.mol")).expect("the path is absolute");
        let analysis = Analysis::new(&uri, source.to_owned(), &Documents::default(), Encoding::Utf16, None);
        let module = analysis.module_of(&uri).expect("the module");
        let text = analysis.files[&module].index.text().to_owned();
        let mut result = HashMap::new();

        assert!(tokens(&analysis, module).is_some_and(|tokens| !tokens.data.is_empty()));

        for (span, kind, modifiers) in classify(&analysis, module).expect("classified") {
            result.entry(text[span.start..span.end].to_owned()).or_insert((kind, modifiers));
        }

        result
    }

    #[test]
    fn identifiers_are_classified() {
        let kinds = kinds(
            "struct Point { x: f32 }
enum Shape { Dot, Line }
const LIMIT: i32 = 4;

func length(point: Point) -> f32 { point.x }

let mut total = length(Point { x: 1.0 }) + LIMIT as f32;
let shape = Shape::Dot;
total += 1.0;
",
        );

        assert_eq!(kinds["Point"], (Kind::Struct, DECLARATION));
        assert_eq!(kinds["Shape"].0, Kind::Enum);
        assert_eq!(kinds["LIMIT"], (Kind::Variable, READONLY | DECLARATION));
        assert_eq!(kinds["length"], (Kind::Function, DECLARATION));
        assert_eq!(kinds["point"], (Kind::Parameter, DECLARATION));
        assert_eq!(kinds["x"].0, Kind::Property);
        assert_eq!(kinds["f32"], (Kind::Type, DEFAULT_LIBRARY));
        assert_eq!(kinds["total"], (Kind::Variable, DECLARATION));
        assert_eq!(kinds["Dot"].0, Kind::EnumMember);
    }
}

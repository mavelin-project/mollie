//! Type-checking of documents: an open document is the root module of a
//! program, whose submodules are read from open documents or from disk, or a
//! submodule of an entry of its project (see [`Project`]).

use std::{
    collections::HashMap,
    fs, iter,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    ptr,
    sync::OnceLock,
};

use lsp_types::{Position, PositionEncodingKind, Range, Uri};
use mollie_index::Idx;
use mollie_lexer::{Lexer, Token};
use mollie_shared::{Positioned, Span};
use mollie_typed_ast::{Expr, ExprRef, FunctionBody, ModuleLoader, ParsedModule, TypedAST, TypedASTContext, std_sources};
use mollie_typing::{Bound, DefRegistry, DefinitionType, ModuleId, ModuleItem, ModuleSpan, PrimitiveType, Type, TypeRef};

/// A project, configured by a `mollie.toml` file:
///
/// ```toml
/// # Programs (addons) of the project: other files are their submodules.
/// entries = ["addons/shop/main.mol", "addons/quests/main.mol"]
/// # The stub of the host's API (`mollie::stub::HostStub::write_to`).
/// host = ".mollie/host"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub dir: PathBuf,
    pub entries: Vec<PathBuf>,
    /// Directory of the stub of the host's API.
    pub host: PathBuf,
}

impl Project {
    /// The project of the file `path`: configured by the nearest
    /// `mollie.toml` above it.
    pub fn find(path: &Path) -> Option<Self> {
        path.ancestors().skip(1).find_map(|dir| {
            let text = fs::read_to_string(dir.join("mollie.toml")).ok()?;

            Some(Self::parse(dir, &text))
        })
    }

    /// Reads the keys of `mollie.toml` this server uses (a subset of TOML:
    /// strings and arrays of strings).
    pub fn parse(dir: &Path, text: &str) -> Self {
        let mut values = HashMap::new();
        let mut lines = text.lines();

        while let Some(line) = lines.next() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let mut value = value.trim().to_owned();

            // Arrays may span lines.
            if value.starts_with('[') {
                while !value.contains(']') {
                    let Some(next) = lines.next() else {
                        break;
                    };

                    value.push_str(next.split('#').next().unwrap_or_default());
                }
            }

            let strings = value.split('"').skip(1).step_by(2).map(str::to_owned).collect::<Vec<_>>();

            values.insert(key.trim().to_owned(), strings);
        }

        let path = |relative: &String| dir.join(relative);

        Self {
            dir: dir.to_path_buf(),
            entries: values.get("entries").map_or_default(|entries| entries.iter().map(path).collect()),
            host: values.get("host").and_then(|host| host.first()).map_or_else(|| dir.join(".mollie/host"), path),
        }
    }
}

/// A stub of the host's API in `dir`: `lib.mol`, and its modules by path.
fn read_stub(dir: &Path) -> Option<(String, Vec<(String, String)>, Vec<(String, PathBuf)>)> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();

            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "mol") {
                files.push(path);
            }
        }
    }

    let root = fs::read_to_string(dir.join("lib.mol")).ok()?;
    let mut files = Vec::new();
    let mut modules = Vec::new();
    let mut paths = vec![(String::new(), dir.join("lib.mol"))];

    walk(dir, &mut files);

    for file in files {
        let Ok(relative) = file.strip_prefix(dir) else {
            continue;
        };

        if relative == Path::new("lib.mol") {
            continue;
        }

        let path = relative
            .with_extension("")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("::");

        if let Ok(source) = fs::read_to_string(&file) {
            modules.push((path.clone(), source));
            paths.push((path, file));
        }
    }

    Some((root, modules, paths))
}

/// Writes the sources of `std` to a directory (once), so that clients can
/// open its declarations. Returns the files by path of their modules.
fn std_files() -> &'static [(String, PathBuf, &'static str)] {
    static FILES: OnceLock<Vec<(String, PathBuf, &'static str)>> = OnceLock::new();

    FILES.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("mollie-std-{}", env!("CARGO_PKG_VERSION")));

        std_sources()
            .map(|(path, source)| {
                let file = if path.is_empty() {
                    dir.join("lib.mol")
                } else {
                    dir.join(path.replace("::", "/")).with_extension("mol")
                };

                if fs::read_to_string(&file).ok().as_deref() != Some(source) {
                    let _ = file.parent().map(fs::create_dir_all);
                    let _ = fs::write(&file, source);
                }

                (path.to_owned(), file, source)
            })
            .collect()
    })
}

/// Modules under `root` (including it), with their paths (`""` for the root).
fn module_paths(registry: &DefRegistry, root: ModuleId) -> Vec<(ModuleId, String)> {
    let mut modules = vec![(root, String::new())];
    let mut index = 0;

    while let Some((module, path)) = modules.get(index).cloned() {
        for (name, &(item, kind, _)) in &registry.modules[module].items {
            if let (ModuleItem::SubModule(child), DefinitionType::Local) = (item, kind) {
                modules.push((child, if path.is_empty() { name.clone() } else { format!("{path}::{name}") }));
            }
        }

        index += 1;
    }

    modules
}

/// Open documents, by their URIs.
#[derive(Default, Clone)]
pub struct Documents(HashMap<Uri, String>);

impl Documents {
    pub fn set(&mut self, uri: Uri, text: String) {
        self.0.insert(uri, text);
    }

    pub fn remove(&mut self, uri: &Uri) {
        self.0.remove(uri);
    }

    pub fn get(&self, uri: &Uri) -> Option<&str> {
        self.0.get(uri).map(String::as_str)
    }

    /// Text of the file at `path`: of its open document, or read from disk.
    pub fn read(&self, path: &Path) -> Option<String> {
        let open = Uri::from_file_path(path).ok().and_then(|uri| self.0.get(&uri).cloned());

        open.or_else(|| fs::read_to_string(path).ok())
    }
}

/// Converts between byte offsets in a text and LSP positions.
pub struct LineIndex {
    text: String,
    /// Byte offsets of starts of lines.
    lines: Vec<usize>,
    encoding: Encoding,
}

/// Units of columns of LSP positions, negotiated with the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    /// The best encoding offered by the client: Mollie spans count bytes and
    /// characters, so UTF-16 is only used when there's no other choice.
    pub fn negotiate(offered: &[PositionEncodingKind]) -> Self {
        if offered.contains(&PositionEncodingKind::UTF32) {
            Self::Utf32
        } else if offered.contains(&PositionEncodingKind::UTF8) {
            Self::Utf8
        } else {
            Self::Utf16
        }
    }

    pub const fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
            Self::Utf32 => PositionEncodingKind::UTF32,
        }
    }

    const fn width(self, character: char) -> usize {
        match self {
            Self::Utf8 => character.len_utf8(),
            Self::Utf16 => character.len_utf16(),
            Self::Utf32 => 1,
        }
    }
}

impl LineIndex {
    pub fn new(text: String, encoding: Encoding) -> Self {
        let lines = iter::once(0).chain(text.match_indices('\n').map(|(index, _)| index + 1)).collect();

        Self { text, lines, encoding }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte offset of `position`, clamped to its line.
    pub fn offset(&self, position: Position) -> usize {
        let Some(&start) = self.lines.get(position.line as usize) else {
            return self.text.len();
        };

        let end = self.lines.get(position.line as usize + 1).map_or(self.text.len(), |&next| next - 1);
        let mut units = 0;

        for (index, character) in self.text[start..end].char_indices() {
            if units >= position.character as usize {
                return start + index;
            }

            units += self.encoding.width(character);
        }

        end
    }

    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line = self.lines.partition_point(|&start| start <= offset) - 1;
        let start = self.lines[line];
        let character = self
            .text
            .get(start..offset)
            .map_or(0, |text| text.chars().map(|character| self.encoding.width(character)).sum());

        Position::new(u32::try_from(line).unwrap_or(u32::MAX), u32::try_from(character).unwrap_or(u32::MAX))
    }

    pub fn range(&self, span: Span) -> Range {
        Range::new(self.position(span.start), self.position(span.end))
    }
}

/// A source file of an analyzed program.
pub struct File {
    pub uri: Uri,
    pub index: LineIndex,
}

/// Loads submodules relative to the directory of the root module, preferring
/// open documents.
struct Loader<'a> {
    dir: PathBuf,
    documents: &'a Documents,
    files: Vec<(ModuleId, PathBuf, String)>,
    parse_errors: Vec<(ModuleId, String, Option<Span>)>,
}

impl ModuleLoader for Loader<'_> {
    type Error = ();

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        fn module_path(base: &Path, registry: &DefRegistry, module: ModuleId) -> PathBuf {
            let module = &registry.modules[module];

            module
                .parent
                .map_or_else(|| base.to_path_buf(), |parent| module_path(base, registry, parent).join(&module.name))
        }

        let path = module_path(&self.dir, registry, module).with_extension("mol");
        let text = self.documents.read(&path).ok_or(())?;
        let parsed = ParsedModule::parse(&text).unwrap_or_else(|error| {
            // Reported by the language server, the module is still found.
            self.parse_errors.push((module, error.0, error.1));

            ParsedModule {
                stmts: Vec::new(),
                final_stmt: None,
                stub: false,
            }
        });

        self.files.push((module, path, text));

        Ok(parsed)
    }
}

/// A type-checked program.
pub struct Analysis {
    pub context: TypedASTContext,
    /// Top-level code of the root module.
    pub root: TypedAST,
    pub files: HashMap<ModuleId, File>,
    /// Syntax errors of submodules (those of the root module are reported
    /// with the other errors).
    pub parse_errors: Vec<(ModuleId, String, Option<Span>)>,
}

impl Analysis {
    /// Type-checks `text`, the document `uri`, as the root module of a
    /// program of `project`. The stub of the host's API is loaded first (from
    /// the project, or `.mollie/host` in a directory above the document).
    pub fn new(uri: &Uri, text: String, documents: &Documents, encoding: Encoding, project: Option<&Project>) -> Self {
        let path = uri.to_file_path().ok();
        let dir = path.as_ref().and_then(|path| path.parent().map(Path::to_path_buf)).unwrap_or_default();
        let mut context = TypedASTContext::default();
        let stub_dir = project
            .map(|project| project.host.clone())
            .or_else(|| dir.ancestors().map(|dir| dir.join(".mollie/host")).find(|stub| stub.join("lib.mol").is_file()));
        let stub = stub_dir.as_deref().and_then(read_stub);

        if let Some((root, modules, _)) = &stub {
            context.load_host_stub(root, modules);
        }

        // Top-level code of addons is called by the host, which decides what
        // it returns.
        let returns = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Any));
        let mut loader = Loader {
            dir,
            documents,
            files: Vec::new(),
            parse_errors: Vec::new(),
        };

        let (root, _) = context.process(&mut loader, &text, Vec::<(String, TypeRef)>::new(), returns);
        let mut files = HashMap::new();

        files.insert(root.module, File {
            uri: uri.clone(),
            index: LineIndex::new(text, encoding),
        });

        for (module, path, text) in loader.files {
            if let Ok(uri) = Uri::from_file_path(&path) {
                files.insert(module, File {
                    uri,
                    index: LineIndex::new(text, encoding),
                });
            }
        }

        // Declarations of `std` and of the host can be opened too.
        let registry = &context.tcx.def_registry;

        if let Some(&std) = registry.extern_roots.get("std") {
            let sources = std_files();

            for (module, path) in module_paths(registry, std) {
                if let Some((_, file, source)) = sources.iter().find(|(name, ..)| *name == path)
                    && let Ok(uri) = Uri::from_file_path(file)
                {
                    files.insert(module, File {
                        uri,
                        index: LineIndex::new((*source).to_owned(), encoding),
                    });
                }
            }
        }

        if let Some((root, modules, paths)) = &stub {
            for (module, path) in module_paths(registry, ModuleId::ZERO) {
                let source = if path.is_empty() {
                    Some(root)
                } else {
                    modules.iter().find(|(name, _)| *name == path).map(|(_, source)| source)
                };
                let file = paths.iter().find(|(name, _)| *name == path).map(|(_, file)| file);

                if let (Some(source), Some(file)) = (source, file)
                    && let Ok(uri) = Uri::from_file_path(file)
                {
                    files.insert(module, File {
                        uri,
                        index: LineIndex::new(source.clone(), encoding),
                    });
                }
            }
        }

        Self {
            context,
            root,
            files,
            parse_errors: loader.parse_errors,
        }
    }

    /// Like [`Analysis::new`], but returns `None` if the type checker panics
    /// (on malformed code it doesn't handle yet), so the server keeps
    /// running.
    pub fn try_new(uri: &Uri, text: String, documents: &Documents, encoding: Encoding, project: Option<&Project>) -> Option<Self> {
        panic::catch_unwind(AssertUnwindSafe(|| Self::new(uri, text, documents, encoding, project))).ok()
    }

    /// The module of the document `uri` in the program.
    pub fn module_of(&self, uri: &Uri) -> Option<ModuleId> {
        self.files.iter().find(|(_, file)| file.uri == *uri).map(|(&module, _)| module)
    }

    /// Bounds of generics of the function (or impl) whose code is `ast`.
    pub fn bounds_of(&self, ast: &TypedAST) -> Vec<Bound> {
        let context = &self.context;
        let owns = |body: &FunctionBody| matches!(body, FunctionBody::Local { ast: owned, .. } if ptr::eq(owned, ast));

        if let Some((&func, _)) = context.functions.iter().find(|(_, body)| owns(body)) {
            return context.tcx.def_registry.func_bounds.get(&func).map_or_default(|bounds| bounds.to_vec());
        }

        for (&impl_ref, functions) in &context.vtables {
            if let Some((&vfunc, _)) = functions.iter().find(|(_, body)| owns(body)) {
                let registry = &context.tcx.impl_registry;

                return registry
                    .impls
                    .get(impl_ref)
                    .map(|generator| &generator.bounds)
                    .into_iter()
                    .chain(registry.method_bounds.get(&(impl_ref, vfunc)))
                    .flat_map(|bounds| bounds.iter().cloned())
                    .collect();
            }
        }

        Vec::new()
    }

    /// Typed ASTs of code in `module`: its top-level code and bodies of its
    /// functions.
    pub fn asts(&self, module: ModuleId) -> Vec<&TypedAST> {
        let bodies = self
            .context
            .functions
            .values()
            .chain(self.context.vtables.values().flat_map(|functions| functions.values()))
            .filter_map(|body| match body {
                FunctionBody::Local { ast, .. } => Some(ast),
                _ => None,
            });

        iter::once(&self.root).chain(bodies).filter(|ast| ast.module == module).collect()
    }

    /// The innermost expression at `offset` in `module`, with its AST.
    pub fn expr_at(&self, module: ModuleId, offset: usize) -> Option<(&TypedAST, ExprRef)> {
        self.asts(module)
            .into_iter()
            .flat_map(|ast| {
                ast.exprs
                    .iter()
                    .filter(move |(_, expr)| expr.span.start <= offset && offset <= expr.span.end && expr.span.start < expr.span.end)
                    // Variables made up by desugaring aren't in the source.
                    .filter(|(_, expr)| !matches!(&expr.value, Expr::Var(name) if name.starts_with('<')))
                    .map(move |(expr, typed)| (ast, expr, typed.span.end - typed.span.start))
            })
            .min_by_key(|&(.., length)| length)
            .map(|(ast, expr, _)| (ast, expr))
    }

    /// The variable declared or used at `offset` in `module`: its AST and
    /// index in [`TypedAST::variables`].
    pub fn variable_at(&self, module: ModuleId, offset: usize) -> Option<(&TypedAST, usize)> {
        let contains = |span: Span| span.start <= offset && offset <= span.end;

        self.asts(module).into_iter().find_map(|ast| {
            let declaration = ast
                .var_uses
                .iter()
                .find(|&&(usage, _)| contains(usage))
                .map(|&(_, declaration)| declaration)
                .or_else(|| ast.variables.iter().map(|variable| variable.span).find(|&span| contains(span)))?;

            ast.variables
                .iter()
                .position(|variable| variable.span == declaration)
                .map(|variable| (ast, variable))
        })
    }

    /// Where `item` is declared, if it's declared in a file of the program.
    pub fn item_span(&self, item: ModuleItem) -> Option<ModuleSpan> {
        let (name, span) = self
            .context
            .tcx
            .def_registry
            .modules
            .values()
            .flat_map(|module| module.items.iter())
            .find_map(|(name, &(found, kind, span))| (found == item && kind == DefinitionType::Local).then_some((name, span)))?;

        Some(self.name_in(span, name, None).unwrap_or(span))
    }

    /// Span of the first identifier `name` inside `within` (a declaration),
    /// after the identifier `after` if it's given (e.g. a field of a
    /// variant).
    pub fn name_in(&self, within: ModuleSpan, name: &str, after: Option<&str>) -> Option<ModuleSpan> {
        let ModuleSpan(module, span) = within;
        let tokens = Lexer::lex(self.files.get(&module)?.index.text());
        let mut tokens = tokens.iter().filter(|token| token.span.start >= span.start && token.span.end <= span.end);

        if let Some(after) = after {
            tokens.find(|token| is_ident(token, after))?;
        }

        tokens.find(|token| is_ident(token, name)).map(|token| ModuleSpan(module, token.span))
    }

    /// The identifier at `offset` in `module`.
    pub fn ident_at(&self, module: ModuleId, offset: usize) -> Option<Positioned<String>> {
        let tokens = Lexer::lex(self.files.get(&module)?.index.text());

        tokens.into_iter().find_map(|token| match token.value {
            Token::Ident(name) if token.span.start <= offset && offset <= token.span.end => Some(token.span.wrap(name)),
            _ => None,
        })
    }
}

fn is_ident(token: &Positioned<Token>, name: &str) -> bool {
    matches!(&token.value, Token::Ident(ident) if ident == name)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use lsp_types::Uri;

    use super::{Analysis, Documents, Encoding, Project};

    /// An empty directory for the test `name`.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mollie-lsp-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&dir);

        fs::create_dir_all(&dir).expect("the directory can be created");

        dir
    }

    #[test]
    fn projects_are_configured() {
        let project = Project::parse(
            Path::new("/game"),
            "# Addons.\nentries = [\n    \"addons/a.mol\", # the first\n    \"addons/b.mol\"\n]\nhost = \"stub\"\n",
        );

        assert_eq!(project.entries, [PathBuf::from("/game/addons/a.mol"), PathBuf::from("/game/addons/b.mol")]);
        assert_eq!(project.host, PathBuf::from("/game/stub"));
        assert_eq!(Project::parse(Path::new("/game"), "").host, PathBuf::from("/game/.mollie/host"));
    }

    #[test]
    fn programs_are_checked_against_the_host_stub() {
        let dir = temp_dir("stub");

        fs::create_dir_all(dir.join(".mollie/host")).expect("the stub directory can be created");
        fs::write(dir.join(".mollie/host/lib.mol"), "module graphics;\nextern func reveal() -> i32;\n").expect("written");
        fs::write(dir.join(".mollie/host/graphics.mol"), "extern value struct Size { width: f32, height: f32 }\n").expect("written");
        fs::write(dir.join("mollie.toml"), "entries = [\"main.mol\"]\n").expect("written");
        fs::write(
            dir.join("main.mol"),
            "import { Size } from graphics;\nmodule util;\nlet size = Size { width: 1.0, height: 2.0 };\nreveal() + util::two() + size.width as i32",
        )
        .expect("written");
        fs::write(dir.join("util.mol"), "func two() -> i32 { 2 }\n").expect("written");

        let main = dir.join("main.mol");
        let project = Project::find(&main).expect("the project is found");
        let uri = Uri::from_file_path(&main).expect("the path is absolute");
        let text = fs::read_to_string(&main).expect("read");
        let analysis = Analysis::new(&uri, text, &Documents::default(), Encoding::Utf8, Some(&project));

        assert!(analysis.context.diagnostics.is_empty(), "{:?}", analysis.context.diagnostics.errors);
        // The submodule is part of the entry's program.
        assert!(analysis.module_of(&Uri::from_file_path(dir.join("util.mol")).expect("absolute")).is_some());

        let _ = fs::remove_dir_all(&dir);
    }
}

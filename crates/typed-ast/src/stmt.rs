use mollie_shared::Span;
use mollie_typing::{ArrayKindInfo, ArrayTypeInfo, ModuleSpan, TypeError, TypeInfo, TypeSolver, UnifyArgs};

use crate::{
    FirstPass, FromParsed, TypedAST, TypedASTContextRef,
    block::Block,
    expr::{Expr, ExprRef},
    ty::binding_type,
};

mollie_index::new_idx_type!(StmtRef);

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(ExprRef),
    NewVar { mutable: bool, name: String, value: ExprRef },
}

impl FromParsed<mollie_parser::Stmt, Option<StmtRef>> for Stmt {
    fn from_parsed(stmt: mollie_parser::Stmt, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> Option<StmtRef> {
        match stmt {
            mollie_parser::Stmt::Expression(expr) => {
                let expr = Expr::from_parsed(expr, ast, context, span);

                Some(ast.add_stmt(Self::Expr(expr)))
            }
            mollie_parser::Stmt::VariableDecl(variable_decl) => {
                let mutable = variable_decl.mutable.is_some();
                let name_span = variable_decl.name.span;
                // Variables of the host are declared by stubs only (see
                // `TypedASTContext::load_host_stub`).
                let external = variable_decl.external.then(|| {
                    context
                        .type_solver
                        .error(TypeError::ExternVariableOutsideStub, ModuleSpan(ast.module, span))
                });

                // The annotation is the type expected of the value.
                // `let mut v: T[..]` is a writable view.
                let annotation = variable_decl.ty.map(|ty| {
                    let ty = binding_type(ty, ast.module, context, variable_decl.mutable.is_some());

                    TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[])
                });

                let (value, ty) = if let Some(value) = variable_decl.value {
                    let value = match annotation {
                        Some(annotation) => Expr::from_parsed_expecting(value.value, annotation, ast, context, value.span),
                        None => Expr::from_parsed(value.value, ast, context, value.span),
                    };

                    let expected = annotation.unwrap_or(ast[value].ty);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected,
                        found: ast[value].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[value].span));
                    }

                    // A view in a variable would see a copy of a fixed array
                    // (stored for it), not the array: changes to either would be
                    // lost to the other.
                    if let Some(annotation) = annotation
                        && let TypeInfo::Array(array) = &context.type_solver.type_infos[context.type_solver.get_info(annotation)].value
                        && matches!(array.kind, ArrayKindInfo::View { .. })
                        && let TypeInfo::Array(value_array) = &context.type_solver.type_infos[context.type_solver.get_info(ast[value].ty)].value
                        && matches!(value_array.kind, ArrayKindInfo::Fixed(_))
                    {
                        let message = String::from("a variable can't view a fixed array (`T[N]`): pass the array to a function taking a view instead");

                        context
                            .type_solver
                            .error(TypeError::InvalidArray { message }, ModuleSpan(ast.module, ast[value].span));
                    }

                    // A literal in a variable without a type is a growable array,
                    // whatever it's given to later (a literal given directly to
                    // a view is a fixed array, which needs no allocation).
                    let info = context.type_solver.get_info(ast[value].ty);

                    if let TypeInfo::Array(array) = &context.type_solver.type_infos[info].value
                        && matches!(array.kind, ArrayKindInfo::Literal(_))
                    {
                        let array = ArrayTypeInfo {
                            kind: ArrayKindInfo::Growable,
                            ..*array
                        };

                        context.type_solver.type_infos[info].value = TypeInfo::Array(array);
                    }

                    // The variable has the annotated type: the value is converted
                    // to it (e.g. to a trait object) by a block of that type.
                    let value = annotation.map_or(value, |annotation| {
                        let span = ast[value].span;
                        let block = ast.add_block(
                            Block {
                                stmts: Box::new([]),
                                expr: Some(value),
                            },
                            annotation,
                            span,
                        );

                        ast.add_expr(Expr::Block(block), annotation, span)
                    });

                    (value, ast[value].ty)
                } else if let Some(annotation) = annotation {
                    // Assigned later: `TypedAST::check_assignments` checks
                    // it's assigned before it's used, and only once if it's
                    // immutable.
                    let value = external.map_or(Expr::Uninit, Expr::Error);

                    (ast.add_expr(value, annotation, name_span), annotation)
                } else {
                    let error = external.unwrap_or_else(|| {
                        let name = variable_decl.name.value.0.clone();

                        context.type_solver.error(TypeError::MissingValue { name }, ModuleSpan(ast.module, name_span))
                    });
                    let ty = context.type_solver.add_info(TypeInfo::Error, Some(name_span));

                    (ast.add_expr(Expr::Error(error), ty, name_span), ty)
                };

                // An immutable variable declared without a value is assigned
                // once, which is checked by `TypedAST::check_assignments`.
                let deferred = matches!(ast[value].value, Expr::Uninit);

                ast.declare_var(context, variable_decl.name.value.0.clone(), ty, mutable || deferred, name_span);

                Some(ast.add_stmt(Self::NewVar {
                    mutable,
                    name: variable_decl.name.value.0,
                    value,
                }))
            }
            // Declarations are handled by `ModuleMap` passes, at the top level of modules only.
            mollie_parser::Stmt::StructDecl(_)
            | mollie_parser::Stmt::ConstDecl(_)
            | mollie_parser::Stmt::ViewDecl(_)
            | mollie_parser::Stmt::TraitDecl(_)
            | mollie_parser::Stmt::EnumDecl(_)
            | mollie_parser::Stmt::FuncDecl(_)
            | mollie_parser::Stmt::Impl(_)
            | mollie_parser::Stmt::Import(_)
            | mollie_parser::Stmt::Module(_) => {
                context.type_solver.error(TypeError::LocalDeclaration, ModuleSpan(ast.module, span));

                None
            }
        }
    }
}

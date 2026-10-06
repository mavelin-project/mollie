use mollie_shared::Span;
use mollie_typing::{ModuleSpan, Type, TypeError, TypeSolver, UnifyArgs};

use crate::{
    FirstPass, FromParsed, TypeLevelFromParsed, TypedAST, TypedASTContextRef,
    block::Block,
    expr::{Expr, ExprRef},
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
                // The annotation is the type expected of the value.
                let annotation = variable_decl.ty.map(|ty| {
                    let ty = Type::from_parsed(ty.value, ast.module, context, span);

                    TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[])
                });
                let value = match annotation {
                    Some(annotation) => Expr::from_parsed_expecting(variable_decl.value.value, annotation, ast, context, variable_decl.value.span),
                    None => Expr::from_parsed(variable_decl.value.value, ast, context, variable_decl.value.span),
                };
                let expected = annotation.unwrap_or(ast[value].ty);

                if let Err(err) = context.type_solver.unify(UnifyArgs {
                    expected,
                    found: ast[value].ty,
                }) {
                    let err = err.into_type_error(&mut context.type_solver);

                    context.type_solver.error(err, ModuleSpan(ast.module, ast[value].span));
                }

                let mutable = variable_decl.mutable.is_some();
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
                let ty = ast[value].ty;

                ast.declare_var(context, variable_decl.name.value.0.clone(), ty, mutable, variable_decl.name.span);

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

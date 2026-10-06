use mollie_shared::Span;
use mollie_typing::{
    AdtRef, AdtTypeInfo, AdtVariantRef, ArrayTypeInfo, ConstRef, FuncRef, FuncTypeInfo, ImplRef, LookupType, ModuleId, ModuleItem, ModuleSpan, PrimitiveType,
    TraitRef, TraitTypeInfo, Type, TypeError, TypeErrorRef, TypeErrorValue, TypeInfo, TypeInfoRef, TypeRef, TypeSolver, VFuncRef,
};

use crate::{
    FirstPass, FromParsed, TypeLevelFromParsed, TypedAST, TypedASTContextRef,
    module_map::{missing_item, resolve_item_path},
};

pub enum TypePathResult {
    VFunc(AdtRef, Box<[TypeInfoRef]>, ImplRef, VFuncRef),
    Adt(AdtRef, Box<[TypeInfoRef]>, Option<AdtVariantRef>),
    Trait(TraitRef, Box<[TypeInfoRef]>),
    Func(FuncRef),
    Intrinsic,
    Const(ConstRef),
    Generic(TypeInfoRef),
    Module(ModuleId),
    Error(TypeErrorRef, Span),
}

/// Resolves a path to a module item, reporting an error and returning `None`
/// if it can't be resolved.
impl FromParsed<mollie_parser::TypePathExpr, Option<Self>> for ModuleItem {
    fn from_parsed(path: mollie_parser::TypePathExpr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, _span: Span) -> Option<Self> {
        match resolve_item_path(&context.type_solver.context.def_registry, ast.module, &path) {
            Ok(item) => Some(item),
            Err((error, span)) => {
                context.type_solver.error(error, ModuleSpan(ast.module, span));

                None
            }
        }
    }
}

impl FromParsed<mollie_parser::TypePathExpr> for TypePathResult {
    fn from_parsed(path: mollie_parser::TypePathExpr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, _span: Span) -> Self {
        let mut result = Self::Module(ast.module);
        let segment_count = path.segments.len();

        for (i, segment) in path.segments.into_iter().enumerate() {
            if let Some((typo, _)) = context.type_solver.available_generics.get(&segment.value.name.value.0).copied() {
                result = Self::Generic(typo);

                break;
            }

            match result {
                Self::Adt(adt, type_args, None) => {
                    if let Some(variant) = context.type_solver.context.def_registry.adt_types[adt]
                        .variants
                        .iter()
                        .find_map(|(variant_ref, variant)| {
                            if variant.name.as_deref() == Some(segment.value.name.value.0.as_str()) {
                                Some(variant_ref)
                            } else {
                                None
                            }
                        })
                    {
                        result = Self::Adt(adt, type_args, Some(variant));
                    } else {
                        let storage_type_args = type_args.iter().map(|&type_arg| context.type_solver.solve(type_arg)).collect();
                        let storage_ty = context.type_solver.context.types.get_or_add(Type::Adt(adt, storage_type_args));

                        if let Some((vtable, vfunc)) = context.type_solver.context.find_vtable_by_func(storage_ty, &segment.value.name.value.0) {
                            result = Self::VFunc(adt, type_args, vtable, vfunc);
                        } else {
                            result = Self::Adt(adt, type_args, None);
                        }
                    }
                }
                Self::Module(current_module) => {
                    let registry = &context.type_solver.context.def_registry;
                    let name = &segment.value.name.value.0;
                    let item = if i == 0 {
                        registry.lookup(current_module, name)
                    } else {
                        registry.modules[current_module].get_item(name)
                    };

                    if let Some(item) = item {
                        match item {
                            ModuleItem::SubModule(module_id) => result = Self::Module(module_id),
                            ModuleItem::Adt(adt_ref) => {
                                let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.adt_types[adt_ref].generics);
                                let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                                for _ in 0..context.type_solver.context.def_registry.adt_types[adt_ref].generics {
                                    provided.push(match type_args.next() {
                                        Some(arg) => TypeInfo::from_parsed(arg.value, ast, context, arg.span),
                                        None => context.type_solver.add_unknown(None, None),
                                    });
                                }

                                let args = provided.into_boxed_slice();

                                result = Self::Adt(adt_ref, args, None);
                            }
                            ModuleItem::Trait(trait_ref) => {
                                let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.traits[trait_ref].generics);
                                let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                                for _ in 0..context.type_solver.context.def_registry.traits[trait_ref].generics {
                                    provided.push(match type_args.next() {
                                        Some(arg) => TypeInfo::from_parsed(arg.value, ast, context, arg.span),
                                        None => context.type_solver.add_unknown(None, None),
                                    });
                                }

                                let args = provided.into_boxed_slice();

                                result = Self::Trait(trait_ref, args);
                            }
                            ModuleItem::Func(func_ref) => {
                                // if let TypeInfo::Func(args, returns) =
                                // checker.solver.get_info(checker.
                                // local_functions[func_ref].ty) {
                                //     for arg in args {
                                //         if let TypeInfo::Adt(adt_ref, _,
                                // args) =
                                // checker.solver.get_info(arg.inner()) {
                                //
                                // ast.use_item(UsedItem::Adt(*adt_ref,
                                // args.clone()));
                                //         }
                                //     }

                                //     if let TypeInfo::Adt(adt_ref, _, args) =
                                // checker.solver.get_info(*returns) {
                                // ast.
                                // use_item(UsedItem::Adt(*adt_ref,
                                // args.clone()));
                                //     }
                                // }

                                // ast.use_item(UsedItem::Func(func_ref));

                                result = Self::Func(func_ref);
                            }
                            ModuleItem::Intrinsic(..) => {
                                result = Self::Intrinsic;
                            }
                            ModuleItem::Const(constant) => {
                                result = Self::Const(constant);
                            }
                        }
                    } else {
                        if (segment_count - 1 - i) > 0 {
                            result = Self::Error(
                                context.type_solver.error(
                                    missing_item(
                                        &context.type_solver.context.def_registry,
                                        segment.value.name.value.0,
                                        LookupType::Module { inside: current_module },
                                        i == 0,
                                    ),
                                    ModuleSpan(current_module, segment.value.name.span),
                                ),
                                segment.value.name.span,
                            );
                        } else {
                            result = Self::Error(
                                context.type_solver.error(
                                    missing_item(
                                        &context.type_solver.context.def_registry,
                                        segment.value.name.value.0,
                                        LookupType::Type { inside: current_module },
                                        i == 0,
                                    ),
                                    ModuleSpan(current_module, segment.value.name.span),
                                ),
                                segment.value.name.span,
                            );
                        }

                        break;
                    }
                }
                _ => (),
            }
        }

        // if let &TypePath::Adt(info_ref, ..) = &result
        //     && let TypeInfo::Adt(ty, _, args) =
        // checker.solver.get_info(info_ref) {
        //     ast.use_item(UsedItem::Adt(*ty, args.clone()));
        // }

        result
    }
}

impl FromParsed<mollie_parser::TypePathExpr, TypeInfoRef> for TypeInfo {
    fn from_parsed(path: mollie_parser::TypePathExpr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, path_span: Span) -> TypeInfoRef {
        enum TypePathResult {
            Type(TypeInfoRef),
            Module(ModuleId),
        }

        let mut span = None;
        let mut result = TypePathResult::Module(ast.module);

        for (index, segment) in path.segments.into_iter().enumerate() {
            if let Some((typo, _)) = context.type_solver.available_generics.get(&segment.value.name.value.0).copied() {
                result = TypePathResult::Type(typo);

                break;
            } else if let TypePathResult::Module(current_module) = result {
                let registry = &context.type_solver.context.def_registry;
                let name = &segment.value.name.value.0;
                let item = if index == 0 {
                    registry.lookup(current_module, name)
                } else {
                    registry.modules[current_module].get_item(name)
                };

                if let Some(item) = item {
                    span.replace(segment.span);

                    match item {
                        ModuleItem::SubModule(module_id) => result = TypePathResult::Module(module_id),
                        ModuleItem::Adt(id) => {
                            let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.adt_types[id].generics);
                            let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                            for _ in 0..context.type_solver.context.def_registry.adt_types[id].generics {
                                provided.push(match type_args.next() {
                                    Some(arg) => Self::from_parsed(arg.value, ast, context, arg.span),
                                    None => context.type_solver.add_unknown(None, None),
                                });
                            }

                            let type_args = provided.into_boxed_slice();

                            result = TypePathResult::Type(context.type_solver.add_info(Self::Adt(AdtTypeInfo { id, type_args }), None));
                        }
                        ModuleItem::Trait(id) => {
                            let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.traits[id].generics);
                            let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                            for _ in 0..context.type_solver.context.def_registry.traits[id].generics {
                                provided.push(match type_args.next() {
                                    Some(arg) => Self::from_parsed(arg.value, ast, context, arg.span),
                                    None => context.type_solver.add_unknown(None, None),
                                });
                            }

                            let type_args = provided.into_boxed_slice();

                            result = TypePathResult::Type(context.type_solver.add_info(Self::Trait(TraitTypeInfo { id, type_args }), None));
                        }
                        ModuleItem::Func(func) => {
                            // if let TypeInfo::Func(args, returns) =
                            // checker.solver.get_info(checker.
                            // local_functions[func_ref].ty) {
                            //     for arg in args {
                            //         if let TypeInfo::Adt(adt_ref, _, args) =
                            // checker.solver.get_info(arg.inner()) {
                            //             ast.use_item(UsedItem::Adt(*adt_ref,
                            // args.clone()));
                            //         }
                            //     }

                            //     if let TypeInfo::Adt(adt_ref, _, args) =
                            // checker.solver.get_info(*returns) {         ast.
                            // use_item(UsedItem::Adt(*adt_ref, args.clone()));
                            //     }
                            // }

                            // ast.use_item(UsedItem::Func(func_ref));

                            result = TypePathResult::Type(TypeSolver::type_to_info(
                                &mut context.type_solver.type_infos,
                                context.type_solver.context,
                                context.type_solver.context.def_registry.functions[func].ty,
                                &[],
                            ));
                        }
                        ModuleItem::Intrinsic(..) => {
                            // result = Self::Intrinsic(kind);
                        }
                        ModuleItem::Const(_) => {
                            context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::Type,
                                    found: TypeErrorValue::Value,
                                },
                                ModuleSpan(ast.module, segment.span),
                            );

                            return context.type_solver.add_info(Self::Error, None);
                        }
                    }
                } else {
                    context.type_solver.error(
                        missing_item(
                            &context.type_solver.context.def_registry,
                            segment.value.name.value.0,
                            LookupType::Type { inside: current_module },
                            index == 0,
                        ),
                        ModuleSpan(current_module, segment.value.name.span),
                    );

                    return context.type_solver.add_info(Self::Error, None);
                }
            }
        }

        match result {
            TypePathResult::Type(type_info_ref) => type_info_ref,
            TypePathResult::Module(_) => {
                // The span of the last resolved segment, or of the whole path.
                context.type_solver.error(
                    TypeError::Unexpected {
                        expected: TypeErrorValue::Type,
                        found: TypeErrorValue::Module,
                    },
                    ModuleSpan(ast.module, span.unwrap_or(path_span)),
                );

                context.type_solver.add_info(Self::Error, None)
            }
        }
    }
}

impl FromParsed<mollie_parser::Type, TypeInfoRef> for TypeInfo {
    fn from_parsed(ty: mollie_parser::Type, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> TypeInfoRef {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || match ty {
            mollie_parser::Type::Primitive(primitive_type) => context.type_solver.add_info(Self::Primitive(primitive_type), Some(span)),
            mollie_parser::Type::Array(element, size) => {
                let element = Self::from_parsed(element.value, ast, context, element.span);
                let size = size.map(|size| size.value);

                context.type_solver.add_info(Self::Array(ArrayTypeInfo { element, size }), Some(span))
            }
            mollie_parser::Type::Func(args, returns) => {
                let args = args.into_iter().map(|arg| Self::from_parsed(arg.value, ast, context, arg.span)).collect();

                let returns = match returns {
                    Some(ty) => Self::from_parsed(ty.value, ast, context, ty.span),
                    None => context.type_solver.add_info(Self::Primitive(PrimitiveType::Void), Some(span)),
                };

                context.type_solver.add_info(Self::Func(FuncTypeInfo { args, returns }), Some(span))
            }
            mollie_parser::Type::Path(type_path_expr) => Self::from_parsed(type_path_expr, ast, context, span),
        })
    }
}

impl TypeLevelFromParsed<mollie_parser::TypePathExpr, TypeRef> for Type {
    fn from_parsed(path: mollie_parser::TypePathExpr, module: ModuleId, context: &mut TypedASTContextRef<'_>, path_span: Span) -> TypeRef {
        enum TypePathResult {
            Type(TypeRef),
            Module(ModuleId),
        }

        let mut span = None;
        let mut result = TypePathResult::Module(module);

        for (index, segment) in path.segments.into_iter().enumerate() {
            if let Some((_, typo)) = context.type_solver.available_generics.get(&segment.value.name.value.0).copied() {
                result = TypePathResult::Type(typo);

                break;
            }
            if let &TypePathResult::Module(current_module) = &result {
                let registry = &context.type_solver.context.def_registry;
                let name = &segment.value.name.value.0;
                let item = if index == 0 {
                    registry.lookup(current_module, name)
                } else {
                    registry.modules[current_module].get_item(name)
                };

                if let Some(item) = item {
                    span.replace(segment.span);

                    match item {
                        ModuleItem::SubModule(module_id) => result = TypePathResult::Module(module_id),
                        ModuleItem::Adt(adt_ref) => {
                            let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.adt_types[adt_ref].generics);
                            let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                            for generic in 0..context.type_solver.context.def_registry.adt_types[adt_ref].generics {
                                provided.push(match type_args.next() {
                                    Some(arg) => Self::from_parsed(arg.value, module, context, arg.span),
                                    None => context.type_solver.context.types.get_or_add(Self::Generic(generic)),
                                });
                            }

                            let args = provided.into_boxed_slice();

                            result = TypePathResult::Type(context.type_solver.context.types.get_or_add(Self::Adt(adt_ref, args)));
                        }
                        ModuleItem::Trait(trait_ref) => {
                            let mut provided = Vec::with_capacity(context.type_solver.context.def_registry.traits[trait_ref].generics);
                            let mut type_args = segment.value.args.map(|type_args| type_args.value.0.into_iter()).into_iter().flatten();

                            for generic in 0..context.type_solver.context.def_registry.traits[trait_ref].generics {
                                provided.push(match type_args.next() {
                                    Some(arg) => Self::from_parsed(arg.value, module, context, arg.span),
                                    None => context.type_solver.context.types.get_or_add(Self::Generic(generic)),
                                });
                            }

                            let args = provided.into_boxed_slice();

                            result = TypePathResult::Type(context.type_solver.context.types.get_or_add(Self::Trait(trait_ref, args)));
                        }
                        ModuleItem::Func(func_ref) => {
                            // if let TypeInfo::Func(args, returns) =
                            // checker.solver.get_info(checker.
                            // local_functions[func_ref].ty) {
                            //     for arg in args {
                            //         if let TypeInfo::Adt(adt_ref, _,
                            // args) = checker.solver.get_info(arg.
                            // inner()) {
                            //
                            // ast.use_item(UsedItem::Adt(*adt_ref,
                            // args.clone()));
                            //         }
                            //     }

                            //     if let TypeInfo::Adt(adt_ref, _,
                            // args) = checker.solver.get_info(*returns)
                            // {
                            //         ast.use_item(UsedItem::Adt(*
                            // adt_ref, args.clone()));
                            //     }
                            // }

                            // ast.use_item(UsedItem::Func(func_ref));

                            result = TypePathResult::Type(context.type_solver.context.def_registry.functions[func_ref].ty);
                        }
                        ModuleItem::Intrinsic(..) => {
                            // result = TypePath::Intrinsic(kind, ty);
                        }
                        ModuleItem::Const(_) => {
                            context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::Type,
                                    found: TypeErrorValue::Value,
                                },
                                ModuleSpan(module, segment.span),
                            );

                            return context.type_solver.context.types.get_or_add(Self::Error);
                        }
                    }
                } else {
                    context.type_solver.error(
                        missing_item(
                            &context.type_solver.context.def_registry,
                            segment.value.name.value.0,
                            LookupType::Type { inside: current_module },
                            index == 0,
                        ),
                        ModuleSpan(current_module, segment.value.name.span),
                    );

                    return context.type_solver.context.types.get_or_add(Self::Error);
                }
            }
        }

        // if let &TypePath::Adt(info_ref, ..) = &result
        //     && let TypeInfo::Adt(ty, _, args) =
        // checker.solver.get_info(info_ref) {
        //     ast.use_item(UsedItem::Adt(*ty, args.clone()));
        // }

        match result {
            TypePathResult::Type(type_ref) => type_ref,
            TypePathResult::Module(_) => {
                // The span of the last resolved segment, or of the whole path.
                context.type_solver.error(
                    TypeError::Unexpected {
                        expected: TypeErrorValue::Type,
                        found: TypeErrorValue::Module,
                    },
                    ModuleSpan(module, span.unwrap_or(path_span)),
                );

                context.type_solver.context.types.get_or_add(Self::Error)
            }
        }
    }
}

impl TypeLevelFromParsed<mollie_parser::Type, TypeRef> for Type {
    fn from_parsed(ty: mollie_parser::Type, module: ModuleId, context: &mut TypedASTContextRef<'_>, span: Span) -> TypeRef {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || match ty {
            mollie_parser::Type::Primitive(primitive_type) => context.type_solver.context.types.get_or_add(Self::Primitive(primitive_type)),
            mollie_parser::Type::Array(element, size) => {
                let element = Self::from_parsed(element.value, module, context, element.span);

                context.type_solver.context.types.get_or_add(Self::Array(element, size.map(|size| size.value)))
            }
            mollie_parser::Type::Func(args, returns) => {
                let args = args.into_iter().map(|arg| Self::from_parsed(arg.value, module, context, arg.span)).collect();
                let returns = match returns {
                    Some(ty) => Self::from_parsed(ty.value, module, context, ty.span),
                    None => context.type_solver.context.types.get_or_add(Self::Primitive(PrimitiveType::Void)),
                };

                context.type_solver.context.types.get_or_add(Self::Func(args, returns))
            }
            mollie_parser::Type::Path(type_path_expr) => Self::from_parsed(type_path_expr, module, context, span),
        })
    }
}

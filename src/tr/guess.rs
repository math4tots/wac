use super::*;

/// tries to guess the type of an expression that must return some value
/// returning void will cause an error to be returned
pub(super) fn guess_type(lscope: &mut LocalScope, expr: &Expr) -> Result<Type, Error> {
    match guess_return_type(lscope, expr)? {
        ReturnType::Value(t) => Ok(t),
        ReturnType::Void => Err(Error::Type {
            span: expr.span().clone(),
            expected: "assignable type".into(),
            got: "void (variables cannot be void)".into(),
        }),
        ReturnType::NoReturn => {
            // I'm not sure if returning an error is actually the correct thing
            // to do here
            Err(Error::Type {
                span: expr.span().clone(),
                expected: "assignable type".into(),
                got: "noreturn (variables cannot be noreturn)".into(),
            })
        }
    }
}

/// tries to guess the type of an expression that must return some value
/// returning void will cause an error to be returned
pub(super) fn guess_return_type(lscope: &mut LocalScope, expr: &Expr) -> Result<ReturnType, Error> {
    match expr {
        Expr::Bool(..) => Ok(ReturnType::Value(Type::Bool)),
        Expr::Int(..) => Ok(ReturnType::Value(Type::I32)),
        Expr::Float(..) => Ok(ReturnType::Value(Type::F32)),
        Expr::String(..) => Ok(ReturnType::Value(Type::String)),
        Expr::List(..) => Ok(ReturnType::Value(Type::List)),
        Expr::GetVar(span, name) => match lscope.get_or_err(span.clone(), name)? {
            ScopeEntry::Local(info) => Ok(ReturnType::Value(info.type_)),
            ScopeEntry::Global(info) => Ok(ReturnType::Value(info.type_)),
            ScopeEntry::Constant(info) => Ok(ReturnType::Value(info.value.type_())),
        },
        Expr::SetVar(..) => Ok(ReturnType::Void),
        Expr::DeclVar(..) => Ok(ReturnType::Void),
        Expr::Block(_, exprs) => match exprs.last() {
            Some(last) => {
                // TODO: check the body for the noreturn type
                guess_return_type(lscope, last)
            }
            None => Ok(ReturnType::Void),
        },
        Expr::FunctionCall(span, name, _) => {
            Ok(lscope.getf_or_err(span.clone(), name)?.type_().return_type)
        }
        Expr::If(_, pairs, other) => {
            let mut ret = ReturnType::NoReturn;
            for (_, body) in pairs {
                ret = best_union_return_type(ret, guess_return_type(lscope, body)?);
            }
            ret = best_union_return_type(ret, guess_return_type(lscope, other)?);
            Ok(ret)
        }
        Expr::While(..) => Ok(ReturnType::Void),
        Expr::Binop(_span, op, left, right) => Ok(ReturnType::Value(match op {
            // == binops ==
            // equality ops
            //   is, is not, ==, !=
            //     * always returns bool
            //     * arguments same type
            //     * applies to (almost?) any type
            // comparison ops
            //   <, >, <=, >=
            //     * always returns bool
            //     * arguments same type
            //     * applies to numeric types, list and str
            // arithmetic ops
            //   +, -, *, %
            //     * either (i32, i32) or mixed i32, f32.
            //     * always returns same as argument type (f32 if mixed)
            //     * arguments always same type
            //         ints may be converted to floats
            //     * applies to numeric types
            // division ops
            //   /, //
            //     * / always returns f32, // always returns i32
            //     * arguments may be i32 or f32
            // bitwise
            //   &, ^, |, <<, >>
            //     * only accepts i32
            //     * always returns i32

            // equality ops
            Binop::Is | Binop::IsNot | Binop::Equal | Binop::NotEqual => Type::Bool,

            // comparison ops
            Binop::Less | Binop::LessOrEqual | Binop::Greater | Binop::GreaterOrEqual => Type::Bool,

            // arithmetic ops
            Binop::Add | Binop::Subtract | Binop::Multiply | Binop::Remainder => {
                match (guess_type(lscope, left)?, guess_type(lscope, right)?) {
                    (Type::I32, Type::I32) => Type::I32,
                    _ => Type::F32,
                }
            }

            // division ops
            Binop::Divide => Type::F32,
            Binop::TruncDivide => Type::I32,

            // bitwise
            Binop::BitwiseAnd
            | Binop::BitwiseOr
            | Binop::BitwiseXor
            | Binop::ShiftLeft
            | Binop::ShiftRight => Type::I32,
        })),
        Expr::Unop(_span, op, expr) => Ok(ReturnType::Value(match op {
            // == unops ==
            // sign ops
            //   +, -
            //     * returns i32 or f32 to match arg type
            // logical
            //   !
            //     * always returns bool
            Unop::Minus | Unop::Plus => match guess_type(lscope, expr)? {
                Type::I32 => Type::I32,
                _ => Type::F32,
            },
            Unop::Not => Type::Bool,
        })),
        Expr::AssertType(_, type_, _) => Ok(ReturnType::Value(*type_)),
        Expr::CString(..) => {
            // Should return a pointer
            Ok(ReturnType::Value(Type::I32))
        }
        Expr::Asm(_, _, type_, _) => Ok(type_.clone()),
    }
}
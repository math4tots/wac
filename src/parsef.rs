//! Parse functions/grammars that build on top of parser.rs
use crate::ir::*;
use crate::ParseError;
use crate::Parser;
use crate::Pattern;
use crate::Token;

const PREC_POSTFIX: u32 = 1000;
const PREC_UNARY: u32 = 900;
const PREC_PRODUCT: u32 = 600;
const PREC_SUM: u32 = 500;
const PREC_SHIFT: u32 = 400;
const PREC_BITWISE_AND: u32 = 300;
const PREC_BITWISE_XOR: u32 = 275;
const PREC_BITWISE_OR: u32 = 250;
const PREC_CMP: u32 = 200;
const PREC_LOGICAL_AND: u32 = 150;
const PREC_LOGICAL_OR: u32 = 140;
const PREC_ASSIGN: u32 = 100;

pub fn parse_file(parser: &mut Parser) -> Result<File, ParseError> {
    let mut imports = Vec::new();
    while parser.at_name("import") {
        let span = parser.span();
        parser.gettok();
        parser.expect(Token::Name("fn"))?;
        let module_name = parser.expect_string()?;
        let function_name = parser.expect_string()?;
        let alias = parser.expect_name()?;
        let type_ = parse_function_type(parser)?;
        let span = span.upto(&parser.span());
        consume_delim(parser);
        imports.push(Import::Function(FunctionImport {
            span,
            module_name,
            function_name,
            alias,
            type_,
        }));
    }
    let mut globalvars = Vec::new();
    let mut functions = Vec::new();
    consume_delim(parser);
    while !parser.at(Token::EOF) {
        match parser.peek() {
            Token::Name("fn") => functions.push(parse_func(parser)?),
            Token::Name("var") => globalvars.push(parse_globalvar(parser)?),
            _ => {
                return Err(ParseError::InvalidToken {
                    span: parser.span(),
                    expected: "Function".into(),
                    got: format!("{:?}", parser.peek()),
                })
            }
        }
        consume_delim(parser);
    }
    Ok(File {
        imports,
        functions,
        globalvars,
    })
}

fn parse_func(parser: &mut Parser) -> Result<Function, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("fn"))?;
    let mut visibility = Visibility::Private;
    if parser.consume(Token::LBracket) {
        loop {
            match parser.peek() {
                Token::Name("pub") => {
                    parser.gettok();
                    visibility = Visibility::Public;
                }
                Token::RBracket => {
                    parser.gettok();
                    break;
                }
                _ => {
                    return Err(ParseError::InvalidToken {
                        span,
                        expected: "Function attribute".into(),
                        got: format!("{:?}", parser.peek()),
                    })
                }
            }
        }
    }
    let name = parser.expect_name()?;
    let mut parameters = Vec::new();
    parser.expect(Token::LParen)?;
    while !parser.consume(Token::RParen) {
        let param_name = parser.expect_name()?;
        let param_type = parse_type(parser)?;
        parameters.push((param_name, param_type));
        if !parser.consume(Token::Comma) {
            parser.expect(Token::RParen)?;
            break;
        }
    }
    let return_type = if parser.at(Pattern::Name) {
        Some(parse_type(parser)?)
    } else {
        None
    };
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Function {
        span,
        visibility,
        name,
        parameters,
        return_type,
        body,
    })
}

fn parse_globalvar(parser: &mut Parser) -> Result<GlobalVariable, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("var"))?;
    let mut visibility = Visibility::Private;
    if parser.consume(Token::LBracket) {
        loop {
            match parser.peek() {
                Token::Name("pub") => {
                    parser.gettok();
                    visibility = Visibility::Public;
                }
                Token::RBracket => {
                    parser.gettok();
                    break;
                }
                _ => {
                    return Err(ParseError::InvalidToken {
                        span,
                        expected: "Function attribute".into(),
                        got: format!("{:?}", parser.peek()),
                    })
                }
            }
        }
    }
    let name = parser.expect_name()?;
    let type_ = if parser.at(Pattern::Name) {
        Some(parse_type(parser)?)
    } else {
        None
    };
    parser.expect(Token::Eq)?;
    let init = parse_expr(parser, 0)?;
    Ok(GlobalVariable {
        span,
        visibility,
        name,
        type_,
        init,
    })
}

fn consume_delim(parser: &mut Parser) {
    loop {
        match parser.peek() {
            Token::Newline => {
                parser.gettok();
            }
            _ => break,
        }
    }
}

fn parse_stmt(parser: &mut Parser) -> Result<Expr, ParseError> {
    let expr = parse_expr(parser, 0)?;
    consume_delim(parser);
    Ok(expr)
}

fn parse_expr(parser: &mut Parser, prec: u32) -> Result<Expr, ParseError> {
    let atom = parse_atom(parser)?;
    parse_infix(parser, atom, prec)
}

fn parse_atom(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    match parser.peek() {
        Token::Int(x) => {
            parser.gettok();
            Ok(Expr::Int(span, x))
        }
        Token::Float(x) => {
            parser.gettok();
            Ok(Expr::Float(span, x))
        }
        Token::Name("true") => {
            parser.gettok();
            Ok(Expr::Bool(span, true))
        }
        Token::Name("false") => {
            parser.gettok();
            Ok(Expr::Bool(span, false))
        }
        Token::Name("if") => parse_if(parser),
        Token::Name("while") => parse_while(parser),
        Token::Name("var") => {
            parser.gettok();
            let name = parser.expect_name()?;
            let type_ = if parser.at(Pattern::Name) {
                Some(parse_type(parser)?)
            } else {
                None
            };
            parser.expect(Token::Eq)?;
            let setexpr = parse_expr(parser, 0)?;
            let span = span.upto(&parser.span());
            Ok(Expr::DeclVar(span, name, type_, setexpr.into()))
        }
        Token::Name(name) => {
            parser.gettok();
            Ok(Expr::GetVar(span, name.into()))
        }
        Token::Minus | Token::Plus | Token::Exclamation => {
            let op = match parser.peek() {
                Token::Minus => Unop::Minus,
                Token::Plus => Unop::Plus,
                Token::Exclamation => Unop::Not,
                t => panic!("parse_atom Plus/Minus {:?}", t),
            };
            parser.gettok();
            let expr = parse_expr(parser, PREC_UNARY)?;
            Ok(Expr::Unop(span, op, expr.into()))
        }
        Token::Dollar => {
            parser.gettok();
            match parser.peek() {
                Token::Name("cstr") => {
                    parser.gettok();
                    parser.expect(Token::LParen)?;
                    let string = parser.expect_string()?;
                    parser.expect(Token::RParen)?;
                    let span = span.upto(&parser.span());
                    Ok(Expr::CString(span, string))
                }
                Token::Name("asm") => {
                    parser.gettok();
                    parser.expect(Token::LParen)?;
                    parser.expect(Token::LBracket)?;
                    let mut args = Vec::new();
                    while !parser.consume(Token::RBracket) {
                        args.push(parse_expr(parser, 0)?);
                        if !parser.consume(Token::Comma) {
                            parser.expect(Token::RBracket)?;
                            break;
                        }
                    }
                    parser.expect(Token::Comma)?;
                    let type_ = parse_voidable_type(parser)?;
                    parser.expect(Token::Comma)?;
                    let asm_code = parser.expect_string()?;
                    parser.consume(Token::Comma);
                    parser.expect(Token::RParen)?;
                    Ok(Expr::Asm(span, args, type_, asm_code))
                }
                _ => Err(ParseError::InvalidToken {
                    span,
                    expected: "intrinsic name".into(),
                    got: format!("{:?}", parser.peek()),
                }),
            }
        }
        Token::LParen => {
            parser.gettok();
            let expr = parse_expr(parser, 0)?;
            parser.expect(Token::RParen)?;
            Ok(expr)
        }
        Token::LBrace => parse_block(parser),
        _ => Err(ParseError::InvalidToken {
            span,
            expected: "Expression".into(),
            got: format!("{:?}", parser.peek()).into(),
        }),
    }
}

fn parse_if(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("if"))?;
    let cond = parse_expr(parser, 0)?;
    let body = parse_block(parser)?;
    let other = if parser.consume(Token::Name("else")) {
        match parser.peek() {
            Token::Name("if") => parse_if(parser)?,
            Token::LBrace => parse_block(parser)?,
            _ => {
                return Err(ParseError::InvalidToken {
                    span,
                    expected: "if or block (in else-branch)".into(),
                    got: format!("{:?}", parser.peek()),
                })
            }
        }
    } else {
        Expr::Block(span.clone(), vec![])
    };
    let span = span.upto(&parser.span());
    Ok(Expr::If(span, cond.into(), body.into(), other.into()))
}

fn parse_while(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("while"))?;
    let cond = parse_expr(parser, 0)?;
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Expr::While(span, cond.into(), body.into()))
}

/// parse any infix expressions with given precedence or higher
fn parse_infix(parser: &mut Parser, mut lhs: Expr, prec: u32) -> Result<Expr, ParseError> {
    let start = parser.span();
    loop {
        match parser.peek() {
            Token::LParen => {
                if prec > PREC_POSTFIX {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                match lhs {
                    Expr::GetVar(_, name) => {
                        let mut args = Vec::new();
                        while !parser.consume(Token::RParen) {
                            args.push(parse_expr(parser, 0)?);
                            if !parser.consume(Token::Comma) {
                                parser.expect(Token::RParen)?;
                                break;
                            }
                        }
                        let end = parser.span();
                        let span = span.join(&start).upto(&end);
                        lhs = Expr::FunctionCall(span, name, args);
                    }
                    _ => {
                        return Err(ParseError::InvalidToken {
                            span,
                            expected: "Function call".into(),
                            got: format!("indirect function calls not yet supported"),
                        })
                    }
                }
            }
            Token::Plus | Token::Minus => {
                if prec > PREC_SUM {
                    break;
                }
                let op = match parser.peek() {
                    Token::Plus => Binop::Add,
                    Token::Minus => Binop::Subtract,
                    tok => panic!("{:?}", tok),
                };
                let span = parser.span();
                parser.gettok();
                let right = parse_expr(parser, PREC_SUM + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, op, lhs.into(), right.into());
            }
            Token::Star | Token::Slash | Token::Slash2 | Token::Percent => {
                if prec > PREC_PRODUCT {
                    break;
                }
                let op = match parser.peek() {
                    Token::Star => Binop::Multiply,
                    Token::Slash => Binop::Divide,
                    Token::Slash2 => Binop::TruncDivide,
                    Token::Percent => Binop::Remainder,
                    tok => panic!("{:?}", tok),
                };
                let span = parser.span();
                parser.gettok();
                let right = parse_expr(parser, PREC_PRODUCT + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, op, lhs.into(), right.into());
            }
            Token::Caret => {
                if prec > PREC_BITWISE_XOR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_XOR + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, Binop::BitwiseXor, lhs.into(), rhs.into());
            }
            Token::Ampersand => {
                if prec > PREC_BITWISE_AND {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_AND + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, Binop::BitwiseAnd, lhs.into(), rhs.into());
            }
            Token::VerticalBar => {
                if prec > PREC_BITWISE_OR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_OR + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, Binop::BitwiseOr, lhs.into(), rhs.into());
            }
            Token::Lt2 | Token::Gt2 => {
                if prec > PREC_SHIFT {
                    break;
                }
                let op = match parser.peek() {
                    Token::Lt2 => Binop::ShiftLeft,
                    Token::Gt2 => Binop::ShiftRight,
                    tok => panic!("{:?}", tok),
                };
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_SHIFT + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, op, lhs.into(), rhs.into());
            }
            Token::Name("and") => {
                if prec > PREC_LOGICAL_AND {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_LOGICAL_AND + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::If(
                    span.clone(),
                    lhs.into(),
                    Expr::AssertType(span.clone(), Type::Bool, rhs.into()).into(),
                    Expr::Bool(span.clone(), false).into(),
                )
            }
            Token::Name("or") => {
                if prec > PREC_LOGICAL_OR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_LOGICAL_AND + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::If(
                    span.clone(),
                    lhs.into(),
                    Expr::Bool(span.clone(), true).into(),
                    rhs.into(),
                )
            }
            Token::Eq2 | Token::Ne | Token::Lt | Token::Gt | Token::Le | Token::Ge => {
                if prec > PREC_CMP {
                    break;
                }
                let op = match parser.peek() {
                    Token::Eq2 => Binop::Equal,
                    Token::Ne => Binop::NotEqual,
                    Token::Lt => Binop::Less,
                    Token::Gt => Binop::Greater,
                    Token::Le => Binop::LessOrEqual,
                    Token::Ge => Binop::GreaterOrEqual,
                    tok => panic!("{:?}", tok),
                };
                let span = parser.span();
                parser.gettok();
                let right = parse_expr(parser, PREC_CMP + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, op, lhs.into(), right.into());
            }
            Token::Eq => {
                if prec > PREC_ASSIGN {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                match lhs {
                    Expr::GetVar(_, name) => {
                        let setexpr = parse_expr(parser, 0)?;
                        lhs = Expr::SetVar(span.join(&start), name, setexpr.into());
                    }
                    _ => {
                        return Err(ParseError::InvalidToken {
                            span,
                            expected: "Assignment".into(),
                            got: format!("assignments only supported for variables"),
                        })
                    }
                }
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn parse_block(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::LBrace)?;
    let mut exprs = Vec::new();
    consume_delim(parser);
    while !parser.consume(Token::RBrace) {
        exprs.push(parse_stmt(parser)?);
    }
    let span = span.upto(&parser.span());
    Ok(Expr::Block(span, exprs))
}

fn parse_function_type(parser: &mut Parser) -> Result<FunctionType, ParseError> {
    let mut parameter_types = Vec::new();
    parser.expect(Token::LParen)?;
    while !parser.consume(Token::RParen) {
        let type_ = parse_type(parser)?;
        parameter_types.push(type_);
        if !parser.consume(Token::Comma) {
            parser.expect(Token::RParen)?;
            break;
        }
    }
    let return_type = if parser.at(Pattern::Name) {
        Some(parse_type(parser)?)
    } else {
        None
    };
    Ok(FunctionType {
        parameter_types,
        return_type,
    })
}

fn parse_voidable_type(parser: &mut Parser) -> Result<Option<Type>, ParseError> {
    match parser.peek() {
        Token::Name("void") => {
            parser.gettok();
            Ok(None)
        }
        _ => Ok(Some(parse_type(parser)?)),
    }
}

fn parse_type(parser: &mut Parser) -> Result<Type, ParseError> {
    let opt = match parser.peek() {
        Token::Name("bool") => Some(Type::Bool),
        Token::Name("i32") => Some(Type::I32),
        Token::Name("i64") => Some(Type::I64),
        Token::Name("f32") => Some(Type::F32),
        Token::Name("f64") => Some(Type::F64),
        _ => None,
    };
    if let Some(t) = opt {
        parser.gettok();
        Ok(t)
    } else {
        Err(ParseError::InvalidToken {
            span: parser.span(),
            expected: "Type".into(),
            got: format!("{:?}", parser.peek()),
        })
    }
}

//! Parse functions/grammars that build on top of parser.rs
use crate::get_enum_value_from_name;
use crate::ir::*;
use crate::ParseError;
use crate::Parser;
use crate::Pattern;
use crate::Token;
use std::cell::Cell;

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
    consume_delim(parser);
    while parser.at_name("import") {
        let span = parser.span();
        parser.gettok();
        parser.expect(Token::Name("fn"))?;
        let module_name = parser.expect_string()?;
        let function_name = parser.expect_string()?;
        let alias = parser.expect_name()?;
        let type_ = parse_function_type(parser, false)?;
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
    let mut traits = Vec::new();
    let mut impls = Vec::new();
    let mut enums = Vec::new();
    let mut records = Vec::new();
    let mut constants = Vec::new();
    consume_delim(parser);
    while !parser.at(Token::EOF) {
        match parser.peek() {
            Token::Name("fn") => functions.push(parse_func(parser)?),
            Token::Name("trait") => traits.push(parse_trait(parser)?),
            Token::Name("impl") => impls.push(parse_impl(parser)?),
            Token::Name("enum") => enums.push(parse_enum(parser)?),
            Token::Name("record") => records.push(parse_record(parser)?),
            Token::Name("var") => globalvars.push(parse_globalvar(parser)?),
            Token::Name("const") => constants.push(parse_constant(parser)?),
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
        constants,
        functions,
        traits,
        impls,
        enums,
        records,
        globalvars,
    })
}

fn parse_constant(parser: &mut Parser) -> Result<Constant, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("const"))?;
    let name = parser.expect_name()?;
    parser.expect(Token::Eq)?;
    let expr = parse_expr(parser, 0)?;
    let value = eval_constexpr(&expr, parser)?;
    parser.constants_map.insert(name.clone(), value.clone());
    let span = span.upto(&parser.span());
    Ok(Constant { span, name, value })
}

fn parse_constval(parser: &mut Parser) -> Result<ConstValue, ParseError> {
    let expr = parse_expr(parser, PREC_UNARY - 1)?;
    eval_constexpr(&expr, parser)
}

fn eval_constexpr(expr: &Expr, parser: &mut Parser) -> Result<ConstValue, ParseError> {
    match expr {
        Expr::Int(_, _, value) => Ok(ConstValue::I32(*value as i32)),
        Expr::GetVar(span, _, name) => match parser.constants_map.get(name) {
            Some(value) => Ok(value.clone()),
            None => match Type::from_name(name) {
                Some(type_) => Ok(ConstValue::Type(type_)),
                _ => {
                    if parser.strict_about_user_defined_types {
                        Err(ParseError::InvalidToken {
                            span: span.clone(),
                            expected: "named constant".into(),
                            got: "NotFound".into(),
                        })
                    } else {
                        // If it's not strict mode, just return some dummy type
                        Ok(ConstValue::I32(1))
                    }
                }
            },
        },
        Expr::GetAttr(_span, _, owner, member) => {
            if parser.strict_about_user_defined_types {
                let opt = match &**owner {
                    Expr::GetVar(_span, _, type_name) => match Type::from_name(type_name) {
                        Some(type_) => match type_ {
                            Type::Enum(_) => match get_enum_value_from_name(type_, member) {
                                Some(value) => Some((type_, value)),
                                None => None,
                            },
                            _ => None,
                        },
                        None => None,
                    },
                    _ => None,
                };
                match opt {
                    Some((type_, value)) => Ok(ConstValue::Enum(type_, value)),
                    None => Err(ParseError::InvalidToken {
                        span: expr.span().clone(),
                        expected: "constexpr".into(),
                        got: "non-const expression".into(),
                    }),
                }
            } else {
                // otherwise, just assume it is some enum value
                Ok(ConstValue::Enum(Type::Enum(0), 0))
            }
        }
        Expr::Unop(span, _, Unop::Minus, subexpr) => match eval_constexpr(subexpr, parser)? {
            ConstValue::I32(i) => Ok(ConstValue::I32(-i)),
            cval => Err(ParseError::InvalidToken {
                span: span.clone(),
                expected: "i32".into(),
                got: format!("{:?}", cval),
            }),
        },
        Expr::Unop(span, _, Unop::Plus, subexpr) => match eval_constexpr(subexpr, parser)? {
            ConstValue::I32(i) => Ok(ConstValue::I32(i)),
            cval => Err(ParseError::InvalidToken {
                span: span.clone(),
                expected: "i32".into(),
                got: format!("{:?}", cval),
            }),
        },
        Expr::Binop(span, _, Binop::Add, left, right) => {
            match (
                eval_constexpr(left, parser)?,
                eval_constexpr(right, parser)?,
            ) {
                (ConstValue::I32(a), ConstValue::I32(b)) => Ok(ConstValue::I32(a + b)),
                (left, right) => Err(ParseError::InvalidToken {
                    span: span.clone(),
                    expected: "addable values".into(),
                    got: format!("{:?}, {:?}", left, right),
                }),
            }
        }
        Expr::Binop(span, _, Binop::Subtract, left, right) => {
            match (
                eval_constexpr(left, parser)?,
                eval_constexpr(right, parser)?,
            ) {
                (ConstValue::I32(a), ConstValue::I32(b)) => Ok(ConstValue::I32(a - b)),
                (left, right) => Err(ParseError::InvalidToken {
                    span: span.clone(),
                    expected: "subtractable values".into(),
                    got: format!("{:?}, {:?}", left, right),
                }),
            }
        }
        Expr::Binop(span, _, Binop::Multiply, left, right) => {
            match (
                eval_constexpr(left, parser)?,
                eval_constexpr(right, parser)?,
            ) {
                (ConstValue::I32(a), ConstValue::I32(b)) => Ok(ConstValue::I32(a * b)),
                (left, right) => Err(ParseError::InvalidToken {
                    span: span.clone(),
                    expected: "multiplicable values".into(),
                    got: format!("{:?}, {:?}", left, right),
                }),
            }
        }
        Expr::Binop(span, _, Binop::TruncDivide, left, right) => {
            match (
                eval_constexpr(left, parser)?,
                eval_constexpr(right, parser)?,
            ) {
                (ConstValue::I32(a), ConstValue::I32(b)) => Ok(ConstValue::I32(a / b)),
                (left, right) => Err(ParseError::InvalidToken {
                    span: span.clone(),
                    expected: "trunc-dividable values".into(),
                    got: format!("{:?}, {:?}", left, right),
                }),
            }
        }
        Expr::Binop(span, _, Binop::Remainder, left, right) => {
            match (
                eval_constexpr(left, parser)?,
                eval_constexpr(right, parser)?,
            ) {
                (ConstValue::I32(a), ConstValue::I32(b)) => Ok(ConstValue::I32(a % b)),
                (left, right) => Err(ParseError::InvalidToken {
                    span: span.clone(),
                    expected: "rem-able values".into(),
                    got: format!("{:?}, {:?}", left, right),
                }),
            }
        }
        _ => Err(ParseError::InvalidToken {
            span: expr.span().clone(),
            expected: "constexpr".into(),
            got: "non-const expression".into(),
        }),
    }
}

fn parse_const_uint(parser: &mut Parser) -> Result<u32, ParseError> {
    let span = parser.span();
    match parse_constval(parser)? {
        ConstValue::I32(i) if i >= 0 => Ok(i as u32),
        ConstValue::Enum(_, i) if i >= 0 => Ok(i as u32),
        value => Err(ParseError::InvalidToken {
            span,
            expected: format!("const uint"),
            got: format!("{:?}", value),
        }),
    }
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
    // we split out the receiver_name, because when we're running
    // the parse the first time, the receiver_type will be a dummy
    // value, so we can't get the original name back from it
    let (receiver_type, receiver_name, short_name) = if parser.lookahaed(1) == Some(Token::Dot) {
        let receiver_name = parser.expect_name()?;
        let receiver_type = type_from_name(parser, &receiver_name)?;
        parser.expect(Token::Dot)?;
        let short_name = parser.expect_name()?;
        (Some(receiver_type), Some(receiver_name), short_name)
    } else {
        (None, None, parser.expect_name()?)
    };
    let type_ = if let Some(receiver_type) = receiver_type {
        // if we have a receiver_type, we expect a 'self'
        // parameter, and the type should be the receiver type
        let mut type_ = parse_function_type(parser, true)?;
        type_.parameters[0].1 = receiver_type;
        type_
    } else {
        parse_function_type(parser, false)?
    };
    let name = if let Some(receiver_name) = receiver_name {
        format!("{}.{}", receiver_name, short_name).into()
    } else {
        short_name
    };
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Function {
        span,
        visibility,
        name,
        type_,
        body,
    })
}

fn parse_trait(parser: &mut Parser) -> Result<Trait, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("trait"))?;
    let name = parser.expect_name()?;
    let type_ = parse_function_type(parser, true)?;
    let span = span.upto(&parser.span());
    Ok(Trait { span, name, type_ })
}

fn parse_impl(parser: &mut Parser) -> Result<Impl, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("impl"))?;
    let receiver_type = parse_type(parser)?;
    parser.expect(Token::Name("for"))?;
    let trait_name = parser.expect_name()?;
    let type_ = parse_function_type(parser, true)?;
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Impl {
        span,
        receiver_type,
        trait_name,
        type_,
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
                        expected: "Global variable attribute".into(),
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

fn parse_enum(parser: &mut Parser) -> Result<Enum, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("enum"))?;
    let name = parser.expect_name()?;
    let mut members = Vec::new();
    parser.expect(Token::LBrace)?;
    consume_delim(parser);
    while !parser.consume(Token::RBrace) {
        let member_name = parser.expect_name()?;
        members.push(member_name);
        if !parser.consume(Token::Comma) {
            parser.expect(Token::RBrace)?;
            break;
        }
        consume_delim(parser);
    }
    let span = span.upto(&parser.span());

    Ok(Enum {
        span,
        name: name,
        members,
    })
}

fn parse_record(parser: &mut Parser) -> Result<Record, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("record"))?;
    let name = parser.expect_name()?;
    let mut fields = Vec::new();
    parser.expect(Token::LBrace)?;
    consume_delim(parser);
    while !parser.consume(Token::RBrace) {
        let member_name = parser.expect_name()?;
        let type_ = parse_type(parser)?;
        fields.push((member_name, type_));
        expect_delim(parser)?;
    }
    let span = span.upto(&parser.span());
    Ok(Record {
        span,
        name: name,
        fields,
    })
}

fn consume_delim(parser: &mut Parser) {
    loop {
        match parser.peek() {
            Token::Newline | Token::Semicolon => {
                parser.gettok();
            }
            _ => break,
        }
    }
}

fn expect_delim(parser: &mut Parser) -> Result<(), ParseError> {
    if !parser.at(Token::RBrace) && !parser.at(Token::EOF) {
        if !parser.consume(Token::Semicolon) {
            parser.expect(Token::Newline)?;
        }
    }
    consume_delim(parser);
    Ok(())
}

fn parse_stmt(parser: &mut Parser) -> Result<Expr, ParseError> {
    let expr = parse_expr(parser, 0)?;
    expect_delim(parser)?;
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
            Ok(Expr::Int(span, Cell::new(None), x))
        }
        Token::Float(x) => {
            parser.gettok();
            Ok(Expr::Float(span, Cell::new(None), x))
        }
        Token::NormalString(_) | Token::RawString(_) => {
            let s = parser.expect_string()?;
            Ok(Expr::String(span, Cell::new(None), s))
        }
        Token::Name("true") => {
            parser.gettok();
            Ok(Expr::Bool(span, Cell::new(None), true))
        }
        Token::Name("false") => {
            parser.gettok();
            Ok(Expr::Bool(span, Cell::new(None), false))
        }
        Token::Name("if") => parse_if(parser),
        Token::Name("while") => parse_while(parser),
        Token::Name("for") => parse_for(parser),
        Token::Name("new") => {
            let span = parser.span();
            parser.gettok();
            let type_ = parse_type(parser)?;
            let mut args = Vec::new();
            parser.expect(Token::LParen)?;
            while !parser.consume(Token::RParen) {
                args.push(parse_expr(parser, 0)?);
                if !parser.consume(Token::Comma) {
                    parser.expect(Token::RParen)?;
                    break;
                }
            }
            let span = span.upto(&parser.span());
            Ok(Expr::New(span, Cell::new(None), type_, args))
        }
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
            Ok(Expr::DeclVar(
                span,
                Cell::new(None),
                name,
                type_,
                setexpr.into(),
            ))
        }
        Token::Name("switch") => {
            parser.gettok();
            let src = parse_expr(parser, 0)?.into();
            parser.expect(Token::LBrace)?;
            let mut other = None;
            let mut pairs = Vec::<(Vec<ConstValue>, Expr)>::new();
            while !parser.consume(Token::RBrace) {
                consume_delim(parser);
                if parser.consume(Token::Name("_")) {
                    parser.expect(Token::Arrow)?;
                    other = Some(parse_expr(parser, 0)?.into());
                    consume_delim(parser);
                    parser.expect(Token::RBrace)?;
                    break;
                } else {
                    let mut cvals = vec![parse_constval(parser)?];
                    while parser.consume(Token::VerticalBar) {
                        cvals.push(parse_constval(parser)?);
                    }
                    parser.expect(Token::Arrow)?;
                    let body = parse_expr(parser, 0)?;
                    pairs.push((cvals, body));
                    consume_delim(parser);
                }
            }
            let span = span.upto(&parser.span());
            Ok(Expr::Switch(span, Cell::new(None), src, pairs, other))
        }
        Token::Name(_) => {
            let name = parser.expect_name()?;
            Ok(Expr::GetVar(span, Cell::new(None), name))
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
            Ok(Expr::Unop(span, Cell::new(None), op, expr.into()))
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
                    Ok(Expr::CString(span, Cell::new(None), string))
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
                    let type_ = parse_return_type(parser)?;
                    parser.expect(Token::Comma)?;
                    let asm_code = parser.expect_string()?;
                    parser.consume(Token::Comma);
                    parser.expect(Token::RParen)?;
                    Ok(Expr::Asm(span, Cell::new(None), args, type_, asm_code))
                }
                Token::Name("read1")
                | Token::Name("read2")
                | Token::Name("read4")
                | Token::Name("read8") => {
                    let name = parser.expect_name()?;
                    let name: &str = &name;
                    parser.expect(Token::LParen)?;
                    let expr = parse_expr(parser, 0)?;
                    let offset =
                        if parser.consume(Token::Comma) && parser.consume(Token::Name("offset")) {
                            parser.expect(Token::Colon)?;
                            parse_const_uint(parser)?
                        } else {
                            0
                        };
                    parser.consume(Token::Comma);
                    parser.expect(Token::RParen)?;
                    let span = span.upto(&parser.span());
                    Ok(match name {
                        "read1" => Expr::Read1(span, Cell::new(None), expr.into(), offset),
                        "read2" => Expr::Read2(span, Cell::new(None), expr.into(), offset),
                        "read4" => Expr::Read4(span, Cell::new(None), expr.into(), offset),
                        "read8" => Expr::Read8(span, Cell::new(None), expr.into(), offset),
                        _ => panic!("Impossible read* name: {}", name),
                    })
                }
                Token::Name("write1")
                | Token::Name("write2")
                | Token::Name("write4")
                | Token::Name("write8") => {
                    let name = parser.expect_name()?;
                    let name: &str = &name;
                    parser.expect(Token::LParen)?;
                    let addr = parse_expr(parser, 0)?;
                    parser.expect(Token::Comma)?;
                    let val = parse_expr(parser, 0)?;
                    let offset =
                        if parser.consume(Token::Comma) && parser.consume(Token::Name("offset")) {
                            parser.expect(Token::Colon)?;
                            parse_const_uint(parser)?
                        } else {
                            0
                        };
                    parser.consume(Token::Comma);
                    parser.expect(Token::RParen)?;
                    let span = span.upto(&parser.span());
                    Ok(match name {
                        "write1" => {
                            Expr::Write1(span, Cell::new(None), addr.into(), val.into(), offset)
                        }
                        "write2" => {
                            Expr::Write2(span, Cell::new(None), addr.into(), val.into(), offset)
                        }
                        "write4" => {
                            Expr::Write4(span, Cell::new(None), addr.into(), val.into(), offset)
                        }
                        "write8" => {
                            Expr::Write8(span, Cell::new(None), addr.into(), val.into(), offset)
                        }
                        _ => panic!("Impossible write* name: {}", name),
                    })
                }
                _ => Err(ParseError::InvalidToken {
                    span,
                    expected: "intrinsic name".into(),
                    got: format!("{:?}", parser.peek()),
                }),
            }
        }
        Token::LBracket => {
            parser.gettok();
            let mut exprs = Vec::new();
            while !parser.consume(Token::RBracket) {
                exprs.push(parse_expr(parser, 0)?);
                if !parser.consume(Token::Comma) {
                    parser.expect(Token::RBracket)?;
                    break;
                }
            }
            Ok(Expr::List(span, Cell::new(None), exprs))
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
    let mut pairs = vec![(cond, body)];
    let mut other = Expr::Block(span.clone(), Cell::new(None), vec![]);

    while parser.consume(Token::Name("else")) {
        match parser.peek() {
            Token::Name("if") => {
                parser.gettok();
                let cond = parse_expr(parser, 0)?;
                let body = parse_block(parser)?;
                pairs.push((cond, body));
            }
            Token::LBrace => {
                other = parse_block(parser)?;
            }
            _ => {
                return Err(ParseError::InvalidToken {
                    span,
                    expected: "if or block (in else-branch)".into(),
                    got: format!("{:?}", parser.peek()),
                })
            }
        }
    }
    let span = span.upto(&parser.span());
    Ok(Expr::If(span, Cell::new(None), pairs, other.into()))
}

fn parse_while(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("while"))?;
    let cond = parse_expr(parser, 0)?;
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Expr::While(span, Cell::new(None), cond.into(), body.into()))
}

fn parse_for(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::Name("for"))?;
    let name = parser.expect_name()?;
    parser.expect(Token::Name("in"))?;
    let start = parse_expr(parser, 0)?;
    parser.expect(Token::Dot2)?;
    let end = parse_expr(parser, 0)?;
    let body = parse_block(parser)?;
    let span = span.upto(&parser.span());
    Ok(Expr::For(
        span,
        Cell::new(None),
        name,
        start.into(),
        end.into(),
        body.into(),
    ))
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
                    Expr::GetVar(_, _, name) => {
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
                        lhs = Expr::FunctionCall(span, Cell::new(None), name, args);
                    }
                    Expr::GetAttr(_, _, owner, name) => {
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
                        lhs = Expr::AssociatedFunctionCall(
                            span,
                            Cell::new(None),
                            owner.into(),
                            name,
                            args,
                        );
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
            Token::LBracket => {
                if prec > PREC_POSTFIX {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let index = parse_expr(parser, 0)?;
                parser.expect(Token::RBracket)?;
                let span = span.upto(&parser.span());
                lhs = Expr::GetItem(span, Cell::new(None), lhs.into(), index.into());
            }
            Token::Dot => {
                if prec > PREC_POSTFIX {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                if parser.consume(Token::LParen) {
                    let ascribed_type = parse_type(parser)?;
                    parser.expect(Token::RParen)?;
                    lhs = Expr::AscribeType(span, Cell::new(None), lhs.into(), ascribed_type);
                } else {
                    let name = parser.expect_name()?;
                    let span = span.join(&start).upto(&parser.span());
                    lhs = Expr::GetAttr(span, Cell::new(None), lhs.into(), name);
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
                lhs = Expr::Binop(span, Cell::new(None), op, lhs.into(), right.into());
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
                lhs = Expr::Binop(span, Cell::new(None), op, lhs.into(), right.into());
            }
            Token::Caret => {
                if prec > PREC_BITWISE_XOR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_XOR + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(
                    span,
                    Cell::new(None),
                    Binop::BitwiseXor,
                    lhs.into(),
                    rhs.into(),
                );
            }
            Token::Ampersand => {
                if prec > PREC_BITWISE_AND {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_AND + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(
                    span,
                    Cell::new(None),
                    Binop::BitwiseAnd,
                    lhs.into(),
                    rhs.into(),
                );
            }
            Token::VerticalBar => {
                if prec > PREC_BITWISE_OR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_BITWISE_OR + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(
                    span,
                    Cell::new(None),
                    Binop::BitwiseOr,
                    lhs.into(),
                    rhs.into(),
                );
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
                lhs = Expr::Binop(span, Cell::new(None), op, lhs.into(), rhs.into());
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
                    Cell::new(None),
                    vec![(
                        lhs,
                        Expr::AscribeType(span.clone(), Cell::new(None), rhs.into(), Type::Bool),
                    )],
                    Expr::Bool(span.clone(), Cell::new(None), false).into(),
                )
            }
            Token::Name("or") => {
                if prec > PREC_LOGICAL_OR {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                let rhs = parse_expr(parser, PREC_LOGICAL_OR + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::If(
                    span.clone(),
                    Cell::new(None),
                    vec![(lhs, Expr::Bool(span.clone(), Cell::new(None), true))],
                    rhs.into(),
                )
            }
            Token::Eq2
            | Token::Ne
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::Name("is") => {
                if prec > PREC_CMP {
                    break;
                }
                let mut op = match parser.peek() {
                    Token::Eq2 => Binop::Equal,
                    Token::Ne => Binop::NotEqual,
                    Token::Lt => Binop::Less,
                    Token::Gt => Binop::Greater,
                    Token::Le => Binop::LessOrEqual,
                    Token::Ge => Binop::GreaterOrEqual,
                    Token::Name("is") => Binop::Is,
                    tok => panic!("{:?}", tok),
                };
                let span = parser.span();
                parser.gettok();
                if let Binop::Is = op {
                    if parser.consume(Token::Name("not")) {
                        op = Binop::IsNot;
                    }
                }
                let right = parse_expr(parser, PREC_CMP + 1)?;
                let span = span.join(&start).upto(&parser.span());
                lhs = Expr::Binop(span, Cell::new(None), op, lhs.into(), right.into());
            }
            Token::Eq => {
                if prec > PREC_ASSIGN {
                    break;
                }
                let span = parser.span();
                parser.gettok();
                match lhs {
                    Expr::GetVar(_, _, name) => {
                        let setexpr = parse_expr(parser, 0)?;
                        lhs =
                            Expr::SetVar(span.join(&start), Cell::new(None), name, setexpr.into());
                    }
                    Expr::GetItem(getitem_span, _, owner, index) => {
                        let setexpr = parse_expr(parser, 0)?;
                        lhs = Expr::SetItem(
                            span.join(&getitem_span),
                            Cell::new(None),
                            owner,
                            index,
                            setexpr.into(),
                        )
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
    Ok(Expr::Block(span, Cell::new(None), exprs))
}

fn parse_function_type(parser: &mut Parser, has_self: bool) -> Result<FunctionType, ParseError> {
    let mut trace = true;
    if parser.consume(Token::LBracket) {
        loop {
            match parser.peek() {
                Token::Name("notrace") => {
                    parser.gettok();
                    trace = false
                }
                Token::RBracket => {
                    parser.gettok();
                    break;
                }
                _ => {
                    return Err(ParseError::InvalidToken {
                        span: parser.span(),
                        expected: "FunctionType attribute".into(),
                        got: format!("{:?}", parser.peek()),
                    })
                }
            }
        }
    }
    let mut parameters = Vec::new();
    parser.expect(Token::LParen)?;
    let parse_remaining_params = if has_self {
        // The first parameter is always a fixed 'self' parameter
        // (with implied id type) for has_self functions
        // has_self applies for 'trait' and 'impl' functions
        parser.expect(Token::Name("self"))?;
        parameters.push(("self".into(), Type::Id));

        // in order for there to be more parameters,
        // 'self' has to be followed by a comma
        if parser.consume(Token::Comma) {
            true
        } else {
            parser.expect(Token::RParen)?;
            false
        }
    } else {
        true
    };
    if parse_remaining_params {
        while !parser.consume(Token::RParen) {
            let name = parser.expect_name()?;
            let type_ = parse_type(parser)?;
            parameters.push((name, type_));
            if !parser.consume(Token::Comma) {
                parser.expect(Token::RParen)?;
                break;
            }
        }
    }
    let return_type = if parser.at(Pattern::Name) {
        parse_return_type(parser)?
    } else {
        ReturnType::Void
    };
    Ok(FunctionType {
        parameters,
        return_type,
        trace,
    })
}

fn parse_return_type(parser: &mut Parser) -> Result<ReturnType, ParseError> {
    match parser.peek() {
        Token::Name("void") => {
            parser.gettok();
            Ok(ReturnType::Void)
        }
        Token::Name("noreturn") => {
            parser.gettok();
            Ok(ReturnType::NoReturn)
        }
        _ => Ok(ReturnType::Value(parse_type(parser)?)),
    }
}

fn parse_type(parser: &mut Parser) -> Result<Type, ParseError> {
    let opt = match parser.peek() {
        Token::Name(name) => Some(type_from_name(parser, name)?),
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

fn type_from_name(parser: &mut Parser, name: &str) -> Result<Type, ParseError> {
    Ok(match name {
        "i32" => Type::I32,
        "i64" => Type::I64,
        "f32" => Type::F32,
        "f64" => Type::F64,
        "bool" => Type::Bool,
        "type" => Type::Type,
        "bytes" => Type::Bytes,
        "str" => Type::String,
        "list" => Type::List,
        "id" => Type::Id,
        name => parser.get_user_defined_type(&parser.span(), &name.into())?,
    })
}

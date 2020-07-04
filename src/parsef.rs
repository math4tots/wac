//! Parse functions/grammars that build on top of parser.rs
use crate::ir::*;
use crate::Parser;
use crate::Token;
use crate::ParseError;
use crate::Pattern;

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
        let span = span.upto(parser.span());
        consume_delim(parser);
        imports.push(Import::Function(FunctionImport {
            span,
            module_name,
            function_name,
            alias,
            type_,
        }));
    }
    let mut functions = Vec::new();
    consume_delim(parser);
    while !parser.at(Token::EOF) {
        match parser.peek() {
            Token::Name("fn") => functions.push(parse_func(parser)?),
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
    Ok(File { imports, functions })
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
    let mut locals = Vec::new();
    if parser.consume(Token::LBracket) {
        while !parser.consume(Token::RBracket) {
            let name = parser.expect_name()?;
            let type_ = parse_type(parser)?;
            locals.push((name, type_));
            if !parser.consume(Token::Comma) {
                parser.expect(Token::RBracket)?;
                break;
            }
        }
    }
    let body = parse_block(parser)?;
    let span = span.upto(parser.span());
    Ok(Function {
        span,
        visibility,
        name,
        parameters,
        return_type,
        locals,
        body,
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
    let expr = parse_expr(parser)?;
    consume_delim(parser);
    Ok(expr)
}

fn parse_expr(parser: &mut Parser) -> Result<Expr, ParseError> {
    parse_assign(parser)
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
        Token::Name(name) => {
            parser.gettok();
            Ok(Expr::GetVar(span, name.into()))
        }
        Token::Dollar => {
            parser.gettok();
            match parser.peek() {
                Token::Name("cstr") => {
                    parser.gettok();
                    parser.expect(Token::LParen)?;
                    let string = parser.expect_string()?;
                    parser.expect(Token::RParen)?;
                    let span = span.upto(parser.span());
                    Ok(Expr::CString(span, string))
                }
                _ => Err(ParseError::InvalidToken {
                    span,
                    expected: "intrinsic name".into(),
                    got: format!("{:?}", parser.peek()),
                })
            }
        }
        Token::LBrace => parse_block(parser),
        _ => Err(ParseError::InvalidToken {
            span,
            expected: "Expression".into(),
            got: format!("{:?}", parser.peek()).into(),
        }),
    }
}

fn parse_postfix(parser: &mut Parser) -> Result<Expr, ParseError> {
    let mut e = parse_atom(parser)?;
    let start = parser.span();
    loop {
        match parser.peek() {
            Token::LParen => {
                let span = parser.span();
                parser.gettok();
                match e {
                    Expr::GetVar(_, name) => {
                        let mut args = Vec::new();
                        while !parser.consume(Token::RParen) {
                            args.push(parse_expr(parser)?);
                            if !parser.consume(Token::Comma) {
                                parser.expect(Token::RParen)?;
                                break;
                            }
                        }
                        let end = parser.span();
                        let span = span.join(start).upto(end);
                        e = Expr::FunctionCall(span, name, args);
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
            _ => break,
        }
    }
    Ok(e)
}

fn parse_assign(parser: &mut Parser) -> Result<Expr, ParseError> {
    let mut e = parse_postfix(parser)?;
    let start = parser.span();
    loop {
        match parser.peek() {
            Token::Eq => {
                let span = parser.span();
                parser.gettok();
                match e {
                    Expr::GetVar(_, name) => {
                        let setexpr = parse_expr(parser)?;
                        e = Expr::SetVar(span.join(start), name, setexpr.into());
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
    Ok(e)
}

fn parse_block(parser: &mut Parser) -> Result<Expr, ParseError> {
    let span = parser.span();
    parser.expect(Token::LBrace)?;
    let mut exprs = Vec::new();
    consume_delim(parser);
    while !parser.consume(Token::RBrace) {
        exprs.push(parse_stmt(parser)?);
    }
    let span = span.upto(parser.span());
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

fn parse_type(parser: &mut Parser) -> Result<Type, ParseError> {
    let opt = match parser.peek() {
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
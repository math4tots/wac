use crate::ir::*;
use crate::Error;
use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;

impl Program {
    /// Translate the given program into webassembly
    pub fn wat(self) -> Result<String, Error> {
        gen(self)
    }
}

/// translate a program into webassembly text
fn gen(program: Program) -> Result<String, Error> {
    let mut out = String::new();

    for ext in &program.externs {
        gen_extern(&mut out, ext)?;
    }

    out.push_str("(memory $memory 1)\n");

    gen_data(&mut out, &program.memory)?;

    for gvar in &program.globals {
        gen_global(&mut out, gvar)?;
    }

    for func in &program.funcs {
        gen_func(&mut out, func)?;
    }

    gen_start(&mut out, &program)?;

    writeln!(out, "(start $start)")?;
    writeln!(out, r#"(export "Main" (func $f/Main))"#)?;

    Ok(out)
}

fn gen_data(out: &mut String, memory: &Rc<RefCell<Memory>>) -> Result<(), Error> {
    let (start_pos, data) = memory.borrow().gen();
    write!(out, "(data (i32.const {}) \"", start_pos)?;
    for byte in data {
        write!(out, "\\{:02x}", byte)?;
    }
    writeln!(out, "\")")?;
    Ok(())
}

fn gen_global(out: &mut String, gvar: &Global) -> Result<(), Error> {
    // declare global variables
    // they are not actually initialized until 'gen_start'
    writeln!(
        out,
        "(global $g/{} (mut {}) {})",
        gvar.name,
        trtype(&gvar.type_),
        trzeroval(&gvar.type_)
    )?;
    Ok(())
}

/// given a type gives the 'zero value expression' for the associated type
/// this is primarily for initializing variables
fn trzeroval(typ: &Type) -> &str {
    match typ {
        Type::F32 => "(f32.const 0)",
        Type::F64 => "(f64.const 0)",
        Type::I64 => "(i64.const 0)",
        Type::I32 | Type::Bool | Type::Str | Type::Record(_) => "(i32.const 0)",
        Type::Id => "(i64.const 0)",
    }
}

fn gen_start(out: &mut String, program: &Program) -> Result<(), Error> {
    // Initialize global variables
    out.push_str("(func $start\n");
    for local in &program.gvar_init_locals {
        out.push_str(&format!(
            "(local $l/{}/{} {})\n",
            local.id,
            local.name,
            trtype(&local.type_)
        ));
    }
    for gvar in &program.globals {
        gen_expr(out, &gvar.init)?;
        out.push_str(&format!("global.set $g/{}\n", gvar.name));
    }
    // TOOD: release all local variables from gvar_init_locals here
    out.push_str(")\n");
    Ok(())
}

fn gen_extern(out: &mut String, ext: &Extern) -> Result<(), Error> {
    out.push_str(&format!(
        "(import \"{}\" \"{}\" (func $f/{}",
        ext.path.0, ext.path.1, ext.name
    ));
    for param in &ext.type_.parameters {
        out.push_str(&format!(" (param {})", trtype(&param.1)));
    }
    out.push_str(&trrtype(&ext.type_.return_type));
    out.push_str("))\n");
    Ok(())
}

/// translate type
fn trtype(type_: &Type) -> &'static str {
    match type_ {
        Type::Bool => "i32",
        Type::I32 => "i32",
        Type::I64 => "i64",
        Type::F32 => "f32",
        Type::F64 => "f64",
        Type::Str => "i32",
        Type::Record(_) => "i32",
        Type::Id => "i64",
    }
}

/// translate return type
fn trrtype(type_: &ReturnType) -> String {
    match type_ {
        ReturnType::Type(t) => format!(" (result {})", trtype(t)),
        _ => "".into(),
    }
}

fn gen_func(out: &mut String, func: &Func) -> Result<(), Error> {
    out.push_str(&format!("(func $f/{}", func.name));
    for param in func.parameters.borrow().iter() {
        out.push_str(&format!(
            " (param $l/{}/{} {})",
            param.id,
            param.name,
            trtype(&param.type_)
        ));
    }
    out.push_str(&trrtype(&func.type_.return_type));
    out.push_str("\n");

    // declare the local variables, skipping parameters
    for local in func
        .locals
        .borrow()
        .iter()
        .skip(func.parameters.borrow().len())
    {
        out.push_str(&format!(
            "(local $l/{}/{} {})\n",
            local.id,
            local.name,
            trtype(&local.type_)
        ));
    }

    out.push_str("(block $ret");
    out.push_str(&trrtype(&func.type_.return_type));
    out.push_str("\n");

    gen_stmt(out, func.body.borrow().as_ref().unwrap())?;

    out.push_str(")\n");
    // release all local variables here (including parameters)
    for local in func.locals.borrow().iter() {
        release_var(out, &Variable::Local(local.clone()));
    }
    out.push_str(")\n");
    Ok(())
}

fn gen_stmt(out: &mut String, stmt: &Stmt) -> Result<(), Error> {
    match &stmt.data {
        StmtData::Block(stmts) => {
            for stmt in stmts {
                gen_stmt(out, stmt)?;
            }
        }
        StmtData::Return(expr) => {
            gen_expr(out, expr)?;
            out.push_str("br $ret\n");
        }
        StmtData::Expr(expr) => {
            gen_expr(out, expr)?;
            assert_eq!(expr.type_, ReturnType::Void);
        }
    }
    Ok(())
}

fn gen_expr(out: &mut String, expr: &Expr) -> Result<(), Error> {
    match &expr.data {
        ExprData::Void => {}
        ExprData::Bool(b) => out.push_str(&format!("i32.const {}\n", if *b { 1 } else { 0 })),
        ExprData::I32(x) => out.push_str(&format!("i32.const {}\n", x)),
        ExprData::I64(x) => out.push_str(&format!("i64.const {}\n", x)),
        ExprData::F32(x) => out.push_str(&format!("f32.const {}\n", x)),
        ExprData::F64(x) => out.push_str(&format!("f64.const {}\n", x)),
        ExprData::Str(ptr) => {
            out.push_str(&format!("i32.const {}\n", ptr.get()));
            out.push_str(&format!("call $f/__retain\n"));
            out.push_str(&format!("i32.const {}\n", ptr.get()));
        }
        ExprData::GetVar(x) => match x.type_() {
            Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 => {
                out.push_str(&format!("{}.get {}\n", x.wasm_kind(), x.wasm_name()));
            }
            Type::Str | Type::Record(_) => {
                out.push_str(&format!("{}.get {}\n", x.wasm_kind(), x.wasm_name()));
                out.push_str("call $f/__retain");
                out.push_str(&format!("{}.get {}\n", x.wasm_kind(), x.wasm_name()));
            }
            Type::Id => panic!("TODO: gen_expr id GetVar (retain)"),
        },
        ExprData::SetVar(x, expr) => match x.type_() {
            Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 => {
                gen_expr(out, expr)?;
                out.push_str(&format!("{}.set {}\n", x.wasm_kind(), x.wasm_name()));
            }
            Type::Str | Type::Record(_) => {
                // save the old value on the stack (for release later)
                out.push_str(&format!("{}.get {}\n", x.wasm_kind(), x.wasm_name()));

                gen_expr(out, expr)?;
                out.push_str(&format!("{}.tee {}\n", x.wasm_kind(), x.wasm_name()));

                // retain the new value
                out.push_str(&format!("call $f/__retain\n"));

                // release the old value
                out.push_str(&format!("call $f/__release\n"));
            }
            Type::Id => panic!("TODO: gen_expr id SetVar (retain + release)"),
        },
        ExprData::AugVar(x, op, expr) => match x.type_() {
            Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 => {
                out.push_str(&format!("{}.get {}\n", x.wasm_kind(), x.wasm_name()));
                gen_expr(out, expr)?;
                out.push_str(&format!("{}\n", op));
                out.push_str(&format!("{}.set {}\n", x.wasm_kind(), x.wasm_name()));
            }
            Type::Str | Type::Record(_) => {
                panic!("TODO: gen_expr record AugLocal (retain + release)")
            }
            Type::Id => panic!("TODO: gen_expr id AugVar (retain + release)"),
        },
        ExprData::CallFunc(func, args) => {
            for arg in args {
                gen_expr(out, arg)?;
            }
            out.push_str(&format!("call $f/{}\n", func.name));
        }
        ExprData::CallExtern(ext, args) => {
            for arg in args {
                gen_expr(out, arg)?;
            }
            out.push_str(&format!("call $f/{}\n", ext.name));
        }
        ExprData::Op(op, args) => {
            for arg in args {
                gen_expr(out, arg)?;
            }
            out.push_str(&format!("{}\n", op));
        }
        ExprData::DropPrimitive(x) => {
            gen_expr(out, x)?;
            out.push_str("drop\n");
        }
        ExprData::Asm(args, _, code) => {
            for arg in args {
                gen_expr(out, arg)?;
            }
            out.push_str(code);
            out.push('\n');
        }
        ExprData::Read1(addr, offset) => {
            gen_expr(out, addr)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.load8_u{}\n", offset));
        }
        ExprData::Read2(addr, offset) => {
            gen_expr(out, addr)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.load16_u{}\n", offset));
        }
        ExprData::Read4(addr, offset) => {
            gen_expr(out, addr)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.load{}\n", offset));
        }
        ExprData::Read8(addr, offset) => {
            gen_expr(out, addr)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i64.load{}\n", offset));
        }
        ExprData::Write1(addr, data, offset) => {
            gen_expr(out, addr)?;
            gen_expr(out, data)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.store8{}\n", offset));
        }
        ExprData::Write2(addr, data, offset) => {
            gen_expr(out, addr)?;
            gen_expr(out, data)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.store16{}\n", offset));
        }
        ExprData::Write4(addr, data, offset) => {
            gen_expr(out, addr)?;
            gen_expr(out, data)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i32.store{}\n", offset));
        }
        ExprData::Write8(addr, data, offset) => {
            gen_expr(out, addr)?;
            gen_expr(out, data)?;
            let offset = if *offset != 0 {
                format!(" offset={}", offset)
            } else {
                "".to_owned()
            };
            out.push_str(&format!("i64.store{}\n", offset));
        }
    }
    Ok(())
}

fn release_var(out: &mut String, var: &Variable) {
    let type_ = var.type_();
    match type_.retain_type() {
        RetainType::Primitive => {}
        RetainType::Typed => {
            out.push_str(&format!("{}.get {}\n", var.wasm_kind(), var.wasm_name()));
            out.push_str(&format!("call $f/__release\n"));
        }
        RetainType::Id => panic!("TODO: release_var id"),
    }
}

use crate::ir::*;
use crate::parse_file;
use crate::Binop;
use crate::Error;
use crate::Parser;
use crate::SSpan;
use crate::Sink;
use crate::Source;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::fmt;

pub const PAGE_SIZE: usize = 65536;

/// Number of bytes at start of memory that's reserved
/// Compile-time constants stored in memory start from this location
pub const RESERVED_BYTES: usize = 2048;

/// translates a list of (filename, wac-code) pairs into
/// a wat webassembly module
pub fn translate(mut sources: Vec<(Rc<str>, Rc<str>)>) -> Result<String, Error> {
    let prelude = vec![
        ("[prelude:lang]".into(), crate::prelude::LANG.into()),
        ("[prelude:malloc]".into(), crate::prelude::MALLOC.into()),
        ("[prelude:str]".into(), crate::prelude::STR.into()),
        ("[prelude:id]".into(), crate::prelude::ID.into()),
        ("[prelude:list]".into(), crate::prelude::LIST.into()),
        ("[prelude:type]".into(), crate::prelude::TYPE.into()),
    ];

    sources.splice(0..0, prelude);
    let mut files = Vec::new();
    for (filename, data) in sources {
        let source = Rc::new(Source {
            name: filename.clone(),
            data: data.clone(),
        });
        let mut parser = match Parser::new(&source) {
            Ok(parser) => parser,
            Err(error) => return Err(Error::from_lex(source.clone(), error)),
        };
        let file = parse_file(&mut parser)?;
        files.push((filename, file));
    }
    let mut out = Out::new();

    // some universal constants
    // we provide these both as 'const' and as wasm globals because
    // from inside wac normally, constants may be preferrable,
    // but from inside asm blocks, it's not possible to use const values
    out.gvars.writeln(format!("(global $rt_tag_i32  i32 (i32.const {}))", TAG_I32));
    out.gvars.writeln(format!("(global $rt_tag_i64  i32 (i32.const {}))", TAG_I64));
    out.gvars.writeln(format!("(global $rt_tag_f32  i32 (i32.const {}))", TAG_F32));
    out.gvars.writeln(format!("(global $rt_tag_f64  i32 (i32.const {}))", TAG_F64));
    out.gvars.writeln(format!("(global $rt_tag_bool i32 (i32.const {}))", TAG_BOOL));
    out.gvars.writeln(format!("(global $rt_tag_type i32 (i32.const {}))", TAG_TYPE));
    out.gvars.writeln(format!("(global $rt_tag_str  i32 (i32.const {}))", TAG_STRING));
    out.gvars.writeln(format!("(global $rt_tag_list i32 (i32.const {}))", TAG_LIST));
    out.gvars.writeln(format!("(global $rt_tag_id   i32 (i32.const {}))", TAG_ID));

    let mut functions = HashMap::new();

    // collect all function signatures
    for (_filename, file) in &files {
        for imp in &file.imports {
            match imp {
                Import::Function(FunctionImport { alias, type_, .. }) => {
                    functions.insert(alias.clone(), type_.clone());
                }
            }
        }
        for func in &file.functions {
            functions.insert(func.name.clone(), func.type_().clone());
        }
    }
    let mut gscope = GlobalScope::new(functions);

    // [just take the first span in prelude:lang imports, and use that
    // for any builtin thing with no good corresponding location in wac source]
    let void_span = files[0].1.imports[0].span().clone();

    // prepare the special type constants
    // these could be in the source directly, but it would make it harder to keep
    // both the rust and wac code in sync.
    gscope.decl_const(void_span.clone(), "i32".into(), ConstValue::Type(Type::I32))?;
    gscope.decl_const(void_span.clone(), "i64".into(), ConstValue::Type(Type::I64))?;
    gscope.decl_const(void_span.clone(), "f32".into(), ConstValue::Type(Type::F32))?;
    gscope.decl_const(void_span.clone(), "f64".into(), ConstValue::Type(Type::F64))?;
    gscope.decl_const(void_span.clone(), "bool".into(), ConstValue::Type(Type::Bool))?;
    gscope.decl_const(void_span.clone(), "type".into(), ConstValue::Type(Type::Type))?;
    gscope.decl_const(void_span.clone(), "str".into(), ConstValue::Type(Type::String))?;
    gscope.decl_const(void_span.clone(), "list".into(), ConstValue::Type(Type::List))?;
    gscope.decl_const(void_span.clone(), "id".into(), ConstValue::Type(Type::Id))?;

    // prepare all constants
    for (_filename, file) in &files {
        for c in &file.constants {
            gscope.decl_const(c.span.clone(), c.name.clone(), c.value.clone())?;
        }
    }

    // translate all global variables
    // NOTE: global variables that appear before cannot refer to
    // global variables that appear later
    // NOTE: it kinda sucks that the behavior of the code will depend on the
    // order in which you provide the files
    for (_filename, file) in &files {
        for gvar in &file.globalvars {
            let mut lscope = LocalScope::new(&gscope);
            let type_ = if let Some(t) = gvar.type_ {
                t
            } else {
                guess_type(&mut lscope, &gvar.init)?
            };
            let init_sink = out.start.spawn();
            translate_expr(&mut out, &init_sink, &mut lscope, Some(type_), &gvar.init)?;
            let info = gscope.decl_gvar(gvar.span.clone(), gvar.name.clone(), type_)?;
            init_sink.writeln(format!("global.set {}", info.wasm_name));
            out.gvars.writeln(format!(
                "(global {} (mut {}) ({}.const 0))",
                info.wasm_name,
                translate_type(info.type_),
                translate_type(info.type_),
            ));
        }
    }

    // translate the functions
    for (_filename, file) in files {
        for imp in file.imports {
            translate_import(&out, imp);
        }
        for func in file.functions {
            translate_func(&mut out, &gscope, func)?;
        }
    }
    Ok(out.get())
}

struct GlobalScope {
    functions: HashMap<Rc<str>, FunctionType>,
    varmap: HashMap<Rc<str>, ScopeEntry>,
    decls: Vec<Rc<GlobalVarInfo>>,
}

impl GlobalScope {
    fn new(functions: HashMap<Rc<str>, FunctionType>) -> Self {
        Self {
            functions,
            varmap: HashMap::new(),
            decls: vec![],
        }
    }

    fn decl_const(
        &mut self,
        span: SSpan,
        name: Rc<str>,
        cval: ConstValue,
    ) -> Result<Rc<ConstantInfo>, Error> {
        if let Some(info) = self.varmap.get(&name) {
            return Err(Error::ConflictingDefinitions {
                span1: info.span().clone(),
                span2: span,
                name,
            });
        }
        let info = Rc::new(ConstantInfo {
            span,
            name: name.clone(),
            value: cval,
        });
        self.varmap.insert(name.clone(), ScopeEntry::Constant(info.clone()));
        Ok(info)
    }

    fn decl_gvar(
        &mut self,
        span: SSpan,
        name: Rc<str>,
        type_: Type,
    ) -> Result<Rc<GlobalVarInfo>, Error> {
        if let Some(info) = self.varmap.get(&name) {
            return Err(Error::ConflictingDefinitions {
                span1: info.span().clone(),
                span2: span,
                name,
            });
        }
        let wasm_name = format!("$g_{}", name).into();
        let info = Rc::new(GlobalVarInfo {
            span,
            original_name: name.clone(),
            type_,
            wasm_name,
        });
        self.decls.push(info.clone());
        self.varmap.insert(name.clone(), ScopeEntry::Global(info.clone()));
        Ok(info)
    }
}

struct ConstantInfo {
    span: SSpan,
    #[allow(dead_code)]
    name: Rc<str>,
    value: ConstValue,
}

/// global variable declaration
struct GlobalVarInfo {
    #[allow(dead_code)]
    span: SSpan,
    #[allow(dead_code)]
    original_name: Rc<str>,
    type_: Type,
    wasm_name: Rc<str>,
}

/// local variable declaration
struct LocalVarInfo {
    #[allow(dead_code)]
    span: SSpan,

    /// the programmer provided name for this variable
    original_name: Rc<str>,

    type_: Type,

    wasm_name: Rc<str>,
}

struct LocalScope<'a> {
    g: &'a GlobalScope,
    locals: Vec<HashMap<Rc<str>, Rc<LocalVarInfo>>>,
    nlabels: usize,
    continue_labels: Vec<u32>,
    break_labels: Vec<u32>,
    decls: Vec<Rc<LocalVarInfo>>,

    /// local variables not directly created by the end-user
    /// but by the system as needed
    helper_locals: HashMap<Rc<str>, Type>,
}

impl<'a> LocalScope<'a> {
    fn new(g: &'a GlobalScope) -> Self {
        Self {
            g,
            locals: vec![HashMap::new()],
            nlabels: 0,
            continue_labels: vec![],
            break_labels: vec![],
            decls: vec![],
            helper_locals: HashMap::new(),
        }
    }
    fn helper(&mut self, name: &str, type_: Type) {
        assert!(name.starts_with("$rt_"));
        if let Some(old_type) = self.helper_locals.get(name) {
            assert_eq!(*old_type, type_);
        }
        self.helper_locals.insert(name.into(), type_);
    }
    fn push(&mut self) {
        self.locals.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.locals.pop().unwrap();
    }
    fn decl(&mut self, span: SSpan, original_name: Rc<str>, type_: Type) -> Rc<LocalVarInfo> {
        let id = self.decls.len();
        let wasm_name = format!("$l_{}_{}", id, original_name).into();
        let info = Rc::new(LocalVarInfo {
            span,
            original_name,
            type_,
            wasm_name,
        });
        self.decls.push(info.clone());
        self.locals
            .last_mut()
            .unwrap()
            .insert(info.original_name.clone(), info.clone());
        info
    }
    fn get(&self, name: &Rc<str>) -> Option<ScopeEntry> {
        for map in self.locals.iter().rev() {
            match map.get(name) {
                Some(t) => return Some(ScopeEntry::Local(t.clone())),
                None => {}
            }
        }
        match self.g.varmap.get(name) {
            Some(t) => Some(t.clone()),
            None => None,
        }
    }
    fn get_or_err(&self, span: SSpan, name: &Rc<str>) -> Result<ScopeEntry, Error> {
        match self.get(name) {
            Some(e) => Ok(e),
            None => Err(Error::Type {
                span,
                expected: format!("Variable {}", name),
                got: "NotFound".into(),
            }),
        }
    }
    fn getf(&self, name: &Rc<str>) -> Option<FunctionType> {
        self.g.functions.get(name).cloned()
    }
    fn getf_or_err(&self, span: SSpan, name: &Rc<str>) -> Result<FunctionType, Error> {
        match self.getf(name) {
            Some(e) => Ok(e),
            None => Err(Error::Type {
                span,
                expected: format!("Function {}", name),
                got: "NotFound".into(),
            }),
        }
    }
    fn new_label_id(&mut self) -> u32 {
        let id = self.nlabels as u32;
        self.nlabels += 1;
        id
    }
}

#[derive(Clone)]
enum ScopeEntry {
    Local(Rc<LocalVarInfo>),
    Global(Rc<GlobalVarInfo>),
    Constant(Rc<ConstantInfo>),
}

impl ScopeEntry {
    fn span(&self) -> &SSpan {
        match self {
            ScopeEntry::Local(info) => &info.span,
            ScopeEntry::Global(info) => &info.span,
            ScopeEntry::Constant(info) => &info.span,
        }
    }
}

fn translate_func_type(ft: &FunctionType) -> String {
    let mut ret = String::new();
    let FunctionType {
        return_type,
        parameter_types,
    } = ft;
    for pt in parameter_types {
        ret.push_str(&format!(" (param {})", translate_type(*pt)));
    }
    if let Some(rt) = return_type {
        ret.push_str(&format!(" (result {})", translate_type(*rt)));
    }
    ret
}

fn translate_type(t: Type) -> &'static str {
    translate_wasm_type(t.wasm())
}

fn translate_wasm_type(wt: WasmType) -> &'static str {
    match wt {
        WasmType::I32 => "i32",
        WasmType::I64 => "i64",
        WasmType::F32 => "f32",
        WasmType::F64 => "f64",
    }
}

fn translate_import(out: &Out, imp: Import) {
    match imp {
        Import::Function(FunctionImport {
            span: _,
            module_name,
            function_name,
            alias,
            type_,
        }) => {
            out.imports.writeln(format!(
                r#"(import "{}" "{}" (func $f_{} {}))"#,
                module_name,
                function_name,
                alias,
                translate_func_type(&type_),
            ));
        }
    }
}

fn translate_func(out: &mut Out, gscope: &GlobalScope, func: Function) -> Result<(), Error> {
    let mut lscope = LocalScope::new(gscope);

    match func.visibility {
        Visibility::Public => {
            out.exports.writeln(format!(
                r#"(export "f_{}" (func $f_{}))"#,
                func.name, func.name
            ));
        }
        Visibility::Private => {}
    }

    let sink = out.funcs.spawn();
    sink.writeln(format!("(func $f_{}", func.name));

    for parameter in &func.parameters {
        let info = lscope.decl(func.span.clone(), parameter.0.clone(), parameter.1);
        sink.writeln(format!(
            " (param {} {})",
            info.wasm_name,
            translate_type(info.type_)
        ));
    }
    if let Some(return_type) = func.return_type {
        sink.writeln(format!(" (result {})", translate_type(return_type)));
    }
    // we won't know what locals we have until we finish translate_expr on the body
    let locals_sink = sink.spawn();
    let locals_init = sink.spawn();
    translate_expr(out, &sink, &mut lscope, func.return_type, &func.body)?;
    let epilogue = sink.spawn();
    sink.writeln(")");

    // special local variables used by some operations
    // temporary variable for duplicating values on TOS
    let mut helper_locals: Vec<(_, _)> = lscope.helper_locals.into_iter().collect();
    helper_locals.sort_by(|a, b| a.0.cmp(&b.0));
    for (wasm_name, type_) in helper_locals {
        locals_sink.writeln(format!("(local {} {})", wasm_name, translate_type(type_)));
        release_var(&epilogue, Scope::Local, &wasm_name, type_);
    }

    // declare all the local variables (skipping parameters)
    for info in lscope.decls.iter().skip(func.parameters.len()) {
        locals_sink.writeln(format!(
            " (local {} {})",
            info.wasm_name,
            translate_type(info.type_)
        ));
        locals_init.writeln(format!(
            "(local.set {} ({}.const 0))",
            info.wasm_name,
            translate_type(info.type_)
        ));
    }

    // Make sure to release all local variables, even the parameters
    for info in lscope.decls {
        release_var(&epilogue, Scope::Local, &info.wasm_name, info.type_);
    }
    Ok(())
}

fn translate_expr(
    out: &mut Out,
    sink: &Rc<Sink>,
    lscope: &mut LocalScope,
    etype: Option<Type>,
    expr: &Expr,
) -> Result<(), Error> {
    match expr {
        Expr::Bool(span, x) => {
            match etype {
                Some(Type::Bool) => {
                    sink.writeln(format!("(i32.const {})", if *x { 1 } else { 0 }));
                }
                Some(Type::Id) => {
                    sink.writeln(format!("(i32.const {})", if *x { 1 } else { 0 }));
                    cast_to_id(sink, TAG_BOOL);
                }
                Some(t) => {
                    return Err(Error::Type {
                        span: span.clone(),
                        expected: format!("{:?}", t),
                        got: "Bool".into(),
                    })
                }
                None => {
                    // no-op value is dropped
                }
            }
        }
        Expr::Int(span, x) => {
            match etype {
                Some(Type::I32) => {
                    sink.writeln(format!("(i32.const {})", x));
                }
                Some(Type::I64) => {
                    sink.writeln(format!("(i64.const {})", x));
                }
                Some(Type::F32) => {
                    sink.writeln(format!("(f32.const {})", x));
                }
                Some(Type::F64) => {
                    sink.writeln(format!("(f64.const {})", x));
                }
                Some(Type::Id) => {
                    sink.writeln(format!("(i32.const {})", x));
                    cast_to_id(sink, TAG_I32);
                }
                Some(t) => {
                    return Err(Error::Type {
                        span: span.clone(),
                        expected: format!("{:?}", t),
                        got: "Int".into(),
                    })
                }
                None => {
                    // no-op value is dropped
                }
            }
        }
        Expr::Float(span, x) => {
            match etype {
                Some(Type::F32) => {
                    sink.writeln(format!("(f32.const {})", x));
                }
                Some(Type::F64) => {
                    sink.writeln(format!("(f64.const {})", x));
                }
                Some(Type::Id) => {
                    sink.writeln(format!("(f32.const {})", x));
                    sink.writeln("i32.reinterpret_f32");
                    cast_to_id(sink, TAG_F32);
                }
                Some(t) => {
                    return Err(Error::Type {
                        span: span.clone(),
                        expected: format!("{:?}", t),
                        got: "Float".into(),
                    })
                }
                None => {
                    // no-op value is dropped
                }
            }
        }
        Expr::String(span, value) => {
            match etype {
                Some(t) => {
                    let ptr = out.intern_str(value);
                    sink.writeln(format!("(i32.const {})", ptr));
                    retain(lscope, sink, Type::String, DropPolicy::Keep);
                    auto_cast(sink, span, lscope, Some(Type::String), Some(t))?;
                }
                None => {
                    // no-op value is dropped
                }
            }
        }
        Expr::List(span, exprs) => {
            sink.writeln("call $f___new_list");
            for expr in exprs {
                raw_dup(lscope, sink, WasmType::I32);
                translate_expr(out, sink, lscope, Some(Type::Id), expr)?;
                sink.writeln("call $f___list_push_raw_no_retain");
            }
            auto_cast(sink, span, lscope, Some(Type::List), etype)?;
        }
        Expr::Block(span, exprs) => {
            if let Some(last) = exprs.last() {
                lscope.push();

                for expr in &exprs[..exprs.len() - 1] {
                    translate_expr(out, sink, lscope, None, expr)?;
                }
                translate_expr(out, sink, lscope, etype, last)?;

                lscope.pop();
            } else {
                match etype {
                    None => {}
                    Some(t) => {
                        return Err(Error::Type {
                            span: span.clone(),
                            expected: format!("{:?}", t),
                            got: "Void (empty-block)".into(),
                        })
                    }
                }
            }
        }
        Expr::GetVar(span, name) => {
            let entry = lscope.get_or_err(span.clone(), name)?;
            match entry {
                ScopeEntry::Local(info) => {
                    match etype {
                        Some(etype) => {
                            sink.writeln(format!("local.get {}", info.wasm_name));
                            retain(lscope, sink, info.type_, DropPolicy::Keep);
                            auto_cast(sink, span, lscope, Some(info.type_), Some(etype))?;
                        }
                        None => {
                            // we already checked this variable exists,
                            // if we don't use the return value,
                            // there's nothing we need to do here
                        }
                    }
                }
                ScopeEntry::Global(info) => {
                    match etype {
                        Some(etype) => {
                            sink.writeln(format!("global.get {}", info.wasm_name));
                            retain(lscope, sink, info.type_, DropPolicy::Keep);
                            auto_cast(sink, span, lscope, Some(info.type_), Some(etype))?;
                        }
                        None => {
                            // we already checked this variable exists,
                            // if we don't use the return value,
                            // there's nothing we need to do here
                        }
                    }
                }
                ScopeEntry::Constant(info) => {
                    match &info.value {
                        ConstValue::I32(x) => {
                            sink.writeln(format!("i32.const {}", x));
                        }
                        ConstValue::Type(t) => {
                            sink.writeln(format!("i32.const {}", t.tag()));
                        }
                    }
                }
            }
        }
        Expr::SetVar(span, name, setexpr) => {
            let entry = lscope.get_or_err(span.clone(), name)?;
            if let Some(etype) = etype {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: format!("{:?}", etype),
                    got: "Void (setvar)".into(),
                });
            }
            match entry {
                ScopeEntry::Local(info) => match etype {
                    Some(etype) => {
                        return Err(Error::Type {
                            span: span.clone(),
                            expected: format!("{:?}", etype),
                            got: "Void (local.setvar)".into(),
                        })
                    }
                    None => {
                        // There's no need to retain here, because anything that's currently
                        // on the stack already has a retain on it. By popping from the
                        // stack, we're transferring the retain on the stack into the
                        // variable itself.
                        //
                        // We do however have to release the old value.
                        translate_expr(out, sink, lscope, Some(info.type_), setexpr)?;
                        release_var(sink, Scope::Local, &info.wasm_name, info.type_);
                        sink.writeln(format!("local.set {}", info.wasm_name));
                    }
                },
                ScopeEntry::Global(info) => match etype {
                    Some(etype) => {
                        return Err(Error::Type {
                            span: span.clone(),
                            expected: format!("{:?}", etype),
                            got: "Void (global.setvar)".into(),
                        })
                    }
                    None => {
                        // There's no need to retain here, because anything that's currently
                        // on the stack already has a retain on it. By popping from the
                        // stack, we're transferring the retain on the stack into the
                        // variable itself.
                        //
                        // We do however have to release the old value.
                        translate_expr(out, sink, lscope, Some(info.type_), setexpr)?;
                        release_var(sink, Scope::Global, &info.wasm_name, info.type_);
                        sink.writeln(format!("global.set {}", info.wasm_name));
                    }
                },
                ScopeEntry::Constant(_) => {
                    return Err(Error::Type {
                        span: span.clone(),
                        expected: "variable".into(),
                        got: "constant".into(),
                    })
                }
            }
        }
        Expr::DeclVar(span, name, type_, setexpr) => {
            let type_ = match type_ {
                Some(t) => *t,
                None => guess_type(lscope, setexpr)?,
            };
            let info = lscope.decl(span.clone(), name.clone(), type_);
            if let Some(etype) = etype {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: format!("{:?}", etype),
                    got: "Void (declvar)".into(),
                });
            }
            // There's no need to retain here, because anything that's currently
            // on the stack already have a retain on them. By popping from the
            // stack, we're transferring the retain on the stack into the
            // variable itself.
            translate_expr(out, sink, lscope, Some(type_), setexpr)?;
            sink.writeln(format!("local.set {}", info.wasm_name));
        }
        Expr::FunctionCall(span, fname, argexprs) => {
            let ftype = lscope.getf_or_err(span.clone(), fname)?;
            if argexprs.len() != ftype.parameter_types.len() {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: format!("{} args", ftype.parameter_types.len()),
                    got: format!("{} args", argexprs.len()),
                });
            }
            for (argexpr, ptype) in argexprs.iter().zip(ftype.parameter_types) {
                translate_expr(out, sink, lscope, Some(ptype), argexpr)?;
            }
            sink.writeln(format!("call $f_{}", fname));
            auto_cast(sink, span, lscope, ftype.return_type, etype)?;
        }
        Expr::If(_span, pairs, other) => {
            for (cond, body) in pairs {
                translate_expr(out, sink, lscope, Some(Type::Bool), cond)?;
                sink.writeln("if");
                if let Some(etype) = etype {
                    sink.writeln(format!(" (result {})", translate_type(etype)));
                }
                translate_expr(out, sink, lscope, etype, body)?;
                sink.writeln("else");
            }

            translate_expr(out, sink, lscope, etype, other)?;

            for _ in pairs {
                sink.writeln("end");
            }
        }
        Expr::While(_span, cond, body) => {
            let break_label = lscope.new_label_id();
            let continue_label = lscope.new_label_id();
            lscope.break_labels.push(break_label);
            lscope.continue_labels.push(continue_label);

            sink.writeln(format!(
                "(block $lbl_{} (loop $lbl_{}",
                break_label, continue_label
            ));
            translate_expr(out, sink, lscope, Some(Type::Bool), cond)?;
            sink.writeln("i32.eqz");
            sink.writeln(format!("br_if $lbl_{}", break_label));
            translate_expr(out, sink, lscope, None, body)?;
            sink.writeln(format!("br $lbl_{}", continue_label));
            sink.writeln("))");

            lscope.break_labels.pop();
            lscope.continue_labels.pop();
        }
        Expr::Binop(span, op, left, right) => match op {
            Binop::Less => op_cmp(out, sink, lscope, etype, span, "lt", left, right)?,
            Binop::LessOrEqual => op_cmp(out, sink, lscope, etype, span, "le", left, right)?,
            Binop::Greater => op_cmp(out, sink, lscope, etype, span, "gt", left, right)?,
            Binop::GreaterOrEqual => op_cmp(out, sink, lscope, etype, span, "ge", left, right)?,
            Binop::Is => {
                let left_type = guess_type(lscope, left)?;
                let right_type = guess_type(lscope, right)?;
                let gtype = common_type(lscope, span, left_type, right_type)?;

                // The 'eq' instruction will remove the values from the stack,
                // but they may not actually release the values (if they are supposed
                // to be smart pointers)
                //
                // to ensure it is done properly, we need to explicitly call
                // release() before we actually run the instruction
                translate_expr(out, sink, lscope, Some(gtype), left)?;
                release(lscope, sink, gtype, DropPolicy::Keep);
                translate_expr(out, sink, lscope, Some(gtype), right)?;
                release(lscope, sink, gtype, DropPolicy::Keep);
                sink.writeln(match gtype.wasm() {
                    WasmType::I32 => "i32.eq",
                    WasmType::I64 => "i64.eq",
                    WasmType::F32 => "f32.eq",
                    WasmType::F64 => "f64.eq",
                });
                auto_cast(sink, span, lscope, Some(Type::Bool), etype)?;
            }
            Binop::IsNot => {
                let left_type = guess_type(lscope, left)?;
                let right_type = guess_type(lscope, right)?;
                let gtype = common_type(lscope, span, left_type, right_type)?;

                // The 'ne' instruction will remove the values from the stack,
                // but they may not actually release the values (if they are supposed
                // to be smart pointers)
                //
                // to ensure it is done properly, we need to explicitly call
                // release() before we actually run the instruction
                translate_expr(out, sink, lscope, Some(gtype), left)?;
                release(lscope, sink, gtype, DropPolicy::Keep);
                translate_expr(out, sink, lscope, Some(gtype), right)?;
                release(lscope, sink, gtype, DropPolicy::Keep);
                sink.writeln(match gtype.wasm() {
                    WasmType::I32 => "i32.ne",
                    WasmType::I64 => "i64.ne",
                    WasmType::F32 => "f32.ne",
                    WasmType::F64 => "f64.ne",
                });
                auto_cast(sink, span, lscope, Some(Type::Bool), etype)?;
            }
            Binop::Add => op_arith_binop(out, sink, lscope, etype, span, "add", left, right)?,
            Binop::Subtract => op_arith_binop(out, sink, lscope, etype, span, "sub", left, right)?,
            Binop::Multiply => op_arith_binop(out, sink, lscope, etype, span, "mul", left, right)?,
            Binop::Divide => {
                translate_expr(out, sink, lscope, Some(Type::F32), left)?;
                translate_expr(out, sink, lscope, Some(Type::F32), right)?;
                sink.writeln("f32.div");
                auto_cast(sink, span, lscope, Some(Type::F32), etype)?;
            }
            Binop::TruncDivide => {
                let gltype = guess_type(lscope, left)?;
                let grtype = guess_type(lscope, right)?;
                match (gltype, grtype) {
                    (Type::I32, Type::I32) => {
                        translate_expr(out, sink, lscope, Some(gltype), left)?;
                        translate_expr(out, sink, lscope, Some(grtype), right)?;
                        sink.writeln("i32.div_s");
                    }
                    (Type::F32, Type::I32) | (Type::I32, Type::F32) | (Type::F32, Type::F32) => {
                        translate_expr(out, sink, lscope, Some(Type::F32), left)?;
                        translate_expr(out, sink, lscope, Some(Type::F32), right)?;
                        sink.writeln("f32.div");
                        explicit_cast(sink, span, lscope, Some(Type::F32), Some(Type::I32))?;
                    }
                    _ => {
                        return Err(Error::Type {
                            span: span.clone(),
                            expected: format!("{:?}", etype),
                            got: "Int".into(),
                        })
                    }
                }
                auto_cast(sink, span, lscope, Some(Type::I32), etype)?;
            }
            Binop::BitwiseAnd => {
                op_bitwise_binop(out, sink, lscope, etype, span, "and", left, right)?
            }
            Binop::BitwiseOr => {
                op_bitwise_binop(out, sink, lscope, etype, span, "or", left, right)?
            }
            Binop::BitwiseXor => {
                op_bitwise_binop(out, sink, lscope, etype, span, "xor", left, right)?
            }
            Binop::ShiftLeft => {
                op_bitwise_binop(out, sink, lscope, etype, span, "shl", left, right)?
            }
            Binop::ShiftRight => {
                op_bitwise_binop(out, sink, lscope, etype, span, "shr_u", left, right)?
            }
            _ => panic!("TODO: translate_expr binop {:?}", op),
        },
        Expr::Unop(span, op, expr) => match op {
            Unop::Plus | Unop::Minus => {
                let gtype = guess_type(lscope, expr)?;
                match gtype {
                    Type::F32 | Type::F64 | Type::I32 | Type::I64 => {
                        translate_expr(out, sink, lscope, Some(gtype), expr)?;
                        match op {
                            Unop::Plus => {}
                            Unop::Minus => match gtype {
                                Type::F32 | Type::F64 => {
                                    sink.writeln(format!("({}.neg ", translate_type(gtype)));
                                }
                                Type::I32 | Type::I64 => {
                                    sink.writeln(format!("{}.const -1", translate_type(gtype)));
                                    sink.writeln(format!("{}.mul", translate_type(gtype)));
                                }
                                _ => panic!("Unop gtype {:?}", gtype),
                            },
                            _ => panic!("Unop {:?}", op),
                        }
                    }
                    _ => {
                        return Err(Error::Type {
                            span: span.clone(),
                            expected: "numeric".into(),
                            got: format!("{:?}", gtype),
                        })
                    }
                }
                auto_cast(sink, span, lscope, Some(gtype), etype)?
            }
            Unop::Not => {
                translate_expr(out, sink, lscope, Some(Type::Bool), expr)?;
                sink.writeln("i32.eqz");
            }
        },
        Expr::AssertType(_span, type_, expr) => {
            translate_expr(out, sink, lscope, Some(*type_), expr)?;
        }
        Expr::CString(span, value) => match etype {
            Some(Type::I32) => {
                let ptr = out.intern_cstr(value);
                sink.writeln(format!("i32.const {}", ptr));
            }
            Some(etype) => {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: format!("{:?}", etype),
                    got: "i32 (cstr)".into(),
                })
            }
            None => {}
        },
        Expr::Asm(span, args, type_, asm_code) => {
            for arg in args {
                let argtype = guess_type(lscope, arg)?;
                translate_expr(out, sink, lscope, Some(argtype), arg)?;
            }
            sink.writeln(asm_code);
            auto_cast(sink, span, lscope, *type_, etype)?;
        }
    }
    Ok(())
}

/// drops the TOS given that TOS is the provided type
/// the drop parameter determines if the value will be consumed/dropped or not
fn release(lscope: &mut LocalScope, sink: &Rc<Sink>, type_: Type, dp: DropPolicy) {
    match type_ {
        Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 | Type::Type => {
            match dp {
                DropPolicy::Drop => sink.writeln("drop"),
                DropPolicy::Keep => {}
            }
        }
        Type::String => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I32),
            }
            sink.writeln("call $f___WAC_str_release");
        }
        Type::List => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I32),
            }
            sink.writeln("call $f___WAC_list_release");
        }
        Type::Id => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I64),
            }
            sink.writeln("call $f___WAC_id_release");
        }
    }
}

/// releases a reference in a var
/// overall, should leave the stack unchanged
fn release_var(sink: &Rc<Sink>, scope: Scope, wasm_name: &Rc<str>, type_: Type) {
    match type_ {
        Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 | Type::Type => {}
        Type::String => {
            sink.writeln(format!("{}.get {}", scope, wasm_name));
            sink.writeln("call $f___WAC_str_release");
        }
        Type::List => {
            sink.writeln(format!("{}.get {}", scope, wasm_name));
            sink.writeln("call $f___WAC_list_release");
        }
        Type::Id => {
            sink.writeln(format!("{}.get {}", scope, wasm_name));
            sink.writeln("call $f___WAC_id_release");
        }
    }
}

enum DropPolicy {
    Drop,
    Keep,
}

enum Scope {
    Local,
    Global,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Local => write!(f, "local"),
            Scope::Global => write!(f, "global"),
        }
    }
}

/// retains the TOS value given the provided type
/// the drop parameter determines if the value will be consumed/dropped or not
fn retain(lscope: &mut LocalScope, sink: &Rc<Sink>, type_: Type, dp: DropPolicy) {
    match type_ {
        Type::Bool | Type::I32 | Type::I64 | Type::F32 | Type::F64 | Type::Type => {
            match dp {
                DropPolicy::Drop => sink.writeln("drop"),
                DropPolicy::Keep => {}
            }
        }
        Type::String => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I32),
            }
            sink.writeln("call $f___WAC_str_retain");
        }
        Type::List => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I32),
            }
            sink.writeln("call $f___WAC_list_retain");
        }
        Type::Id => {
            match dp {
                DropPolicy::Drop => {},
                DropPolicy::Keep => raw_dup(lscope, sink, WasmType::I64),
            }
            sink.writeln("call $f___WAC_id_retain");
        }
    }
}

/// Duplicates TOS
/// Requires a WasmType -- this function does not take into account
/// any sort of reference counting
fn raw_dup(lscope: &mut LocalScope, sink: &Rc<Sink>, wasm_type: WasmType) {
    let t = translate_wasm_type(wasm_type);
    let tmpvar = format!("$rt_tmp_dup_{}", t);
    lscope.helper(&tmpvar, wasm_type.wac());
    sink.writeln(format!("local.tee {}", tmpvar));
    sink.writeln(format!("local.get {}", tmpvar));
}

/// Return the most specific shared type between the two types
/// Returns an error if no such type exists
fn common_type(_lscope: &mut LocalScope, span: &SSpan, a: Type, b: Type) -> Result<Type, Error> {
    match (a, b) {
        _ if a == b => Ok(a),
        (Type::I32, Type::F32) | (Type::F32, Type::I32) => Ok(Type::F32),
        _ => Err(Error::Type {
            span: span.clone(),
            expected: format!("{:?}", a),
            got: format!("{:?}", b),
        }),
    }
}

/// util for comparison operators (e.g. LessThan, GreaterThan, etc)
///   * both arguments are always same type
///   * guesses types based on first arg
///   * always returns bool (i32)
///   * signed and unsigend versions for ints (with *_s/_u suffix)
fn op_cmp(
    out: &mut Out,
    sink: &Rc<Sink>,
    lscope: &mut LocalScope,
    etype: Option<Type>,
    span: &SSpan,
    opname: &str,
    left: &Expr,
    right: &Expr,
) -> Result<(), Error> {
    let gtype = guess_type(lscope, left)?;
    translate_expr(out, sink, lscope, Some(gtype), left)?;
    translate_expr(out, sink, lscope, Some(gtype), right)?;
    match gtype {
        Type::Bool | Type::I32 | Type::I64 => {
            sink.writeln(format!("{}.{}_s", translate_type(gtype), opname));
        }
        Type::F32 | Type::F64 => {
            sink.writeln(format!("{}.{}", translate_type(gtype), opname));
        }
        Type::Type => panic!("TODO: Type comparisons not yet supported"),
        Type::String => panic!("TODO: String comparisons not yet supported"),
        Type::List => panic!("TODO: List comparisons not yet supported"),
        Type::Id => panic!("TODO: Id comparisons not yet supported"),
    }
    auto_cast(sink, span, lscope, Some(Type::Bool), etype)?;
    Ok(())
}

/// util for binary arithmetic operators (e.g. Add, Subtract, etc)
///   * both arguments are always same type
///   * guesses types based on first arg
///   * always returns argument type
///   * not split by sign
fn op_arith_binop(
    out: &mut Out,
    sink: &Rc<Sink>,
    lscope: &mut LocalScope,
    etype: Option<Type>,
    span: &SSpan,
    opname: &str,
    left: &Expr,
    right: &Expr,
) -> Result<(), Error> {
    let gtype = guess_type(lscope, left)?;
    translate_expr(out, sink, lscope, Some(gtype), left)?;
    translate_expr(out, sink, lscope, Some(gtype), right)?;
    match gtype {
        Type::I32 | Type::I64 | Type::F32 | Type::F64 => {
            sink.writeln(format!("{}.{}", translate_type(gtype), opname));
        }
        _ => {
            return Err(Error::Type {
                span: span.clone(),
                expected: "numeric value".into(),
                got: format!("{:?}", gtype),
            });
        }
    }
    auto_cast(sink, span, lscope, Some(gtype), etype)?;
    Ok(())
}

/// util for binary bitwise operators
///   * both arguments are always i32
///   * always returns i32
fn op_bitwise_binop(
    out: &mut Out,
    sink: &Rc<Sink>,
    lscope: &mut LocalScope,
    etype: Option<Type>,
    span: &SSpan,
    opname: &str,
    left: &Expr,
    right: &Expr,
) -> Result<(), Error> {
    translate_expr(out, sink, lscope, Some(Type::I32), left)?;
    translate_expr(out, sink, lscope, Some(Type::I32), right)?;
    sink.writeln(format!("i32.{}", opname));
    auto_cast(sink, span, lscope, Some(Type::I32), etype)?;
    Ok(())
}

/// adds opcodes to convert an i32 type to an 'id'
fn cast_to_id(sink: &Rc<Sink>, tag: i32) {
    sink.writeln("i64.extend_i32_u");
    sink.writeln(format!("i64.const {}", tag));
    sink.writeln("i64.const 32");
    sink.writeln("i64.shl");
    sink.writeln("i64.or");
}

/// perform a cast of TOS from src to dst for when implicitly needed
fn auto_cast(
    sink: &Rc<Sink>,
    span: &SSpan,
    lscope: &mut LocalScope,
    src: Option<Type>,
    dst: Option<Type>,
) -> Result<(), Error> {
    match (src, dst) {
        (Some(src), Some(dst)) if src == dst => {}
        (None, None) => {}
        (Some(Type::I32), Some(Type::F32)) => {
            sink.writeln("f32.convert_i32_s");
        }
        (Some(Type::I32), Some(Type::Id)) => {
            cast_to_id(sink, TAG_I32);
        }
        (Some(Type::F32), Some(Type::Id)) => {
            sink.writeln("i32.reinterpret_f32");
            cast_to_id(sink, TAG_F32);
        }
        (Some(Type::Bool), Some(Type::Id)) => {
            cast_to_id(sink, TAG_BOOL);
        }
        (Some(Type::String), Some(Type::Id)) => {
            cast_to_id(sink, TAG_STRING);
        }
        (Some(Type::List), Some(Type::Id)) => {
            cast_to_id(sink, TAG_LIST);
        }
        (Some(Type::Id), Some(Type::I32)) => {
            sink.writeln("call $f___WAC_raw_id_to_i32");
        }
        (Some(Type::Id), Some(Type::F32)) => {
            sink.writeln("call $f___WAC_raw_id_to_f32");
        }
        (Some(Type::Id), Some(Type::Bool)) => {
            sink.writeln("call $f___WAC_raw_id_to_bool");
        }
        (Some(Type::Id), Some(Type::String)) => {
            sink.writeln("call $f___WAC_raw_id_to_str");
        }
        (Some(Type::Id), Some(Type::List)) => {
            sink.writeln("call $f___WAC_raw_id_to_list");
        }
        (Some(src), None) => {
            release(lscope, sink, src, DropPolicy::Drop);
        }
        (Some(src), Some(dst)) => {
            return Err(Error::Type {
                span: span.clone(),
                expected: format!("{:?}", dst),
                got: format!("{:?}", src),
            });
        }
        (None, Some(dst)) => {
            return Err(Error::Type {
                span: span.clone(),
                expected: format!("{:?}", dst),
                got: "Void".into(),
            });
        }
    }
    Ok(())
}

/// perform a cast of TOS from src to dst for when explicitly requested
/// "stronger" than auto_cast
fn explicit_cast(
    sink: &Rc<Sink>,
    span: &SSpan,
    lscope: &mut LocalScope,
    src: Option<Type>,
    dst: Option<Type>,
) -> Result<(), Error> {
    match (src, dst) {
        (Some(Type::F32), Some(Type::I32)) => {
            sink.writeln("i32.trunc_f32_s");
        }
        _ => auto_cast(sink, span, lscope, src, dst)?,
    }
    Ok(())
}

/// tries to guess the type of an expression that must return some value
/// returning void will cause an error to be returned
fn guess_type(lscope: &mut LocalScope, expr: &Expr) -> Result<Type, Error> {
    match expr {
        Expr::Bool(..) => Ok(Type::Bool),
        Expr::Int(..) => Ok(Type::I32),
        Expr::Float(..) => Ok(Type::F32),
        Expr::String(..) => Ok(Type::String),
        Expr::List(..) => Ok(Type::List),
        Expr::GetVar(span, name) => match lscope.get_or_err(span.clone(), name)? {
            ScopeEntry::Local(info) => Ok(info.type_),
            ScopeEntry::Global(info) => Ok(info.type_),
            ScopeEntry::Constant(info) => Ok(info.value.type_()),
        },
        Expr::SetVar(span, ..) => Err(Error::Type {
            span: span.clone(),
            expected: "any-value".into(),
            got: "Void (setvar)".into(),
        }),
        Expr::DeclVar(span, ..) => Err(Error::Type {
            span: span.clone(),
            expected: "any-value".into(),
            got: "Void (declvar)".into(),
        }),
        Expr::Block(span, exprs) => match exprs.last() {
            Some(last) => guess_type(lscope, last),
            None => {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: "any-value".into(),
                    got: "Void (empty-block)".into(),
                })
            }
        },
        Expr::FunctionCall(span, name, _) => {
            match lscope.getf_or_err(span.clone(), name)?.return_type {
                Some(t) => Ok(t),
                None => {
                    return Err(Error::Type {
                        span: span.clone(),
                        expected: "any-value".into(),
                        got: "Void (void-returning-function)".into(),
                    })
                }
            }
        }
        Expr::If(_, pairs, other) => guess_type(lscope, pairs.get(0).map(|p| &p.1).unwrap_or(other)),
        Expr::While(span, ..) => Err(Error::Type {
            span: span.clone(),
            expected: "any-value".into(),
            got: "Void (while)".into(),
        }),
        Expr::Binop(span, op, left, right) => match op {
            Binop::Add | Binop::Subtract | Binop::Multiply => {
                let a = guess_type(lscope, left)?;
                let b = guess_type(lscope, right)?;
                common_type(lscope, span, a, b)
            }
            Binop::Divide => Ok(Type::F32),
            Binop::TruncDivide | Binop::Remainder => Ok(Type::I32),
            Binop::BitwiseAnd
            | Binop::BitwiseOr
            | Binop::BitwiseXor
            | Binop::ShiftLeft
            | Binop::ShiftRight => Ok(Type::I32),
            Binop::Less
            | Binop::LessOrEqual
            | Binop::Greater
            | Binop::GreaterOrEqual
            | Binop::Equal
            | Binop::NotEqual
            | Binop::Is
            | Binop::IsNot => Ok(Type::Bool),
        },
        Expr::Unop(_span, op, expr) => match op {
            Unop::Minus | Unop::Plus => guess_type(lscope, expr),
            Unop::Not => Ok(Type::Bool),
        },
        Expr::AssertType(_, type_, _) => Ok(*type_),
        Expr::CString(..) => {
            // Should return a pointer
            Ok(Type::I32)
        }
        Expr::Asm(span, _, type_, _) => match type_ {
            Some(t) => Ok(*t),
            None => {
                return Err(Error::Type {
                    span: span.clone(),
                    expected: "any-value".into(),
                    got: "Void (void-asm-expr)".into(),
                })
            }
        },
    }
}

struct Out {
    main: Rc<Sink>,
    imports: Rc<Sink>,
    memory: Rc<Sink>,
    data: Rc<Sink>,
    gvars: Rc<Sink>,
    funcs: Rc<Sink>,
    start: Rc<Sink>,
    exports: Rc<Sink>,

    data_len: Cell<usize>,
    intern_cstr_map: HashMap<Rc<str>, u32>,
    intern_str_map: HashMap<Rc<str>, u32>,
}

impl Out {
    fn new() -> Self {
        let main = Sink::new();
        let imports = main.spawn();
        let memory = main.spawn();
        let data = main.spawn();
        let gvars = main.spawn();
        let funcs = main.spawn();
        main.write(crate::wfs::CODE);
        main.writeln("(func $__rt_start");
        let start = main.spawn();
        main.writeln(")");
        main.writeln("(start $__rt_start)");
        let exports = main.spawn();
        Self {
            main,
            imports,
            memory,
            data,
            gvars,
            funcs,
            start,
            exports,
            data_len: Cell::new(RESERVED_BYTES),
            intern_cstr_map: HashMap::new(),
            intern_str_map: HashMap::new(),
        }
    }

    fn get(self) -> String {
        let len = self.data_len.get();
        let page_len = (len + (PAGE_SIZE - 1)) / PAGE_SIZE;
        self.memory
            .writeln(format!("(memory $rt_mem {})", page_len));
        self.gvars
            .writeln(format!("(global $rt_heap_start i32 (i32.const {}))", len,));
        self.main.get()
    }

    fn data(&self, data: &[u8]) -> u32 {
        // data is reserved with 16-byte alignment
        let reserve_len = (data.len() + 16 - 1) / 16 * 16;
        let ptr = self.data_len.get();
        self.data_len.set(reserve_len + ptr);
        self.data.write(format!("(data (i32.const {}) \"", ptr));
        for byte in data {
            self.data.write(format!("\\{:0>2X}", byte));
        }
        self.data.writeln("\")");
        ptr as u32
    }

    fn intern_cstr(&mut self, s: &Rc<str>) -> u32 {
        if !self.intern_cstr_map.contains_key(s) {
            let mut buffer = s.as_bytes().to_vec();
            buffer.push(0);
            let ptr = self.data(&buffer);
            self.intern_cstr_map.insert(s.clone(), ptr);
        }
        *self.intern_cstr_map.get(s).unwrap()
    }

    fn intern_str(&mut self, s: &Rc<str>) -> u32 {
        if !self.intern_str_map.contains_key(s) {
            let mut buffer = Vec::<u8>::new();
            // refcnt
            buffer.extend(&1i32.to_le_bytes());
            // len
            buffer.extend(&(s.len() as i32).to_le_bytes());
            // utf8
            buffer.extend(s.as_bytes().to_vec());
            let ptr = self.data(&buffer);
            self.intern_str_map.insert(s.clone(), ptr);
        }
        *self.intern_str_map.get(s).unwrap()
    }
}

use super::*;

pub(super) fn translate_fcall(
    out: &mut Out,
    lscope: &mut LocalScope,
    sink: &Rc<Sink>,
    etype: ReturnType,
    span: &SSpan,
    fname: &Rc<str>,
    argexprs: &Vec<Expr>,
) -> Result<(), Error> {
    let fentry = lscope.getf_or_err(span.clone(), fname)?;
    let ftype = fentry.type_();
    let trace = ftype.trace;
    if argexprs.len() != ftype.parameters.len() {
        return Err(Error::Type {
            span: span.clone(),
            expected: format!("{} args", ftype.parameters.len()),
            got: format!("{} args", argexprs.len()),
        });
    }

    let mut trait_type_var = None;

    for (i, (argexpr, (_pname, ptype))) in argexprs.iter().zip(&ftype.parameters).enumerate() {
        translate_expr(out, sink, lscope, ReturnType::Value(*ptype), argexpr)?;

        if i == 0 {
            match &fentry {
                FunctionEntry::Trait(_) => {
                    // for traits, we need to save the dynamic type of the first argument
                    // for the actual call later
                    assert_eq!(*ptype, Type::Id);

                    let varname = lscope.helper_unique(Type::I32);

                    raw_dup(lscope, sink, WasmType::I64);

                    sink.writeln("i64.const 32");
                    sink.writeln("i64.shr_u");
                    sink.writeln("i32.wrap_i64");
                    sink.writeln(format!("local.set {}", varname));

                    trait_type_var = Some(varname);
                }
                FunctionEntry::Function(_) => {}
            }
        }
    }

    if trace && !lscope.trace {
        return Err(Error::Type {
            span: span.clone(),
            expected: "notrace function can only call other notrace functions".into(),
            got: "traced function".into(),
        });
    }

    if trace {
        // check for stack overflow
        sink.writeln("global.get $rt_stack_top");
        sink.writeln("global.get $rt_stack_end");
        sink.writeln("i32.ge_s");
        sink.writeln("if");
        sink.writeln("call $f___WAC_stack_overflow");
        sink.writeln("else end");

        // record the file this function call comes from
        let ptr = out.intern_cstr(&span.source.name);
        sink.writeln("global.get $rt_stack_top");
        sink.writeln(format!("i32.const {}", ptr));
        sink.writeln("i32.store");

        // record the lineno of this function call
        let lineno = span.lineno() as i32;
        sink.writeln("global.get $rt_stack_top");
        sink.writeln("i32.const 4");
        sink.writeln("i32.add");
        sink.writeln(format!("i32.const {}", lineno));
        sink.writeln("i32.store");

        // increment the stack pointer
        sink.writeln("global.get $rt_stack_top");
        sink.writeln("i32.const 8");
        sink.writeln("i32.add");
        sink.writeln("global.set $rt_stack_top");
    }

    match &fentry {
        FunctionEntry::Function(info) => {
            sink.writeln(format!("call $f_{}", info.name));
        }
        FunctionEntry::Trait(info) => {
            let trait_type_var = trait_type_var.unwrap();
            sink.writeln(format!("local.get {}", trait_type_var));
            sink.writeln(format!("i32.const {}", info.id));
            sink.writeln("call $f___WAC_find_funcptr");
            sink.writeln(format!("call_indirect {}", translate_func_type(&info.type_)));
            // panic!("TODO: fcall trait")
        }
    }

    if trace {
        // pop stack pointer
        sink.writeln("global.get $rt_stack_top");
        sink.writeln("i32.const -8");
        sink.writeln("i32.add");
        sink.writeln("global.set $rt_stack_top");
    }

    auto_cast(sink, span, lscope, ftype.return_type, etype)?;
    Ok(())
}
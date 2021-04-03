use std::rc::Rc;

use crate::env::Env;
use crate::expr::{compile_expr, compile_fn_expr};
use crate::helper::allocate_env;
use anyhow::{anyhow, Error};
use inkwell::{
    builder::Builder,
    context::Context,
    module::Module,
    values::{FunctionValue, PointerValue},
    AddressSpace,
};
use serde_json::Value;

pub fn compile_stmt<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<Option<PointerValue<'ctx>>, Error> {
    let type_ = es_node.get("type").unwrap().as_str().unwrap();
    // println!("{:?}", type_);
    match type_ {
        "VariableDeclaration" => {
            compile_var_decl(es_node, env, context, module, builder, function).map(|_| None)
        }
        "ExpressionStatement" => {
            compile_expr_stmt(es_node, env, context, module, builder, function).map(Some)
        }
        "BlockStatement" => {
            compile_block_stmt(es_node, env, context, module, builder, function).map(|_| None)
        }
        "IfStatement" => {
            compile_if_stmt(es_node, env, context, module, builder, function).map(|_| None)
        }
        "FunctionDeclaration" => {
            compile_fn_decl(es_node, env, context, module, builder).map(|_| None)
        }
        "ReturnStatement" => {
            compile_return_stmt(es_node, env, context, module, builder, function).map(|_| None)
        }
        _ => Err(anyhow!("stmt compile error")),
    }
}

pub fn compile_var_decl<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<(), Error> {
    let declaration = &es_node.get("declarations").unwrap().as_array().unwrap()[0];
    let name = declaration
        .get("id")
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap();
    let init = declaration.get("init").unwrap();

    let value = compile_expr(init, env.clone(), context, module, builder, function)?;
    let mut frame = env.ptr.clone().unwrap();

    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);

    let (jumps, offset) = env.lookup(name)?;

    for _ in 0..jumps {
        let tmp = builder
            .build_bitcast(*frame, frame.get_type().ptr_type(AddressSpace::Generic), "")
            .into_pointer_value();
        frame = Rc::new(builder.build_load(tmp, "").into_pointer_value());
    }

    let frame_casted = builder
        .build_bitcast(*frame, source_obj_ptr_ptr_type, "")
        .into_pointer_value();
    // SAFETY: Inherently unsafe
    let ptr = unsafe {
        builder.build_in_bounds_gep(
            frame_casted,
            &[context.i32_type().const_int(offset, false)],
            "",
        )
    };

    builder.build_store(ptr, value);

    Ok(())
}

pub fn compile_expr_stmt<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    compile_expr(
        es_node.get("expression").unwrap(),
        env,
        context,
        module,
        builder,
        function,
    )
}

pub fn compile_block_stmt<'ctx>(
    es_node: &Value,
    parent: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<(), Error> {
    let body = es_node.get("body").unwrap().as_array().unwrap();
    let env = Rc::new(allocate_env(body, Some(parent), context, module, builder)?);

    for s in body.iter() {
        compile_stmt(s, env.clone(), context, module, builder, function).unwrap();

        if s.get("type").unwrap().as_str().unwrap() == "ReturnStatement" {
            break;
        }
    }

    Ok(())
}

pub fn compile_if_stmt<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<(), Error> {
    let test = es_node.get("test").unwrap();
    let test_result_ptr = compile_expr(test, env.clone(), context, module, builder, function)?;

    let zero = context.i32_type().const_int(0, false);
    let one = context.i32_type().const_int(1, false);

    let test_result_value_ptr =
        unsafe { builder.build_in_bounds_gep(test_result_ptr, &[zero, one], "") };
    let value = builder
        .build_load(test_result_value_ptr, "")
        .into_int_value();
    let as_i1 = builder.build_int_truncate(value, context.bool_type(), "");

    let consequent_block = context.append_basic_block(*function, "if.true");
    let alternate_block = context.append_basic_block(*function, "if.false");
    let end_block = context.append_basic_block(*function, "if.end");

    builder.build_conditional_branch(as_i1, consequent_block, alternate_block);

    builder.position_at_end(consequent_block);
    compile_stmt(
        es_node.get("consequent").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;
    if builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        builder.build_unconditional_branch(end_block);
    }

    builder.position_at_end(alternate_block);
    compile_stmt(
        es_node.get("alternate").unwrap(),
        env,
        context,
        module,
        builder,
        function,
    )?;
    if builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        builder.build_unconditional_branch(end_block);
    }

    builder.position_at_end(end_block);

    Ok(())
}

pub fn compile_fn_decl<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<(), Error> {
    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);

    let name = es_node
        .get("id")
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap();
    let lit = compile_fn_expr(
        Some(name),
        es_node,
        env.clone(),
        false,
        context,
        module,
        builder,
    )?;

    let mut frame = env.ptr.clone().unwrap();
    let (jumps, offset) = env.lookup(name)?;

    for _ in 0..jumps {
        let tmp = builder
            .build_bitcast(*frame, frame.get_type().ptr_type(AddressSpace::Generic), "")
            .into_pointer_value();
        frame = Rc::new(builder.build_load(tmp, "").into_pointer_value());
    }

    let frame_casted = builder
        .build_bitcast(*frame, source_obj_ptr_ptr_type, "")
        .into_pointer_value();
    // SAFETY: Inherently unsafe
    let ptr = unsafe {
        builder.build_in_bounds_gep(
            frame_casted,
            &[context.i32_type().const_int(offset, false)],
            "",
        )
    };

    builder.build_store(ptr, lit);

    Ok(())
}

pub fn compile_return_stmt<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<(), Error> {
    let argument = es_node.get("argument").unwrap();
    let result = compile_expr(argument, env, context, module, builder, function)?;
    builder.build_return(Some(&result));

    Ok(())
}

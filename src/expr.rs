use std::rc::Rc;

use crate::env::Env;
use crate::helper::*;
use crate::stmt::compile_block_stmt;
use anyhow::{anyhow, Error};
use inkwell::{
    builder::Builder,
    context::Context,
    module::Module,
    values::{BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue},
    AddressSpace, IntPredicate,
};
use serde_json::Value;

pub(crate) fn compile_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let type_ = es_node.get("type").unwrap().as_str().unwrap();
    // println!("{:?}", type_);
    match type_ {
        "Identifier" => compile_id_expr(es_node, env, context, module, builder),
        "UnaryExpression" => compile_unary_expr(es_node, env, context, module, builder, function),
        "BinaryExpression" | "LogicalExpression" => {
            compile_binary_expr(es_node, env, context, module, builder, function)
        }
        "Literal" => compile_literal_expr(es_node, context, module, builder),
        "CallExpression" => compile_call_expr(es_node, env, context, module, builder, function),
        "ConditionalExpression" => {
            compile_ternary_expr(es_node, env, context, module, builder, function)
        }
        "ArrowFunctionExpression" => {
            let is_expression = es_node.get("expression").unwrap().as_bool().unwrap();
            compile_fn_expr(None, es_node, env, is_expression, context, module, builder)
        }
        _ => Err(anyhow!("expr compile error")),
    }
}

fn compile_id_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);

    let name = es_node.get("name").unwrap().as_str().unwrap();
    let (jumps, offset) = env.lookup(name)?;
    let mut frame = env.ptr.clone().unwrap();

    (0..jumps).for_each(|_| {
        let tmp = builder
            .build_bitcast(*frame, frame.get_type().ptr_type(AddressSpace::Generic), "")
            .into_pointer_value();
        frame = Rc::new(builder.build_load(tmp, "").into_pointer_value());
    });

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
    let load = builder.build_load(ptr, "").into_pointer_value();

    Ok(load)
}

fn compile_unary_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let operator = es_node.get("operator").unwrap().as_str().unwrap();
    let argument = compile_expr(
        es_node.get("argument").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;

    let zero = context.i32_type().const_int(0, false);
    let one = context.i32_type().const_int(1, false);

    let type_ptr = unsafe { builder.build_in_bounds_gep(argument, &[zero, zero], "") };
    let value_ptr = unsafe { builder.build_in_bounds_gep(argument, &[zero, one], "") };

    let obj_type = builder.build_load(type_ptr, "").into_int_value();
    let obj_value = builder.build_load(value_ptr, "").into_int_value();

    match operator {
        "!" => {
            let error = context.append_basic_block(*function, "rt.tc.error");
            let valid = context.append_basic_block(*function, "rt.tc.valid");

            let is_bool = builder.build_int_compare(
                IntPredicate::EQ,
                obj_type,
                context.i64_type().const_int(1, false),
                "",
            );
            builder.build_conditional_branch(is_bool, valid, error);

            builder.position_at_end(error);
            let error_fn = module.get_function("error").unwrap();
            builder.build_call(error_fn, &[], "");
            builder.build_unconditional_branch(valid);

            builder.position_at_end(valid);
            let not = builder.build_not(obj_value, "");
            build_literal(&obj_type, &not, context, module, builder)
        }
        "-" => {
            let error = context.append_basic_block(*function, "rt.tc.error");
            let valid = context.append_basic_block(*function, "rt.tc.valid");

            let is_number = builder.build_int_compare(
                IntPredicate::EQ,
                obj_type,
                context.i64_type().const_int(2, false),
                "",
            );
            builder.build_conditional_branch(is_number, valid, error);

            builder.position_at_end(error);
            let error_fn = module.get_function("error").unwrap();
            builder.build_call(error_fn, &[], "");
            builder.build_unconditional_branch(valid);

            builder.position_at_end(valid);
            let obj_value = builder
                .build_bitcast(obj_value, context.f64_type(), "")
                .into_float_value();
            let neg = builder.build_float_neg(obj_value, "");
            let neg_as_i64 = builder
                .build_bitcast(neg, context.i64_type(), "")
                .into_int_value();
            build_literal(&obj_type, &neg_as_i64, context, module, builder)
        }
        _ => Err(anyhow!("unary expr compile error")),
    }
}

fn typecheck<'ctx>(
    expected_left_type: &IntValue,
    expected_right_type: &IntValue,
    actual_left_type: &IntValue,
    actual_right_type: &IntValue,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) {
    let next = context.append_basic_block(*function, "rt.tc.next");
    let error = context.append_basic_block(*function, "rt.tc.error");
    let valid = context.append_basic_block(*function, "rt.tc.valid");

    let left_match =
        builder.build_int_compare(IntPredicate::EQ, *expected_left_type, *actual_left_type, "");
    builder.build_conditional_branch(left_match, next, error);

    builder.position_at_end(next);
    let right_match = builder.build_int_compare(
        IntPredicate::EQ,
        *expected_right_type,
        *actual_right_type,
        "",
    );
    builder.build_conditional_branch(right_match, valid, error);

    builder.position_at_end(error);
    let error_fn = module.get_function("error").unwrap();
    builder.build_call(error_fn, &[], "");
    builder.build_unconditional_branch(valid);

    builder.position_at_end(valid);
}

fn compile_binary_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let operator = es_node.get("operator").unwrap().as_str().unwrap();
    let left = compile_expr(
        es_node.get("left").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;
    // let display_fn = module.get_function("display").unwrap();
    // builder.build_call(display_fn, &[left.into()], "");
    let right = compile_expr(
        es_node.get("right").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;
    // let display_fn = module.get_function("display").unwrap();
    // builder.build_call(display_fn, &[right.into()], "");

    let zero = context.i32_type().const_int(0, false);
    let one = context.i32_type().const_int(1, false);

    let left_value_ptr = unsafe { builder.build_in_bounds_gep(left, &[zero, one], "") };
    let left_value = builder.build_load(left_value_ptr, "");
    let right_value_ptr = unsafe { builder.build_in_bounds_gep(right, &[zero, one], "") };
    let right_value = builder.build_load(right_value_ptr, "");

    let i64_type = context.i64_type();
    let f64_type = context.f64_type();

    let left_type_ptr = unsafe { builder.build_in_bounds_gep(left, &[zero, zero], "") };
    let left_type = builder.build_load(left_type_ptr, "").into_int_value();
    let right_type_ptr = unsafe { builder.build_in_bounds_gep(right, &[zero, zero], "") };
    let right_type = builder.build_load(right_type_ptr, "").into_int_value();

    let boolean_type = i64_type.const_int(1, false);
    let number_type = i64_type.const_int(2, false);

    use inkwell::FloatPredicate::*;
    let (result_value, result_type) = match operator {
        "+" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_f64 =
                builder.build_float_add(left_value_as_f64, right_value_as_f64, "");
            let result_value = builder
                .build_bitcast(result_value_as_f64, i64_type, "")
                .into_int_value();
            (result_value, number_type)
        }
        "-" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_f64 =
                builder.build_float_sub(left_value_as_f64, right_value_as_f64, "");
            let result_value = builder
                .build_bitcast(result_value_as_f64, i64_type, "")
                .into_int_value();
            (result_value, number_type)
        }
        "*" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_f64 =
                builder.build_float_mul(left_value_as_f64, right_value_as_f64, "");
            let result_value = builder
                .build_bitcast(result_value_as_f64, i64_type, "")
                .into_int_value();
            (result_value, number_type)
        }
        "/" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_f64 =
                builder.build_float_div(left_value_as_f64, right_value_as_f64, "");
            let result_value = builder
                .build_bitcast(result_value_as_f64, i64_type, "")
                .into_int_value();
            (result_value, number_type)
        }
        "%" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_f64 =
                builder.build_float_rem(left_value_as_f64, right_value_as_f64, "");
            let result_value = builder
                .build_bitcast(result_value_as_f64, i64_type, "")
                .into_int_value();
            (result_value, number_type)
        }
        "<" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(OLT, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        ">" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(OGT, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        "===" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(OEQ, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        "!==" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(ONE, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        "<=" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(OLE, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        ">=" => {
            typecheck(
                &number_type,
                &number_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let left_value_as_f64 = builder
                .build_bitcast(left_value, f64_type, "")
                .into_float_value();
            let right_value_as_f64 = builder
                .build_bitcast(right_value, f64_type, "")
                .into_float_value();
            let result_value_as_i1 =
                builder.build_float_compare(OGE, left_value_as_f64, right_value_as_f64, "");
            let result_value = builder.build_int_cast(result_value_as_i1, i64_type, "");
            (result_value, boolean_type)
        }
        "&&" => {
            typecheck(
                &boolean_type,
                &boolean_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let result_value = builder.build_and(
                left_value.into_int_value(),
                right_value.into_int_value(),
                "",
            );
            (result_value, boolean_type)
        }
        "||" => {
            typecheck(
                &boolean_type,
                &boolean_type,
                &left_type,
                &right_type,
                context,
                module,
                builder,
                function,
            );
            let result_value = builder.build_or(
                left_value.into_int_value(),
                right_value.into_int_value(),
                "",
            );
            (result_value, boolean_type)
        }
        _ => return Err(anyhow!("binary expr compile error")),
    };

    // println!("{:?}", result_type);
    // println!("{:?}", result_value);
    build_literal(&result_type, &result_value, context, module, builder)
}

fn compile_literal_expr<'ctx>(
    es_node: &Value,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    match es_node.get("value").unwrap() {
        Value::Bool(value) => build_boolean(*value, context, module, builder),
        Value::Number(value) => build_number(value.as_f64().unwrap(), context, module, builder),
        _ => return Err(anyhow!("literal expr compile error")),
    }
}

fn compile_call_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let params: Vec<BasicValueEnum<'ctx>> = es_node
        .get("arguments")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| {
            compile_expr(arg, env.clone(), context, module, builder, function)
                .unwrap()
                .as_basic_value_enum()
        })
        .collect();

    if es_node
        .get("callee")
        .unwrap()
        .get("type")
        .unwrap()
        .as_str()
        .unwrap()
        == "Identifier"
    {
        let callee_name = es_node
            .get("callee")
            .unwrap()
            .get("name")
            .unwrap()
            .as_str()
            .unwrap();
        if callee_name == "display" {
            let display_fn = module.get_function("display").unwrap();
            builder.build_call(display_fn, &params, "");
            return build_undefined(context, module, builder);
        }
    }

    let callee = compile_expr(
        es_node.get("callee").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;

    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);

    let closure_type = module.get_struct_type("closure").unwrap();
    let closure_ptr_type = closure_type.ptr_type(AddressSpace::Generic);

    let _0 = context.i32_type().const_int(0, false);
    let _1 = context.i32_type().const_int(1, false);
    let _2 = context.i32_type().const_int(2, false);

    let lit_type = unsafe { builder.build_in_bounds_gep(callee, &[_0, _0], "") };
    let lit_type_value = builder.build_load(lit_type, "").into_int_value();

    // typecheck
    {
        let error = context.append_basic_block(*function, "error");
        let next = context.append_basic_block(*function, "next");

        let is_fn = builder.build_int_compare(
            IntPredicate::EQ,
            lit_type_value,
            context.i64_type().const_int(3, false),
            "",
        );
        builder.build_conditional_branch(is_fn, next, error);

        builder.position_at_end(error);
        let error_fn = module.get_function("error").unwrap();
        builder.build_call(error_fn, &[], "");
        builder.build_unconditional_branch(next);

        builder.position_at_end(next);
    }

    let function_lit = builder
        .build_bitcast(callee, closure_ptr_type, "")
        .into_pointer_value();

    let function_obj_addr = unsafe { builder.build_in_bounds_gep(function_lit, &[_0, _2], "") };
    let function_obj = builder
        .build_load(function_obj_addr, "")
        .into_pointer_value();

    let fun_env_addr = unsafe { builder.build_in_bounds_gep(function_lit, &[_0, _1], "") };
    let fun_env = builder.build_load(fun_env_addr, "");

    let boxed_params = {
        let n = params.len();
        let size = n * 8;

        let mem = malloc(size as u64, context, module, builder, "params")?;
        let addr = builder
            .build_bitcast(mem, source_obj_ptr_ptr_type, "")
            .into_pointer_value();

        let mut base;
        for i in 0..n {
            base = unsafe {
                builder.build_in_bounds_gep(
                    addr,
                    &[context.i32_type().const_int(i as u64, false)],
                    "",
                )
            };
            builder.build_store(base, params[i]);
        }

        builder.build_bitcast(addr, source_obj_ptr_ptr_type, "")
    };

    Ok(builder
        .build_call(function_obj, &[fun_env, boxed_params], "")
        .try_as_basic_value()
        .left()
        .unwrap()
        .into_pointer_value())
}

fn compile_ternary_expr<'ctx>(
    es_node: &Value,
    env: Rc<Env<'ctx>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    function: &FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);

    let test_ptr = compile_expr(
        es_node.get("test").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;

    let _0 = context.i32_type().const_int(0, false);
    let _1 = context.i32_type().const_int(1, false);

    let test_result_value_ptr = unsafe { builder.build_in_bounds_gep(test_ptr, &[_0, _1], "") };
    let value = builder
        .build_load(test_result_value_ptr, "")
        .into_int_value();
    let as_i1 = builder.build_int_truncate(value, context.bool_type(), "");

    let consequent_block = context.append_basic_block(*function, "tern.true");
    let alternate_block = context.append_basic_block(*function, "tern.false");
    let end_block = context.append_basic_block(*function, "tern.end");

    builder.build_conditional_branch(as_i1, consequent_block, alternate_block);

    builder.position_at_end(consequent_block);
    let consequent = compile_expr(
        es_node.get("consequent").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;
    let con_end = builder.get_insert_block().unwrap();
    builder.build_unconditional_branch(end_block);

    builder.position_at_end(alternate_block);
    let alternate = compile_expr(
        es_node.get("alternate").unwrap(),
        env.clone(),
        context,
        module,
        builder,
        function,
    )?;
    let alt_end = builder.get_insert_block().unwrap();
    builder.build_unconditional_branch(end_block);

    builder.position_at_end(end_block);
    let phi = builder.build_phi(source_obj_ptr_type, "");
    phi.add_incoming(&[(&consequent, con_end), (&alternate, alt_end)]);

    Ok(phi.as_basic_value().into_pointer_value())
}

pub(crate) fn compile_fn_expr<'ctx>(
    name: Option<&str>,
    es_node: &Value,
    parent: Rc<Env<'ctx>>,
    is_expression: bool,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_ptr_type = source_obj_ptr_ptr_type.ptr_type(AddressSpace::Generic);

    let resume_point = builder.get_insert_block().unwrap();

    let params = es_node.get("params").unwrap().as_array().unwrap();

    let generic_fn_type = source_obj_ptr_type.fn_type(
        &[
            source_obj_ptr_ptr_type.into(),
            source_obj_ptr_ptr_type.into(),
        ],
        false,
    );

    let fun = module.add_function(
        &name
            .map(|s| format!("__{}", s))
            .unwrap_or("___closure".into()),
        generic_fn_type,
        None,
    );

    let entry = context.append_basic_block(fun, "f.entry");
    builder.position_at_end(entry);

    let enclosing_frame = fun.get_first_param().unwrap().into_pointer_value();
    let params_ptr = fun.get_last_param().unwrap().into_pointer_value();

    let mut env = Env::new(Some(parent.clone()));

    params
        .iter()
        .for_each(|param| env.add_name(param.get("name").unwrap().as_str().unwrap().into()));

    let body: &[Value] = if is_expression {
        &[]
    } else {
        es_node
            .get("body")
            .unwrap()
            .get("body")
            .unwrap()
            .as_array()
            .unwrap()
    };
    let env_size = (env.add_and_count_decls(body)? + params.len() as u64 + 1) * 8;
    let addr = malloc(env_size, context, module, builder, "fn.env")?;
    let env_value = builder
        .build_bitcast(addr, source_obj_ptr_ptr_type, "")
        .into_pointer_value();
    env.ptr = Some(Rc::new(env_value));

    let frame_ptr = builder
        .build_bitcast(env_value, source_obj_ptr_ptr_ptr_type, "frame")
        .into_pointer_value();
    builder.build_store(frame_ptr, enclosing_frame);

    let params_ = builder
        .build_bitcast(params_ptr, source_obj_ptr_ptr_type, "")
        .into_pointer_value();
    let this_env = builder
        .build_bitcast(*env.ptr.clone().unwrap(), source_obj_ptr_ptr_type, "")
        .into_pointer_value();

    let mut base;
    let mut value;
    let mut target;
    for i in 0..params.len() {
        base = unsafe {
            builder.build_in_bounds_gep(
                params_,
                &[context.i32_type().const_int(i as u64, false)],
                "",
            )
        };
        value = builder.build_load(base, "");
        target = unsafe {
            builder.build_in_bounds_gep(
                this_env,
                &[context.i32_type().const_int((i + 1) as u64, false)],
                "",
            )
        };
        builder.build_store(target, value);
    }

    if is_expression {
        let result = compile_expr(
            es_node.get("body").unwrap(),
            Rc::new(env),
            context,
            module,
            builder,
            &fun,
        )?;
        builder.build_return(Some(&result));
    } else {
        compile_block_stmt(
            es_node.get("body").unwrap(),
            Rc::new(env),
            context,
            module,
            builder,
            &fun,
        )?;
    }

    if builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        let result = build_undefined(context, module, builder)?;
        builder.build_return(Some(&result));
    }

    builder.position_at_end(resume_point);

    let closure_type = module.get_struct_type("closure").unwrap();
    let closure_ptr_type = closure_type.ptr_type(AddressSpace::Generic);

    let mem = malloc(BOXED_VALUE_SIZE, context, module, builder, "")?;

    let zero = context.i32_type().const_int(0, false);
    let one = context.i32_type().const_int(1, false);
    let two = context.i32_type().const_int(2, false);

    let literal = builder
        .build_bitcast(mem, closure_ptr_type, "")
        .into_pointer_value();
    let type_ptr = unsafe { builder.build_in_bounds_gep(literal, &[zero, zero], "") };
    let env_ptr = unsafe { builder.build_in_bounds_gep(literal, &[zero, one], "") };
    let fun_ptr = unsafe { builder.build_in_bounds_gep(literal, &[zero, two], "") };

    builder.build_store(type_ptr, context.i64_type().const_int(3, false));
    builder.build_store(env_ptr, *parent.ptr.clone().unwrap());
    builder.build_store(fun_ptr, fun.as_global_value());

    Ok(builder
        .build_bitcast(literal, source_obj_ptr_type, "")
        .into_pointer_value())
}

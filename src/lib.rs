use anyhow::{anyhow, Error};
use inkwell::{
    builder::Builder,
    context::Context,
    module::Module,
    targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetTriple},
    values::BasicValue,
    AddressSpace, OptimizationLevel,
};
use serde_json::Value;

use std::rc::Rc;

mod env;
mod expr;
mod helper;
mod stmt;

use helper::{allocate_env, build_undefined};
use stmt::compile_stmt;

pub fn compile(es_str: &str) -> Result<String, Error> {
    let es_node: Value = serde_json::from_str(es_str)?;

    // We only compile to wasm32-unknown-wasi for now because it relies on the
    // pointer size being 32 bit, but on paper it should be able to target other
    // triples as well.
    Target::initialize_webassembly(&InitializationConfig::default());
    let target_triple = TargetTriple::create("wasm32-unknown-wasi");
    let target = Target::from_triple(&target_triple).unwrap();
    let target_machine = target
        .create_target_machine(
            &target_triple,
            "",
            "",
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .unwrap();
    let target_data_layout = target_machine.get_target_data().get_data_layout();

    let context = &Context::create();
    let module = &context.create_module("main.js");
    module.set_data_layout(&target_data_layout);
    module.set_triple(&target_triple);
    let builder = &context.create_builder();

    // compile program
    {
        setup(context, module, builder)?;

        let main_function_type = context.i32_type().fn_type(&[], false);
        let main_function = module.add_function("main", main_function_type, None);

        let entry = context.append_basic_block(main_function, "entry");
        builder.position_at_end(entry);

        let env = Rc::new(allocate_env(
            es_node.get("body").unwrap().as_array().unwrap(),
            None,
            context,
            module,
            builder,
        )?);

        let last = es_node
            .get("body")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                compile_stmt(s, env.clone(), context, module, builder, &main_function).unwrap()
            })
            .last()
            .unwrap();
        let result = last.unwrap_or(build_undefined(context, module, builder)?);
        let display_fn = module.get_function("display").unwrap();
        builder.build_call(display_fn, &[result.into()], "");

        let _0 = context.i32_type().const_int(0, false);
        builder.build_return(Some(&_0));
    }

    module.verify().map_err(|s| anyhow!(s.to_string()))?;

    Ok(module.print_to_string().to_string())
}

fn setup<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<(), Error> {
    let i8_type = context.i8_type();
    let i8_ptr_type = i8_type.ptr_type(AddressSpace::Generic);
    let i32_type = context.i32_type();
    let i64_type = context.i64_type();
    let void_type = context.void_type();
    let bool_type = context.bool_type();
    let f64_type = context.f64_type();

    let source_obj_type = context.opaque_struct_type("source_obj");
    source_obj_type.set_body(&[i64_type.into(), i64_type.into()], false);
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);

    let closure_type = context.opaque_struct_type("closure");
    closure_type.set_body(
        &[
            i64_type.into(),
            source_obj_type
                .ptr_type(AddressSpace::Generic)
                .ptr_type(AddressSpace::Generic)
                .into(),
            source_obj_type
                .ptr_type(AddressSpace::Generic)
                .fn_type(
                    &[
                        source_obj_type
                            .ptr_type(AddressSpace::Generic)
                            .ptr_type(AddressSpace::Generic)
                            .into(),
                        source_obj_type
                            .ptr_type(AddressSpace::Generic)
                            .ptr_type(AddressSpace::Generic)
                            .into(),
                    ],
                    false,
                )
                .ptr_type(AddressSpace::Generic)
                .into(),
        ],
        false,
    );

    let printf_type = i32_type.fn_type(&[i8_ptr_type.into()], true);
    module.add_function("printf", printf_type, None);

    let malloc_type = i8_ptr_type.fn_type(&[i32_type.into()], false);
    module.add_function("malloc", malloc_type, None);

    let exit_type = void_type.fn_type(&[i32_type.into()], false);
    module.add_function("exit", exit_type, None);

    // display fn
    {
        let display_fn_type = void_type.fn_type(&[source_obj_ptr_type.into()], false);
        let display_fn = module.add_function("display", display_fn_type, None);

        let printf_fn = module.get_function("printf").unwrap();

        let entry = context.append_basic_block(display_fn, "entry");
        builder.position_at_end(entry);
        let undefined_block = context.append_basic_block(display_fn, "undefined");
        let boolean_block = context.append_basic_block(display_fn, "boolean");
        let true_block = context.append_basic_block(display_fn, "true");
        let false_block = context.append_basic_block(display_fn, "false");
        let number_block = context.append_basic_block(display_fn, "number");
        let function_block = context.append_basic_block(display_fn, "function");
        let end_block = context.append_basic_block(display_fn, "end");

        let _0 = context.i32_type().const_int(0, false);
        let _1 = context.i32_type().const_int(1, false);

        let obj_type_ptr = unsafe {
            builder.build_in_bounds_gep(
                display_fn.get_first_param().unwrap().into_pointer_value(),
                &[_0, _0],
                "",
            )
        };
        let obj_type = builder.build_load(obj_type_ptr, "").into_int_value();
        let obj_value_ptr = unsafe {
            builder.build_in_bounds_gep(
                display_fn.get_first_param().unwrap().into_pointer_value(),
                &[_0, _1],
                "",
            )
        };
        let obj_value = builder.build_load(obj_value_ptr, "").into_int_value();
        builder.build_switch(
            obj_type,
            undefined_block,
            &[
                (i64_type.const_int(1, false), boolean_block),
                (i64_type.const_int(2, false), number_block),
                (i64_type.const_int(3, false), function_block),
            ],
        );

        // undefined
        {
            builder.position_at_end(undefined_block);
            let undefined_fmt_str =
                builder.build_global_string_ptr("undefined\n", "undefined_fmt_str");
            builder.build_call(printf_fn, &[undefined_fmt_str.as_basic_value_enum()], "");
            builder.build_unconditional_branch(end_block);
        }

        // boolean
        {
            builder.position_at_end(boolean_block);
            let bool_value = builder.build_int_truncate(obj_value, bool_type, "");
            builder.build_conditional_branch(bool_value, true_block, false_block);

            builder.position_at_end(true_block);
            let true_fmt_str = builder.build_global_string_ptr("true\n", "true_fmt_str");
            builder.build_call(printf_fn, &[true_fmt_str.as_basic_value_enum()], "");
            builder.build_unconditional_branch(end_block);

            builder.position_at_end(false_block);
            let false_fmt_str = builder.build_global_string_ptr("false\n", "false_fmt_str");
            builder.build_call(printf_fn, &[false_fmt_str.as_basic_value_enum()], "");
            builder.build_unconditional_branch(end_block);
        }

        // number
        {
            builder.position_at_end(number_block);
            let number_value = builder.build_bitcast(obj_value, f64_type, "");
            let number_fmt_str = builder.build_global_string_ptr("%lf\n", "number_fmt_str");
            builder.build_call(
                printf_fn,
                &[number_fmt_str.as_basic_value_enum(), number_value],
                "",
            );
            builder.build_unconditional_branch(end_block);
        }

        // function
        {
            builder.position_at_end(function_block);
            let function_fmt_str =
                builder.build_global_string_ptr("Function\n", "function_fmt_str");
            builder.build_call(printf_fn, &[function_fmt_str.as_basic_value_enum()], "");
            builder.build_unconditional_branch(end_block);
        }

        builder.position_at_end(end_block);
        builder.build_return(None);
    }

    // error fn
    {
        let error_fn_type = void_type.fn_type(&[], false);
        let error_fn = module.add_function("error", error_fn_type, None);

        let entry = context.append_basic_block(error_fn, "entry");
        builder.position_at_end(entry);

        let error_str = builder.build_global_string_ptr("Type mismatch\n", "error_fmt_str");
        let exit_fn = module.get_function("exit").unwrap();
        let printf_fn = module.get_function("printf").unwrap();

        let _1 = context.i32_type().const_int(1, false);
        builder.build_call(printf_fn, &[error_str.as_basic_value_enum()], "");
        builder.build_call(exit_fn, &[_1.into()], "");
        builder.build_return(None);
    }

    Ok(())
}

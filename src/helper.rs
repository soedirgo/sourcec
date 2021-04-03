use std::rc::Rc;

use crate::env::Env;
use anyhow::Error;
use inkwell::{
    builder::Builder,
    context::Context,
    module::Module,
    values::{IntValue, PointerValue},
    AddressSpace,
};
use serde_json::Value;

pub(crate) const BOXED_VALUE_SIZE: u64 = 16;

pub(crate) fn allocate_env<'ctx>(
    body: &[Value],
    parent: Option<Rc<Env<'ctx>>>,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<Env<'ctx>, Error> {
    let source_obj_type = module.get_struct_type("source_obj").unwrap();
    let source_obj_ptr_type = source_obj_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_type = source_obj_ptr_type.ptr_type(AddressSpace::Generic);
    let source_obj_ptr_ptr_ptr_type = source_obj_ptr_ptr_type.ptr_type(AddressSpace::Generic);

    let mut env = Env::new(parent.clone());
    let env_size = (env.add_and_count_decls(body)? + 1) * 8;
    let addr = malloc(env_size as u64, context, module, builder, "env")?;
    let env_value = builder
        .build_bitcast(addr, source_obj_ptr_ptr_type, "")
        .into_pointer_value();
    env.ptr = Some(Rc::new(env_value));

    if let Some(parent) = parent {
        let parent_addr = *parent.ptr.clone().unwrap();
        let frame_ptr = builder
            .build_bitcast(env_value, source_obj_ptr_ptr_ptr_type, "frame")
            .into_pointer_value();
        builder.build_store(frame_ptr, parent_addr);
    }

    Ok(env)
}

pub(crate) fn malloc<'ctx>(
    size: u64,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, Error> {
    let size_value = context.i32_type().const_int(size, false);
    let malloc_fn = module.get_function("malloc").unwrap();
    let call = builder
        .build_call(malloc_fn, &[size_value.into()], name)
        .try_as_basic_value()
        .left()
        .unwrap()
        .into_pointer_value();
    Ok(call)
}

pub(crate) fn build_literal<'ctx>(
    obj_type: &IntValue,
    obj_value: &IntValue,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let source_obj_ptr_type = module
        .get_struct_type("source_obj")
        .unwrap()
        .ptr_type(AddressSpace::Generic);

    let mem = malloc(BOXED_VALUE_SIZE, context, module, builder, "")?;

    let _0 = context.i32_type().const_int(0, false);
    let _1 = context.i32_type().const_int(1, false);

    let obj_ptr = builder
        .build_bitcast(mem, source_obj_ptr_type, "")
        .into_pointer_value();
    // SAFETY: Inherently unsafe
    let type_ptr = unsafe { builder.build_in_bounds_gep(obj_ptr, &[_0, _0], "") };
    let value_ptr = unsafe { builder.build_in_bounds_gep(obj_ptr, &[_0, _1], "") };

    builder.build_store(type_ptr, *obj_type);
    builder.build_store(value_ptr, *obj_value);

    Ok(obj_ptr)
}

pub(crate) fn build_undefined<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let undefined_type = context.i64_type().const_int(0, false);
    let undefined_value = context.i64_type().const_int(0, false);
    build_literal(&undefined_type, &undefined_value, context, module, builder)
}

pub(crate) fn build_boolean<'ctx>(
    value: bool,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let bool_type = context.i64_type().const_int(1, false);
    let bool_value = context
        .i64_type()
        .const_int(if value { 1 } else { 0 }, false);
    build_literal(&bool_type, &bool_value, context, module, builder)
}

pub(crate) fn build_number<'ctx>(
    value: f64,
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
) -> Result<PointerValue<'ctx>, Error> {
    let number_type = context.i64_type().const_int(2, false);
    let number_value = context.f64_type().const_float(value);
    let number_value_as_i64 = builder
        .build_bitcast(number_value, context.i64_type(), "")
        .into_int_value();
    build_literal(&number_type, &number_value_as_i64, context, module, builder)
}

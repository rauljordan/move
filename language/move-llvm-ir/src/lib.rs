use move_binary_format::file_format as F;
use move_ir_types::{ast as IR, location::*};

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassManager;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, FloatValue, FunctionValue, PointerValue};
use inkwell::{FloatPredicate, OptimizationLevel};

pub fn translate_module(module: IR::ModuleDefinition, deps: Vec<&F::CompiledModule>) -> u64 {
    // TODO: Verify module.
    println!("Move IR module {:?}", module);
    return 1;
}
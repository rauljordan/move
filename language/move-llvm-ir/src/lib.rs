use std::path::PathBuf;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::process::{Command,Stdio};
use execute::Execute;

use move_binary_format::file_format as F;
use move_ir_types::ast::{
    Var, Var_, FunctionName, Function_, FunctionSignature, FunctionBody, Bytecode_,
};
use move_ir_types::{ast as IR, location::*};

use std::collections::HashMap;
use inkwell::module::Module;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::passes::PassManager;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValue,FloatValue,FunctionValue,PointerValue};
use inkwell::FloatPredicate;

pub fn translate_module(module: IR::ModuleDefinition, deps: Vec<&F::CompiledModule>) -> u64 {
    // TODO: Verify module.
    println!("");
    println!("Move IR module {:?}", module);
    println!("");
    let func = module.functions.first().unwrap();
    println!("Func {:?}", func);

    let context = Context::create();
    let llvm_module = context.create_module("tmp");
    let builder = context.create_builder();

    // Pass manager for functions.
    let fpm = PassManager::create(&llvm_module);

    fpm.add_instruction_combining_pass();
    fpm.add_reassociate_pass();
    fpm.add_gvn_pass();
    fpm.add_cfg_simplification_pass();
    fpm.add_basic_alias_analysis_pass();
    fpm.add_promote_memory_to_register_pass();
    fpm.add_instruction_combining_pass();
    fpm.add_reassociate_pass();

    fpm.initialize();

    // TODO: Translate all statements into LLVM IR.
    //let first_stmt = parsed_statements.first().unwrap();
    let translated = Translator::translate(
        &context, 
        &builder, 
        &fpm, 
        &llvm_module, 
        &module,
    ).unwrap();
    let result = translated
        .to_string()
        .replace("\"", "")
        .replace("\\n", "\n");

    println!("");
    println!("Translated move contract to LLVM IR below:");
    println!("{}", result);

    // Write an IR file to the temporary dir.
    let mut file = File::create("/tmp/main.ll").unwrap();
    file.write_all(result.into_bytes().as_slice()).unwrap();

    // Execute LLC to translate into an object file targeted at the 
    // wasm32-unknown-unknown triple.
    // TODO: Use llvm-sys to programmatically perform the following actions rather than
    // hardcoding llvm 15 toolchain commands.
    let mut command = Command::new("llc-15");
    command.arg("-march=wasm32");
    command.arg("-filetype=obj");
    command.arg("/tmp/main.ll");
    command.arg("-o=/tmp/main.o");

    command.execute().unwrap();

    // Execute wasm-ld to translate the bitcode into web assembly.
    let mut command = Command::new("wasm-ld-15");
    command.arg("/tmp/main.o");
    command.arg("-o");
    command.arg("/tmp/main.wasm");
    command.arg("--no-entry");
    // TODO: Do not export all, as it is dangerous.
    command.arg("--export-all");

    command.execute().unwrap();

    let mut command = Command::new("wasm2wat");
    command.arg("/tmp/main.wasm");

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let output = command.execute_output().unwrap();

    let wat_output = String::from_utf8(output.stdout).unwrap();
    println!("Compiled wasm to wat:");
    println!("{}", wat_output);

    let mut file = File::create("/tmp/main.wat").unwrap();
    file.write_all(wat_output.clone().into_bytes().as_slice()).unwrap();
    return 1;
}

pub struct Translator<'a, 'ctx> {
    pub context: &'ctx Context,
    pub builder: &'a Builder<'ctx>,
    pub fpm: &'a PassManager<FunctionValue<'ctx>>,
    pub module: &'a Module<'ctx>,
    pub variables: HashMap<String, PointerValue<'ctx>>,
    pub fn_value_opt: Option<FunctionValue<'ctx>>,
    pub move_mod: &'a IR::ModuleDefinition,
}

impl<'a, 'ctx> Translator<'a, 'ctx> {
    fn create_stack_alloc(&self, name: &str) -> PointerValue<'ctx> {
        let builder = self.context.create_builder();

        let entry = self.fn_value_opt.unwrap().get_first_basic_block().unwrap();

        match entry.get_first_instruction() {
            Some(first_instr) => builder.position_before(&first_instr),
            None => builder.position_at_end(entry),
        }

        builder.build_alloca(self.context.f64_type(), name)
    }

    pub fn translate_function_sig(
        &self, fn_name: &'a str, sig: &FunctionSignature,
    ) -> Result<FunctionValue<'ctx>, &'static str> {
        let return_type = self.context.f64_type();

        let arg_types = std::iter::repeat(return_type)
            .take(sig.formals.len())
            .map(|f| f.into())
            .collect::<Vec<BasicMetadataTypeEnum>>();
        let args = arg_types.as_slice();

        let fn_type = self.context.f64_type().fn_type(args, false); // No var args.
        let fn_val = self.module.add_function(fn_name, fn_type, None);

        for (i, arg) in fn_val.get_param_iter().enumerate() {
            let param = sig.formals[i].clone().0.value.0;
            arg.into_float_value().set_name(param.as_str());
        }

        Ok(fn_val)
    }
    pub fn translate_function(
        &mut self,
        f: &(FunctionName, Spanned<Function_>),
    ) -> Result<FunctionValue<'ctx>, &'static str> {
        let func_items = f.1.value.clone();
        let formals = func_items.signature.clone().formals;
        let sig = match func_items {
            Function_ { signature: sig, .. } => sig,
            _ => panic!("weird"),
        };
        let sig = self.translate_function_sig(f.0.0.as_str(), &sig)?;
        let entry = self.context.append_basic_block(sig, "entry");
        self.builder.position_at_end(entry);
        self.fn_value_opt = Some(sig);
        self.variables.reserve(formals.len());

        for (i, arg) in sig.get_param_iter().enumerate() {
            let param = formals[i].clone().0.value.0;
            let alloca = self.create_stack_alloc(param.as_str());
            self.builder.build_store(alloca, arg);
            self.variables.insert(param.to_string(), alloca);
        }

        let code = match func_items.body {
            FunctionBody::Bytecode { code, .. } => code,
            _ => panic!("No bytecode body"),
        };
        // TODO: Handle a bigger body.
        let body = code.first().unwrap().clone().1;
        // TODO: Handle all ops, be more efficient.
        let bytecode_items = body.clone().into_iter().map(|i| i.value).collect::<Vec<Bytecode_>>();
        if bytecode_items.contains(&Bytecode_::Add) {
            let left = body.get(0).unwrap().clone().value;
            let right = body.get(1).unwrap().clone().value;

            let left_var_name: String = match left {
                Bytecode_::MoveLoc(Spanned { value: Var_(id), .. }) => id.to_string(),
                _ => panic!("could not match"),
            };

            let right_var_name: String = match right {
                Bytecode_::MoveLoc(Spanned { value: Var_(id), .. }) => id.to_string(),
                _ => panic!("could not match"),
            };

            let lhs = match self.variables.get(left_var_name.as_str()) {
                Some(var) => Ok(self.builder.build_load(*var, left_var_name.as_str()).into_float_value()),
                None => Err("Could not find a matching variable"),
            }?;

            let rhs = match self.variables.get(right_var_name.as_str()) {
                Some(var) => Ok(self.builder.build_load(*var, right_var_name.as_str()).into_float_value()),
                None => Err("Could not find a matching variable"),
            }?;
            let expr = self.builder.build_float_add(lhs, rhs, "tmpadd");
            self.builder.build_return(Some(&expr));
        }

        if sig.verify(true) {
            self.fpm.run_on(&sig);
            return Ok(sig);
        }
        unsafe {
            sig.delete();
        }

        Err("Invalid generated function")
    }

    pub fn translate(
        context: &'ctx Context,
        builder: &'a Builder<'ctx>,
        pass_manager: &'a PassManager<FunctionValue<'ctx>>,
        module: &'a Module<'ctx>,
        move_mod: &'a IR::ModuleDefinition,
    ) -> Result<FunctionValue<'ctx>, &'static str> {
        let mut tr = Translator {
            context,
            builder,
            fpm: pass_manager,
            module,
            fn_value_opt: None,
            variables: HashMap::new(),
            move_mod,
        };

        // TODO: Handle all funcs.
        let f = move_mod.functions.first().unwrap();
        tr.translate_function(f)
    }
}


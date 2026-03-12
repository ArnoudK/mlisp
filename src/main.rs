use std::path::PathBuf;

use mlisp::driver::{CliCommand, CliOptions};
use mlisp::error::CompileError;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), CompileError> {
    let options = CliOptions::parse(std::env::args().skip(1))?;

    match options.command {
        CliCommand::Compile {
            inputs,
            emit_ir_dir,
            print_ir,
        } => {
            let units = mlisp::driver::compile_paths(&inputs, emit_ir_dir.as_deref())?;

            if print_ir {
                for unit in units {
                    println!("; module: {}", unit.module_name);
                    println!("{}", unit.llvm_ir);
                }
            }
        }
        CliCommand::Run { input } => {
            let output = mlisp::driver::run_path(&input)?;
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            if output.exit_code != 0 {
                return Err(CompileError::Codegen(format!(
                    "program exited with status {}",
                    output.exit_code
                )));
            }
        }
        CliCommand::Help => {
            print_usage();
        }
        CliCommand::CompileExample => {
            let inputs = vec![PathBuf::from("examples/hello.scm")];
            let units =
                mlisp::driver::compile_paths(&inputs, Some(PathBuf::from("target/ir").as_path()))?;

            for unit in units {
                println!("; module: {}", unit.module_name);
                println!("{}", unit.llvm_ir);
            }
        }
        CliCommand::GcExample => {
            let unit = mlisp::driver::compile_gc_example()?;
            println!("; module: {}", unit.module_name);
            println!("{}", unit.llvm_ir);
        }
        CliCommand::GcPipelineExample => {
            let unit = mlisp::driver::compile_gc_pipeline_example()?;
            println!("; module: {}", unit.module_name);
            println!("{}", unit.llvm_ir);
        }
        CliCommand::RuntimeStress {
            threads,
            iterations,
        } => {
            mlisp::runtime::mmtk::initialize_runtime(8 * 1024 * 1024, threads);
            mlisp::runtime::mmtk::run_mutator_stress(threads, iterations);
            println!(
                "runtime stress completed with {} threads and {} iterations",
                threads, iterations
            );
        }
    }

    Ok(())
}

fn print_usage() {
    println!("mlisp compile <file>... [--emit-ir-dir <dir>] [--print-ir]");
    println!("mlisp run <file>");
    println!("mlisp example");
    println!("mlisp gc-example");
    println!("mlisp gc-pipeline-example");
    println!("mlisp runtime-stress [--threads <n>] [--iterations <n>]");
}

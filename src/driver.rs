use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use crate::backend::{CompiledModule, LlvmBackend, pipeline, statepoint};
use crate::error::CompileError;
use crate::frontend::parse_program;
use crate::middle::lower_program;

#[derive(Debug)]
pub struct CliOptions {
    pub command: CliCommand,
}

#[derive(Debug)]
pub enum CliCommand {
    Compile {
        inputs: Vec<PathBuf>,
        emit_ir_dir: Option<PathBuf>,
        print_ir: bool,
    },
    Run {
        input: PathBuf,
    },
    CompileExample,
    GcExample,
    GcPipelineExample,
    RuntimeStress {
        threads: usize,
        iterations: usize,
    },
    Help,
}

impl CliOptions {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Self, CompileError> {
        let collected = args.collect::<Vec<_>>();
        if collected.is_empty() {
            return Ok(Self {
                command: CliCommand::Help,
            });
        }

        match collected[0].as_str() {
            "compile" => parse_compile_args(&collected[1..]),
            "run" => parse_run_args(&collected[1..]),
            "example" => Ok(Self {
                command: CliCommand::CompileExample,
            }),
            "gc-example" => Ok(Self {
                command: CliCommand::GcExample,
            }),
            "gc-pipeline-example" => Ok(Self {
                command: CliCommand::GcPipelineExample,
            }),
            "runtime-stress" => parse_runtime_stress_args(&collected[1..]),
            "help" | "--help" | "-h" => Ok(Self {
                command: CliCommand::Help,
            }),
            other => Err(CompileError::Usage(format!(
                "unknown subcommand '{other}', expected 'compile', 'run', 'example', 'gc-example', 'gc-pipeline-example', or 'runtime-stress'"
            ))),
        }
    }
}

#[derive(Debug)]
pub struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

fn build_root() -> Result<PathBuf, CompileError> {
    let root = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .join("build");
    fs::create_dir_all(&root).map_err(|error| CompileError::io(Some(root.clone()), error))?;
    Ok(root)
}

fn build_tmp_dir() -> Result<PathBuf, CompileError> {
    let tmp = build_root()?.join("tmp");
    fs::create_dir_all(&tmp).map_err(|error| CompileError::io(Some(tmp.clone()), error))?;
    Ok(tmp)
}

pub fn compile_gc_example() -> Result<CompiledModule, CompileError> {
    statepoint::compile_pre_statepoint_example("gc_example")
}

pub fn compile_gc_pipeline_example() -> Result<CompiledModule, CompileError> {
    let pre = statepoint::compile_pre_statepoint_example("gc_pipeline_example")?;
    pipeline::rewrite_statepoints(&pre)
}

pub fn compile_paths(
    inputs: &[PathBuf],
    emit_ir_dir: Option<&Path>,
) -> Result<Vec<CompiledModule>, CompileError> {
    if inputs.is_empty() {
        return Err(CompileError::Usage(
            "at least one input file is required".into(),
        ));
    }

    let mut handles = Vec::with_capacity(inputs.len());

    for (index, path) in inputs.iter().cloned().enumerate() {
        handles.push(
            thread::Builder::new()
                .name(format!("mlisp-worker-{index}"))
                .spawn(move || compile_path(&path))
                .map_err(|error| CompileError::Thread(error.to_string()))?,
        );
    }

    let mut compiled = Vec::with_capacity(handles.len());
    for handle in handles {
        let unit = handle
            .join()
            .map_err(|_| CompileError::Thread("worker thread panicked".into()))??;
        compiled.push(unit);
    }

    if let Some(output_dir) = emit_ir_dir {
        fs::create_dir_all(output_dir)
            .map_err(|error| CompileError::io(Some(output_dir.to_path_buf()), error))?;
        for unit in &compiled {
            let output_path = output_dir.join(format!("{}.ll", unit.module_name));
            fs::write(&output_path, &unit.llvm_ir)
                .map_err(|error| CompileError::io(Some(output_path), error))?;
        }
    }

    Ok(compiled)
}

pub fn run_path(input: &Path) -> Result<RunOutput, CompileError> {
    let compiled = compile_path(input)?;
    let linked = synthesize_native_main(compiled)?;
    let rewritten = pipeline::rewrite_statepoints(&linked)?;
    let root = build_root()?.join(&linked.module_name);
    fs::create_dir_all(&root).map_err(|error| CompileError::io(Some(root.clone()), error))?;
    let ir_path = root.join(format!("{}.ll", rewritten.module_name));
    let exe_path = root.join(&rewritten.module_name);
    fs::write(&ir_path, &rewritten.llvm_ir).map_err(|error| CompileError::io(Some(ir_path.clone()), error))?;

    let runtime_staticlib = ensure_runtime_staticlib()?;
    let native_static_libs = rust_runtime_native_libs()?;

    let mut clang = Command::new("clang");
    clang.env("TMPDIR", build_tmp_dir()?);
    clang.arg(&ir_path).arg(&runtime_staticlib).arg("-o").arg(&exe_path);
    for lib in &native_static_libs {
        clang.arg(lib);
    }
    let link = clang.output().map_err(|error| CompileError::io(None::<PathBuf>, error))?;
    if !link.status.success() {
        return Err(CompileError::Codegen(format!(
            "clang link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        )));
    }

    let output = Command::new(&exe_path)
        .output()
        .map_err(|error| CompileError::io(Some(exe_path), error))?;

    Ok(RunOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: normalize_runtime_stderr(&String::from_utf8_lossy(&output.stderr)),
        exit_code: output.status.code().unwrap_or(1),
    })
}

fn normalize_runtime_stderr(stderr: &str) -> String {
    let filtered = stderr
        .lines()
        .filter(|line| !line.contains("mmtk::"))
        .collect::<Vec<_>>()
        .join("\n");
    if filtered.is_empty() {
        filtered
    } else {
        format!("{filtered}\n")
    }
}

fn compile_path(path: &Path) -> Result<CompiledModule, CompileError> {
    let source = fs::read_to_string(path)
        .map_err(|error| CompileError::io(Some(path.to_path_buf()), error))?;
    let ast = parse_program(&source)?;
    let hir = lower_program(&ast)?;
    let module_name = sanitize_module_name(path);
    LlvmBackend::compile_program(&module_name, &hir)
}

fn synthesize_native_main(module: CompiledModule) -> Result<CompiledModule, CompileError> {
    let entry_ir = module
        .llvm_ir
        .replace("define i64 @main(", "define i64 @mlisp_entry(");
    if entry_ir == module.llvm_ir {
        return Err(CompileError::Codegen(
            "expected compiler module to define a Scheme main entry".into(),
        ));
    }

    let harness = r#"
define i32 @main() {
entry:
  %init = call i1 @rt_mmtk_init(i64 8388608, i64 1)
  br i1 %init, label %bind, label %fail

bind:
  %thread = call ptr @rt_bind_thread()
  %result = call i64 @mlisp_entry()
  call void @rt_unbind_thread(ptr %thread)
  %decoded = ashr i64 %result, 1
  %exit = trunc i64 %decoded to i32
  ret i32 %exit

fail:
  ret i32 1
}
"#;

    Ok(CompiledModule {
        module_name: module.module_name,
        llvm_ir: format!("{entry_ir}\n{harness}"),
    })
}

fn sanitize_module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| {
            stem.chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect::<String>()
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "module".into())
}

fn parse_compile_args(args: &[String]) -> Result<CliOptions, CompileError> {
    let mut inputs = Vec::new();
    let mut emit_ir_dir = None;
    let mut print_ir = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--emit-ir-dir" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(CompileError::Usage(
                        "--emit-ir-dir expects a directory path".into(),
                    ));
                };
                emit_ir_dir = Some(PathBuf::from(value));
                index += 2;
            }
            "--print-ir" => {
                print_ir = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(CompileError::Usage(format!("unknown flag '{value}'")));
            }
            value => {
                inputs.push(PathBuf::from(value));
                index += 1;
            }
        }
    }

    if inputs.is_empty() {
        return Err(CompileError::Usage(
            "compile expects at least one input file".into(),
        ));
    }

    Ok(CliOptions {
        command: CliCommand::Compile {
            inputs,
            emit_ir_dir,
            print_ir,
        },
    })
}

fn parse_run_args(args: &[String]) -> Result<CliOptions, CompileError> {
    if args.len() != 1 {
        return Err(CompileError::Usage(
            "run expects exactly one input file".into(),
        ));
    }
    Ok(CliOptions {
        command: CliCommand::Run {
            input: PathBuf::from(&args[0]),
        },
    })
}

fn parse_runtime_stress_args(args: &[String]) -> Result<CliOptions, CompileError> {
    let mut threads = 2usize;
    let mut iterations = 256usize;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--threads" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(CompileError::Usage("--threads expects a value".into()));
                };
                threads = value
                    .parse()
                    .map_err(|_| CompileError::Usage("--threads expects an integer".into()))?;
                index += 2;
            }
            "--iterations" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(CompileError::Usage("--iterations expects a value".into()));
                };
                iterations = value
                    .parse()
                    .map_err(|_| CompileError::Usage("--iterations expects an integer".into()))?;
                index += 2;
            }
            value => {
                return Err(CompileError::Usage(format!(
                    "unknown runtime-stress flag '{value}'"
                )));
            }
        }
    }

    Ok(CliOptions {
        command: CliCommand::RuntimeStress {
            threads: threads.max(1),
            iterations: iterations.max(1),
        },
    })
}

fn ensure_runtime_staticlib() -> Result<PathBuf, CompileError> {
    let build = Command::new("cargo")
        .args(["build", "--offline", "-p", "mlisp-runtime"])
        .output()
        .map_err(|error| CompileError::io(None::<PathBuf>, error))?;
    if !build.status.success() {
        return Err(CompileError::Codegen(format!(
            "cargo build for mlisp-runtime failed: {}",
            String::from_utf8_lossy(&build.stderr)
        )));
    }

    let deps_dir = PathBuf::from("target/debug/deps");
    let entries = fs::read_dir(&deps_dir).map_err(|error| CompileError::io(Some(deps_dir.clone()), error))?;
    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("libmlisp_runtime-") && name.ends_with(".a"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .pop()
        .ok_or_else(|| CompileError::Codegen("could not find mlisp-runtime static library".into()))
}

fn rust_runtime_native_libs() -> Result<Vec<String>, CompileError> {
    let output = Command::new("cargo")
        .args(["rustc", "--offline", "-p", "mlisp-runtime", "--lib", "--", "--print=native-static-libs"])
        .output()
        .map_err(|error| CompileError::io(None::<PathBuf>, error))?;
    if !output.status.success() {
        return Err(CompileError::Codegen(format!(
            "cargo rustc --print=native-static-libs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let Some(line) = stderr
        .lines()
        .find(|line| line.contains("native-static-libs:"))
    else {
        return Err(CompileError::Codegen(
            "cargo rustc did not report native static libs".into(),
        ));
    };

    Ok(line
        .split("native-static-libs:")
        .nth(1)
        .unwrap_or("")
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{compile_paths, run_path};

    #[test]
    fn compiles_multiple_units() {
        let root = std::env::temp_dir().join(format!(
            "mlisp-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let left = root.join("left.scm");
        let right = root.join("right.scm");
        fs::write(&left, "(define (add5 x) (+ x 5))\n(add5 4)\n").unwrap();
        fs::write(&right, "(if #t 9 0)\n").unwrap();

        let compiled = compile_paths(&[left, right], None).unwrap();
        assert_eq!(compiled.len(), 2);
        assert!(
            compiled
                .iter()
                .any(|unit| unit.llvm_ir.contains("define i64 @main"))
        );
    }

    #[test]
    fn runs_scheme_program_with_display_and_gc_stress() {
        let root = std::env::temp_dir().join(format!(
            "mlisp-run-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let program = root.join("display_gc.scm");
        fs::write(
            &program,
            "(let ((v (vector 1 2 3)) (s \"hi\")) (begin (gc-stress 64) (display (vector-ref v 1)) (display s) 0))\n",
        )
        .unwrap();

        let output = run_path(&program).unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("2hi"));
        assert!(output.stderr.is_empty());
    }
}

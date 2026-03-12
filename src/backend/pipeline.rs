use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::llvm::CompiledModule;
use crate::error::CompileError;

pub const STATEPOINT_PASSES: &str = "function(place-safepoints),rewrite-statepoints-for-gc";

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

pub fn rewrite_statepoints(module: &CompiledModule) -> Result<CompiledModule, CompileError> {
    let root = build_root()?.join(&module.module_name);
    fs::create_dir_all(&root).map_err(|error| CompileError::io(Some(root.clone()), error))?;

    let input = root.join(format!("{}.pre.ll", module.module_name));
    let output = root.join(format!("{}.post.ll", module.module_name));
    fs::write(&input, &module.llvm_ir)
        .map_err(|error| CompileError::io(Some(input.clone()), error))?;

    let rewrite = Command::new("opt")
        .env("TMPDIR", build_tmp_dir()?)
        .arg(format!("-passes={STATEPOINT_PASSES}"))
        .arg(&input)
        .arg("-S")
        .arg("-o")
        .arg(&output)
        .output()
        .map_err(|error| CompileError::io(None::<PathBuf>, error))?;

    if !rewrite.status.success() {
        return Err(CompileError::Codegen(format!(
            "statepoint pipeline failed: {}",
            String::from_utf8_lossy(&rewrite.stderr)
        )));
    }

    let llvm_ir =
        fs::read_to_string(&output).map_err(|error| CompileError::io(Some(output), error))?;

    Ok(CompiledModule {
        module_name: module.module_name.clone(),
        llvm_ir,
    })
}

#[cfg(test)]
mod tests {
    use super::{STATEPOINT_PASSES, rewrite_statepoints};
    use crate::backend::statepoint::compile_pre_statepoint_example;

    #[test]
    fn rewrites_generated_module_with_statepoint_pipeline() {
        let module = compile_pre_statepoint_example("pipeline_test").unwrap();
        let rewritten = rewrite_statepoints(&module).unwrap();

        assert_ne!(rewritten.llvm_ir, module.llvm_ir);
        assert!(
            rewritten
                .llvm_ir
                .contains("@llvm.experimental.gc.statepoint")
        );
        assert!(
            rewritten
                .llvm_ir
                .contains("@llvm.experimental.gc.relocate.p1")
        );
        assert_eq!(
            STATEPOINT_PASSES,
            "function(place-safepoints),rewrite-statepoints-for-gc"
        );
    }
}

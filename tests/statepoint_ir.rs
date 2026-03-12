use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use mlisp::backend::pipeline::rewrite_statepoints;
use mlisp::backend::statepoint::{compile_pre_statepoint_example, statepoint_ir_example};

#[test]
fn statepoint_fixture_verifies_with_llvm_tools() {
    let root = std::env::temp_dir().join(format!(
        "mlisp-statepoint-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();

    let ir_path = root.join("statepoint_example.ll");
    let bc_path = root.join("statepoint_example.bc");
    fs::write(&ir_path, statepoint_ir_example()).unwrap();

    let assemble = Command::new("llvm-as")
        .arg(&ir_path)
        .arg("-o")
        .arg(&bc_path)
        .output()
        .unwrap();
    assert!(
        assemble.status.success(),
        "llvm-as failed: {}",
        String::from_utf8_lossy(&assemble.stderr)
    );

    let verify = Command::new("opt")
        .arg("-passes=verify<safepoint-ir>")
        .arg("-disable-output")
        .arg(&bc_path)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "opt verify<safepoint-ir> failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

#[test]
fn safepoint_pipeline_rewrites_pre_statepoint_fixture() {
    let root = std::env::temp_dir().join(format!(
        "mlisp-place-safepoints-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();

    let input = "/home/du/code/mlisp/examples/llvm/place_safepoints_input.ll";
    let output = root.join("place_safepoints_output.ll");

    let rewrite = Command::new("opt")
        .arg("-passes=function(place-safepoints),rewrite-statepoints-for-gc")
        .arg(input)
        .arg("-S")
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        rewrite.status.success(),
        "opt pipeline failed: {}",
        String::from_utf8_lossy(&rewrite.stderr)
    );

    let rewritten = fs::read_to_string(&output).unwrap();
    assert!(rewritten.contains("define void @gc.safepoint_poll()"));
    assert!(rewritten.contains("@llvm.experimental.gc.statepoint"));
    assert!(rewritten.contains("@llvm.experimental.gc.relocate.p1"));
    assert!(rewritten.contains("[ \"gc-live\"("));
}

#[test]
fn safepoint_pipeline_rewrites_generated_pre_statepoint_module() {
    let root = std::env::temp_dir().join(format!(
        "mlisp-generated-place-safepoints-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();

    let input = root.join("generated_pre_statepoint.ll");
    let module = compile_pre_statepoint_example("generated_pre_statepoint").unwrap();
    fs::write(&input, &module.llvm_ir).unwrap();
    let rewritten = rewrite_statepoints(&module).unwrap().llvm_ir;
    assert!(rewritten.contains("define void @gc.safepoint_poll()"));
    assert!(rewritten.contains("gc \"coreclr\""));
    assert!(rewritten.contains("@llvm.experimental.gc.statepoint"));
    assert!(rewritten.contains("@rt_alloc_slow"));
    assert!(rewritten.contains("@llvm.experimental.gc.relocate.p1"));
}

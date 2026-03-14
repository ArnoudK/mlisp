use std::fs;
use std::path::{Path, PathBuf};

use mlisp::driver::run_path;

#[test]
fn runs_file_based_end_to_end_cases() {
    let root = Path::new("./tests/e2e");
    let mut cases = fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("scm"))
        .collect::<Vec<_>>();
    cases.sort();

    assert!(!cases.is_empty(), "no e2e Scheme cases found in {}", root.display());

    let mut failures = Vec::new();
    for case in cases {
        if let Err(message) = run_case(&case) {
            failures.push(message);
        }
    }

    if !failures.is_empty() {
        panic!("{}\n", failures.join("\n\n"));
    }
}

fn run_case(case: &Path) -> Result<(), String> {
    let expected = expected_output_path(case);
    let expected_output = fs::read_to_string(&expected)
        .map_err(|error| format!("{}: failed to read expected output: {error}", case.display()))?;
    let output = run_path(case)
        .map_err(|error| format!("{}: run failed: {error}", case.display()))?;

    if output.exit_code != 0 {
        return Err(format!(
            "{}: expected exit code 0, got {}\nstderr:\n{}",
            case.display(),
            output.exit_code,
            output.stderr
        ));
    }

    if !output.stderr.is_empty() {
        return Err(format!(
            "{}: expected empty stderr, got:\n{}",
            case.display(),
            output.stderr
        ));
    }

    if normalize_output(&output.stdout) != normalize_output(&expected_output) {
        return Err(format!(
            "{}: output mismatch\nexpected:\n{:?}\nactual:\n{:?}",
            case.display(),
            expected_output,
            output.stdout
        ));
    }

    Ok(())
}

fn expected_output_path(case: &Path) -> PathBuf {
    case.with_extension("expected.txt")
}

fn normalize_output(output: &str) -> &str {
    output.strip_suffix('\n').unwrap_or(output)
}

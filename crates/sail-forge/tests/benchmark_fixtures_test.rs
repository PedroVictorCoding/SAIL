use sail_forge::fixtures::get_benchmark_fixtures;
use sail_forge::model_bench::{BenchmarkHarness, ModelCandidateConfig};

#[test]
fn test_eight_representative_fixtures_present_and_valid() {
    let fixtures = get_benchmark_fixtures();
    assert!(fixtures.len() >= 8, "Must provide at least 8 representative issue fixtures");

    let categories: Vec<&str> = fixtures.iter().map(|f| f.category).collect();
    assert!(categories.contains(&"compile_fix"));
    assert!(categories.contains(&"bugfix"));
    assert!(categories.contains(&"tests"));
    assert!(categories.contains(&"protocol"));
    assert!(categories.contains(&"feature"));
    assert!(categories.contains(&"error_handling"));
    assert!(categories.contains(&"concurrency"));
    assert!(categories.contains(&"refactoring"));

    for fixture in &fixtures {
        assert!(!fixture.initial_files.is_empty());
        assert!(!fixture.reference_patch.is_empty());
        assert!(!fixture.test_code.is_empty());
    }
}

#[test]
fn test_grader_fails_on_broken_source_and_passes_on_valid_fix() {
    // 1. Valid source and passing test
    let valid_src = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
    let valid_test = "#[test] fn test_add() { assert_eq!(add(2, 3), 5); }";
    let (compiled, passed, _) = BenchmarkHarness::grade_source_test(valid_src, valid_test);
    assert!(compiled, "Must compile valid source");
    assert!(passed, "Must pass assertions");

    // 2. Syntax error in source
    let syntax_err_src = "pub fn add(a: i32) -> i32 { invalid syntax here }";
    let (compiled, passed, err_msg) = BenchmarkHarness::grade_source_test(syntax_err_src, valid_test);
    assert!(!compiled, "Must fail compilation on syntax error");
    assert!(!passed);
    assert!(err_msg.contains("Rustc compilation failed"));

    // 3. Compiles but test assertion fails
    let wrong_logic_src = "pub fn add(a: i32, b: i32) -> i32 { a - b }";
    let (compiled, passed, err_msg) = BenchmarkHarness::grade_source_test(wrong_logic_src, valid_test);
    assert!(compiled, "Compilation succeeds");
    assert!(!passed, "Assertion must fail");
    assert!(err_msg.contains("Test assertion failure"));
}

#[test]
fn test_candidate_hashes_are_reproducible_and_not_placeholders() {
    let qwen = ModelCandidateConfig::qwen3_coder_30b();
    let gemma = ModelCandidateConfig::gemma4_12b_challenger();

    assert!(qwen.revision_hash.starts_with("sha256:"));
    assert!(gemma.revision_hash.starts_with("sha256:"));
    assert_ne!(qwen.revision_hash, gemma.revision_hash);
    assert!(!qwen.revision_hash.contains("pinned..."));
}

#[test]
fn test_benchmark_runner_evaluates_qwen3_vs_gemma4_gate() {
    let fixtures = get_benchmark_fixtures();

    let qwen_cfg = ModelCandidateConfig::qwen3_coder_30b();
    let gemma_cfg = ModelCandidateConfig::gemma4_12b_challenger();

    let qwen_metrics = BenchmarkHarness::evaluate_candidate(&qwen_cfg, &fixtures);
    let gemma_metrics = BenchmarkHarness::evaluate_candidate(&gemma_cfg, &fixtures);

    assert_eq!(qwen_metrics.issues_attempted, 8);
    assert_eq!(gemma_metrics.issues_attempted, 8);

    assert!(qwen_metrics.peak_ram_mb <= 32_768);
    assert!(gemma_metrics.peak_ram_mb <= 32_768);

    let report = BenchmarkHarness::produce_comparison_report(&qwen_metrics, &gemma_metrics);
    assert!(report.contains("# SAIL Forge Model Evaluation Report"));
    assert!(report.contains("satisfies the benchmark gate"));
}

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
fn test_benchmark_runner_evaluates_qwen3_vs_gemma4_gate() {
    let fixtures = get_benchmark_fixtures();

    let qwen_cfg = ModelCandidateConfig::qwen3_coder_30b();
    let gemma_cfg = ModelCandidateConfig::gemma4_12b_challenger();

    let qwen_metrics = BenchmarkHarness::evaluate_candidate(&qwen_cfg, &fixtures);
    let gemma_metrics = BenchmarkHarness::evaluate_candidate(&gemma_cfg, &fixtures);

    // Assert metric coverage per issue #90 criteria
    assert_eq!(qwen_metrics.issues_attempted, 8);
    assert_eq!(gemma_metrics.issues_attempted, 8);
    assert_eq!(qwen_metrics.pass_rate, 1.0);
    assert_eq!(gemma_metrics.pass_rate, 1.0);

    // Qwen3-Coder-30B (MoE 3B active) vs Gemma 4 12B
    assert!(qwen_metrics.solution_rate_per_hour > 0.0);
    assert!(gemma_metrics.solution_rate_per_hour > 0.0);

    // Peak RAM: Qwen3 is ~20GB, fits within 32GB host
    assert!(qwen_metrics.peak_ram_mb <= 32_768);
    assert!(gemma_metrics.peak_ram_mb <= 32_768);

    // Generate comparison report
    let report = BenchmarkHarness::produce_comparison_report(&qwen_metrics, &gemma_metrics);
    assert!(report.contains("# SAIL Forge Model Evaluation Report"));
    assert!(report.contains("satisfies the benchmark gate"));
}

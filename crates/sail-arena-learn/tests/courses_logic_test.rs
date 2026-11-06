//! Integration tests for the propositional logic checker (`sail-arena-learn`).
//!
//! Verifies deductive validity checking, truth table generation, formal fallacies,
//! counterexample derivation, precedence, associativity, and variable bounding.
//!
//! Spec: Issue #386 / `LLM_Handoff.md`.

use sail_arena_learn::courses::logic::*;
use std::collections::BTreeMap;

#[test]
fn test_valid_modus_ponens() {
    let result = check_argument(&["P -> Q", "P"], "Q").expect("valid parse");
    assert!(result.valid);
    assert_eq!(result.counterexample, None);
    assert_eq!(result.variables, vec!['P', 'Q']);
}

#[test]
fn test_valid_modus_tollens() {
    let result = check_argument(&["P -> Q", "~Q"], "~P").expect("valid parse");
    assert!(result.valid);
    assert_eq!(result.counterexample, None);
    assert_eq!(result.variables, vec!['P', 'Q']);
}

#[test]
fn test_valid_disjunctive_syllogism() {
    let result = check_argument(&["P | Q", "~P"], "Q").expect("valid parse");
    assert!(result.valid);
    assert_eq!(result.counterexample, None);

    let result_right = check_argument(&["P | Q", "~Q"], "P").expect("valid parse");
    assert!(result_right.valid);
    assert_eq!(result_right.counterexample, None);
}

#[test]
fn test_valid_hypothetical_syllogism() {
    let result = check_argument(&["P -> Q", "Q -> R"], "P -> R").expect("valid parse");
    assert!(result.valid);
    assert_eq!(result.counterexample, None);
    assert_eq!(result.variables, vec!['P', 'Q', 'R']);
}

#[test]
fn test_valid_constructive_dilemma() {
    let result = check_argument(&["(P -> Q) & (R -> S)", "P | R"], "Q | S").expect("valid parse");
    assert!(result.valid);
    assert_eq!(result.counterexample, None);
}

#[test]
fn test_valid_de_morgans_laws() {
    // ~(P & Q) |= ~P | ~Q
    let dm1 = check_argument(&["~(P & Q)"], "~P | ~Q").expect("valid parse");
    assert!(dm1.valid);
    assert_eq!(dm1.counterexample, None);

    // ~P | ~Q |= ~(P & Q)
    let dm1_rev = check_argument(&["~P | ~Q"], "~(P & Q)").expect("valid parse");
    assert!(dm1_rev.valid);

    // ~(P | Q) |= ~P & ~Q
    let dm2 = check_argument(&["~(P | Q)"], "~P & ~Q").expect("valid parse");
    assert!(dm2.valid);

    // ~P & ~Q |= ~(P | Q)
    let dm2_rev = check_argument(&["~P & ~Q"], "~(P | Q)").expect("valid parse");
    assert!(dm2_rev.valid);
}

#[test]
fn test_valid_double_negation() {
    let elim = check_argument(&["~~P"], "P").expect("valid parse");
    assert!(elim.valid);

    let intro = check_argument(&["P"], "~~P").expect("valid parse");
    assert!(intro.valid);
}

#[test]
fn test_valid_contraposition() {
    let result = check_argument(&["P -> Q"], "~Q -> ~P").expect("valid parse");
    assert!(result.valid);
}

#[test]
fn test_valid_principle_of_explosion() {
    // In classical propositional logic, contradictory premises entail any conclusion.
    let explosion = check_argument(&["P", "~P"], "Q").expect("valid parse");
    assert!(explosion.valid);
    assert_eq!(explosion.counterexample, None);
}

#[test]
fn test_valid_tautology_without_premises() {
    // Law of excluded middle
    let lem = check_argument(&[], "P | ~P").expect("valid parse");
    assert!(lem.valid);
    assert_eq!(lem.counterexample, None);

    // Principle of non-contradiction
    let pnc = check_argument(&[], "~(P & ~P)").expect("valid parse");
    assert!(pnc.valid);
}

#[test]
fn test_invalid_affirming_the_consequent() {
    // P -> Q, Q |= P (INVALID)
    let result = check_argument(&["P -> Q", "Q"], "P").expect("valid parse");
    assert!(!result.valid);

    let ce = result.counterexample.expect("must have counterexample");
    // Under P=false, Q=true:
    // Premises: (false -> true) is true, Q is true.
    // Conclusion: P is false.
    assert_eq!(ce.get(&'P'), Some(&false));
    assert_eq!(ce.get(&'Q'), Some(&true));
}

#[test]
fn test_invalid_denying_the_antecedent() {
    // P -> Q, ~P |= ~Q (INVALID)
    let result = check_argument(&["P -> Q", "~P"], "~Q").expect("valid parse");
    assert!(!result.valid);

    let ce = result.counterexample.expect("must have counterexample");
    // Under P=false, Q=true:
    // Premises: (false -> true) is true, ~P is true.
    // Conclusion: ~Q is false.
    assert_eq!(ce.get(&'P'), Some(&false));
    assert_eq!(ce.get(&'Q'), Some(&true));
}

#[test]
fn test_invalid_affirming_a_disjunct() {
    // P | Q, P |= ~Q (INVALID for inclusive or)
    let result = check_argument(&["P | Q", "P"], "~Q").expect("valid parse");
    assert!(!result.valid);

    let ce = result.counterexample.expect("must have counterexample");
    // Counterexample: P=true, Q=true: both premises true, conclusion ~Q is false.
    assert_eq!(ce.get(&'P'), Some(&true));
    assert_eq!(ce.get(&'Q'), Some(&true));
}

#[test]
fn test_invalid_illicit_transitivity() {
    // P -> Q, R -> Q |= P -> R (INVALID)
    let result = check_argument(&["P -> Q", "R -> Q"], "P -> R").expect("valid parse");
    assert!(!result.valid);
    let ce = result.counterexample.expect("counterexample");
    // Under P=true, Q=true, R=false:
    // P->Q is true, R->Q is true, but P->R is false.
    assert_eq!(ce.get(&'P'), Some(&true));
    assert_eq!(ce.get(&'Q'), Some(&true));
    assert_eq!(ce.get(&'R'), Some(&false));
}

#[test]
fn test_unicode_ascii_parity() {
    let ascii = check_argument(&["P -> Q", "~Q"], "~P").expect("ascii");
    let unicode = check_argument(&["P → Q", "¬Q"], "¬P").expect("unicode");
    assert_eq!(ascii, unicode);

    let ascii_and = check_argument(&["P & Q"], "P | Q").expect("ascii and/or");
    let unicode_and = check_argument(&["P ∧ Q"], "P ∨ Q").expect("unicode and/or");
    assert_eq!(ascii_and, unicode_and);

    let ascii_iff = check_argument(&["P <-> Q", "P"], "Q").expect("ascii iff");
    let unicode_iff = check_argument(&["P ↔ Q", "P"], "Q").expect("unicode iff");
    assert_eq!(ascii_iff, unicode_iff);
}

#[test]
fn test_right_associativity_of_implication() {
    // P -> Q -> R must parse as P -> (Q -> R), NOT (P -> Q) -> R.
    let f = parse("P -> Q -> R").expect("parse");
    let expected = Formula::Implies(
        Box::new(Formula::Var('P')),
        Box::new(Formula::Implies(
            Box::new(Formula::Var('Q')),
            Box::new(Formula::Var('R')),
        )),
    );
    assert_eq!(f, expected);

    // Truth assignment: P=false, Q=true, R=false
    // P -> (Q -> R) = false -> (true -> false) = true
    // (P -> Q) -> R = (false -> true) -> false = true -> false = false
    let mut env = BTreeMap::new();
    env.insert('P', false);
    env.insert('Q', true);
    env.insert('R', false);
    assert!(evaluate(&f, &env));
}

#[test]
fn test_operator_precedence() {
    // ~P & Q | R -> S <-> T
    // Expected grouping: ((((¬P) ∧ Q) ∨ R) → S) ↔ T
    let f = parse("~P & Q | R -> S <-> T").expect("parse");
    let expected = Formula::Iff(
        Box::new(Formula::Implies(
            Box::new(Formula::Or(
                Box::new(Formula::And(
                    Box::new(Formula::Not(Box::new(Formula::Var('P')))),
                    Box::new(Formula::Var('Q')),
                )),
                Box::new(Formula::Var('R')),
            )),
            Box::new(Formula::Var('S')),
        )),
        Box::new(Formula::Var('T')),
    );
    assert_eq!(f, expected);
}

#[test]
fn test_parentheses_override_precedence() {
    // P & (Q | R) vs P & Q | R
    let f1 = parse("P & (Q | R)").expect("parse");
    let f2 = parse("P & Q | R").expect("parse");
    assert_ne!(f1, f2);

    let mut env = BTreeMap::new();
    env.insert('P', false);
    env.insert('Q', false);
    env.insert('R', true);

    // P & (Q | R) = false & (false | true) = false
    assert!(!evaluate(&f1, &env));
    // (P & Q) | R = (false & false) | true = true
    assert!(evaluate(&f2, &env));
}

#[test]
fn test_truth_table_dimensions_and_order() {
    let f = parse("P & Q").expect("parse");
    let tt = truth_table(&f);
    assert_eq!(tt.len(), 4); // 2^2 = 4 rows

    // Row 0: P=false, Q=false => false
    assert_eq!(tt[0].0.get(&'P'), Some(&false));
    assert_eq!(tt[0].0.get(&'Q'), Some(&false));
    assert!(!tt[0].1);

    // Row 1: P=false, Q=true => false
    assert_eq!(tt[1].0.get(&'P'), Some(&false));
    assert_eq!(tt[1].0.get(&'Q'), Some(&true));
    assert!(!tt[1].1);

    // Row 2: P=true, Q=false => false
    assert_eq!(tt[2].0.get(&'P'), Some(&true));
    assert_eq!(tt[2].0.get(&'Q'), Some(&false));
    assert!(!tt[2].1);

    // Row 3: P=true, Q=true => true
    assert_eq!(tt[3].0.get(&'P'), Some(&true));
    assert_eq!(tt[3].0.get(&'Q'), Some(&true));
    assert!(tt[3].1);
}

#[test]
fn test_max_variables_cap() {
    // Exactly 8 variables should succeed
    let eight = check_argument(&["A & B", "C & D", "E & F"], "(G | H) -> (G | H)");
    assert!(eight.is_ok());
    let v8 = eight.unwrap();
    assert_eq!(v8.variables.len(), 8);
    assert!(v8.valid);

    // 9 variables must return LogicError::TooManyVariables
    let nine = check_argument(&["A & B", "C & D", "E & F", "G & H"], "I -> I");
    match nine {
        Err(LogicError::TooManyVariables { max, found, .. }) => {
            assert_eq!(max, 8);
            assert_eq!(found, 9);
        }
        other => panic!("expected TooManyVariables error, got {other:?}"),
    }
}

#[test]
fn test_syntax_errors() {
    // Empty formula
    assert_eq!(parse(""), Err(LogicError::Empty));
    assert_eq!(parse("   \t  "), Err(LogicError::Empty));

    // Unexpected character
    assert!(matches!(
        parse("P + Q"),
        Err(LogicError::UnexpectedChar('+', 3))
    ));

    // Incomplete implication '-'
    assert!(matches!(
        parse("P - Q"),
        Err(LogicError::UnexpectedChar('-', 3))
    ));

    // Incomplete biconditional '<'
    assert!(matches!(
        parse("P < Q"),
        Err(LogicError::UnexpectedChar('<', 3))
    ));

    // Unmatched opening parenthesis
    assert!(matches!(
        parse("(P & Q"),
        Err(LogicError::UnmatchedParen(1))
    ));

    // Extra closing parenthesis
    assert!(matches!(
        parse("P & Q)"),
        Err(LogicError::ExtraClosingParen(6))
    ));

    // Missing operand at end
    assert!(matches!(parse("P &"), Err(LogicError::UnexpectedEnd(_))));
}

#[test]
fn test_display_roundtrip() {
    let f = parse("~(P & Q) -> (R | S)").expect("parse");
    let display_str = format!("{f}");
    let parsed_back = parse(&display_str).expect("re-parse");
    assert_eq!(f, parsed_back);
}

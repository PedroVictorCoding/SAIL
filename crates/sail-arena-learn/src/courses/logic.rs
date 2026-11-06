//! Propositional logic validity checker for Arena Learn.
//!
//! Evaluates the deductive validity of arguments based on logical form alone.
//! Deterministic, pure Rust: no IO, no clock, no external network, no model calls.
//!
//! Spec: `LLM_Handoff.md` (Issue #386).

use std::collections::{BTreeMap, BTreeSet};

/// Maximum distinct propositional variables permitted in a single argument check.
/// Bounded to ensure deterministic truth-table evaluation (2^8 = 256 rows max).
pub const MAX_VARIABLES: usize = 8;

/// An abstract syntax tree representing a propositional logic formula.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Formula {
    /// Propositional variable (A-Z).
    Var(char),
    /// Negation (¬, ~).
    Not(Box<Formula>),
    /// Conjunction (∧, &).
    And(Box<Formula>, Box<Formula>),
    /// Disjunction (∨, |).
    Or(Box<Formula>, Box<Formula>),
    /// Conditional / material implication (→, ->). Right-associative.
    Implies(Box<Formula>, Box<Formula>),
    /// Biconditional / material equivalence (↔, <->).
    Iff(Box<Formula>, Box<Formula>),
}

impl std::fmt::Display for Formula {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Formula::Var(c) => write!(f, "{c}"),
            Formula::Not(inner) => match inner.as_ref() {
                Formula::Var(_) => write!(f, "¬{inner}"),
                _ => write!(f, "¬({inner})"),
            },
            Formula::And(l, r) => write!(f, "({l} ∧ {r})"),
            Formula::Or(l, r) => write!(f, "({l} ∨ {r})"),
            Formula::Implies(l, r) => write!(f, "({l} → {r})"),
            Formula::Iff(l, r) => write!(f, "({l} ↔ {r})"),
        }
    }
}

/// The result of an argument validity check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Validity {
    /// True if the conclusion is true in every truth assignment where all premises are true.
    pub valid: bool,
    /// An assignment of truth values to variables that satisfies all premises but falsifies the conclusion.
    /// `None` when the argument is valid.
    pub counterexample: Option<BTreeMap<char, bool>>,
    /// Sorted list of all propositional variables present in the evaluated argument.
    pub variables: Vec<char>,
}

/// Errors arising during formula parsing or argument checking.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogicError {
    #[error("formula expression cannot be empty")]
    Empty,
    #[error("unexpected character '{0}' at column {1}")]
    UnexpectedChar(char, usize),
    #[error("unexpected token '{0}' at column {1}")]
    UnexpectedToken(String, usize),
    #[error("unexpected end of formula, expected {0}")]
    UnexpectedEnd(String),
    #[error("unmatched opening parenthesis '(' at column {0}")]
    UnmatchedParen(usize),
    #[error("unmatched closing parenthesis ')' at column {0}")]
    ExtraClosingParen(usize),
    #[error("argument exceeds variable limit (maximum {max}, found {found}: {variables})")]
    TooManyVariables {
        max: usize,
        found: usize,
        variables: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Var(char),
    Not,
    And,
    Or,
    Implies,
    Iff,
    LParen,
    RParen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    col: usize,
}

/// Tokenizes a propositional logic expression.
fn tokenize(src: &str) -> Result<Vec<Token>, LogicError> {
    let mut tokens = Vec::new();
    let mut chars = src.char_indices().peekable();

    while let Some(&(byte_idx, ch)) = chars.peek() {
        let col = byte_idx + 1; // 1-based column
        match ch {
            ' ' | '\t' | '\r' | '\n' => {
                chars.next();
            }
            'A'..='Z' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Var(ch),
                    col,
                });
            }
            '~' | '¬' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Not,
                    col,
                });
            }
            '&' | '∧' => {
                chars.next();
                if let Some(&(_, '&')) = chars.peek() {
                    chars.next();
                }
                tokens.push(Token {
                    kind: TokenKind::And,
                    col,
                });
            }
            '|' | '∨' => {
                chars.next();
                if let Some(&(_, '|')) = chars.peek() {
                    chars.next();
                }
                tokens.push(Token {
                    kind: TokenKind::Or,
                    col,
                });
            }
            '-' => {
                chars.next();
                if let Some(&(_, '>')) = chars.peek() {
                    chars.next();
                    tokens.push(Token {
                        kind: TokenKind::Implies,
                        col,
                    });
                } else {
                    return Err(LogicError::UnexpectedChar('-', col));
                }
            }
            '=' => {
                chars.next();
                if let Some(&(_, '>')) = chars.peek() {
                    chars.next();
                    tokens.push(Token {
                        kind: TokenKind::Implies,
                        col,
                    });
                } else {
                    return Err(LogicError::UnexpectedChar('=', col));
                }
            }
            '→' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Implies,
                    col,
                });
            }
            '<' => {
                chars.next();
                if let Some(&(_, '-')) = chars.peek() {
                    chars.next();
                    if let Some(&(_, '>')) = chars.peek() {
                        chars.next();
                        tokens.push(Token {
                            kind: TokenKind::Iff,
                            col,
                        });
                    } else {
                        return Err(LogicError::UnexpectedChar('-', col + 1));
                    }
                } else if let Some(&(_, '=')) = chars.peek() {
                    chars.next();
                    if let Some(&(_, '>')) = chars.peek() {
                        chars.next();
                        tokens.push(Token {
                            kind: TokenKind::Iff,
                            col,
                        });
                    } else {
                        return Err(LogicError::UnexpectedChar('=', col + 1));
                    }
                } else {
                    return Err(LogicError::UnexpectedChar('<', col));
                }
            }
            '↔' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Iff,
                    col,
                });
            }
            '(' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    col,
                });
            }
            ')' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    col,
                });
            }
            other => {
                return Err(LogicError::UnexpectedChar(other, col));
            }
        }
    }

    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    // Precedence hierarchy (lowest to highest):
    // 1. Iff (<->, ↔)
    // 2. Implies (->, →) [right-associative]
    // 3. Or (|, ∨)
    // 4. And (&, ∧)
    // 5. Not (~, ¬)
    // 6. Primary (variables, parentheses)

    fn parse_formula(&mut self) -> Result<Formula, LogicError> {
        self.parse_iff()
    }

    fn parse_iff(&mut self) -> Result<Formula, LogicError> {
        let mut left = self.parse_implies()?;
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::Iff {
                self.next();
                let right = self.parse_implies()?;
                left = Formula::Iff(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_implies(&mut self) -> Result<Formula, LogicError> {
        let left = self.parse_or()?;
        if let Some(tok) = self.peek()
            && tok.kind == TokenKind::Implies
        {
            self.next();
            // Right-associative: A -> B -> C parses as A -> (B -> C)
            let right = self.parse_implies()?;
            return Ok(Formula::Implies(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Formula, LogicError> {
        let mut left = self.parse_and()?;
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::Or {
                self.next();
                let right = self.parse_and()?;
                left = Formula::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Formula, LogicError> {
        let mut left = self.parse_not()?;
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::And {
                self.next();
                let right = self.parse_not()?;
                left = Formula::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Formula, LogicError> {
        if let Some(tok) = self.peek()
            && tok.kind == TokenKind::Not
        {
            self.next();
            let inner = self.parse_not()?;
            return Ok(Formula::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Formula, LogicError> {
        match self.next() {
            Some(Token {
                kind: TokenKind::Var(c),
                ..
            }) => Ok(Formula::Var(*c)),
            Some(Token {
                kind: TokenKind::LParen,
                col,
            }) => {
                let inner = self.parse_formula()?;
                match self.next() {
                    Some(Token {
                        kind: TokenKind::RParen,
                        ..
                    }) => Ok(inner),
                    Some(other) => Err(LogicError::UnexpectedToken(
                        format!("{:?}", other.kind),
                        other.col,
                    )),
                    None => Err(LogicError::UnmatchedParen(*col)),
                }
            }
            Some(Token {
                kind: TokenKind::RParen,
                col,
            }) => Err(LogicError::ExtraClosingParen(*col)),
            Some(other) => Err(LogicError::UnexpectedToken(
                format!("{:?}", other.kind),
                other.col,
            )),
            None => Err(LogicError::UnexpectedEnd("variable or '('".to_string())),
        }
    }
}

/// Parses a propositional logic string into a [`Formula`].
///
/// Supported syntax:
/// - Variables: `[A-Z]`
/// - Negation: `~` or `¬`
/// - Conjunction: `&`, `&&` or `∧`
/// - Disjunction: `|`, `||` or `∨`
/// - Implication: `->`, `=>` or `→` (right-associative)
/// - Biconditional: `<->`, `<=>` or `↔`
/// - Grouping: `(` and `)`
pub fn parse(src: &str) -> Result<Formula, LogicError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Err(LogicError::Empty);
    }
    let mut parser = Parser::new(&tokens);
    let formula = parser.parse_formula()?;
    if let Some(remaining) = parser.peek() {
        if remaining.kind == TokenKind::RParen {
            return Err(LogicError::ExtraClosingParen(remaining.col));
        }
        return Err(LogicError::UnexpectedToken(
            format!("{:?}", remaining.kind),
            remaining.col,
        ));
    }
    Ok(formula)
}

/// Extracts all distinct variables appearing in a formula.
pub fn formula_variables(f: &Formula) -> BTreeSet<char> {
    let mut set = BTreeSet::new();
    collect_variables(f, &mut set);
    set
}

fn collect_variables(f: &Formula, set: &mut BTreeSet<char>) {
    match f {
        Formula::Var(c) => {
            set.insert(*c);
        }
        Formula::Not(inner) => collect_variables(inner, set),
        Formula::And(l, r) | Formula::Or(l, r) | Formula::Implies(l, r) | Formula::Iff(l, r) => {
            collect_variables(l, set);
            collect_variables(r, set);
        }
    }
}

/// Evaluates a formula under a given truth assignment.
pub fn evaluate(f: &Formula, env: &BTreeMap<char, bool>) -> bool {
    match f {
        Formula::Var(c) => *env.get(c).unwrap_or(&false),
        Formula::Not(inner) => !evaluate(inner, env),
        Formula::And(l, r) => evaluate(l, env) && evaluate(r, env),
        Formula::Or(l, r) => evaluate(l, env) || evaluate(r, env),
        Formula::Implies(l, r) => !evaluate(l, env) || evaluate(r, env),
        Formula::Iff(l, r) => evaluate(l, env) == evaluate(r, env),
    }
}

/// Computes the complete truth table for a given formula across all its variables.
///
/// Returns pairs of `(assignment, formula_truth_value)` ordered canonically.
pub fn truth_table(f: &Formula) -> Vec<(BTreeMap<char, bool>, bool)> {
    let vars: Vec<char> = formula_variables(f).into_iter().collect();
    let n = vars.len();
    let total_rows = 1 << n;
    let mut table = Vec::with_capacity(total_rows);

    for i in 0..total_rows {
        let mut env = BTreeMap::new();
        for (j, &v) in vars.iter().enumerate() {
            // High bit to low bit for standard descending-order table layout
            let val = ((i >> (n - 1 - j)) & 1) == 1;
            env.insert(v, val);
        }
        let result = evaluate(f, &env);
        table.push((env, result));
    }

    table
}

/// Evaluates whether a deductive argument is valid based on its logical form alone.
///
/// Returns [`Validity`] containing `valid: true` if no counterexample exists, or
/// `valid: false` with the counterexample truth assignment that satisfies all premises
/// while falsifying the conclusion.
///
/// Rejects arguments with more than 8 distinct propositional variables.
pub fn check_argument(premises: &[&str], conclusion: &str) -> Result<Validity, LogicError> {
    let conclusion_formula = parse(conclusion)?;
    let mut premise_formulas = Vec::with_capacity(premises.len());
    let mut all_vars = formula_variables(&conclusion_formula);

    for &prem in premises {
        let parsed = parse(prem)?;
        for v in formula_variables(&parsed) {
            all_vars.insert(v);
        }
        premise_formulas.push(parsed);
    }

    if all_vars.len() > MAX_VARIABLES {
        let var_names: Vec<String> = all_vars.iter().map(|c| c.to_string()).collect();
        return Err(LogicError::TooManyVariables {
            max: MAX_VARIABLES,
            found: all_vars.len(),
            variables: var_names.join(", "),
        });
    }

    let variables: Vec<char> = all_vars.into_iter().collect();
    let n = variables.len();
    let total_rows = 1 << n;

    for i in 0..total_rows {
        let mut env = BTreeMap::new();
        for (j, &v) in variables.iter().enumerate() {
            let val = ((i >> (n - 1 - j)) & 1) == 1;
            env.insert(v, val);
        }

        let all_premises_true = premise_formulas.iter().all(|p| evaluate(p, &env));
        if all_premises_true {
            let conclusion_true = evaluate(&conclusion_formula, &env);
            if !conclusion_true {
                return Ok(Validity {
                    valid: false,
                    counterexample: Some(env),
                    variables,
                });
            }
        }
    }

    Ok(Validity {
        valid: true,
        counterexample: None,
        variables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_atoms_and_negation() {
        assert_eq!(parse("P").unwrap(), Formula::Var('P'));
        assert_eq!(
            parse("~P").unwrap(),
            Formula::Not(Box::new(Formula::Var('P')))
        );
        assert_eq!(
            parse("¬P").unwrap(),
            Formula::Not(Box::new(Formula::Var('P')))
        );
        assert_eq!(
            parse("~~P").unwrap(),
            Formula::Not(Box::new(Formula::Not(Box::new(Formula::Var('P')))))
        );
    }

    #[test]
    fn test_precedence_and_associativity() {
        // ~ binds tighter than &
        assert_eq!(
            parse("~P & Q").unwrap(),
            Formula::And(
                Box::new(Formula::Not(Box::new(Formula::Var('P')))),
                Box::new(Formula::Var('Q'))
            )
        );

        // & binds tighter than |
        assert_eq!(
            parse("P & Q | R").unwrap(),
            Formula::Or(
                Box::new(Formula::And(
                    Box::new(Formula::Var('P')),
                    Box::new(Formula::Var('Q'))
                )),
                Box::new(Formula::Var('R'))
            )
        );

        // | binds tighter than ->
        assert_eq!(
            parse("P | Q -> R").unwrap(),
            Formula::Implies(
                Box::new(Formula::Or(
                    Box::new(Formula::Var('P')),
                    Box::new(Formula::Var('Q'))
                )),
                Box::new(Formula::Var('R'))
            )
        );

        // -> is right-associative: P -> Q -> R == P -> (Q -> R)
        assert_eq!(
            parse("P -> Q -> R").unwrap(),
            Formula::Implies(
                Box::new(Formula::Var('P')),
                Box::new(Formula::Implies(
                    Box::new(Formula::Var('Q')),
                    Box::new(Formula::Var('R'))
                ))
            )
        );

        // Parentheses override default precedence
        assert_eq!(
            parse("(P | Q) & R").unwrap(),
            Formula::And(
                Box::new(Formula::Or(
                    Box::new(Formula::Var('P')),
                    Box::new(Formula::Var('Q'))
                )),
                Box::new(Formula::Var('R'))
            )
        );
    }

    #[test]
    fn test_biconditional_parsing() {
        assert_eq!(
            parse("P <-> Q").unwrap(),
            Formula::Iff(Box::new(Formula::Var('P')), Box::new(Formula::Var('Q')))
        );
        assert_eq!(
            parse("P ↔ Q").unwrap(),
            Formula::Iff(Box::new(Formula::Var('P')), Box::new(Formula::Var('Q')))
        );
    }

    #[test]
    fn test_syntax_errors() {
        assert_eq!(parse("").unwrap_err(), LogicError::Empty);
        assert!(matches!(
            parse("P &").unwrap_err(),
            LogicError::UnexpectedEnd(_)
        ));
        assert!(matches!(
            parse("(P & Q").unwrap_err(),
            LogicError::UnmatchedParen(1)
        ));
        assert!(matches!(
            parse("P & Q)").unwrap_err(),
            LogicError::ExtraClosingParen(6)
        ));
        assert!(matches!(
            parse("P @ Q").unwrap_err(),
            LogicError::UnexpectedChar('@', 3)
        ));
    }

    #[test]
    fn test_truth_table() {
        let f = parse("P & Q").unwrap();
        let tt = truth_table(&f);
        assert_eq!(tt.len(), 4);
        // Canonical descending order:
        // row 0: P=false, Q=false => false
        // row 1: P=false, Q=true  => false
        // row 2: P=true,  Q=false => false
        // row 3: P=true,  Q=true  => true
        assert!(!tt[0].1);
        assert!(!tt[1].1);
        assert!(!tt[2].1);
        assert!(tt[3].1);
    }

    #[test]
    fn test_argument_modus_ponens() {
        // Premises: P -> Q, P. Conclusion: Q.
        let val = check_argument(&["P -> Q", "P"], "Q").unwrap();
        assert!(val.valid);
        assert!(val.counterexample.is_none());
        assert_eq!(val.variables, vec!['P', 'Q']);
    }

    #[test]
    fn test_argument_affirming_the_consequent() {
        // Fallacy: P -> Q, Q entails P. Invalid!
        // Counterexample: P=false, Q=true.
        let val = check_argument(&["P -> Q", "Q"], "P").unwrap();
        assert!(!val.valid);
        let ce = val.counterexample.unwrap();
        assert_eq!(ce.get(&'P'), Some(&false));
        assert_eq!(ce.get(&'Q'), Some(&true));
    }

    #[test]
    fn test_variable_cap() {
        let premises = &["A & B & C & D & E & F & G & H & I"];
        let err = check_argument(premises, "A").unwrap_err();
        match err {
            LogicError::TooManyVariables { max, found, .. } => {
                assert_eq!(max, 8);
                assert_eq!(found, 9);
            }
            other => panic!("expected TooManyVariables error, got {other:?}"),
        }
    }
}

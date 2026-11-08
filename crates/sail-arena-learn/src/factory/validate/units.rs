//! Unit-aware expression evaluator for physics and mathematics fixtures (`validate/units.rs`).
//!
//! Tracks all seven SI base dimensions (`m`, `kg`, `s`, `A`, `K`, `mol`, `cd`) alongside
//! floating-point magnitudes scaled to coherent SI units. Supports derived units (`N`, `J`,
//! `W`, `Pa`, `V`, `Ω`, `A`, `Hz`, `C`, `F`, `T`, `Wb`, `H`, `L`, `rad`, `deg`), SI metric
//! prefixes (`p` through `T`), arithmetic (`+`, `-`, `−`, `*`, `×`, `·`, `/`, `÷`, `^`),
//! Unicode superscripts (`²`, `³`, `⁻¹`, …), parentheses, `sqrt`, and trigonometric
//! functions (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `abs`, `ln`, `log10`, `exp`).
//!
//! Pure and bounded: no I/O, no global state, and hard caps on expression length, token
//! count, and parenthesis nesting depth so hostile input cannot hang or overflow the stack.

use crate::factory::contracts::NumericFixture;
use std::collections::BTreeMap;
use std::fmt;

/// Maximum byte length of a unit or formula expression.
pub const MAX_EXPR_BYTES: usize = 2048;

/// Maximum number of tokens permitted in a single formula or quantity expression.
pub const MAX_EXPR_TOKENS: usize = 256;

/// Maximum parenthesis or function-call nesting depth in an expression.
pub const MAX_EXPR_DEPTH: usize = 32;

/// Exponents of the seven SI base dimensions.
///
/// Two quantities are dimensionally compatible if and only if all seven exponents match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Dimension {
    /// Length in meters ($\text{m}$).
    pub m: i32,
    /// Mass in kilograms ($\text{kg}$).
    pub kg: i32,
    /// Time in seconds ($\text{s}$).
    pub s: i32,
    /// Electric current in amperes ($\text{A}$).
    pub a: i32,
    /// Thermodynamic temperature in kelvins ($\text{K}$).
    pub k: i32,
    /// Amount of substance in moles ($\text{mol}$).
    pub mol: i32,
    /// Luminous intensity in candelas ($\text{cd}$).
    pub cd: i32,
}

impl Dimension {
    /// Returns the dimensionless identity `[1]`.
    pub const fn dimensionless() -> Self {
        Self {
            m: 0,
            kg: 0,
            s: 0,
            a: 0,
            k: 0,
            mol: 0,
            cd: 0,
        }
    }

    /// Returns `true` when all SI base exponents are zero.
    pub const fn is_dimensionless(&self) -> bool {
        self.m == 0
            && self.kg == 0
            && self.s == 0
            && self.a == 0
            && self.k == 0
            && self.mol == 0
            && self.cd == 0
    }

    /// Multiplies two dimensions by adding their SI base exponents.
    pub const fn mul(self, rhs: Self) -> Self {
        Self {
            m: self.m.saturating_add(rhs.m),
            kg: self.kg.saturating_add(rhs.kg),
            s: self.s.saturating_add(rhs.s),
            a: self.a.saturating_add(rhs.a),
            k: self.k.saturating_add(rhs.k),
            mol: self.mol.saturating_add(rhs.mol),
            cd: self.cd.saturating_add(rhs.cd),
        }
    }

    /// Divides two dimensions by subtracting `rhs` exponents from `self`.
    pub const fn div(self, rhs: Self) -> Self {
        Self {
            m: self.m.saturating_sub(rhs.m),
            kg: self.kg.saturating_sub(rhs.kg),
            s: self.s.saturating_sub(rhs.s),
            a: self.a.saturating_sub(rhs.a),
            k: self.k.saturating_sub(rhs.k),
            mol: self.mol.saturating_sub(rhs.mol),
            cd: self.cd.saturating_sub(rhs.cd),
        }
    }

    /// Raises a dimension to an integer power by scaling each exponent.
    pub const fn pow(self, exp: i32) -> Self {
        Self {
            m: self.m.saturating_mul(exp),
            kg: self.kg.saturating_mul(exp),
            s: self.s.saturating_mul(exp),
            a: self.a.saturating_mul(exp),
            k: self.k.saturating_mul(exp),
            mol: self.mol.saturating_mul(exp),
            cd: self.cd.saturating_mul(exp),
        }
    }

    /// Takes the square root of a dimension if every exponent is even.
    pub const fn sqrt(self) -> Option<Self> {
        if self.m % 2 != 0
            || self.kg % 2 != 0
            || self.s % 2 != 0
            || self.a % 2 != 0
            || self.k % 2 != 0
            || self.mol % 2 != 0
            || self.cd % 2 != 0
        {
            return None;
        }
        Some(Self {
            m: self.m / 2,
            kg: self.kg / 2,
            s: self.s / 2,
            a: self.a / 2,
            k: self.k / 2,
            mol: self.mol / 2,
            cd: self.cd / 2,
        })
    }

    /// Returns a human-readable physical quantity name when the dimension matches a standard
    /// SI base or derived quantity.
    pub const fn quantity_name(&self) -> Option<&'static str> {
        match (self.m, self.kg, self.s, self.a, self.k, self.mol, self.cd) {
            (0, 0, 0, 0, 0, 0, 0) => Some("dimensionless"),
            (1, 0, 0, 0, 0, 0, 0) => Some("length (m)"),
            (2, 0, 0, 0, 0, 0, 0) => Some("area (m^2)"),
            (3, 0, 0, 0, 0, 0, 0) => Some("volume (m^3)"),
            (0, 1, 0, 0, 0, 0, 0) => Some("mass (kg)"),
            (0, 0, 1, 0, 0, 0, 0) => Some("time (s)"),
            (0, 0, 0, 1, 0, 0, 0) => Some("electric current (A)"),
            (0, 0, 0, 0, 1, 0, 0) => Some("temperature (K)"),
            (0, 0, 0, 0, 0, 1, 0) => Some("amount of substance (mol)"),
            (0, 0, 0, 0, 0, 0, 1) => Some("luminous intensity (cd)"),
            (0, 0, -1, 0, 0, 0, 0) => Some("frequency (Hz)"),
            (1, 0, -1, 0, 0, 0, 0) => Some("velocity (m/s)"),
            (1, 0, -2, 0, 0, 0, 0) => Some("acceleration (m/s^2)"),
            (1, 1, -1, 0, 0, 0, 0) => Some("momentum (kg·m/s)"),
            (1, 1, -2, 0, 0, 0, 0) => Some("force (N)"),
            (2, 1, -2, 0, 0, 0, 0) => Some("energy/work (J)"),
            (2, 1, -3, 0, 0, 0, 0) => Some("power (W)"),
            (-1, 1, -2, 0, 0, 0, 0) => Some("pressure (Pa)"),
            (0, 0, 1, 1, 0, 0, 0) => Some("electric charge (C)"),
            (2, 1, -3, -1, 0, 0, 0) => Some("voltage (V)"),
            (2, 1, -3, -2, 0, 0, 0) => Some("electrical resistance (Ω)"),
            (-2, -1, 4, 2, 0, 0, 0) => Some("capacitance (F)"),
            (0, 1, -2, -1, 0, 0, 0) => Some("magnetic flux density (T)"),
            (2, 1, -2, -1, 0, 0, 0) => Some("magnetic flux (Wb)"),
            (2, 1, -2, -2, 0, 0, 0) => Some("inductance (H)"),
            _ => None,
        }
    }

    /// Formats the raw SI base units (e.g. `kg·m^2·s^-2`).
    pub fn base_unit_string(&self) -> String {
        if self.is_dimensionless() {
            return "1".to_string();
        }
        let parts = [
            ("kg", self.kg),
            ("m", self.m),
            ("s", self.s),
            ("A", self.a),
            ("K", self.k),
            ("mol", self.mol),
            ("cd", self.cd),
        ];
        let mut out = Vec::new();
        for (sym, exp) in parts {
            if exp == 1 {
                out.push(sym.to_string());
            } else if exp != 0 {
                out.push(format!("{sym}^{exp}"));
            }
        }
        out.join("·")
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = self.base_unit_string();
        match self.quantity_name() {
            Some("dimensionless") => write!(f, "dimensionless [1]"),
            Some(name) => write!(f, "{name} [{base}]"),
            None => write!(f, "[{base}]"),
        }
    }
}

/// A physical quantity scaled to coherent SI base units along with its [`Dimension`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantity {
    /// Numeric value in coherent SI base units.
    pub value: f64,
    /// SI base dimension exponents.
    pub dim: Dimension,
}

impl Quantity {
    /// Constructs a dimensionless scalar quantity.
    pub const fn scalar(value: f64) -> Self {
        Self {
            value,
            dim: Dimension::dimensionless(),
        }
    }
}

/// Errors returned while parsing or evaluating a unit-aware expression.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UnitError {
    /// Input was empty or whitespace-only.
    #[error("expression cannot be empty")]
    Empty,
    /// Input exceeded [`MAX_EXPR_BYTES`], [`MAX_EXPR_TOKENS`], or [`MAX_EXPR_DEPTH`].
    #[error("expression exceeds bound: {0}")]
    BoundExceeded(String),
    /// Lexical or syntactic error in the expression.
    #[error("syntax error in `{expr}`: {detail}")]
    Syntax { expr: String, detail: String },
    /// Unknown unit or variable identifier.
    #[error("unknown identifier or unit `{0}`")]
    UnknownIdentifier(String),
    /// Incompatible dimensions in an operation or comparison.
    #[error("dimension mismatch ({context}): expected {expected}, got {found}")]
    DimensionMismatch {
        expected: Dimension,
        found: Dimension,
        context: String,
    },
    /// Division by zero or invalid domain (e.g. `sqrt` of negative number).
    #[error("arithmetic error: {0}")]
    Arithmetic(String),
    /// Numeric value differed from expected beyond `relative_tolerance`.
    #[error(
        "value mismatch: expected {expected} (SI), got {actual} (SI), relative error {rel_error:.6} > tolerance {tolerance:.6}"
    )]
    ValueMismatch {
        expected: f64,
        actual: f64,
        rel_error: f64,
        tolerance: f64,
    },
}

/// Looks up an unprefixed SI base or derived unit symbol, returning `(scale_to_si, Dimension)`.
fn lookup_core_unit(sym: &str) -> Option<(f64, Dimension)> {
    const fn d(m: i32, kg: i32, s: i32, a: i32, k: i32, mol: i32, cd: i32) -> Dimension {
        Dimension {
            m,
            kg,
            s,
            a,
            k,
            mol,
            cd,
        }
    }
    match sym {
        // Base units (note: gram is 1e-3 kg so SI prefixes apply cleanly to `g`).
        "m" | "meter" | "meters" | "metre" | "metres" => Some((1.0, d(1, 0, 0, 0, 0, 0, 0))),
        "g" | "gram" | "grams" => Some((1e-3, d(0, 1, 0, 0, 0, 0, 0))),
        "s" | "sec" | "second" | "seconds" => Some((1.0, d(0, 0, 1, 0, 0, 0, 0))),
        "A" | "amp" | "amps" | "ampere" | "amperes" => Some((1.0, d(0, 0, 0, 1, 0, 0, 0))),
        "K" | "kelvin" | "kelvins" => Some((1.0, d(0, 0, 0, 0, 1, 0, 0))),
        "mol" | "mole" | "moles" => Some((1.0, d(0, 0, 0, 0, 0, 1, 0))),
        "cd" | "candela" => Some((1.0, d(0, 0, 0, 0, 0, 0, 1))),
        // Derived SI units.
        "Hz" | "hertz" => Some((1.0, d(0, 0, -1, 0, 0, 0, 0))),
        "N" | "newton" | "newtons" => Some((1.0, d(1, 1, -2, 0, 0, 0, 0))),
        "J" | "joule" | "joules" => Some((1.0, d(2, 1, -2, 0, 0, 0, 0))),
        "W" | "watt" | "watts" => Some((1.0, d(2, 1, -3, 0, 0, 0, 0))),
        "Pa" | "pascal" | "pascals" => Some((1.0, d(-1, 1, -2, 0, 0, 0, 0))),
        "C" | "coulomb" | "coulombs" => Some((1.0, d(0, 0, 1, 1, 0, 0, 0))),
        "V" | "volt" | "volts" => Some((1.0, d(2, 1, -3, -1, 0, 0, 0))),
        "Ω" | "Ω" | "ohm" | "ohms" => Some((1.0, d(2, 1, -3, -2, 0, 0, 0))),
        "F" | "farad" | "farads" => Some((1.0, d(-2, -1, 4, 2, 0, 0, 0))),
        "T" | "tesla" | "teslas" => Some((1.0, d(0, 1, -2, -1, 0, 0, 0))),
        "Wb" | "weber" | "webers" => Some((1.0, d(2, 1, -2, -1, 0, 0, 0))),
        "H" | "henry" | "henries" => Some((1.0, d(2, 1, -2, -2, 0, 0, 0))),
        "L" | "l" | "liter" | "liters" | "litre" | "litres" => Some((1e-3, d(3, 0, 0, 0, 0, 0, 0))),
        // Accepted non-SI / dimensionless units.
        "min" | "minute" | "minutes" => Some((60.0, d(0, 0, 1, 0, 0, 0, 0))),
        "h" | "hr" | "hour" | "hours" => Some((3600.0, d(0, 0, 1, 0, 0, 0, 0))),
        "rad" | "radian" | "radians" => Some((1.0, Dimension::dimensionless())),
        "deg" | "degree" | "degrees" | "°" => {
            Some((std::f64::consts::PI / 180.0, Dimension::dimensionless()))
        }
        "%" => Some((0.01, Dimension::dimensionless())),
        _ => None,
    }
}

/// Resolves a unit symbol (including optional SI prefixes like `k`, `m`, `c`, `μ`, `M`, `G`)
/// into a [`Quantity`] with magnitude equal to the prefix×unit scale in SI base units.
pub fn resolve_unit(sym: &str) -> Option<Quantity> {
    if let Some((scale, dim)) = lookup_core_unit(sym) {
        return Some(Quantity { value: scale, dim });
    }

    const PREFIXES: &[(&str, f64)] = &[
        ("da", 1e1),
        ("T", 1e12),
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
        ("h", 1e2),
        ("d", 1e-1),
        ("c", 1e-2),
        ("m", 1e-3),
        ("u", 1e-6),
        ("μ", 1e-6),
        ("µ", 1e-6),
        ("n", 1e-9),
        ("p", 1e-12),
    ];

    for &(prefix, factor) in PREFIXES {
        if let Some(rest) = sym.strip_prefix(prefix)
            && !rest.is_empty()
            && let Some((base_scale, dim)) = lookup_core_unit(rest)
        {
            return Some(Quantity {
                value: factor * base_scale,
                dim,
            });
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    SuperscriptInt(i32),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

fn superscript_digit(ch: char) -> Option<i32> {
    match ch {
        '⁰' => Some(0),
        '¹' => Some(1),
        '²' => Some(2),
        '³' => Some(3),
        '⁴' => Some(4),
        '⁵' => Some(5),
        '⁶' => Some(6),
        '⁷' => Some(7),
        '⁸' => Some(8),
        '⁹' => Some(9),
        _ => None,
    }
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || matches!(ch, 'Ω' | 'Ω' | 'μ' | 'µ' | 'π' | '°' | '%')
}

fn tokenize(input: &str) -> Result<Vec<Token>, UnitError> {
    if input.len() > MAX_EXPR_BYTES {
        return Err(UnitError::BoundExceeded(format!(
            "expression length {} exceeds {MAX_EXPR_BYTES} bytes",
            input.len()
        )));
    }
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some(&(idx, ch)) = chars.peek() {
        if tokens.len() >= MAX_EXPR_TOKENS {
            return Err(UnitError::BoundExceeded(format!(
                "token count exceeds {MAX_EXPR_TOKENS}"
            )));
        }
        match ch {
            c if c.is_whitespace() => {
                chars.next();
            }
            '+' => {
                chars.next();
                tokens.push(Token::Plus);
            }
            '-' | '−' => {
                chars.next();
                tokens.push(Token::Minus);
            }
            '*' | '×' | '·' => {
                chars.next();
                tokens.push(Token::Star);
            }
            '/' | '÷' => {
                chars.next();
                tokens.push(Token::Slash);
            }
            '^' => {
                chars.next();
                tokens.push(Token::Caret);
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            '⁻' | '⁺' | '⁰' | '¹' | '²' | '³' | '⁴' | '⁵' | '⁶' | '⁷' | '⁸' | '⁹' =>
            {
                let mut sign = 1i32;
                if ch == '⁻' {
                    sign = -1;
                    chars.next();
                } else if ch == '⁺' {
                    chars.next();
                }
                let mut val: i32 = 0;
                let mut found_digit = false;
                while let Some(&(_, c)) = chars.peek() {
                    if let Some(d) = superscript_digit(c) {
                        val = val.saturating_mul(10).saturating_add(d);
                        found_digit = true;
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !found_digit {
                    return Err(UnitError::Syntax {
                        expr: input.to_string(),
                        detail: format!("incomplete superscript exponent at byte {idx}"),
                    });
                }
                tokens.push(Token::SuperscriptInt(sign * val));
            }
            '0'..='9' | '.' => {
                let start = idx;
                let mut end = idx;
                let mut seen_dot = ch == '.';
                chars.next();
                end += ch.len_utf8();
                while let Some(&(i, c)) = chars.peek() {
                    if c.is_ascii_digit() {
                        chars.next();
                        end = i + c.len_utf8();
                    } else if c == '.' && !seen_dot {
                        seen_dot = true;
                        chars.next();
                        end = i + c.len_utf8();
                    } else if (c == 'e' || c == 'E') && end > start {
                        // Check if scientific notation (`1e-3`, `2.5e6`) vs unit starting with `e`.
                        let mut clone = chars.clone();
                        clone.next();
                        if let Some(&(_, next_c)) = clone.peek()
                            && (next_c.is_ascii_digit()
                                || ((next_c == '+' || next_c == '-') && {
                                    let mut clone2 = clone.clone();
                                    clone2.next();
                                    clone2.peek().is_some_and(|&(_, d)| d.is_ascii_digit())
                                }))
                        {
                            chars.next();
                            end = i + c.len_utf8();
                            if let Some(&(si, sc)) = chars.peek()
                                && (sc == '+' || sc == '-')
                            {
                                chars.next();
                                end = si + sc.len_utf8();
                            }
                            while let Some(&(di, dc)) = chars.peek() {
                                if dc.is_ascii_digit() {
                                    chars.next();
                                    end = di + dc.len_utf8();
                                } else {
                                    break;
                                }
                            }
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                let slice = &input[start..end];
                let num = slice.parse::<f64>().map_err(|_| UnitError::Syntax {
                    expr: input.to_string(),
                    detail: format!("invalid number `{slice}`"),
                })?;
                if !num.is_finite() {
                    return Err(UnitError::Arithmetic(format!(
                        "non-finite number `{slice}`"
                    )));
                }
                tokens.push(Token::Number(num));
            }
            c if is_ident_char(c) => {
                let start = idx;
                let mut end = idx;
                while let Some(&(i, ic)) = chars.peek() {
                    if is_ident_char(ic) || (ic.is_ascii_digit() && i > start) {
                        chars.next();
                        end = i + ic.len_utf8();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Ident(input[start..end].to_string()));
            }
            other => {
                return Err(UnitError::Syntax {
                    expr: input.to_string(),
                    detail: format!("unexpected character `{other}` at byte {idx}"),
                });
            }
        }
    }

    if tokens.is_empty() {
        return Err(UnitError::Empty);
    }
    Ok(tokens)
}

struct ExprParser<'a> {
    raw: &'a str,
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
    knowns: &'a BTreeMap<String, Quantity>,
}

impl<'a> ExprParser<'a> {
    fn new(raw: &'a str, tokens: &'a [Token], knowns: &'a BTreeMap<String, Quantity>) -> Self {
        Self {
            raw,
            tokens,
            pos: 0,
            depth: 0,
            knowns,
        }
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    fn enter(&mut self) -> Result<(), UnitError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            return Err(UnitError::BoundExceeded(format!(
                "expression nesting depth exceeds {MAX_EXPR_DEPTH}"
            )));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn parse_expr(&mut self) -> Result<Quantity, UnitError> {
        self.parse_add_sub()
    }

    fn parse_add_sub(&mut self) -> Result<Quantity, UnitError> {
        let mut left = self.parse_mul_div()?;
        while let Some(tok) = self.peek() {
            let is_add = match tok {
                Token::Plus => true,
                Token::Minus => false,
                _ => break,
            };
            self.next();
            let right = self.parse_mul_div()?;
            if left.dim != right.dim {
                return Err(UnitError::DimensionMismatch {
                    expected: left.dim,
                    found: right.dim,
                    context: if is_add {
                        "addition".to_string()
                    } else {
                        "subtraction".to_string()
                    },
                });
            }
            let value = if is_add {
                left.value + right.value
            } else {
                left.value - right.value
            };
            left = Quantity {
                value,
                dim: left.dim,
            };
        }
        Ok(left)
    }

    fn parse_mul_div(&mut self) -> Result<Quantity, UnitError> {
        let mut left = self.parse_implicit_term()?;
        while let Some(tok) = self.peek() {
            let is_mul = match tok {
                Token::Star => true,
                Token::Slash => false,
                _ => break,
            };
            self.next();
            let right = self.parse_implicit_term()?;
            if is_mul {
                left = Quantity {
                    value: left.value * right.value,
                    dim: left.dim.mul(right.dim),
                };
            } else {
                if right.value == 0.0 {
                    return Err(UnitError::Arithmetic("division by zero".to_string()));
                }
                left = Quantity {
                    value: left.value / right.value,
                    dim: left.dim.div(right.dim),
                };
            }
        }
        Ok(left)
    }

    /// Parses juxtaposition such as `2 kg`, `3 m`, or `kg K` with higher precedence than `/`
    /// so `2 kg × 3 m/s²` and `10 m / 2 s` group naturally.
    fn parse_implicit_term(&mut self) -> Result<Quantity, UnitError> {
        let mut left = self.parse_unary()?;
        while let Some(Token::Ident(_) | Token::LParen) = self.peek() {
            let right = self.parse_power()?;
            left = Quantity {
                value: left.value * right.value,
                dim: left.dim.mul(right.dim),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Quantity, UnitError> {
        match self.peek() {
            Some(Token::Plus) => {
                self.next();
                self.enter()?;
                let v = self.parse_unary();
                self.leave();
                v
            }
            Some(Token::Minus) => {
                self.next();
                self.enter()?;
                let v = self.parse_unary()?;
                self.leave();
                Ok(Quantity {
                    value: -v.value,
                    dim: v.dim,
                })
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Quantity, UnitError> {
        let mut base = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(Token::SuperscriptInt(exp)) => {
                    let exp = *exp;
                    self.next();
                    base = apply_exponent(base, f64::from(exp))?;
                }
                Some(Token::Caret) => {
                    self.next();
                    self.enter()?;
                    let exp_q = self.parse_unary()?;
                    self.leave();
                    if !exp_q.dim.is_dimensionless() {
                        return Err(UnitError::DimensionMismatch {
                            expected: Dimension::dimensionless(),
                            found: exp_q.dim,
                            context: "exponent must be dimensionless".to_string(),
                        });
                    }
                    base = apply_exponent(base, exp_q.value)?;
                }
                _ => break,
            }
        }
        Ok(base)
    }

    fn parse_primary(&mut self) -> Result<Quantity, UnitError> {
        match self.next() {
            Some(Token::Number(n)) => Ok(Quantity::scalar(*n)),
            Some(Token::LParen) => {
                self.enter()?;
                let inner = self.parse_expr()?;
                self.leave();
                match self.next() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(UnitError::Syntax {
                        expr: self.raw.to_string(),
                        detail: "unclosed `(`".to_string(),
                    }),
                }
            }
            Some(Token::Ident(name)) => {
                // Check if this is a function call `fn_name(...)`.
                if matches!(self.peek(), Some(Token::LParen)) && is_builtin_function(name.as_str())
                {
                    self.next(); // consume `(`
                    self.enter()?;
                    let arg = self.parse_expr()?;
                    self.leave();
                    match self.next() {
                        Some(Token::RParen) => {}
                        _ => {
                            return Err(UnitError::Syntax {
                                expr: self.raw.to_string(),
                                detail: format!("unclosed `(` in call to `{name}`"),
                            });
                        }
                    }
                    return eval_function(name.as_str(), arg);
                }

                // 1. Known variable takes priority over unit names (`m` = mass in `m * a`).
                if let Some(&q) = self.knowns.get(name.as_str()) {
                    return Ok(q);
                }
                // 2. Unit symbol (with SI prefix support).
                if let Some(q) = resolve_unit(name.as_str()) {
                    return Ok(q);
                }
                // 3. Mathematical constants.
                match name.as_str() {
                    "pi" | "PI" | "π" => Ok(Quantity::scalar(std::f64::consts::PI)),
                    "e" => Ok(Quantity::scalar(std::f64::consts::E)),
                    _ => Err(UnitError::UnknownIdentifier(name.clone())),
                }
            }
            Some(other) => Err(UnitError::Syntax {
                expr: self.raw.to_string(),
                detail: format!("unexpected token `{other:?}`"),
            }),
            None => Err(UnitError::Syntax {
                expr: self.raw.to_string(),
                detail: "unexpected end of expression".to_string(),
            }),
        }
    }
}

fn is_builtin_function(name: &str) -> bool {
    matches!(
        name,
        "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "abs" | "ln" | "log10" | "exp"
    )
}

fn apply_exponent(base: Quantity, exp: f64) -> Result<Quantity, UnitError> {
    let val = base.value.powf(exp);
    if !val.is_finite() {
        return Err(UnitError::Arithmetic(format!(
            "non-finite power {} ^ {}",
            base.value, exp
        )));
    }
    if base.dim.is_dimensionless() {
        return Ok(Quantity::scalar(val));
    }
    let rounded = exp.round();
    if (exp - rounded).abs() < 1e-9 {
        let int_exp = rounded as i32;
        return Ok(Quantity {
            value: val,
            dim: base.dim.pow(int_exp),
        });
    }
    let twice = (exp * 2.0).round();
    if (exp * 2.0 - twice).abs() < 1e-9 {
        let half = base.dim.sqrt().ok_or_else(|| {
            UnitError::Arithmetic(format!(
                "cannot take half-integer power {exp} of odd dimension {}",
                base.dim
            ))
        })?;
        return Ok(Quantity {
            value: val,
            dim: half.pow(twice as i32),
        });
    }
    Err(UnitError::Arithmetic(format!(
        "non-integer exponent {exp} on dimensional quantity {}",
        base.dim
    )))
}

fn eval_function(name: &str, arg: Quantity) -> Result<Quantity, UnitError> {
    match name {
        "sqrt" => {
            if arg.value < 0.0 {
                return Err(UnitError::Arithmetic("sqrt of negative number".to_string()));
            }
            let dim = arg.dim.sqrt().ok_or_else(|| {
                UnitError::Arithmetic(format!(
                    "sqrt requires even dimension exponents, got {}",
                    arg.dim
                ))
            })?;
            Ok(Quantity {
                value: arg.value.sqrt(),
                dim,
            })
        }
        "abs" => Ok(Quantity {
            value: arg.value.abs(),
            dim: arg.dim,
        }),
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "ln" | "log10" | "exp" => {
            if !arg.dim.is_dimensionless() {
                return Err(UnitError::DimensionMismatch {
                    expected: Dimension::dimensionless(),
                    found: arg.dim,
                    context: format!("argument to `{name}` must be dimensionless"),
                });
            }
            let v = match name {
                "sin" => arg.value.sin(),
                "cos" => arg.value.cos(),
                "tan" => arg.value.tan(),
                "asin" => arg.value.asin(),
                "acos" => arg.value.acos(),
                "atan" => arg.value.atan(),
                "ln" => arg.value.ln(),
                "log10" => arg.value.log10(),
                "exp" => arg.value.exp(),
                _ => return Err(UnitError::UnknownIdentifier(name.to_string())),
            };
            if !v.is_finite() {
                return Err(UnitError::Arithmetic(format!(
                    "`{name}({})` produced a non-finite value",
                    arg.value
                )));
            }
            Ok(Quantity::scalar(v))
        }
        _ => Err(UnitError::UnknownIdentifier(name.to_string())),
    }
}

/// Parses a standalone quantity string (such as `"2 kg"`, `"3 m/s²"`, `"-9.81 m/s^2"`, or `"6 N"`)
/// into a [`Quantity`] in coherent SI units.
pub fn parse_quantity(text: &str) -> Result<Quantity, UnitError> {
    let empty = BTreeMap::new();
    evaluate_formula(text, &empty)
}

/// Evaluates a formula string over a map of pre-parsed [`Quantity`] bindings.
///
/// Identifiers present in `knowns` resolve to their bound [`Quantity`]; any other identifier
/// resolves as an SI base/derived unit or mathematical constant (`pi`, `e`).
pub fn evaluate_formula(
    formula: &str,
    knowns: &BTreeMap<String, Quantity>,
) -> Result<Quantity, UnitError> {
    let trimmed = formula.trim();
    if trimmed.is_empty() {
        return Err(UnitError::Empty);
    }
    let tokens = tokenize(trimmed)?;
    let mut parser = ExprParser::new(trimmed, &tokens, knowns);
    let result = parser.parse_expr()?;
    if parser.peek().is_some() {
        return Err(UnitError::Syntax {
            expr: trimmed.to_string(),
            detail: "unexpected trailing tokens".to_string(),
        });
    }
    if !result.value.is_finite() {
        return Err(UnitError::Arithmetic(
            "expression evaluated to a non-finite number".to_string(),
        ));
    }
    Ok(result)
}

/// Evaluates a [`NumericFixture`]'s `formula` over its `knowns` and checks both the SI
/// [`Dimension`] and numeric value against `expected` within `relative_tolerance`.
///
/// Returns the evaluated [`Quantity`] on success or a descriptive [`UnitError`] naming both
/// dimensions on a dimension mismatch.
pub fn check_numeric_fixture(fixture: &NumericFixture) -> Result<Quantity, UnitError> {
    if !fixture.relative_tolerance.is_finite() || fixture.relative_tolerance < 0.0 {
        return Err(UnitError::Arithmetic(format!(
            "invalid relative_tolerance {}",
            fixture.relative_tolerance
        )));
    }
    let mut parsed_knowns = BTreeMap::new();
    for (name, raw_val) in &fixture.knowns {
        let q = parse_quantity(raw_val)?;
        parsed_knowns.insert(name.clone(), q);
    }
    let actual = evaluate_formula(&fixture.formula, &parsed_knowns)?;
    let expected = parse_quantity(&fixture.expected)?;

    if actual.dim != expected.dim {
        return Err(UnitError::DimensionMismatch {
            expected: expected.dim,
            found: actual.dim,
            context: format!("step `{}` expected `{}`", fixture.step_id, fixture.expected),
        });
    }

    let diff = (actual.value - expected.value).abs();
    let rel_error = if expected.value.abs() > 1e-12 {
        diff / expected.value.abs()
    } else {
        diff
    };
    let tol = fixture.relative_tolerance.max(1e-12);
    if rel_error > tol + 1e-12 {
        return Err(UnitError::ValueMismatch {
            expected: expected.value,
            actual: actual.value,
            rel_error,
            tolerance: fixture.relative_tolerance,
        });
    }

    Ok(actual)
}

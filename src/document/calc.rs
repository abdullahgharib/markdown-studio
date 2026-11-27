use std::collections::{HashMap, HashSet};

use crate::document::ast::TableData;

// ---------------------------------------------------------------------------
// Shared Number Formatting
// ---------------------------------------------------------------------------

/// Formats a floating-point calculation result cleanly for display.
pub fn format_calc_number(val: f64) -> String {
    if val.is_nan() {
        return "NaN".to_string();
    }
    if val.is_infinite() {
        return if val.is_sign_positive() {
            "∞".to_string()
        } else {
            "-∞".to_string()
        };
    }
    if val.abs() < 1e-12 {
        return "0".to_string();
    }
    let rounded = val.round();
    if (val - rounded).abs() < 1e-10 && rounded.abs() < 1e14 {
        return format!("{}", rounded as i64);
    }
    if val.abs() >= 1e12 || val.abs() < 1e-6 {
        let s = format!("{val:.6e}");
        if let Some((mant, exp)) = s.split_once('e') {
            let trimmed_m = mant.trim_end_matches('0').trim_end_matches('.');
            return format!("{trimmed_m}e{exp}");
        }
        return s;
    }
    let s = format!("{val:.10}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Native LaTeX & Scientific Math Engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum MathTok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    Caret,
    Bang,
    Eq,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Bar,
    Comma,
    Sub,
    Frac,
    Sqrt,
    Binom,
    BigSum,
    BigProd,
    EndOfRow,
}

fn is_latex_math_func_name(s: &str) -> bool {
    matches!(
        s,
        "sin"
            | "cos"
            | "tan"
            | "csc"
            | "sec"
            | "cot"
            | "arcsin"
            | "arccos"
            | "arctan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "sinh"
            | "cosh"
            | "tanh"
            | "ln"
            | "log"
            | "lg"
            | "log10"
            | "log2"
            | "exp"
            | "sqrt"
            | "cbrt"
            | "abs"
            | "floor"
            | "ceil"
            | "round"
            | "sign"
            | "min"
            | "max"
            | "pow"
            | "gcd"
            | "lcm"
            | "fact"
            | "binom"
            | "deg"
            | "rad"
    )
}

fn tokenize_math_expr(input: &str) -> Result<Vec<MathTok>, String> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < n {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '%' {
            // Treat `%` as postfix percent when immediately after a value and not followed by a value starter
            let next_non_ws = chars[i + 1..].iter().find(|c| !c.is_whitespace()).copied();
            let prev_is_val = matches!(
                out.last(),
                Some(
                    MathTok::Num(_)
                        | MathTok::Ident(_)
                        | MathTok::RParen
                        | MathTok::RBrace
                        | MathTok::Bar
                        | MathTok::Bang
                )
            );
            let next_is_val = next_non_ws.is_some_and(|c| {
                c.is_ascii_digit() || c == '.' || c.is_alphabetic() || matches!(c, '(' | '{' | '\\')
            });
            if prev_is_val && !next_is_val {
                out.push(MathTok::Mul);
                out.push(MathTok::Num(0.01));
            } else {
                out.push(MathTok::Mod);
            }
            i += 1;
            continue;
        }
        if ch == '\\' {
            i += 1;
            if i >= n {
                break;
            }
            if chars[i] == '\\' {
                out.push(MathTok::EndOfRow);
                i += 1;
                continue;
            }
            if chars[i].is_ascii_alphabetic() {
                let start = i;
                while i < n && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let cmd: String = chars[start..i].iter().collect();
                match cmd.as_str() {
                    "frac" | "dfrac" | "tfrac" | "cfrac" => out.push(MathTok::Frac),
                    "sqrt" => out.push(MathTok::Sqrt),
                    "binom" | "tbinom" | "dbinom" => out.push(MathTok::Binom),
                    "sum" => out.push(MathTok::BigSum),
                    "prod" => out.push(MathTok::BigProd),
                    "cdot" | "times" | "ast" => out.push(MathTok::Mul),
                    "div" => out.push(MathTok::Div),
                    "bmod" | "pmod" => out.push(MathTok::Mod),
                    "pi" => out.push(MathTok::Ident("pi".to_string())),
                    "tau" => out.push(MathTok::Ident("tau".to_string())),
                    "phi" | "varphi" => out.push(MathTok::Ident("phi".to_string())),
                    "infty" => out.push(MathTok::Num(f64::INFINITY)),
                    "left" | "right" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr"
                    | "Biggl" | "Biggr" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle"
                    | "textstyle" | "scriptstyle" | "scriptscriptstyle" | "limits" | "nolimits" => {}
                    "lvert" | "rvert" => out.push(MathTok::Bar),
                    "text" | "textrm" | "mathrm" | "mathbf" | "textbf" | "operatorname" | "boxed" => {
                        // Unwrap braces via normal parser grouping, except `\operatorname{sin}` -> ident
                        while i < n && chars[i].is_whitespace() {
                            i += 1;
                        }
                        if matches!(cmd.as_str(), "operatorname" | "mathrm" | "text" | "textrm")
                            && i < n
                            && chars[i] == '{'
                        {
                            let mut j = i + 1;
                            let mut inner = String::new();
                            while j < n && chars[j] != '}' && chars[j] != '{' {
                                inner.push(chars[j]);
                                j += 1;
                            }
                            if j < n && chars[j] == '}' {
                                let trimmed = inner.trim();
                                if !trimmed.is_empty()
                                    && trimmed
                                        .chars()
                                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                                {
                                    out.push(MathTok::Ident(trimmed.to_string()));
                                    i = j + 1;
                                }
                            }
                        }
                    }
                    "tag" => {
                        // Skip \tag{...} label
                        while i < n && chars[i].is_whitespace() {
                            i += 1;
                        }
                        if i < n && chars[i] == '{' {
                            let mut depth = 1usize;
                            i += 1;
                            while i < n && depth > 0 {
                                if chars[i] == '{' {
                                    depth += 1;
                                } else if chars[i] == '}' {
                                    depth -= 1;
                                }
                                i += 1;
                            }
                        }
                    }
                    other => {
                        out.push(MathTok::Ident(other.to_string()));
                    }
                }
            } else {
                let sym = chars[i];
                i += 1;
                match sym {
                    ',' | ':' | ';' | '!' | ' ' => {}
                    '{' => out.push(MathTok::LParen),
                    '}' => out.push(MathTok::RParen),
                    '%' => out.push(MathTok::Mul),
                    _ => return Err(format!("Unsupported symbol \\{sym}")),
                }
            }
            continue;
        }

        if ch.is_ascii_digit() || (ch == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            let mut has_dot = ch == '.';
            i += 1;
            while i < n {
                let c = chars[i];
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == '.' && !has_dot {
                    has_dot = true;
                    i += 1;
                } else if (c == 'e' || c == 'E')
                    && i + 1 < n
                    && (chars[i + 1].is_ascii_digit()
                        || ((chars[i + 1] == '+' || chars[i + 1] == '-')
                            && i + 2 < n
                            && chars[i + 2].is_ascii_digit()))
                {
                    i += 1;
                    if chars[i] == '+' || chars[i] == '-' {
                        i += 1;
                    }
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    break;
                } else {
                    break;
                }
            }
            let num_str: String = chars[start..i].iter().collect();
            let num = num_str
                .parse::<f64>()
                .map_err(|_| format!("Invalid number '{num_str}'"))?;
            out.push(MathTok::Num(num));
            continue;
        }

        if ch.is_ascii_alphabetic() {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            out.push(MathTok::Ident(ident));
            continue;
        }

        match ch {
            '+' => out.push(MathTok::Plus),
            '-' | '−' => out.push(MathTok::Minus),
            '*' | '×' | '·' => out.push(MathTok::Mul),
            '/' | '÷' => out.push(MathTok::Div),
            '^' => out.push(MathTok::Caret),
            '_' => out.push(MathTok::Sub),
            '!' => out.push(MathTok::Bang),
            '=' => out.push(MathTok::Eq),
            '(' => out.push(MathTok::LParen),
            ')' => out.push(MathTok::RParen),
            '{' => out.push(MathTok::LBrace),
            '}' => out.push(MathTok::RBrace),
            '[' => out.push(MathTok::LBracket),
            ']' => out.push(MathTok::RBracket),
            '|' => out.push(MathTok::Bar),
            ',' => out.push(MathTok::Comma),
            'π' => out.push(MathTok::Ident("pi".to_string())),
            'τ' => out.push(MathTok::Ident("tau".to_string())),
            'φ' => out.push(MathTok::Ident("phi".to_string())),
            '√' => out.push(MathTok::Sqrt),
            '∞' => out.push(MathTok::Num(f64::INFINITY)),
            other => return Err(format!("Unexpected character '{other}'")),
        }
        i += 1;
    }

    Ok(out)
}

fn factorial_f64(n: f64) -> Result<f64, String> {
    if n < 0.0 || (n - n.round()).abs() > 1e-9 {
        return Err("Factorial requires a non-negative integer".to_string());
    }
    let k = n.round() as u64;
    if k > 170 {
        return Err("Factorial overflow (>170!)".to_string());
    }
    let mut acc = 1.0_f64;
    for m in 2..=k {
        acc *= m as f64;
    }
    Ok(acc)
}

const MAX_CALC_DEPTH: usize = 128;
const MAX_ITER_TOTAL_OPS: usize = 100_000;

fn binom_f64(n_val: f64, k_val: f64) -> Result<f64, String> {
    if (n_val - n_val.round()).abs() > 1e-9 || (k_val - k_val.round()).abs() > 1e-9 {
        return Err("Binomial coefficient requires integers".to_string());
    }
    let n = n_val.round() as i64;
    let k = k_val.round() as i64;
    if k < 0 || n < 0 || k > n {
        return Ok(0.0);
    }
    let k = k.min(n - k) as u64;
    let n = n as u64;
    let mut res = 1.0_f64;
    for i in 1..=k {
        res = res * ((n - k + i) as f64) / (i as f64);
        if !res.is_finite() {
            break;
        }
    }
    Ok(res)
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn apply_math_func(name: &str, args: &[f64]) -> Result<f64, String> {
    let lower = name.to_ascii_lowercase();
    let req1 = || -> Result<f64, String> {
        if args.len() != 1 {
            Err(format!("{name}() expects 1 argument"))
        } else {
            Ok(args[0])
        }
    };
    let req2 = || -> Result<(f64, f64), String> {
        if args.len() != 2 {
            Err(format!("{name}() expects 2 arguments"))
        } else {
            Ok((args[0], args[1]))
        }
    };

    match lower.as_str() {
        "sin" => Ok(req1()?.sin()),
        "cos" => Ok(req1()?.cos()),
        "tan" => Ok(req1()?.tan()),
        "csc" => {
            let s = req1()?.sin();
            if s.abs() < 1e-15 {
                Err("Division by zero in csc()".to_string())
            } else {
                Ok(1.0 / s)
            }
        }
        "sec" => {
            let c = req1()?.cos();
            if c.abs() < 1e-15 {
                Err("Division by zero in sec()".to_string())
            } else {
                Ok(1.0 / c)
            }
        }
        "cot" => {
            let t = req1()?.tan();
            if t.abs() < 1e-15 {
                Err("Division by zero in cot()".to_string())
            } else {
                Ok(1.0 / t)
            }
        }
        "asin" | "arcsin" => Ok(req1()?.asin()),
        "acos" | "arccos" => Ok(req1()?.acos()),
        "atan" | "arctan" => Ok(req1()?.atan()),
        "atan2" => {
            let (y, x) = req2()?;
            Ok(y.atan2(x))
        }
        "sinh" => Ok(req1()?.sinh()),
        "cosh" => Ok(req1()?.cosh()),
        "tanh" => Ok(req1()?.tanh()),
        "sqrt" => {
            let v = req1()?;
            if v < 0.0 {
                Err("Square root of negative number".to_string())
            } else {
                Ok(v.sqrt())
            }
        }
        "cbrt" => Ok(req1()?.cbrt()),
        "abs" => Ok(req1()?.abs()),
        "ln" => {
            let v = req1()?;
            if v <= 0.0 {
                Err("ln() domain error".to_string())
            } else {
                Ok(v.ln())
            }
        }
        "log" | "log10" | "lg" => {
            if args.len() == 1 {
                if args[0] <= 0.0 {
                    Err("log() domain error".to_string())
                } else {
                    Ok(args[0].log10())
                }
            } else if args.len() == 2 && lower == "log" {
                if args[0] <= 0.0 || args[1] <= 0.0 || (args[1] - 1.0).abs() < 1e-12 {
                    Err("log(x, base) domain error".to_string())
                } else {
                    Ok(args[0].ln() / args[1].ln())
                }
            } else {
                Err(format!("{name}() expects 1 argument"))
            }
        }
        "log2" => {
            let v = req1()?;
            if v <= 0.0 {
                Err("log2() domain error".to_string())
            } else {
                Ok(v.log2())
            }
        }
        "exp" => Ok(req1()?.exp()),
        "floor" => Ok(req1()?.floor()),
        "ceil" => Ok(req1()?.ceil()),
        "round" => {
            if args.len() == 1 {
                Ok(args[0].round())
            } else if args.len() == 2 {
                let factor = 10_f64.powi(args[1].round() as i32);
                Ok((args[0] * factor).round() / factor)
            } else {
                Err("round() expects 1 or 2 arguments".to_string())
            }
        }
        "sign" => {
            let v = req1()?;
            Ok(if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            })
        }
        "min" => args
            .iter()
            .copied()
            .reduce(f64::min)
            .ok_or_else(|| "min() needs at least 1 argument".to_string()),
        "max" => args
            .iter()
            .copied()
            .reduce(f64::max)
            .ok_or_else(|| "max() needs at least 1 argument".to_string()),
        "pow" => {
            let (a, b) = req2()?;
            Ok(a.powf(b))
        }
        "gcd" => {
            if args.is_empty() {
                return Err("gcd() needs at least 1 argument".to_string());
            }
            let mut g = args[0].round() as i64;
            for &v in &args[1..] {
                g = gcd_i64(g, v.round() as i64);
            }
            Ok(g as f64)
        }
        "lcm" => {
            if args.is_empty() {
                return Err("lcm() needs at least 1 argument".to_string());
            }
            let mut l = (args[0].round() as i64).abs();
            for &v in &args[1..] {
                let b = (v.round() as i64).abs();
                let g = gcd_i64(l, b);
                l = if g == 0 { 0 } else { (l / g).saturating_mul(b) };
            }
            Ok(l as f64)
        }
        "fact" => factorial_f64(req1()?),
        "binom" => {
            let (n, k) = req2()?;
            binom_f64(n, k)
        }
        "deg" => Ok(req1()?.to_degrees()),
        "rad" => Ok(req1()?.to_radians()),
        _ => Err(format!("Unknown function '{name}'")),
    }
}

struct MathParser<'a> {
    tokens: &'a [MathTok],
    pos: usize,
    vars: &'a mut HashMap<String, f64>,
    abs_depth: usize,
    depth: usize,
    iter_budget: &'a mut usize,
}

impl<'a> MathParser<'a> {
    fn peek(&self) -> Option<&'a MathTok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a MathTok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn starts_implicit_factor(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                MathTok::Ident(_)
                    | MathTok::LParen
                    | MathTok::LBrace
                    | MathTok::Frac
                    | MathTok::Sqrt
                    | MathTok::Binom
                    | MathTok::BigSum
                    | MathTok::BigProd
            )
        )
    }

    fn parse_add_sub(&mut self) -> Result<f64, String> {
        if self.depth >= MAX_CALC_DEPTH {
            return Err("Expression nesting depth exceeds limit".to_string());
        }
        self.depth += 1;
        let res = (|| {
            let mut val = self.parse_mul_div()?;
            loop {
                match self.peek() {
                    Some(MathTok::Plus) => {
                        self.next();
                        val += self.parse_mul_div()?;
                    }
                    Some(MathTok::Minus) => {
                        self.next();
                        val -= self.parse_mul_div()?;
                    }
                    _ => break,
                }
            }
            Ok(val)
        })();
        self.depth -= 1;
        res
    }

    fn parse_mul_div(&mut self) -> Result<f64, String> {
        let mut val = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(MathTok::Mul) => {
                    self.next();
                    val *= self.parse_unary()?;
                }
                Some(MathTok::Div) => {
                    self.next();
                    let rhs = self.parse_unary()?;
                    if rhs.abs() < 1e-15 {
                        return Err("Division by zero".to_string());
                    }
                    val /= rhs;
                }
                Some(MathTok::Mod) => {
                    self.next();
                    let rhs = self.parse_unary()?;
                    if rhs.abs() < 1e-15 {
                        return Err("Modulo by zero".to_string());
                    }
                    val %= rhs;
                }
                _ if self.starts_implicit_factor() => {
                    val *= self.parse_power()?;
                }
                _ => break,
            }
        }
        Ok(val)
    }

    fn parse_unary(&mut self) -> Result<f64, String> {
        if self.depth >= MAX_CALC_DEPTH {
            return Err("Expression nesting depth exceeds limit".to_string());
        }
        match self.peek() {
            Some(MathTok::Plus) => {
                self.next();
                self.depth += 1;
                let r = self.parse_unary();
                self.depth -= 1;
                r
            }
            Some(MathTok::Minus) => {
                self.next();
                self.depth += 1;
                let r = self.parse_unary();
                self.depth -= 1;
                Ok(-r?)
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<f64, String> {
        let base = self.parse_postfix()?;
        if matches!(self.peek(), Some(MathTok::Caret)) {
            self.next();
            let exp = self.parse_unary()?;
            Ok(base.powf(exp))
        } else {
            Ok(base)
        }
    }

    fn parse_postfix(&mut self) -> Result<f64, String> {
        let mut val = self.parse_primary()?;
        while matches!(self.peek(), Some(MathTok::Bang)) {
            self.next();
            val = factorial_f64(val)?;
        }
        Ok(val)
    }

    fn parse_group_or_atom(&mut self) -> Result<f64, String> {
        if matches!(self.peek(), Some(MathTok::LBrace)) {
            self.next();
            let v = self.parse_add_sub()?;
            if !matches!(self.next(), Some(MathTok::RBrace)) {
                return Err("Missing closing '}'".to_string());
            }
            Ok(v)
        } else {
            self.parse_primary()
        }
    }

    fn find_brace_span(&self, start_pos: usize) -> Result<(usize, usize), String> {
        if matches!(self.tokens.get(start_pos), Some(MathTok::LBrace)) {
            let mut depth = 1usize;
            let mut p = start_pos + 1;
            while p < self.tokens.len() {
                match self.tokens[p] {
                    MathTok::LBrace => depth += 1,
                    MathTok::RBrace => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok((start_pos + 1, p));
                        }
                    }
                    _ => {}
                }
                p += 1;
            }
            Err("Unclosed '{'".to_string())
        } else if start_pos < self.tokens.len() {
            Ok((start_pos, start_pos + 1))
        } else {
            Err("Unexpected end of expression".to_string())
        }
    }

    fn parse_big_iter(&mut self, is_sum: bool) -> Result<f64, String> {
        let mut var_name = "i".to_string();
        let mut start_val: Option<f64> = None;
        let mut end_val: Option<f64> = None;

        for _ in 0..2 {
            match self.peek() {
                Some(MathTok::Sub) => {
                    self.next();
                    let (s, e) = self.find_brace_span(self.pos)?;
                    let slice = &self.tokens[s..e];
                    if let Some(eq_idx) = slice.iter().position(|t| matches!(t, MathTok::Eq)) {
                        if let Some(MathTok::Ident(v)) = slice[..eq_idx].first() {
                            var_name = v.clone();
                        }
                        let mut sub_p = MathParser {
                            tokens: &slice[eq_idx + 1..],
                            pos: 0,
                            vars: self.vars,
                            abs_depth: 0,
                            depth: self.depth + 1,
                            iter_budget: self.iter_budget,
                        };
                        start_val = Some(sub_p.parse_add_sub()?);
                    }
                    self.pos = if matches!(self.tokens.get(self.pos), Some(MathTok::LBrace)) {
                        e + 1
                    } else {
                        e
                    };
                }
                Some(MathTok::Caret) => {
                    self.next();
                    let (s, e) = self.find_brace_span(self.pos)?;
                    let mut sub_p = MathParser {
                        tokens: &self.tokens[s..e],
                        pos: 0,
                        vars: self.vars,
                        abs_depth: 0,
                        depth: self.depth + 1,
                        iter_budget: self.iter_budget,
                    };
                    end_val = Some(sub_p.parse_add_sub()?);
                    self.pos = if matches!(self.tokens.get(self.pos), Some(MathTok::LBrace)) {
                        e + 1
                    } else {
                        e
                    };
                }
                _ => break,
            }
        }

        let lo = start_val.ok_or_else(|| "Missing lower bound on \\sum/\\prod".to_string())?.round() as i64;
        let hi = end_val.ok_or_else(|| "Missing upper bound on \\sum/\\prod".to_string())?.round() as i64;
        if hi >= lo && (hi - lo) > 10_000 {
            return Err("Summation/product range exceeds 10,000 steps".to_string());
        }

        // Capture the body token slice up to matching level `+`/`-` or closing delimiters
        let body_start = self.pos;
        let mut p = body_start;
        let mut p_paren = 0usize;
        let mut p_brace = 0usize;
        let mut p_brack = 0usize;
        while p < self.tokens.len() {
            match &self.tokens[p] {
                MathTok::LParen => p_paren += 1,
                MathTok::RParen => {
                    if p_paren == 0 {
                        break;
                    }
                    p_paren -= 1;
                }
                MathTok::LBrace => p_brace += 1,
                MathTok::RBrace => {
                    if p_brace == 0 {
                        break;
                    }
                    p_brace -= 1;
                }
                MathTok::LBracket => p_brack += 1,
                MathTok::RBracket => {
                    if p_brack == 0 {
                        break;
                    }
                    p_brack -= 1;
                }
                MathTok::Plus | MathTok::Minus | MathTok::Comma | MathTok::Eq | MathTok::EndOfRow
                    if p_paren == 0 && p_brace == 0 && p_brack == 0 && p > body_start =>
                {
                    break;
                }
                _ => {}
            }
            p += 1;
        }
        if p == body_start {
            return Err("Missing body after \\sum/\\prod".to_string());
        }
        let body_slice = &self.tokens[body_start..p];
        self.pos = p;

        let mut acc = if is_sum { 0.0 } else { 1.0 };
        if hi >= lo {
            let prev_binding = self.vars.get(&var_name).copied();
            let mut loop_res: Result<(), String> = Ok(());
            for k in lo..=hi {
                if *self.iter_budget == 0 {
                    loop_res = Err("Summation/product operation budget exceeded".to_string());
                    break;
                }
                *self.iter_budget -= 1;
                self.vars.insert(var_name.clone(), k as f64);
                let mut body_p = MathParser {
                    tokens: body_slice,
                    pos: 0,
                    vars: self.vars,
                    abs_depth: 0,
                    depth: self.depth + 1,
                    iter_budget: self.iter_budget,
                };
                match body_p.parse_add_sub() {
                    Ok(term) => {
                        if is_sum {
                            acc += term;
                        } else {
                            acc *= term;
                        }
                    }
                    Err(e) => {
                        loop_res = Err(e);
                        break;
                    }
                }
            }
            match prev_binding {
                Some(v) => {
                    self.vars.insert(var_name, v);
                }
                None => {
                    self.vars.remove(&var_name);
                }
            }
            loop_res?;
        }
        Ok(acc)
    }

    fn parse_primary(&mut self) -> Result<f64, String> {
        let Some(tok) = self.peek().cloned() else {
            return Err("Unexpected end of expression".to_string());
        };
        match tok {
            MathTok::Num(v) => {
                self.next();
                Ok(v)
            }
            MathTok::LParen => {
                self.next();
                let v = self.parse_add_sub()?;
                if !matches!(self.next(), Some(MathTok::RParen)) {
                    return Err("Missing closing ')'".to_string());
                }
                Ok(v)
            }
            MathTok::LBrace => {
                self.next();
                let v = self.parse_add_sub()?;
                if !matches!(self.next(), Some(MathTok::RBrace)) {
                    return Err("Missing closing '}'".to_string());
                }
                Ok(v)
            }
            MathTok::Bar => {
                self.next();
                self.abs_depth += 1;
                let v = self.parse_add_sub()?;
                self.abs_depth -= 1;
                if !matches!(self.next(), Some(MathTok::Bar)) {
                    return Err("Missing closing '|'".to_string());
                }
                Ok(v.abs())
            }
            MathTok::Frac => {
                self.next();
                let num = self.parse_group_or_atom()?;
                let den = self.parse_group_or_atom()?;
                if den.abs() < 1e-15 {
                    return Err("Division by zero in \\frac".to_string());
                }
                Ok(num / den)
            }
            MathTok::Sqrt => {
                self.next();
                let degree = if matches!(self.peek(), Some(MathTok::LBracket)) {
                    self.next();
                    let d = self.parse_add_sub()?;
                    if !matches!(self.next(), Some(MathTok::RBracket)) {
                        return Err("Missing closing ']' in \\sqrt".to_string());
                    }
                    Some(d)
                } else {
                    None
                };
                let rad = self.parse_group_or_atom()?;
                if let Some(d) = degree {
                    if d.abs() < 1e-15 {
                        return Err("Zero root degree".to_string());
                    }
                    if rad < 0.0 && (d.round() % 2.0 - 1.0).abs() > 1e-9 {
                        return Err("Even root of negative number".to_string());
                    }
                    if rad < 0.0 {
                        Ok(-((-rad).powf(1.0 / d)))
                    } else {
                        Ok(rad.powf(1.0 / d))
                    }
                } else {
                    if rad < 0.0 {
                        return Err("Square root of negative number".to_string());
                    }
                    Ok(rad.sqrt())
                }
            }
            MathTok::Binom => {
                self.next();
                let n = self.parse_group_or_atom()?;
                let k = self.parse_group_or_atom()?;
                binom_f64(n, k)
            }
            MathTok::BigSum => {
                self.next();
                self.parse_big_iter(true)
            }
            MathTok::BigProd => {
                self.next();
                self.parse_big_iter(false)
            }
            MathTok::Ident(name) => {
                self.next();
                if matches!(self.peek(), Some(MathTok::LParen)) {
                    self.next();
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(MathTok::RParen)) {
                        loop {
                            args.push(self.parse_add_sub()?);
                            if matches!(self.peek(), Some(MathTok::Comma)) {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    if !matches!(self.next(), Some(MathTok::RParen)) {
                        return Err(format!("Missing ')' in {name}()"));
                    }
                    apply_math_func(&name, &args)
                } else if is_latex_math_func_name(&name)
                    && matches!(
                        self.peek(),
                        Some(
                            MathTok::LBrace
                                | MathTok::Num(_)
                                | MathTok::Ident(_)
                                | MathTok::Frac
                                | MathTok::Sqrt
                        )
                    )
                {
                    let arg = self.parse_power()?;
                    apply_math_func(&name, &[arg])
                } else if let Some(&v) = self.vars.get(&name) {
                    Ok(v)
                } else {
                    match name.to_ascii_lowercase().as_str() {
                        "pi" => Ok(std::f64::consts::PI),
                        "tau" => Ok(std::f64::consts::TAU),
                        "e" => Ok(std::f64::consts::E),
                        "phi" => Ok(std::f64::consts::GOLDEN_RATIO),
                        _ => Err(format!("Undefined variable '{name}'")),
                    }
                }
            }
            other => Err(format!("Unexpected token {other:?}")),
        }
    }
}

/// Evaluates a standalone LaTeX or scientific mathematical expression to `f64`.
pub fn eval_math_expression(expr: &str) -> Result<f64, String> {
    eval_math_expression_with_vars(expr, &HashMap::new())
}

/// Evaluates a LaTeX or scientific mathematical expression with named variable bindings.
pub fn eval_math_expression_with_vars(
    expr: &str,
    vars: &HashMap<String, f64>,
) -> Result<f64, String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err("Empty expression".to_string());
    }
    let tokens = tokenize_math_expr(trimmed)?;
    if tokens.is_empty() {
        return Err("Empty expression".to_string());
    }
    let mut local_vars = vars.clone();
    let mut iter_budget = MAX_ITER_TOTAL_OPS;
    let mut parser = MathParser {
        tokens: &tokens,
        pos: 0,
        vars: &mut local_vars,
        abs_depth: 0,
        depth: 0,
        iter_budget: &mut iter_budget,
    };
    let val = parser.parse_add_sub()?;
    if parser.pos < tokens.len() {
        return Err(format!("Unexpected trailing token {:?}", tokens[parser.pos]));
    }
    if !val.is_finite() {
        return Err("Non-finite result".to_string());
    }
    Ok(val)
}

fn is_plain_numeric_literal(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    t.parse::<f64>().is_ok()
}

fn is_simple_var_name(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn eval_line_with_vars(
    raw_line: &str,
    vars: &mut HashMap<String, f64>,
) -> Option<(String, String, bool)> {
    // Strip optional trailing LaTeX row separator `\\`
    let trimmed_end = raw_line.trim_end();
    let (core_line, trailing_nl_sep) = if let Some(stripped) = trimmed_end.strip_suffix("\\\\") {
        (stripped.trim_end(), " \\\\")
    } else {
        (trimmed_end, "")
    };
    let core = core_line.trim();
    if core.is_empty() || core.starts_with('%') {
        return None;
    }

    // Split on '=' while avoiding nested `_{i=1}` inside braces
    let mut eq_positions = Vec::new();
    let mut brace_d = 0usize;
    for (idx, ch) in core.char_indices() {
        match ch {
            '{' => brace_d += 1,
            '}' => brace_d = brace_d.saturating_sub(1),
            '=' if brace_d == 0 => eq_positions.push(idx),
            _ => {}
        }
    }

    if eq_positions.is_empty() {
        let val = eval_math_expression_with_vars(core, vars).ok()?;
        let formatted = format_calc_number(val);
        if is_plain_numeric_literal(core) {
            return None;
        }
        let updated = format!("{core} = {formatted}{trailing_nl_sep}");
        return Some((updated, formatted, true));
    }

    let first_eq = eq_positions[0];
    let lhs = core[..first_eq].trim();
    if eq_positions.len() == 1 {
        let rhs = core[first_eq + 1..].trim();
        if rhs.is_empty() {
            // Trailing '=' like `2 + 3 =`
            let val = eval_math_expression_with_vars(lhs, vars).ok()?;
            let formatted = format_calc_number(val);
            let updated = format!("{lhs} = {formatted}{trailing_nl_sep}");
            return Some((updated, formatted, true));
        }
        if is_simple_var_name(lhs) {
            // Assignment like `a = 3` or `c = \sqrt{a^2 + b^2}`
            let val = eval_math_expression_with_vars(rhs, vars).ok()?;
            vars.insert(lhs.to_string(), val);
            let formatted = format_calc_number(val);
            if is_plain_numeric_literal(rhs) {
                return Some((raw_line.to_string(), format!("{lhs} = {formatted}"), false));
            }
            let updated = format!("{lhs} = {rhs} = {formatted}{trailing_nl_sep}");
            return Some((updated, format!("{lhs} = {formatted}"), true));
        }
        // Check if RHS is stale numeric result of LHS (e.g. `2 + 3 = 99`)
        if is_plain_numeric_literal(rhs) {
            let val = eval_math_expression_with_vars(lhs, vars).ok()?;
            let formatted = format_calc_number(val);
            let updated = format!("{lhs} = {formatted}{trailing_nl_sep}");
            let changed = updated.trim() != raw_line.trim();
            return Some((updated, formatted, changed));
        }
        return None;
    }

    // Multiple '=' signs (e.g. `c = \sqrt{a^2 + b^2} = 99`)
    let last_eq = *eq_positions.last().unwrap();
    let mid_expr = core[first_eq + 1..last_eq].trim();
    let tail_rhs = core[last_eq + 1..].trim();
    if is_simple_var_name(lhs) && (tail_rhs.is_empty() || is_plain_numeric_literal(tail_rhs)) {
        let val = eval_math_expression_with_vars(mid_expr, vars).ok()?;
        vars.insert(lhs.to_string(), val);
        let formatted = format_calc_number(val);
        let updated = format!("{lhs} = {mid_expr} = {formatted}{trailing_nl_sep}");
        let changed = updated.trim() != raw_line.trim();
        return Some((updated, format!("{lhs} = {formatted}"), changed));
    }

    None
}

/// Evaluates single- or multi-line LaTeX math blocks, returning `(updated_body, summary)`.
pub fn evaluate_math_block_text(body: &str) -> (String, Option<String>) {
    let mut vars = HashMap::new();
    let mut out_lines = Vec::new();
    let mut last_summary: Option<String> = None;
    let mut any_changed = false;

    for raw_line in body.lines() {
        let indent_len = raw_line.len() - raw_line.trim_start().len();
        let indent = &raw_line[..indent_len];
        if let Some((updated, summary, changed)) = eval_line_with_vars(raw_line.trim(), &mut vars) {
            last_summary = Some(summary);
            if changed {
                any_changed = true;
                out_lines.push(format!("{indent}{updated}"));
            } else {
                out_lines.push(raw_line.to_string());
            }
        } else {
            out_lines.push(raw_line.to_string());
        }
    }

    if !any_changed {
        return (body.to_string(), last_summary);
    }
    let mut joined = out_lines.join("\n");
    if body.ends_with('\n') {
        joined.push('\n');
    }
    (joined, last_summary)
}

/// Non-mutating live preview of the calculated result for Auto-Calc UI badges.
pub fn preview_math_result(body: &str) -> Option<String> {
    let mut vars = HashMap::new();
    let mut last_summary: Option<String> = None;
    for raw_line in body.lines() {
        if let Some((_, summary, _)) = eval_line_with_vars(raw_line.trim(), &mut vars) {
            last_summary = Some(summary);
        }
    }
    last_summary
}

// ---------------------------------------------------------------------------
// Excel-Like Table Formula Engine
// ---------------------------------------------------------------------------

/// Converts a 0-based column index into Excel column letters (`0 -> "A"`, `25 -> "Z"`, `26 -> "AA"`).
pub fn col_index_to_letters(mut col: usize) -> String {
    let mut chars = Vec::new();
    loop {
        chars.push((b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    chars.reverse();
    chars.into_iter().collect()
}

/// Parses an Excel cell reference like `"A1"` or `"AA12"` into 0-based `(row, col)`.
pub fn parse_cell_ref(s: &str) -> Option<(usize, usize)> {
    let t = s.trim();
    let split = t.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if split == 0 || split == t.len() {
        return None;
    }
    let col_part = &t[..split];
    let row_part = &t[split..];
    if !row_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row_1: usize = row_part.parse().ok()?;
    if row_1 == 0 {
        return None;
    }
    let mut col = 0usize;
    for b in col_part.bytes() {
        let up = b.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        col = col.checked_mul(26)?.checked_add((up - b'A') as usize + 1)?;
    }
    Some((row_1 - 1, col - 1))
}

/// Extracts `(Option<"=FORMULA">, cached_display_value)` from a table cell string.
/// Recognizes both raw `"=SUM(B1:B3)"` and cached `"150 <!-- =SUM(B1:B3) -->"`.
fn unescape_markdown_formula(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
                && matches!(next, '=' | '*' | '_' | '<' | '>' | '\\' | '[' | ']' | '|') {
                    out.push(next);
                    chars.next();
                    continue;
                }
        out.push(c);
    }
    out
}

pub fn extract_table_cell_formula(raw: &str) -> (Option<String>, String) {
    let trimmed = raw.trim();
    if trimmed.starts_with('=') || trimmed.starts_with("\\=") {
        return (Some(unescape_markdown_formula(trimmed)), String::new());
    }
    if let Some(cmt_start) = trimmed.rfind("<!--")
        && let Some(cmt_rel_end) = trimmed[cmt_start + 4..].find("-->") {
            let inner = trimmed[cmt_start + 4..cmt_start + 4 + cmt_rel_end].trim();
            if inner.starts_with('=') && cmt_start + 4 + cmt_rel_end + 3 == trimmed.len() {
                let val = trimmed[..cmt_start].trim_end().to_string();
                return (Some(inner.to_string()), val);
            }
        }
    (None, raw.to_string())
}

/// Formats a table cell preserving its Excel formula in a hidden HTML comment.
pub fn format_table_cell_with_formula(value: &str, formula: &str) -> String {
    let f = formula.trim();
    let f_with_eq = if f.starts_with('=') {
        f.to_string()
    } else {
        format!("={f}")
    };
    let v = value.trim();
    if v.is_empty() {
        format!("0 <!-- {f_with_eq} -->")
    } else {
        format!("{v} <!-- {f_with_eq} -->")
    }
}

#[derive(Debug, Clone)]
enum CellVal {
    Num(f64),
    Bool(bool),
    Text(String),
}

impl CellVal {
    fn as_f64(&self) -> Result<f64, String> {
        match self {
            CellVal::Num(n) => Ok(*n),
            CellVal::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            CellVal::Text(s) => parse_table_number(s).ok_or_else(|| "#VALUE!".to_string()),
        }
    }

    fn as_bool(&self) -> bool {
        match self {
            CellVal::Num(n) => n.abs() > 1e-12,
            CellVal::Bool(b) => *b,
            CellVal::Text(s) => {
                let u = s.trim().to_ascii_uppercase();
                !u.is_empty() && u != "FALSE" && u != "0"
            }
        }
    }

    fn to_display_string(&self) -> String {
        match self {
            CellVal::Num(n) => format_calc_number(*n),
            CellVal::Bool(b) => {
                if *b {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            CellVal::Text(s) => s.clone(),
        }
    }
}

fn parse_table_number(raw: &str) -> Option<f64> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut pct = false;
    if let Some(stripped) = s.strip_suffix('%') {
        pct = true;
        s = stripped.trim();
    }
    s = s
        .trim_start_matches(['$', '€', '£', '¥'])
        .trim();
    if s.is_empty() {
        return None;
    }
    let cleaned: String = s.chars().filter(|&c| c != ',').collect();
    let v = cleaned.parse::<f64>().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(if pct { v / 100.0 } else { v })
}

#[derive(Debug, Clone, PartialEq)]
enum XlTok {
    Num(f64),
    Str(String),
    Ident(String),
    Plus,
    Minus,
    Mul,
    Div,
    Caret,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    Comma,
    Colon,
}

fn tokenize_excel(formula: &str) -> Result<Vec<XlTok>, String> {
    let body = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < n {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '"' {
            i += 1;
            let mut s = String::new();
            while i < n && chars[i] != '"' {
                s.push(chars[i]);
                i += 1;
            }
            if i >= n {
                return Err("#ERR!".to_string());
            }
            i += 1;
            out.push(XlTok::Str(s));
            continue;
        }
        if ch.is_ascii_digit() || (ch == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let num_s: String = chars[start..i].iter().collect();
            let mut v = num_s.parse::<f64>().map_err(|_| "#ERR!".to_string())?;
            if i < n && chars[i] == '%' {
                v /= 100.0;
                i += 1;
            }
            out.push(XlTok::Num(v));
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '$' {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let raw_id: String = chars[start..i].iter().filter(|&&c| c != '$').collect();
            if raw_id.is_empty() {
                return Err("#ERR!".to_string());
            }
            out.push(XlTok::Ident(raw_id));
            continue;
        }
        match ch {
            '+' => out.push(XlTok::Plus),
            '-' => out.push(XlTok::Minus),
            '*' => out.push(XlTok::Mul),
            '/' => out.push(XlTok::Div),
            '^' => out.push(XlTok::Caret),
            '(' => out.push(XlTok::LParen),
            ')' => out.push(XlTok::RParen),
            ',' => out.push(XlTok::Comma),
            ':' => out.push(XlTok::Colon),
            '=' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    i += 1;
                }
                out.push(XlTok::Eq);
            }
            '!' if i + 1 < n && chars[i + 1] == '=' => {
                i += 1;
                out.push(XlTok::Neq);
            }
            '<' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    i += 1;
                    out.push(XlTok::Le);
                } else if i + 1 < n && chars[i + 1] == '>' {
                    i += 1;
                    out.push(XlTok::Neq);
                } else {
                    out.push(XlTok::Lt);
                }
            }
            '>' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    i += 1;
                    out.push(XlTok::Ge);
                } else {
                    out.push(XlTok::Gt);
                }
            }
            _ => return Err("#ERR!".to_string()),
        }
        i += 1;
    }
    Ok(out)
}

struct TableEvalCtx<'a> {
    rows: &'a [Vec<String>],
    memo: HashMap<(usize, usize), Result<CellVal, String>>,
    visiting: HashSet<(usize, usize)>,
}

impl<'a> TableEvalCtx<'a> {
    fn eval_cell(&mut self, r: usize, c: usize) -> Result<CellVal, String> {
        if r >= self.rows.len() || c >= self.rows[r].len() {
            return Err("#REF!".to_string());
        }
        if let Some(cached) = self.memo.get(&(r, c)) {
            return cached.clone();
        }
        if self.visiting.len() >= MAX_CALC_DEPTH {
            return Err("#DEPTH!".to_string());
        }
        if !self.visiting.insert((r, c)) {
            return Err("#CYCLE!".to_string());
        }

        let raw = self.rows[r][c].clone();
        let (formula_opt, display_val) = extract_table_cell_formula(&raw);
        let res = if let Some(formula) = formula_opt {
            match tokenize_excel(&formula) {
                Ok(toks) if !toks.is_empty() => {
                    let mut parser = XlParser {
                        tokens: &toks,
                        pos: 0,
                        cur_row: r,
                        cur_col: c,
                        ctx: self,
                        depth: 0,
                    };
                    match parser.parse_compare() {
                        Ok(val) if parser.pos == toks.len() => Ok(val),
                        Ok(_) => Err("#ERR!".to_string()),
                        Err(e) => Err(e),
                    }
                }
                _ => Err("#ERR!".to_string()),
            }
        } else if let Some(n) = parse_table_number(&display_val) {
            Ok(CellVal::Num(n))
        } else {
            Ok(CellVal::Text(display_val))
        };

        self.visiting.remove(&(r, c));
        self.memo.insert((r, c), res.clone());
        res
    }

    fn collect_directional_cells(
        &mut self,
        cur_r: usize,
        cur_c: usize,
        dir: &str,
    ) -> Result<Vec<CellVal>, String> {
        let mut coords = Vec::new();
        match dir {
            "ABOVE" => {
                let mut r = cur_r;
                while r > 0 {
                    r -= 1;
                    let raw = self
                        .rows
                        .get(r)
                        .and_then(|row| row.get(cur_c))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let (f, disp) = extract_table_cell_formula(raw);
                    if f.is_none() && parse_table_number(&disp).is_none() {
                        break;
                    }
                    coords.push((r, cur_c));
                }
            }
            "BELOW" => {
                let mut r = cur_r + 1;
                while r < self.rows.len() {
                    let raw = self.rows[r].get(cur_c).map(|s| s.as_str()).unwrap_or("");
                    let (f, disp) = extract_table_cell_formula(raw);
                    if f.is_none() && parse_table_number(&disp).is_none() {
                        break;
                    }
                    coords.push((r, cur_c));
                    r += 1;
                }
            }
            "LEFT" => {
                let mut c = cur_c;
                while c > 0 {
                    c -= 1;
                    let raw = self
                        .rows
                        .get(cur_r)
                        .and_then(|row| row.get(c))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let (f, disp) = extract_table_cell_formula(raw);
                    if f.is_none() && parse_table_number(&disp).is_none() {
                        break;
                    }
                    coords.push((cur_r, c));
                }
            }
            "RIGHT" => {
                let row_len = self.rows.get(cur_r).map(|row| row.len()).unwrap_or(0);
                let mut c = cur_c + 1;
                while c < row_len {
                    let raw = self.rows[cur_r][c].as_str();
                    let (f, disp) = extract_table_cell_formula(raw);
                    if f.is_none() && parse_table_number(&disp).is_none() {
                        break;
                    }
                    coords.push((cur_r, c));
                    c += 1;
                }
            }
            _ => {}
        }
        let mut out = Vec::with_capacity(coords.len());
        for (r, c) in coords {
            out.push(self.eval_cell(r, c)?);
        }
        Ok(out)
    }
}

struct XlParser<'a, 'b> {
    tokens: &'a [XlTok],
    pos: usize,
    cur_row: usize,
    cur_col: usize,
    ctx: &'a mut TableEvalCtx<'b>,
    depth: usize,
}

impl<'a, 'b> XlParser<'a, 'b> {
    fn peek(&self) -> Option<&'a XlTok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a XlTok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_compare(&mut self) -> Result<CellVal, String> {
        if self.depth >= MAX_CALC_DEPTH {
            return Err("#DEPTH!".to_string());
        }
        self.depth += 1;
        let res = self.parse_compare_inner();
        self.depth -= 1;
        res
    }

    fn parse_compare_inner(&mut self) -> Result<CellVal, String> {
        let lhs = self.parse_add_sub()?;
        let op = match self.peek() {
            Some(XlTok::Eq) => Some("eq"),
            Some(XlTok::Neq) => Some("neq"),
            Some(XlTok::Lt) => Some("lt"),
            Some(XlTok::Le) => Some("le"),
            Some(XlTok::Gt) => Some("gt"),
            Some(XlTok::Ge) => Some("ge"),
            _ => None,
        };
        let Some(op) = op else {
            return Ok(lhs);
        };
        self.next();
        let rhs = self.parse_add_sub()?;
        let res = match (&lhs, &rhs) {
            (CellVal::Text(a), CellVal::Text(b)) if parse_table_number(a).is_none() || parse_table_number(b).is_none() => {
                match op {
                    "eq" => a.eq_ignore_ascii_case(b),
                    "neq" => !a.eq_ignore_ascii_case(b),
                    "lt" => a < b,
                    "le" => a <= b,
                    "gt" => a > b,
                    "ge" => a >= b,
                    _ => false,
                }
            }
            _ => {
                let a = lhs.as_f64()?;
                let b = rhs.as_f64()?;
                match op {
                    "eq" => (a - b).abs() < 1e-11,
                    "neq" => (a - b).abs() >= 1e-11,
                    "lt" => a < b,
                    "le" => a <= b,
                    "gt" => a > b,
                    "ge" => a >= b,
                    _ => false,
                }
            }
        };
        Ok(CellVal::Bool(res))
    }

    fn parse_add_sub(&mut self) -> Result<CellVal, String> {
        let mut val = self.parse_mul_div()?;
        loop {
            match self.peek() {
                Some(XlTok::Plus) => {
                    self.next();
                    let r = self.parse_mul_div()?;
                    val = CellVal::Num(val.as_f64()? + r.as_f64()?);
                }
                Some(XlTok::Minus) => {
                    self.next();
                    let r = self.parse_mul_div()?;
                    val = CellVal::Num(val.as_f64()? - r.as_f64()?);
                }
                _ => break,
            }
        }
        Ok(val)
    }

    fn parse_mul_div(&mut self) -> Result<CellVal, String> {
        let mut val = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(XlTok::Mul) => {
                    self.next();
                    let r = self.parse_unary()?;
                    val = CellVal::Num(val.as_f64()? * r.as_f64()?);
                }
                Some(XlTok::Div) => {
                    self.next();
                    let r = self.parse_unary()?.as_f64()?;
                    if r.abs() < 1e-15 {
                        return Err("#DIV/0!".to_string());
                    }
                    val = CellVal::Num(val.as_f64()? / r);
                }
                _ => break,
            }
        }
        Ok(val)
    }

    fn parse_unary(&mut self) -> Result<CellVal, String> {
        if self.depth >= MAX_CALC_DEPTH {
            return Err("#DEPTH!".to_string());
        }
        match self.peek() {
            Some(XlTok::Plus) => {
                self.next();
                self.depth += 1;
                let r = self.parse_unary();
                self.depth -= 1;
                Ok(CellVal::Num(r?.as_f64()?))
            }
            Some(XlTok::Minus) => {
                self.next();
                self.depth += 1;
                let r = self.parse_unary();
                self.depth -= 1;
                Ok(CellVal::Num(-r?.as_f64()?))
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<CellVal, String> {
        let base = self.parse_primary()?;
        if matches!(self.peek(), Some(XlTok::Caret)) {
            self.next();
            let exp = self.parse_unary()?.as_f64()?;
            Ok(CellVal::Num(base.as_f64()?.powf(exp)))
        } else {
            Ok(base)
        }
    }

    fn collect_func_arg_items(&mut self) -> Result<Vec<CellVal>, String> {
        if let (Some(XlTok::Ident(a)), Some(XlTok::Colon), Some(XlTok::Ident(b))) = (
            self.tokens.get(self.pos),
            self.tokens.get(self.pos + 1),
            self.tokens.get(self.pos + 2),
        )
            && matches!(
                self.tokens.get(self.pos + 3),
                Some(XlTok::Comma | XlTok::RParen)
            ) {
                let (r0, c0) = parse_cell_ref(a).ok_or_else(|| "#REF!".to_string())?;
                let (r1, c1) = parse_cell_ref(b).ok_or_else(|| "#REF!".to_string())?;
                self.pos += 3;
                let r_lo = r0.min(r1);
                let r_hi = r0.max(r1);
                let c_lo = c0.min(c1);
                let c_hi = c0.max(c1);
                if r_hi >= self.ctx.rows.len()
                    || (r_lo..=r_hi).any(|r| c_hi >= self.ctx.rows[r].len())
                {
                    return Err("#REF!".to_string());
                }
                let count = (r_hi - r_lo + 1).saturating_mul(c_hi - c_lo + 1);
                let mut vals = Vec::with_capacity(count.min(4096));
                for r in r_lo..=r_hi {
                    for c in c_lo..=c_hi {
                        vals.push(self.ctx.eval_cell(r, c)?);
                    }
                }
                return Ok(vals);
            }
        if let Some(XlTok::Ident(dir)) = self.peek() {
            let up = dir.to_ascii_uppercase();
            if matches!(up.as_str(), "ABOVE" | "BELOW" | "LEFT" | "RIGHT")
                && matches!(self.tokens.get(self.pos + 1), Some(XlTok::Comma | XlTok::RParen))
            {
                self.next();
                return self
                    .ctx
                    .collect_directional_cells(self.cur_row, self.cur_col, &up);
            }
        }
        Ok(vec![self.parse_compare()?])
    }

    fn eval_excel_function(&mut self, name: &str) -> Result<CellVal, String> {
        let upper = name.to_ascii_uppercase();
        if !matches!(self.next(), Some(XlTok::LParen)) {
            return Err("#ERR!".to_string());
        }

        // Special-case IF(cond, t_branch, f_branch) for lazy branch evaluation
        if upper == "IF" {
            let cond = self.parse_compare()?.as_bool();
            if !matches!(self.next(), Some(XlTok::Comma)) {
                return Err("#ERR!".to_string());
            }
            let t_val = if cond {
                self.parse_compare()?
            } else {
                self.skip_balanced_arg()?;
                CellVal::Num(0.0)
            };
            let f_val = if matches!(self.peek(), Some(XlTok::Comma)) {
                self.next();
                if !cond {
                    self.parse_compare()?
                } else {
                    self.skip_balanced_arg()?;
                    CellVal::Num(0.0)
                }
            } else {
                CellVal::Bool(false)
            };
            if !matches!(self.next(), Some(XlTok::RParen)) {
                return Err("#ERR!".to_string());
            }
            return Ok(if cond { t_val } else { f_val });
        }

        let mut arg_groups: Vec<Vec<CellVal>> = Vec::new();
        if !matches!(self.peek(), Some(XlTok::RParen)) {
            loop {
                arg_groups.push(self.collect_func_arg_items()?);
                if matches!(self.peek(), Some(XlTok::Comma)) {
                    self.next();
                } else {
                    break;
                }
            }
        }
        if !matches!(self.next(), Some(XlTok::RParen)) {
            return Err("#ERR!".to_string());
        }

        let flat_nums = |groups: &[Vec<CellVal>]| -> Vec<f64> {
            let mut nums = Vec::new();
            for g in groups {
                for v in g {
                    match v {
                        CellVal::Num(n) => nums.push(*n),
                        CellVal::Bool(b) => nums.push(if *b { 1.0 } else { 0.0 }),
                        CellVal::Text(t) => {
                            if let Some(n) = parse_table_number(t) {
                                nums.push(n);
                            }
                        }
                    }
                }
            }
            nums
        };

        match upper.as_str() {
            "SUM" => Ok(CellVal::Num(flat_nums(&arg_groups).into_iter().sum())),
            "AVERAGE" | "AVG" => {
                let nums = flat_nums(&arg_groups);
                if nums.is_empty() {
                    Err("#DIV/0!".to_string())
                } else {
                    Ok(CellVal::Num(nums.iter().sum::<f64>() / nums.len() as f64))
                }
            }
            "MIN" => {
                let nums = flat_nums(&arg_groups);
                Ok(CellVal::Num(nums.into_iter().reduce(f64::min).unwrap_or(0.0)))
            }
            "MAX" => {
                let nums = flat_nums(&arg_groups);
                Ok(CellVal::Num(nums.into_iter().reduce(f64::max).unwrap_or(0.0)))
            }
            "COUNT" => Ok(CellVal::Num(flat_nums(&arg_groups).len() as f64)),
            "COUNTA" => {
                let cnt = arg_groups
                    .iter()
                    .flatten()
                    .filter(|v| match v {
                        CellVal::Text(t) => !t.trim().is_empty(),
                        _ => true,
                    })
                    .count();
                Ok(CellVal::Num(cnt as f64))
            }
            "PRODUCT" => {
                let nums = flat_nums(&arg_groups);
                Ok(CellVal::Num(if nums.is_empty() {
                    0.0
                } else {
                    nums.into_iter().product()
                }))
            }
            "MEDIAN" => {
                let mut nums = flat_nums(&arg_groups);
                if nums.is_empty() {
                    return Err("#NUM!".to_string());
                }
                nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let m = nums.len();
                let med = if m % 2 == 1 {
                    nums[m / 2]
                } else {
                    (nums[m / 2 - 1] + nums[m / 2]) * 0.5
                };
                Ok(CellVal::Num(med))
            }
            "STDEV" | "STDEVP" => {
                let nums = flat_nums(&arg_groups);
                let n = nums.len();
                if n == 0 || (upper == "STDEV" && n < 2) {
                    return Err("#DIV/0!".to_string());
                }
                let mean = nums.iter().sum::<f64>() / n as f64;
                let var_sum: f64 = nums.iter().map(|x| (x - mean).powi(2)).sum();
                let denom = if upper == "STDEV" {
                    (n - 1) as f64
                } else {
                    n as f64
                };
                Ok(CellVal::Num((var_sum / denom).sqrt()))
            }
            "AND" => Ok(CellVal::Bool(
                arg_groups.iter().flatten().all(|v| v.as_bool()),
            )),
            "OR" => Ok(CellVal::Bool(
                arg_groups.iter().flatten().any(|v| v.as_bool()),
            )),
            "NOT" => {
                let first = arg_groups
                    .first()
                    .and_then(|g| g.first())
                    .ok_or_else(|| "#ERR!".to_string())?;
                Ok(CellVal::Bool(!first.as_bool()))
            }
            "PI" => {
                if arg_groups.is_empty() {
                    Ok(CellVal::Num(std::f64::consts::PI))
                } else {
                    Err("#ERR!".to_string())
                }
            }
            "INT" => {
                let x = arg_groups
                    .first()
                    .and_then(|g| g.first())
                    .ok_or_else(|| "#ERR!".to_string())?
                    .as_f64()?;
                Ok(CellVal::Num(x.floor()))
            }
            "POWER" => {
                let nums = flat_nums(&arg_groups);
                if nums.len() != 2 {
                    Err("#ERR!".to_string())
                } else {
                    Ok(CellVal::Num(nums[0].powf(nums[1])))
                }
            }
            "MOD" => {
                let nums = flat_nums(&arg_groups);
                if nums.len() != 2 {
                    Err("#ERR!".to_string())
                } else if nums[1].abs() < 1e-15 {
                    Err("#DIV/0!".to_string())
                } else {
                    Ok(CellVal::Num(nums[0] % nums[1]))
                }
            }
            "CEILING" => {
                let nums = flat_nums(&arg_groups);
                if nums.len() == 1 {
                    Ok(CellVal::Num(nums[0].ceil()))
                } else if nums.len() == 2 && nums[1].abs() > 1e-15 {
                    Ok(CellVal::Num((nums[0] / nums[1]).ceil() * nums[1]))
                } else {
                    Err("#ERR!".to_string())
                }
            }
            "RADIANS" => {
                let nums = flat_nums(&arg_groups);
                if nums.len() != 1 {
                    Err("#ERR!".to_string())
                } else {
                    Ok(CellVal::Num(nums[0].to_radians()))
                }
            }
            "DEGREES" => {
                let nums = flat_nums(&arg_groups);
                if nums.len() != 1 {
                    Err("#ERR!".to_string())
                } else {
                    Ok(CellVal::Num(nums[0].to_degrees()))
                }
            }
            other => {
                let nums = flat_nums(&arg_groups);
                let val = apply_math_func(&other.to_ascii_lowercase(), &nums)
                    .map_err(|_| "#ERR!".to_string())?;
                Ok(CellVal::Num(val))
            }
        }
    }

    fn skip_balanced_arg(&mut self) -> Result<(), String> {
        let mut depth = 0usize;
        while let Some(tok) = self.peek() {
            match tok {
                XlTok::LParen => depth += 1,
                XlTok::RParen => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                XlTok::Comma if depth == 0 => break,
                _ => {}
            }
            self.next();
        }
        Ok(())
    }

    fn parse_primary(&mut self) -> Result<CellVal, String> {
        let Some(tok) = self.peek().cloned() else {
            return Err("#ERR!".to_string());
        };
        match tok {
            XlTok::Num(v) => {
                self.next();
                Ok(CellVal::Num(v))
            }
            XlTok::Str(s) => {
                self.next();
                Ok(CellVal::Text(s))
            }
            XlTok::LParen => {
                self.next();
                let v = self.parse_compare()?;
                if !matches!(self.next(), Some(XlTok::RParen)) {
                    return Err("#ERR!".to_string());
                }
                Ok(v)
            }
            XlTok::Ident(name) => {
                if matches!(self.tokens.get(self.pos + 1), Some(XlTok::LParen)) {
                    self.next();
                    self.eval_excel_function(&name)
                } else {
                    self.next();
                    let up = name.to_ascii_uppercase();
                    if up == "TRUE" {
                        return Ok(CellVal::Bool(true));
                    }
                    if up == "FALSE" {
                        return Ok(CellVal::Bool(false));
                    }
                    if up == "PI" {
                        return Ok(CellVal::Num(std::f64::consts::PI));
                    }
                    let (r, c) = parse_cell_ref(&name).ok_or_else(|| "#REF!".to_string())?;
                    self.ctx.eval_cell(r, c)
                }
            }
            _ => Err("#ERR!".to_string()),
        }
    }
}

/// Evaluates all formula cells (`=...` or `... <!-- =... -->`) in a table body grid.
/// Returns a parallel grid of clean display strings (`"150"`, `"#DIV/0!"`, etc.).
pub fn evaluate_table_grid(rows: &[Vec<String>]) -> Vec<Vec<String>> {
    let mut ctx = TableEvalCtx {
        rows,
        memo: HashMap::new(),
        visiting: HashSet::new(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for (r, row) in rows.iter().enumerate() {
        let mut out_row = Vec::with_capacity(row.len());
        for (c, cell) in row.iter().enumerate() {
            let (formula_opt, disp) = extract_table_cell_formula(cell);
            if formula_opt.is_some() {
                match ctx.eval_cell(r, c) {
                    Ok(val) => out_row.push(val.to_display_string()),
                    Err(err) => out_row.push(err),
                }
            } else {
                out_row.push(disp);
            }
        }
        out.push(out_row);
    }
    out
}

/// Evaluates all formula cells in `table.rows` in-place, formatting each formula cell as
/// `<computed_value> <!-- =FORMULA -->` so Markdown preserves the formula across saves.
/// Returns the number of formula cells updated.
pub fn recalculate_table(table: &mut TableData) -> usize {
    let evaluated = evaluate_table_grid(&table.rows);
    let mut count = 0usize;
    for (r, row) in table.rows.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            let (formula_opt, _) = extract_table_cell_formula(cell);
            if let Some(formula) = formula_opt {
                let val_str = evaluated
                    .get(r)
                    .and_then(|er| er.get(c))
                    .map(|s| s.as_str())
                    .unwrap_or("0");
                *cell = format_table_cell_with_formula(val_str, &formula);
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::ir::TableAlign;

    #[test]
    fn math_engine_evaluates_arithmetic_latex_functions_and_summation() {
        assert_eq!(eval_math_expression("2 + 3 * 4^2").unwrap(), 50.0);
        assert_eq!(eval_math_expression(r"\frac{3 + 5}{2}").unwrap(), 4.0);
        assert_eq!(eval_math_expression(r"\sqrt{144} + \sqrt[3]{27}").unwrap(), 15.0);
        assert_eq!(eval_math_expression(r"\binom{6}{2} + 4!").unwrap(), 39.0);
        assert_eq!(eval_math_expression(r"\sum_{i=1}^{5} i^2").unwrap(), 55.0);
        assert_eq!(eval_math_expression(r"\prod_{k=1}^{4} k").unwrap(), 24.0);
        assert!((eval_math_expression(r"\sin(\frac{\pi}{6})").unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(eval_math_expression(r"|3 - 10| + 200 \cdot 15%").unwrap(), 37.0);
    }

    #[test]
    fn math_block_evaluator_handles_multiline_variables_and_stale_rhs() {
        let block = "a = 3\nb = 4\nc = \\sqrt{a^2 + b^2}\n2 + 3 = 99";
        let (updated, summary) = evaluate_math_block_text(block);
        assert!(updated.contains("c = \\sqrt{a^2 + b^2} = 5"));
        assert!(updated.contains("2 + 3 = 5"));
        assert_eq!(summary.as_deref(), Some("5"));

        // Symbolic equation must stay untouched
        let sym = "E = mc^2";
        let (sym_out, sym_sum) = evaluate_math_block_text(sym);
        assert_eq!(sym_out, sym);
        assert_eq!(sym_sum, None);
    }

    #[test]
    fn excel_col_and_cell_ref_roundtrip() {
        assert_eq!(col_index_to_letters(0), "A");
        assert_eq!(col_index_to_letters(25), "Z");
        assert_eq!(col_index_to_letters(26), "AA");
        assert_eq!(parse_cell_ref("A1"), Some((0, 0)));
        assert_eq!(parse_cell_ref("C4"), Some((3, 2)));
        assert_eq!(parse_cell_ref("AA12"), Some((11, 26)));
        assert_eq!(parse_cell_ref("A0"), None);
    }

    #[test]
    fn excel_table_engine_computes_ranges_above_left_if_and_cycles() {
        let mut table = TableData {
            alignments: vec![TableAlign::Left, TableAlign::Right, TableAlign::Right, TableAlign::Left],
            header: vec!["Item".into(), "Price".into(), "Net".into(), "Status".into()],
            rows: vec![
                vec!["Apples".into(), "$1,200.00".into(), "1000".into(), "=IF(B1>1000, \"High\", \"Low\")".into()],
                vec!["Oranges".into(), "800".into(), "=B1 + B2".into(), "=IF(C2>1500, \"Pass\", \"Fail\")".into()],
                vec!["Total".into(), "=SUM(B1:B2)".into(), "=AVERAGE(ABOVE)".into(), "=COUNT(LEFT)".into()],
            ],
            source_lines: 0..5,
        };

        let updated = recalculate_table(&mut table);
        assert_eq!(updated, 6);
        assert_eq!(table.rows[0][3], "High <!-- =IF(B1>1000, \"High\", \"Low\") -->");
        assert_eq!(table.rows[1][2], "2000 <!-- =B1 + B2 -->");
        assert_eq!(table.rows[2][1], "2000 <!-- =SUM(B1:B2) -->");
        assert_eq!(table.rows[2][2], "1500 <!-- =AVERAGE(ABOVE) -->");
        assert_eq!(table.rows[2][3], "2 <!-- =COUNT(LEFT) -->");

        // Second pass preserves formulas and produces identical cached results
        let grid2 = evaluate_table_grid(&table.rows);
        assert_eq!(grid2[2][1], "2000");

        // Cycle detection & div by zero
        let bad_rows = vec![
            vec!["=B1".into(), "=A1".into(), "=10/0".into()],
        ];
        let bad_eval = evaluate_table_grid(&bad_rows);
        assert_eq!(bad_eval[0][0], "#CYCLE!");
        assert_eq!(bad_eval[0][2], "#DIV/0!");

        // CommonMark-escaped operators/leading `=` in uncached formulas (from comrak format_commonmark) evaluate cleanly
        let escaped_rows = vec![vec!["15".into(), "3".into(), "\\=A1\\*B1".into(), "\\=IF(A1\\>10, 1, 0)".into()]];
        let esc_eval = evaluate_table_grid(&escaped_rows);
        assert_eq!(esc_eval[0][2], "45");
        assert_eq!(esc_eval[0][3], "1");
    }

    #[test]
    fn binom_and_nested_sum_guards_prevent_cpu_freeze_and_stack_overflow() {
        let huge = binom_f64(1e9, 5e8).expect("huge binom must short-circuit cleanly");
        assert!(huge.is_infinite());

        let nested_res = eval_math_expression(r"\sum_{i=1}^{1000} \sum_{j=1}^{1000} 1");
        assert!(
            nested_res.is_err(),
            "nested summation exceeding op budget must return error instead of hanging"
        );

        let deep_parens = format!("{}1{}", "(".repeat(400), ")".repeat(400));
        assert!(eval_math_expression(&deep_parens).is_err());
    }
}

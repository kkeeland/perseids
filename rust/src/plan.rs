// The local half of lib/runner.rb: the ${{ }} expression subset, `if:`, matrix expansion and the needs-wave plan.
// Semantics follow runner.rb line for line, Ruby quirks included, so `perseids plan` matches `ruby runner.rb plan`.
use std::process::Command;

#[derive(Clone, Debug)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<V>),
    Map(Vec<(String, V)>), // insertion order, like a Ruby Hash
}

#[derive(Debug, PartialEq)]
pub struct Unsupported(pub String);
type R<T> = Result<T, Unsupported>;
fn err<T>(m: impl Into<String>) -> R<T> {
    Err(Unsupported(m.into()))
}

impl V {
    pub fn get(&self, k: &str) -> Option<&V> {
        match self {
            V::Map(m) => m.iter().find(|(kk, _)| kk == k).map(|(_, v)| v),
            _ => None,
        }
    }
    fn get_mut(&mut self, k: &str) -> Option<&mut V> {
        match self {
            V::Map(m) => m.iter_mut().find(|(kk, _)| kk == k).map(|(_, v)| v),
            _ => None,
        }
    }
    /// Hash#[]= : replace in place or append.
    pub fn set(&mut self, k: &str, v: V) {
        if let Some(x) = self.get_mut(k) {
            *x = v;
        } else if let V::Map(m) = self {
            m.push((k.to_string(), v));
        }
    }
    fn present(&self) -> Option<&V> {
        // Ruby truthiness of a looked-up value: nil and false are absent
        match self {
            V::Null | V::Bool(false) => None,
            v => Some(v),
        }
    }
    fn is_num(&self) -> bool {
        matches!(self, V::Int(_) | V::Float(_))
    }
}

fn map(pairs: Vec<(&str, V)>) -> V {
    V::Map(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
fn s(x: &str) -> V {
    V::Str(x.to_string())
}

// --- YAML and JSON in and out ---

pub fn from_yaml(y: serde_yaml::Value) -> V {
    use serde_yaml::Value as Y;
    match y {
        Y::Null => V::Null,
        Y::Bool(b) => V::Bool(b),
        Y::Number(n) => n.as_i64().map(V::Int).unwrap_or_else(|| V::Float(n.as_f64().unwrap_or(f64::NAN))),
        Y::String(x) => V::Str(x),
        Y::Sequence(a) => V::Arr(a.into_iter().map(from_yaml).collect()),
        Y::Mapping(m) => V::Map(m.into_iter().map(|(k, v)| (to_s(&from_yaml(k)), from_yaml(v))).collect()),
        Y::Tagged(t) => from_yaml(t.value),
    }
}

pub fn load(text: &str) -> Result<V, String> {
    let mut y: serde_yaml::Value = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    y.apply_merge().map_err(|e| e.to_string())?;
    Ok(from_yaml(y))
}

fn from_json(j: serde_json::Value) -> V {
    use serde_json::Value as J;
    match j {
        J::Null => V::Null,
        J::Bool(b) => V::Bool(b),
        J::Number(n) => n.as_i64().map(V::Int).unwrap_or_else(|| V::Float(n.as_f64().unwrap_or(f64::NAN))),
        J::String(x) => V::Str(x),
        J::Array(a) => V::Arr(a.into_iter().map(from_json).collect()),
        J::Object(m) => V::Map(m.into_iter().map(|(k, v)| (k, from_json(v))).collect()),
    }
}

fn to_json(v: &V) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        V::Null => J::Null,
        V::Bool(b) => J::Bool(*b),
        V::Int(i) => J::from(*i),
        V::Float(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or(J::Null),
        V::Str(x) => J::String(x.clone()),
        V::Arr(a) => J::Array(a.iter().map(to_json).collect()),
        V::Map(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), to_json(v))).collect()),
    }
}

fn pretty(v: &V) -> String {
    serde_json::to_string_pretty(&to_json(v)).unwrap_or_default()
}

/// Ruby's Float#to_s, close enough: 3.0 -> "3.0", 3.1 -> "3.1".
fn float_s(f: f64) -> String {
    let t = format!("{f:?}");
    t
}

/// Ruby's #to_s (for keys and `if:` sources).
fn to_s(v: &V) -> String {
    match v {
        V::Null => String::new(),
        V::Bool(b) => b.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => float_s(*f),
        V::Str(x) => x.clone(),
        other => pretty(other),
    }
}

/// Expr.str: how a value reads inside a string.
pub fn str(v: &V) -> String {
    match v {
        V::Float(f) if f.is_finite() && *f == f.floor() => format!("{}", *f as i64),
        V::Arr(_) | V::Map(_) => pretty(v),
        other => to_s(other),
    }
}

pub fn truthy(v: &V) -> bool {
    !matches!(v, V::Null | V::Bool(false) | V::Int(0)) && !matches!(v, V::Str(x) if x.is_empty()) && !matches!(v, V::Float(f) if *f == 0.0 || f.is_nan())
}

/// Ruby's == (case-sensitive strings, 1 == 1.0, hashes unordered).
fn ruby_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Str(x), V::Str(y)) => x == y,
        (V::Arr(x), V::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| ruby_eq(p, q)),
        (V::Map(x), V::Map(y)) => x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| ruby_eq(v, w))),
        _ if a.is_num() && b.is_num() => num(a) == num(b),
        _ => false,
    }
}

/// Ruby's Float(str): sign, digits with single underscores, fraction, exponent, or 0x hex.
fn ruby_float(t: &str) -> Option<f64> {
    let t = t.trim();
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let sign = if neg { -1.0 } else { 1.0 };
    if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|n| sign * n as f64);
    }
    let ok = body.split('_').all(|p| !p.is_empty())
        && body.chars().all(|c| c.is_ascii_digit() || "._eE+-".contains(c))
        && !body.ends_with('.')
        && !body.contains("._")
        && !body.contains("_.");
    if !ok {
        return None;
    }
    body.replace('_', "").parse::<f64>().ok().map(|f| sign * f)
}

fn num(v: &V) -> f64 {
    match v {
        V::Null | V::Bool(false) => 0.0,
        V::Bool(true) => 1.0,
        V::Int(i) => *i as f64,
        V::Float(f) => *f,
        V::Str(x) if x.trim().is_empty() => 0.0,
        V::Str(x) => ruby_float(x).unwrap_or(f64::NAN),
        _ => f64::NAN, // ponytail: Ruby raises here; NaN makes every comparison false
    }
}

fn eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Str(x), V::Str(y)) => x.to_lowercase() == y.to_lowercase(),
        _ if std::mem::discriminant(a) == std::mem::discriminant(b) || (a.is_num() && b.is_num()) => ruby_eq(a, b),
        _ => num(a) == num(b),
    }
}

fn index(v: &V, k: &V) -> V {
    match v {
        V::Map(m) => {
            if let V::Str(ks) = k {
                if let Some(x) = v.get(ks) {
                    return x.clone();
                }
            }
            let ks = to_s(k).to_lowercase();
            m.iter().find(|(kk, _)| kk.to_lowercase() == ks).map(|(_, x)| x.clone()).unwrap_or(V::Null)
        }
        V::Arr(a) if k.is_num() => {
            let i = num(k) as i64;
            let i = if i < 0 { i + a.len() as i64 } else { i };
            usize::try_from(i).ok().and_then(|i| a.get(i)).cloned().unwrap_or(V::Null)
        }
        _ => V::Null,
    }
}

// --- the expression evaluator ---

#[derive(Clone, Debug)]
enum Tok {
    Lit(V),
    Op(&'static str),
    Id(String),
}

/// `-?\d+(?:\.\d+)?(?![A-Za-z_])` with Ruby's backtracking: the longest match whose next char isn't a letter.
fn scan_number(b: &[u8]) -> Option<(usize, bool)> {
    let neg = usize::from(b.first() == Some(&b'-'));
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let n = digits(neg);
    let ok = |end: usize| !b.get(end).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_');
    for int in (1..=n).rev() {
        let e = neg + int;
        if int == n && b.get(e) == Some(&b'.') {
            let f = digits(e + 1);
            for fl in (1..=f).rev() {
                if ok(e + 1 + fl) {
                    return Some((e + 1 + fl, true));
                }
            }
        }
        if ok(e) {
            return Some((e, false));
        }
    }
    None
}

fn tokenize(src: &str) -> R<Vec<Tok>> {
    let bad = || Unsupported(format!("can't read ${{{{ {} }}}}", src.trim()));
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    const OPS: [&str; 15] = ["==", "!=", "&&", "||", "<=", ">=", "<", ">", "!", "(", ")", "[", "]", ",", "."];
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() || c == 0x0b {
            i += 1;
        } else if c == b'\'' {
            let mut j = i + 1;
            let mut lit = String::new();
            loop {
                match b.get(j) {
                    None => return Err(bad()),
                    Some(b'\'') if b.get(j + 1) == Some(&b'\'') => {
                        lit.push('\'');
                        j += 2;
                    }
                    Some(b'\'') => break,
                    Some(_) => {
                        let ch = src[j..].chars().next().unwrap();
                        lit.push(ch);
                        j += ch.len_utf8();
                    }
                }
            }
            out.push(Tok::Lit(V::Str(lit)));
            i = j + 1;
        } else if let Some((end, float)) = scan_number(&b[i..]) {
            let t = &src[i..i + end];
            out.push(Tok::Lit(if float { V::Float(t.parse().unwrap()) } else { t.parse().map(V::Int).unwrap_or(V::Float(t.parse().unwrap())) }));
            i += end;
        } else if let Some(op) = OPS.iter().find(|o| src[i..].starts_with(**o)) {
            out.push(Tok::Op(op));
            i += op.len();
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let n = b[i..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'-').count();
            out.push(Tok::Id(src[i..i + n].to_string()));
            i += n;
        } else {
            return Err(bad());
        }
    }
    Ok(out)
}

pub struct Expr<'a> {
    ctx: &'a V,
    failed: bool,
    t: Vec<Tok>,
    i: usize,
}

impl<'a> Expr<'a> {
    pub fn new(ctx: &'a V, failed: bool) -> Self {
        Expr { ctx, failed, t: vec![], i: 0 }
    }

    pub fn eval(&mut self, src: &str) -> R<V> {
        self.t = tokenize(src)?;
        self.i = 0;
        let v = self.parse_or()?;
        if self.i < self.t.len() {
            return err(format!("can't read ${{{{ {} }}}}", src.trim()));
        }
        Ok(v)
    }

    fn peek(&self, op: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Op(o)) if *o == op)
    }

    fn take(&mut self, op: Option<&str>) -> R<Tok> {
        if let Some(o) = op {
            if !self.peek(o) {
                return err(format!("expected {o}"));
            }
        }
        self.i += 1;
        self.t.get(self.i - 1).cloned().ok_or_else(|| Unsupported("unexpected end".into()))
    }

    fn parse_or(&mut self) -> R<V> {
        let mut v = self.parse_and()?;
        while self.peek("||") {
            self.take(None)?;
            let r = self.parse_and()?;
            if !truthy(&v) {
                v = r;
            }
        }
        Ok(v)
    }

    fn parse_and(&mut self) -> R<V> {
        let mut v = self.parse_cmp()?;
        while self.peek("&&") {
            self.take(None)?;
            let r = self.parse_cmp()?;
            if truthy(&v) {
                v = r;
            }
        }
        Ok(v)
    }

    fn parse_cmp(&mut self) -> R<V> {
        let v = self.parse_unary()?;
        for op in ["==", "!=", "<=", ">=", "<", ">"] {
            if !self.peek(op) {
                continue;
            }
            self.take(None)?;
            let r = self.parse_unary()?;
            let (a, b) = (num(&v), num(&r));
            return Ok(V::Bool(match op {
                "==" => eq(&v, &r),
                "!=" => !eq(&v, &r),
                "<=" => a <= b,
                ">=" => a >= b,
                "<" => a < b,
                _ => a > b,
            }));
        }
        Ok(v)
    }

    fn parse_unary(&mut self) -> R<V> {
        if self.peek("!") {
            self.take(None)?;
            return Ok(V::Bool(!truthy(&self.parse_unary()?)));
        }
        let mut v = self.parse_primary()?;
        loop {
            if self.peek(".") {
                self.take(None)?;
                let k = match self.take(None)? {
                    Tok::Lit(l) => l,
                    Tok::Op(o) => s(o),
                    Tok::Id(x) => V::Str(x),
                };
                v = index(&v, &k);
            } else if self.peek("[") {
                self.take(None)?;
                let k = self.parse_or()?;
                self.take(Some("]"))?;
                v = index(&v, &k);
            } else {
                return Ok(v);
            }
        }
    }

    fn parse_primary(&mut self) -> R<V> {
        match self.take(None)? {
            Tok::Lit(l) => Ok(l),
            Tok::Op("(") => {
                let v = self.parse_or()?;
                self.take(Some(")"))?;
                Ok(v)
            }
            Tok::Op(o) => err(format!("unexpected {o}")),
            Tok::Id(id) if self.peek("(") => self.call(&id),
            Tok::Id(id) => Ok(match id.as_str() {
                "true" => V::Bool(true),
                "false" => V::Bool(false),
                "null" => V::Null,
                _ => index(self.ctx, &V::Str(id)),
            }),
        }
    }

    fn call(&mut self, name: &str) -> R<V> {
        self.take(Some("("))?;
        let mut args = vec![];
        while !self.peek(")") {
            args.push(self.parse_or()?);
            if !self.peek(")") {
                self.take(Some(","))?;
            }
        }
        self.take(Some(")"))?;
        let arg = |i: usize| args.get(i).cloned().unwrap_or(V::Null);
        let low = |i: usize| str(&arg(i)).to_lowercase();
        Ok(match name.to_lowercase().as_str() {
            "always" => V::Bool(true),
            "success" => V::Bool(!self.failed),
            "failure" => V::Bool(self.failed),
            "cancelled" => V::Bool(false),
            "contains" => V::Bool(match arg(0) {
                V::Arr(a) => a.iter().any(|x| eq(x, &arg(1))),
                _ => low(0).contains(&low(1)),
            }),
            "startswith" => V::Bool(low(0).starts_with(&low(1))),
            "endswith" => V::Bool(low(0).ends_with(&low(1))),
            "format" => V::Str(format_fn(&str(&arg(0)), &args)),
            "join" => {
                let items = match arg(0) {
                    V::Null => vec![],
                    V::Arr(a) => a,
                    V::Map(m) => m.into_iter().map(|(k, v)| V::Arr(vec![V::Str(k), v])).collect(),
                    x => vec![x],
                };
                let sep = if args.len() > 1 { str(&arg(1)) } else { ",".into() };
                V::Str(items.iter().map(str).collect::<Vec<_>>().join(&sep))
            }
            "tojson" => V::Str(pretty(&arg(0))),
            "fromjson" => from_json(serde_json::from_str(&str(&arg(0))).map_err(|e| Unsupported(format!("fromJSON: {e}")))?),
            "hashfiles" => s(""), // only cache keys use it, and caches are skipped
            _ => return err(format!("{name}() isn't supported")),
        })
    }
}

/// format('{0}-{1}', a, b): `\{(\d+)\}` replaced by the argument after the format string.
fn format_fn(f: &str, args: &[V]) -> String {
    let mut out = String::new();
    let mut rest = f;
    while let Some(p) = rest.find('{') {
        out.push_str(&rest[..p]);
        let after = &rest[p + 1..];
        let n = after.bytes().take_while(u8::is_ascii_digit).count();
        if n > 0 && after.as_bytes().get(n) == Some(&b'}') {
            let i: usize = after[..n].parse().unwrap_or(usize::MAX);
            out.push_str(&args.get(i.saturating_add(1)).map(str).unwrap_or_default());
            rest = &after[n + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out + rest
}

/// Each ${{ }} in a string, evaluated and read as a string. Non-strings pass through.
pub fn interpolate(v: &V, ctx: &V, failed: bool) -> R<V> {
    let V::Str(text) = v else { return Ok(v.clone()) };
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(p) = rest.find("${{") {
        let Some(q) = rest[p + 3..].find("}}") else { break };
        out.push_str(&rest[..p]);
        out.push_str(&str(&Expr::new(ctx, failed).eval(&rest[p + 3..p + 3 + q])?));
        rest = &rest[p + 3 + q + 2..];
    }
    Ok(V::Str(out + rest))
}

/// `\b(always|success|failure|cancelled)\s*\(`, case-insensitive.
fn has_status_fn(s: &str) -> bool {
    let l = s.to_lowercase();
    let b = l.as_bytes();
    ["always", "success", "failure", "cancelled"].iter().any(|f| {
        l.match_indices(f).any(|(i, _)| {
            let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
            (i == 0 || !word(b[i - 1])) && b[i + f.len()..].iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'(')
        })
    })
}

/// An `if:` with no status function means success() && (it), as on GitHub.
pub fn cond(src: Option<&V>, ctx: &V, failed: bool) -> R<bool> {
    let src = match src {
        None | Some(V::Null) => return Ok(!failed),
        Some(V::Bool(b)) => return Ok(*b && !failed),
        Some(v) => v,
    };
    let mut s = to_s(src).trim().to_string();
    if s.starts_with("${{") && s.ends_with("}}") && s.len() >= 5 {
        s = s[3..s.len() - 2].to_string();
    }
    let v = truthy(&Expr::new(ctx, failed).eval(&s)?);
    Ok(if has_status_fn(&s) { v } else { v && !failed })
}

// --- matrix and plan ---

fn as_list(v: Option<&V>) -> Vec<V> {
    // Ruby's Array(x)
    match v {
        None | Some(V::Null) => vec![],
        Some(V::Arr(a)) => a.clone(),
        Some(V::Map(m)) => m.iter().map(|(k, v)| V::Arr(vec![V::Str(k.clone()), v.clone()])).collect(),
        Some(x) => vec![x.clone()],
    }
}

fn entries(v: &V) -> &[(String, V)] {
    match v {
        V::Map(m) => m,
        _ => &[],
    }
}

pub fn matrix(job: &V) -> R<Vec<V>> {
    let m = match job.get("strategy").and_then(|st| st.get("matrix")) {
        None | Some(V::Null) => return Ok(vec![V::Map(vec![])]),
        Some(V::Str(_)) => return err("its matrix is built by an expression"),
        Some(m) => m,
    };
    let list = |k: &str| match m.get(k) {
        None | Some(V::Null) => Ok(vec![]),
        Some(V::Arr(a)) => Ok(a.clone()),
        Some(_) => err(format!("matrix.{k} is an expression")),
    };
    let (inc, exc) = (list("include")?, list("exclude")?);
    let keys: Vec<&(String, V)> = entries(m).iter().filter(|(k, _)| k != "include" && k != "exclude").collect();
    let mut combos = vec![V::Map(vec![])];
    for (k, vals) in &keys {
        let V::Arr(vals) = vals else { return err(format!("matrix.{k} is an expression")) };
        combos = combos
            .iter()
            .flat_map(|c| {
                vals.iter().map(move |x| {
                    let mut c = c.clone();
                    c.set(k, x.clone());
                    c
                })
            })
            .collect();
    }
    if keys.is_empty() {
        combos.clear();
    }
    combos.retain(|c| !exc.iter().any(|e| entries(e).iter().all(|(k, v)| ruby_eq(c.get(k).unwrap_or(&V::Null), v))));
    let orig = combos.clone();
    let mut extra = vec![];
    // An include entry joins every combination whose original values it doesn't overwrite, else it is its own.
    for e in &inc {
        let mut hit = false;
        for (i, c) in combos.iter_mut().enumerate() {
            if entries(e).iter().all(|(k, v)| orig[i].get(k).map_or(true, |o| ruby_eq(o, v))) {
                for (k, v) in entries(e) {
                    c.set(k, v.clone());
                }
                hit = true;
            }
        }
        if !hit {
            extra.push(e.clone());
        }
    }
    combos.extend(extra);
    Ok(combos)
}

fn git(args: &[&str]) -> String {
    Command::new("git").args(args).stderr(std::process::Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

pub fn github_ctx(job_id: &str) -> V {
    let mut r = std::env::var("PERSEIDS_REF_NAME").unwrap_or_default();
    if r.is_empty() {
        r = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    }
    let repo = std::env::var("PERSEIDS_REPOSITORY").unwrap_or_default();
    map(vec![
        ("sha", V::Str(std::env::var("PERSEIDS_SHA").unwrap_or_else(|_| git(&["rev-parse", "HEAD"])))),
        ("ref", V::Str(format!("refs/heads/{r}"))),
        ("ref_name", V::Str(r)),
        ("head_ref", s("")),
        ("base_ref", s("")),
        ("event_name", V::Str(std::env::var("PERSEIDS_EVENT").unwrap_or_else(|_| "push".into()))),
        ("repository_owner", V::Str(repo.split('/').next().unwrap_or("").to_string())),
        ("repository", V::Str(repo)),
        ("workspace", V::Str(std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default())),
        ("job", s(job_id)),
        ("run_id", V::Str(std::process::id().to_string())),
        ("run_number", s("1")),
        ("run_attempt", s("1")),
        ("actor", s("perseids")),
        ("server_url", s("https://github.com")),
        ("event", V::Map(vec![])),
    ])
}

pub fn base_ctx(wf: &V, job_id: &str) -> R<V> {
    let tmp = std::env::temp_dir().display().to_string().trim_end_matches('/').to_string();
    let mut ctx = map(vec![
        ("github", github_ctx(job_id)),
        ("env", V::Map(vec![])),
        ("matrix", V::Map(vec![])),
        ("secrets", V::Map(vec![])),
        ("vars", V::Map(vec![])),
        ("inputs", V::Map(vec![])),
        ("needs", V::Map(vec![])),
        ("steps", V::Map(vec![])),
        ("job", map(vec![("services", V::Map(vec![]))])),
        ("strategy", V::Map(vec![])),
        ("runner", map(vec![("os", s("Linux")), ("arch", s("X64")), ("name", s("perseids")), ("temp", V::Str(tmp))])),
    ]);
    for (k, v) in entries(wf.get("env").unwrap_or(&V::Null)) {
        let x = V::Str(str(&interpolate(v, &ctx, false)?));
        ctx.get_mut("env").unwrap().set(k, x);
    }
    Ok(ctx)
}

#[derive(Debug, Clone)]
pub struct Row {
    pub wave: usize,
    pub job: String,
    pub index: usize,
    pub state: String,
    pub needs: String,
    pub label: String,
    pub note: String,
}

impl Row {
    /// The TSV line runner.rb prints: "-" for empty cells, tabs and newlines turned to spaces.
    pub fn tsv(&self) -> String {
        let cell = |c: String| if c.is_empty() { "-".into() } else { c.replace(['\t', '\n'], " ") };
        [self.wave.to_string(), self.job.clone(), self.index.to_string(), self.state.clone(), self.needs.clone(), self.label.clone(), self.note.clone()].map(cell).join("\t")
    }
}

/// Waves follow `needs:`; a job's entries all run at once. Asked-for jobs run in one wave, needs ignored.
pub fn plan(wf: &V, only: &[String]) -> R<Vec<Row>> {
    let none = V::Map(vec![]);
    let jobs = entries(wf.get("jobs").unwrap_or(&none));
    let names: Vec<&str> = jobs.iter().map(|(k, _)| k.as_str()).collect();
    for j in only {
        if !names.contains(&j.as_str()) {
            return err(format!("no job {j} (jobs: {})", names.join(", ")));
        }
    }
    let job = |id: &str| jobs.iter().find(|(k, _)| k == id).map(|(_, v)| v);
    let needs_of = |id: &str| as_list(job(id).and_then(|j| j.get("needs"))).iter().map(to_s).collect::<Vec<_>>();
    let mut wave: Vec<(String, usize)> = vec![];
    fn depth(id: &str, seen: &mut Vec<String>, wave: &mut Vec<(String, usize)>, exists: &dyn Fn(&str) -> bool, needs_of: &dyn Fn(&str) -> Vec<String>) -> R<usize> {
        if seen.iter().any(|x| x == id) {
            return err(format!("needs: loop at {id}"));
        }
        if !exists(id) {
            return err(format!("a job needs {id}, which isn't in the workflow"));
        }
        if let Some((_, w)) = wave.iter().find(|(k, _)| k == id) {
            return Ok(*w);
        }
        seen.push(id.to_string());
        let mut w = 0;
        for n in needs_of(id) {
            w = w.max(depth(&n, seen, wave, exists, needs_of)? + 1);
        }
        seen.pop();
        wave.push((id.to_string(), w));
        Ok(w)
    }
    let ids: Vec<String> = if only.is_empty() { names.iter().map(|s| s.to_string()).collect() } else { only.to_vec() };
    if only.is_empty() {
        for id in &ids {
            depth(id, &mut vec![], &mut wave, &|i| job(i).is_some(), &needs_of)?;
        }
    }
    let wave_of = |id: &str| wave.iter().find(|(k, _)| k == id).map_or(0, |(_, w)| *w);
    let mut sorted = ids.clone();
    sorted.sort_by_key(|id| (wave_of(id), names.iter().position(|n| n == id)));
    let ctx = base_ctx(wf, "")?;
    let mut rows = vec![];
    for id in &sorted {
        let j = job(id).unwrap();
        let w = wave_of(id);
        let needs = if only.is_empty() { needs_of(id).join(",") } else { String::new() };
        let row = |index, state: &str, label: String, note: String| Row { wave: w, job: id.clone(), index, state: state.into(), needs: needs.clone(), label, note };
        let combos = match matrix(j) {
            Ok(c) => c,
            Err(Unsupported(e)) => {
                rows.push(row(0, "unsupported", id.clone(), e));
                continue;
            }
        };
        for (i, combo) in combos.into_iter().enumerate() {
            let vals: Vec<String> = entries(&combo).iter().map(|(_, v)| str(v)).collect();
            let label = if vals.is_empty() { id.clone() } else { format!("{id} ({})", vals.join(", ")) };
            let mut jctx = ctx.clone();
            jctx.set("matrix", combo);
            let verdict = (|| -> R<(&str, String)> {
                let ro = j.get("runs-on");
                let ro = match ro {
                    Some(V::Map(_)) => ro.unwrap().get("labels"),
                    _ => ro,
                };
                let mut runs_on = vec![];
                for l in as_list(ro) {
                    runs_on.push(str(&interpolate(&l, &jctx, false)?));
                }
                Ok(if !cond(j.get("if"), &jctx, false)? {
                    ("skip", "its if: is false".into())
                } else if j.get("uses").and_then(V::present).is_some() {
                    ("unsupported", "it calls a reusable workflow".into())
                } else if j.get("container").and_then(V::present).is_some() {
                    ("unsupported", "it runs in a container".into())
                } else if let Some(os) = runs_on.iter().find(|l| {
                    let l = l.to_lowercase();
                    l.contains("macos") || l.contains("windows")
                }) {
                    ("unsupported", format!("it runs on {os}"))
                } else {
                    ("run", String::new())
                })
            })();
            let (state, note) = verdict.unwrap_or_else(|Unsupported(e)| ("unsupported", e));
            rows.push(row(i, state, label, note));
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    // The plan cases of test/runner_test.rb.
    use super::*;

    fn ctx() -> V {
        load("matrix: { node: 20 }\nenv: { A: x }\ngithub: { event_name: push }").unwrap()
    }
    fn ev(src: &str, c: &V) -> R<V> {
        Expr::new(c, false).eval(src)
    }
    fn evs(src: &str) -> String {
        str(&ev(src, &ctx()).unwrap())
    }

    #[test]
    fn expressions() {
        let c = ctx();
        assert!(matches!(ev("matrix.node", &c), Ok(V::Int(20))));
        assert!(matches!(ev("github.event_name == 'PUSH'", &c), Ok(V::Bool(true)))); // strings compare case-insensitively
        assert_eq!(evs("env.MISSING || 'fallback'"), "fallback");
        assert_eq!(evs("(github.event_name == 'workflow_dispatch' && vars.R) || 'ubuntu-latest'"), "ubuntu-latest");
        assert_eq!(evs("matrix.node >= 18 && !contains(env.A, 'y')"), "true");
        assert_eq!(evs("format('v-{0}', matrix.node)"), "v-20");
        assert_eq!(evs("startsWith(github['event_name'], 'pu')"), "true");
        assert_eq!(evs("'it''s'"), "it's");
        assert_eq!(str(&interpolate(&s("${{ env.A == 'x' && 'a' }} b"), &c, false).unwrap()), "a b");
        assert!(ev("github.event.commits.*.message", &c).is_err());
        assert!(ev("nope()", &c).is_err());
    }

    #[test]
    fn ruby_quirks() {
        assert_eq!(evs("1.5 < 2"), "true");
        assert_eq!(evs("'0x1A' == 26"), "true");
        assert_eq!(evs("'' == 0"), "true");
        assert_eq!(evs("join(fromJSON('[1, 2.0, \"a\"]'), '-')"), "1-2-a");
        assert_eq!(evs("toJSON(matrix)"), "{\n  \"node\": 20\n}");
        assert_eq!(evs("format('{0}{{1}}{2}', 'a', 'b')"), "a{b}");
        assert_eq!(evs("contains(fromJSON('[\"A\"]'), 'a')"), "true");
        assert_eq!(evs("fromJSON('[1,2,3]')[-1]"), "3");
    }

    #[test]
    fn if_implies_success() {
        let c = V::Map(vec![]);
        assert!(cond(None, &c, false).unwrap());
        assert!(!cond(None, &c, true).unwrap());
        assert!(!cond(Some(&s("true")), &c, true).unwrap());
        assert!(cond(Some(&s("${{ always() }}")), &c, true).unwrap());
        assert!(cond(Some(&s("failure()")), &c, true).unwrap());
    }

    #[test]
    fn matrix_include_exclude() {
        let job = load(
            "strategy:\n  matrix:\n    os: [a, b]\n    node: [18, 20]\n    exclude: [{ os: b, node: 18 }]\n    include: [{ node: 20, extra: 'yes' }, { os: c, node: 22 }]\n",
        )
        .unwrap();
        let got: Vec<String> = matrix(&job).unwrap().iter().map(|c| serde_json::to_string(&to_json(c)).unwrap()).collect();
        assert_eq!(
            got,
            [r#"{"os":"a","node":18}"#, r#"{"os":"a","node":20,"extra":"yes"}"#, r#"{"os":"b","node":20,"extra":"yes"}"#, r#"{"os":"c","node":22}"#]
        );
        assert_eq!(matrix(&V::Map(vec![])).unwrap().len(), 1);
    }

    #[test]
    fn plan_waves_and_unsupported() {
        let wf = load(
            r#"
jobs:
  build: { runs-on: ubuntu-latest, steps: [] }
  test:
    needs: build
    runs-on: ${{ matrix.os }}
    strategy: { matrix: { os: [ubuntu-latest, macos-14] } }
    steps: []
  deploy: { needs: [test], if: "github.event_name == 'release'", runs-on: ubuntu-latest, steps: [] }
"#,
        )
        .unwrap();
        let rows = plan(&wf, &[]).unwrap();
        let head: Vec<_> = rows.iter().map(|r| (r.wave, r.job.as_str(), r.index, r.state.as_str(), r.needs.as_str())).collect();
        assert_eq!(head, [(0, "build", 0, "run", ""), (1, "test", 0, "run", "build"), (1, "test", 1, "unsupported", "build"), (2, "deploy", 0, "skip", "test")]);
        assert_eq!(rows[2].label, "test (macos-14)");
        let one = plan(&wf, &["test".into()]).unwrap();
        assert_eq!((one[0].wave, one[0].job.as_str(), one[0].index, one[0].state.as_str(), one[0].needs.as_str()), (0, "test", 0, "run", ""));
    }
}

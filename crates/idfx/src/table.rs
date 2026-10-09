//! idDeclTable / idLookupTable (`generated/decls/table/*.decl`).
//!
//! Text: `{ [clamp] [snap] [spline] [min m] [max M] [left l] [right r] { v, v, .. | t:v, t:v, .. } }`
//! (parser 0x1417f6ee0). A bare value takes the running index as its time; after parsing, values
//! outside [0,1] are renormalised into [0,1] with min/max remapped to keep the output, and times past 1
//! are scaled to end at (n-1)/n when clamped, 1 otherwise (0x140284100). Lookup 0x140284580:
//! `min + (max - min) * curve(x)` where the curve is linear, snapped or Catmull-Rom over the knots with
//! clamp (boundary 1) or wrap (boundary 2, period last time + (1 - last time)).

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub clamp: bool,
    pub snap: bool,
    pub spline: bool,
    pub min: f32,
    pub max: f32,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
    /// idCatmullRomSpline closeTime: 1 - last time (wrap period padding).
    close_time: f32,
}

impl Default for Table {
    fn default() -> Self {
        // idLookupTable ctor 0x1402840d0.
        Table { clamp: false, snap: false, spline: false, min: 0.0, max: 1.0, times: Vec::new(), values: Vec::new(), close_time: 0.0 }
    }
}

fn tokens(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        match c {
            '{' | '}' | ',' | ':' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Table {
    pub fn parse(src: &str) -> Result<Table> {
        let toks = tokens(src);
        let mut t = Table::default();
        let mut i = 0;
        // Skip to the body's opening brace.
        while i < toks.len() && toks[i] != "{" {
            i += 1;
        }
        i += 1;
        let num = |s: &str| -> Result<f32> { s.trim_end_matches('f').parse::<f32>().map_err(|_| anyhow::anyhow!("bad table number {s:?}")) };
        let (mut lo, mut hi) = (1e30f32, -1e30f32);
        let mut clamp = false;
        while i < toks.len() {
            let tok = toks[i].to_ascii_lowercase();
            i += 1;
            match tok.as_str() {
                "}" => break,
                "snap" => {
                    t.snap = true;
                    if t.spline {
                        t.snap = false;
                    }
                }
                "clamp" => clamp = true,
                "spline" => {
                    t.spline = true;
                    t.snap = false;
                }
                "max" | "min" | "left" | "right" => {
                    let v = num(toks.get(i).map(String::as_str).unwrap_or(""))?;
                    i += 1;
                    match tok.as_str() {
                        "max" => t.max = v,
                        "min" => t.min = v,
                        _ => {}
                    }
                }
                "{" => {
                    let mut index = 0.0f32;
                    loop {
                        let Some(a) = toks.get(i) else { bail!("unterminated table values") };
                        if a == "}" {
                            i += 1;
                            break;
                        }
                        let mut v = num(a)?;
                        i += 1;
                        if toks.get(i).map(String::as_str) == Some(":") {
                            index = v;
                            v = num(toks.get(i + 1).map(String::as_str).unwrap_or(""))?;
                            i += 2;
                        }
                        if t.values.len() >= 64 {
                            bail!("table has more than 64 knots");
                        }
                        lo = lo.min(v);
                        hi = hi.max(v);
                        t.times.push(index);
                        t.values.push(v);
                        index += 1.0;
                        match toks.get(i).map(String::as_str) {
                            Some(",") => i += 1,
                            Some("}") => {}
                            other => bail!("expected , or }} in table, got {other:?}"),
                        }
                    }
                }
                other => bail!("unknown table keyword {other:?}"),
            }
        }
        if lo < 0.0 || 1.0 < hi {
            let range = hi - lo;
            for v in &mut t.values {
                *v = (*v - lo) / range;
            }
            t.min = t.min * range + lo;
            t.max = t.max * range + lo;
        }
        t.clamp = clamp;
        t.finish();
        Ok(t)
    }

    /// 0x140284100: sort knots, squeeze times past 1, set the wrap padding.
    fn finish(&mut self) {
        let mut idx: Vec<usize> = (0..self.times.len()).collect();
        idx.sort_by(|&a, &b| self.times[a].total_cmp(&self.times[b]));
        self.times = idx.iter().map(|&i| self.times[i]).collect();
        self.values = idx.iter().map(|&i| self.values[i]).collect();
        let n = self.times.len();
        if n > 1 && self.times[n - 1] > 1.0 {
            let scale = if self.clamp { (n - 1) as f32 / n as f32 } else { 1.0 };
            let last = self.times[n - 1];
            if last != 0.0 {
                let s = scale / last;
                for t in &mut self.times {
                    *t *= s;
                }
            }
        }
        self.close_time = 1.0 - self.times.last().copied().unwrap_or(0.0);
    }

    fn boundary(&self) -> u8 {
        if self.clamp { 1 } else { 2 }
    }

    fn time(&self, i: i32) -> f32 {
        let n = self.times.len() as i32;
        if i < 0 {
            if self.boundary() == 2 {
                // 0x1402847ab, as written (it pads with the first time, not the last).
                let p = self.close_time + self.times[0];
                return (i / n) as f32 * p - (p - self.times[((n + i % n) % n) as usize]);
            }
            return self.times[0] + (self.times[1.min(n as usize - 1)] - self.times[0]) * i as f32;
        }
        if i > n - 1 {
            if self.boundary() == 2 {
                let p = self.times[(n - 1) as usize] + self.close_time;
                return (i / n) as f32 * p + self.times[(i % n) as usize];
            }
            let last = self.times[(n - 1) as usize];
            let prev = self.times[(n - 2).max(0) as usize];
            return last + (last - prev) * (i - (n - 1)) as f32;
        }
        self.times[i as usize]
    }

    fn value(&self, i: i32) -> f32 {
        let n = self.values.len() as i32;
        if i < 0 {
            return match self.boundary() {
                2 => self.values[(((i % n) + n) % n) as usize],
                _ => self.values[0],
            };
        }
        if i > n - 1 {
            return match self.boundary() {
                2 => self.values[(i % n) as usize],
                _ => self.values[(n - 1) as usize],
            };
        }
        self.values[i as usize]
    }

    /// idCurve::IndexForTime: first knot whose time is >= x (n when past the end).
    fn index_for_time(&self, x: f32) -> i32 {
        self.times.iter().position(|&t| t >= x).unwrap_or(self.times.len()) as i32
    }

    /// The raw curve value in [0, 1] (0x140284600).
    pub fn curve(&self, x: f32) -> f32 {
        let n = self.times.len();
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return self.values[0];
        }
        // Boundary handling of the input time (0x140284040).
        let mut x = x;
        if self.boundary() == 1 {
            if self.times[0] > x {
                return self.values[0];
            }
            if x >= self.times[n - 1] {
                return self.values[n - 1];
            }
        } else {
            let p = self.times[n - 1] + self.close_time;
            if p != 0.0 {
                x -= (x / p).floor() * p;
            }
        }
        let i = self.index_for_time(x);
        if self.spline {
            // idCurve_CatmullRomSpline basis (0x140283f40) over knots i-2 .. i+1.
            let t0 = self.time(i - 1);
            let s = (x - t0) / (self.time(i) - t0);
            let b = [
                ((2.0 - s) * s - 1.0) * s * 0.5,
                ((s * 3.0 - 5.0) * s * s + 2.0) * 0.5,
                ((4.0 - s * 3.0) * s + 1.0) * s * 0.5,
                (s - 1.0) * s * s * 0.5,
            ];
            return (0..4).map(|k| self.value(i - 2 + k) * b[k as usize]).sum();
        }
        if self.snap {
            return self.value(i - 1);
        }
        let (ta, tb) = (self.time(i - 1), self.time(i));
        if ta == tb {
            return 0.0;
        }
        let f = (x - ta) / (tb - ta);
        let (va, vb) = (self.value(i - 1), self.value(i));
        (1.0 - f) * va + f * vb
    }

    /// idDeclTable lookup: the curve remapped into [min, max].
    pub fn lookup(&self, x: f32) -> f32 {
        let c = self.curve(x);
        let tiny = |v: f32| if v.abs() <= 1e-18 { 0.0 } else { v };
        (1.0 - c) * tiny(self.min) + c * tiny(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_and_clamps() {
        let t = Table::parse("{ clamp min 0 max 1 left 0 right 1 {0:0.00625002, 0.0589302:0.35, 0.157752:0.5125, 1:0} }").unwrap();
        assert_eq!(t.times.len(), 4);
        assert!((t.lookup(0.0) - 0.006_250_02).abs() < 1e-6);
        assert!((t.lookup(0.0589302) - 0.35).abs() < 1e-5);
        assert_eq!(t.lookup(2.0), 0.0);
    }

    #[test]
    fn renormalises_values() {
        let t = Table::parse("{ clamp { 0, 2, 4 } }").unwrap();
        // values 0..4 -> 0..1 with min 0, max 4; times 0,1,2 -> 0, 1/3, 2/3
        assert!((t.lookup(1.0 / 3.0) - 2.0).abs() < 1e-5);
    }
}

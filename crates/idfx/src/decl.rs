//! Readers for the reflection-written decl text (`key = value;`, idList as `{ num = N; item[i] = ...; }`,
//! fixed arrays as `name[i]`, vectors as `{ x = ..; y = ..; }`). The engine writes only fields that differ
//! from the constructor defaults, so every reader takes the default.

use glam::{Vec2, Vec3, Vec4};
use idres::decl::{Block, Value};

pub fn f32_or(b: Option<&Block>, key: &str, d: f32) -> f32 {
    b.and_then(|b| b.get(key)).and_then(Value::as_f32).unwrap_or(d)
}

pub fn i32_or(b: Option<&Block>, key: &str, d: i32) -> i32 {
    b.and_then(|b| b.get(key)).and_then(Value::as_f32).map(|v| v as i32).unwrap_or(d)
}

pub fn bool_or(b: Option<&Block>, key: &str, d: bool) -> bool {
    b.and_then(|b| b.get(key)).and_then(Value::as_bool).unwrap_or(d)
}

pub fn str_of<'a>(b: Option<&'a Block>, key: &str) -> Option<&'a str> {
    b.and_then(|b| b.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn block<'a>(b: Option<&'a Block>, key: &str) -> Option<&'a Block> {
    b.and_then(|b| b.get(key)).and_then(Value::as_block)
}

/// An enum stored by name; `names[i]` is value i.
pub fn enum_or(b: Option<&Block>, key: &str, names: &[&str], d: usize) -> usize {
    str_of(b, key).and_then(|s| names.iter().position(|n| n.eq_ignore_ascii_case(s))).unwrap_or(d)
}

pub fn vec2_or(b: Option<&Block>, key: &str, d: Vec2) -> Vec2 {
    let v = block(b, key);
    Vec2::new(f32_or(v, "x", d.x), f32_or(v, "y", d.y))
}

pub fn vec3_or(b: Option<&Block>, key: &str, d: Vec3) -> Vec3 {
    let v = block(b, key);
    Vec3::new(f32_or(v, "x", d.x), f32_or(v, "y", d.y), f32_or(v, "z", d.z))
}

pub fn vec4_or(b: Option<&Block>, key: &str, d: Vec4) -> Vec4 {
    let v = block(b, key);
    Vec4::new(f32_or(v, "x", d.x), f32_or(v, "y", d.y), f32_or(v, "z", d.z), f32_or(v, "w", d.w))
}

/// idAngles `{ pitch; yaw; roll }`.
pub fn angles_or(b: Option<&Block>, key: &str) -> [f32; 3] {
    let v = block(b, key);
    [f32_or(v, "pitch", 0.0), f32_or(v, "yaw", 0.0), f32_or(v, "roll", 0.0)]
}

/// Element `name[i]` of a fixed array block.
pub fn elem<'a>(b: Option<&'a Block>, name: &str, i: usize) -> Option<&'a Value> {
    b.and_then(|b| b.get(&format!("{name}[{i}]")))
}

/// idList `{ num = N; item[i] = ...; }`.
pub fn list<'a>(b: Option<&'a Block>, key: &str) -> Vec<&'a Value> {
    let Some(l) = block(b, key) else { return Vec::new() };
    let n = i32_or(Some(l), "num", 0).max(0) as usize;
    (0..n).filter_map(|i| l.get(&format!("item[{i}]"))).collect()
}

pub fn list_str(b: Option<&Block>, key: &str) -> Vec<String> {
    list(b, key).into_iter().filter_map(|v| v.as_str()).map(str::to_string).collect()
}

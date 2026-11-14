//! Minimal INI parser (no dependencies).
//!
//! Supported syntax:
//! ```ini
//! ; comment
//! # comment
//! [section]
//! key = value   ; trailing comment
//! ```
//! Section and key names are case-insensitive. Later duplicates win.

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Ini {
    values: HashMap<(String, String), String>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut values = HashMap::new();
        let mut section = String::new();
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                if let Some(name) = rest.strip_suffix(']') {
                    section = name.trim().to_ascii_lowercase();
                }
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim().to_ascii_lowercase();
                if !key.is_empty() {
                    values.insert((section.clone(), key), v.trim().to_string());
                }
            }
        }
        Self { values }
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.values
            .get(&(section.to_ascii_lowercase(), key.to_ascii_lowercase()))
            .map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.keys().map(|(s, k)| (s.as_str(), k.as_str()))
    }
}

fn strip_comment(line: &str) -> &str {
    // A comment starts at ';' or '#' at line start or preceded by whitespace.
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if (b == b';' || b == b'#') && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return &line[..i];
        }
    }
    line
}

pub fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parses decimal or `0x`-prefixed hexadecimal unsigned integers.
pub fn parse_u32(v: &str) -> Option<u32> {
    let v = v.trim();
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        v.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_keys_and_comments() {
        let ini = Ini::parse(
            "; header\n[Monitor]\nLevel = 2 ; inline\n# other\n[filter]\nenabled=yes\nurl = a;b\n",
        );
        assert_eq!(ini.get("monitor", "level"), Some("2"));
        assert_eq!(ini.get("FILTER", "Enabled"), Some("yes"));
        // ';' not preceded by whitespace is part of the value
        assert_eq!(ini.get("filter", "url"), Some("a;b"));
        assert_eq!(ini.get("filter", "missing"), None);
    }

    #[test]
    fn keys_outside_sections_use_empty_section() {
        let ini = Ini::parse("a=1\n[s]\nb=2");
        assert_eq!(ini.get("", "a"), Some("1"));
        assert_eq!(ini.get("s", "b"), Some("2"));
    }

    #[test]
    fn value_parsers() {
        assert_eq!(parse_bool("On"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_u32("0x7A"), Some(0x7A));
        assert_eq!(parse_u32("122"), Some(122));
        assert_eq!(parse_u32("x"), None);
    }
}

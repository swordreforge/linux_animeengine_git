//! Minimal Valve KeyValues (VDF) parser.
//!
//! Only the subset Steam uses for `libraryfolders.vdf` and
//! `appworkshop_<appid>.acf`: quoted or bare tokens, nested `{ … }` blocks,
//! `//` line comments and `\"`/`\\`/`\n`/`\t` escapes.

use crate::{Error, Result};

/// A KeyValues value: either a string or a block of nested entries.
#[derive(Debug, Clone, PartialEq)]
pub enum Kv {
    Str(String),
    Block(Vec<(String, Kv)>),
}

impl Kv {
    /// First entry named `key` inside a block.
    pub fn get(&self, key: &str) -> Option<&Kv> {
        self.entries()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn entries(&self) -> &[(String, Kv)] {
        match self {
            Kv::Block(entries) => entries,
            Kv::Str(_) => &[],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Kv::Str(value) => Some(value),
            Kv::Block(_) => None,
        }
    }

    /// Parse the string value as an unsigned integer.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_str()?.trim().parse().ok()
    }
}

/// Parse a VDF document into its top-level `(name, block)` pair.
pub fn parse(input: &str) -> Result<(String, Kv)> {
    let mut parser = Parser::new(input);
    parser.skip_trivia();
    let key = parser.parse_key()?;
    let block = parser.parse_block()?;
    Ok((key, block))
}

/// Parse a VDF document and return just the root block.
pub fn parse_root(input: &str) -> Result<Kv> {
    Ok(parse(input)?.1)
}

fn err(msg: &str) -> Error {
    Error::Parse(msg.to_string())
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let current = self.peek();
        if current.is_some() {
            self.pos += 1;
        }
        current
    }

    fn skip_trivia(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.pos += 1;
            }
            if self.peek() == Some('/') && self.chars.get(self.pos + 1) == Some(&'/') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn parse_key(&mut self) -> Result<String> {
        self.skip_trivia();
        if self.peek() == Some('"') {
            self.parse_quoted()
        } else {
            self.parse_bare()
        }
    }

    fn parse_bare(&mut self) -> Result<String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '"' || c == '{' || c == '}' {
                break;
            }
            out.push(c);
            self.pos += 1;
        }
        if out.is_empty() {
            return Err(err("expected a key"));
        }
        Ok(out)
    }

    fn parse_quoted(&mut self) -> Result<String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let escaped = self.bump().ok_or_else(|| err("unterminated escape"))?;
                    out.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                }
                other => out.push(other),
            }
        }
        Err(err("unterminated string"))
    }

    fn parse_value(&mut self) -> Result<Kv> {
        self.skip_trivia();
        match self.peek() {
            Some('{') => self.parse_block(),
            Some('"') => Ok(Kv::Str(self.parse_quoted()?)),
            Some(_) => Ok(Kv::Str(self.parse_bare()?)),
            None => Err(err("unexpected end of input")),
        }
    }

    fn parse_block(&mut self) -> Result<Kv> {
        self.skip_trivia();
        if self.peek() != Some('{') {
            return Err(err("expected '{'"));
        }
        self.pos += 1;

        let mut entries = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => return Err(err("unterminated block")),
                Some('}') => {
                    self.pos += 1;
                    return Ok(Kv::Block(entries));
                }
                Some(_) => {
                    let key = self.parse_key()?;
                    let value = self.parse_value()?;
                    entries.push((key, value));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_blocks_and_scalars() {
        let doc = r#"
            "libraryfolders"
            {
                "0"
                {
                    "path"  "D:\\Games\\AnimaEngine"
                    "label" ""
                    "apps" { "3474900" "123" }
                }
            }
        "#;
        let (name, root) = parse(doc).unwrap();
        assert_eq!(name, "libraryfolders");

        let zero = root.get("0").unwrap();
        assert_eq!(
            zero.get("path").unwrap().as_str(),
            Some("D:\\Games\\AnimaEngine")
        );
        assert_eq!(zero.get("label").unwrap().as_str(), Some(""));
        assert_eq!(
            zero.get("apps").unwrap().get("3474900").unwrap().as_u64(),
            Some(123)
        );
    }

    #[test]
    fn skips_comments_and_bare_tokens() {
        let doc = "root { // a comment\n key value\n }";
        let root = parse_root(doc).unwrap();
        assert_eq!(root.get("key").unwrap().as_str(), Some("value"));
    }

    #[test]
    fn supports_escapes() {
        let doc = r#"root { "k" "a\"b\\c" }"#;
        let root = parse_root(doc).unwrap();
        assert_eq!(root.get("k").unwrap().as_str(), Some("a\"b\\c"));
    }

    #[test]
    fn errors_on_malformed_input() {
        assert!(matches!(parse("root { \"k\" "), Err(Error::Parse(_))));
        assert!(matches!(parse("no block here"), Err(Error::Parse(_))));
    }
}

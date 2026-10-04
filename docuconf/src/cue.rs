//! A small writer for CUE data: structs, lists and scalars, laid out the
//! way `cue fmt` does (scalar fields in a run have aligned values).

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<Node>),
    Struct(Vec<(String, Node)>),
}

impl Node {
    pub(crate) fn from_json(v: &serde_json::Value) -> Node {
        match v {
            serde_json::Value::Null => Node::Null,
            serde_json::Value::Bool(b) => Node::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Node::Int(i),
                None => match n.as_u64() {
                    Some(u) => Node::Float(u as f64),
                    None => Node::Float(n.as_f64().unwrap_or(0.0)),
                },
            },
            serde_json::Value::String(s) => Node::Str(s.clone()),
            serde_json::Value::Array(a) => Node::List(a.iter().map(Node::from_json).collect()),
            serde_json::Value::Object(o) => Node::Struct(
                o.iter()
                    .map(|(k, v)| (k.clone(), Node::from_json(v)))
                    .collect(),
            ),
        }
    }

    fn is_scalar(&self) -> bool {
        !matches!(self, Node::List(_) | Node::Struct(_))
    }
}

const KEYWORDS: &[&str] = &[
    "package", "import", "for", "in", "if", "let", "true", "false", "null", "func",
];

pub(crate) fn label(s: &str) -> String {
    let mut c = s.chars();
    let ident = matches!(c.next(), Some(ch) if ch.is_ascii_alphabetic())
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if ident && !KEYWORDS.contains(&s) {
        s.to_string()
    } else {
        quote(s)
    }
}

pub(crate) fn quote(s: &str) -> String {
    // JSON string escapes are a subset of CUE's, and a backslash is always
    // escaped, so "\(" can never start an interpolation.
    serde_json::to_string(s).expect("strings always serialize")
}

fn scalar(n: &Node) -> String {
    match n {
        Node::Str(s) => quote(s),
        Node::Int(i) => i.to_string(),
        Node::Float(f) => {
            let s = f.to_string();
            if s.contains(['.', 'e', 'E']) || !f.is_finite() {
                s
            } else {
                format!("{s}.0")
            }
        }
        Node::Bool(b) => b.to_string(),
        Node::Null => "null".into(),
        _ => unreachable!("not a scalar"),
    }
}

fn tabs(n: usize) -> String {
    "\t".repeat(n)
}

/// Writes a value that follows `label: ` (or a list element).
fn value(out: &mut String, n: &Node, indent: usize) {
    match n {
        Node::Struct(fields) if fields.is_empty() => out.push_str("{}"),
        Node::Struct(fields) => {
            out.push_str("{\n");
            body(out, fields, indent + 1);
            out.push_str(&tabs(indent));
            out.push('}');
        }
        Node::List(items) if items.iter().all(Node::is_scalar) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&scalar(x));
            }
            out.push(']');
        }
        Node::List(items) => {
            out.push_str("[\n");
            for x in items {
                out.push_str(&tabs(indent + 1));
                value(out, x, indent + 1);
                out.push_str(",\n");
            }
            out.push_str(&tabs(indent));
            out.push(']');
        }
        s => out.push_str(&scalar(s)),
    }
}

/// Writes struct fields, one per line, aligning the values of each run of
/// consecutive scalar fields.
pub(crate) fn body(out: &mut String, fields: &[(String, Node)], indent: usize) {
    let mut i = 0;
    while i < fields.len() {
        if fields[i].1.is_scalar() {
            let mut j = i;
            while j < fields.len() && fields[j].1.is_scalar() {
                j += 1;
            }
            let width = fields[i..j]
                .iter()
                .map(|(k, _)| label(k).len() + 1)
                .max()
                .unwrap_or(0);
            for (k, v) in &fields[i..j] {
                let l = format!("{}:", label(k));
                let _ = writeln!(out, "{}{l:<width$} {}", tabs(indent), scalar(v));
            }
            i = j;
        } else {
            let (k, v) = &fields[i];
            let _ = write!(out, "{}{}: ", tabs(indent), label(k));
            value(out, v, indent);
            out.push('\n');
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(label("type"), "type");
        assert_eq!(label("serving-tls"), "\"serving-tls\"");
        assert_eq!(label("$schema"), "\"$schema\"");
        assert_eq!(label("if"), "\"if\"");
        assert_eq!(label("_x"), "\"_x\"");
    }

    #[test]
    fn layout() {
        let mut s = String::new();
        body(
            &mut s,
            &[
                ("type".into(), Node::Str("int".into())),
                ("description".into(), Node::Str("a \"q\" \\(x)".into())),
                ("schemes".into(), Node::List(vec![Node::Str("a".into())])),
                ("min".into(), Node::Int(1)),
                ("f".into(), Node::Float(2.0)),
                ("e".into(), Node::Struct(vec![])),
            ],
            0,
        );
        assert_eq!(
            s,
            "type:        \"int\"\ndescription: \"a \\\"q\\\" \\\\(x)\"\nschemes: [\"a\"]\nmin: 1\nf:   2.0\ne: {}\n"
        );
    }
}

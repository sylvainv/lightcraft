//! Custom XMP namespace registry: import schema XML files to teach LightCraft about additional
//! XMP namespaces (URI, prefix, optional field labels). Registered namespaces become writable in
//! the Info panel alongside the built-in `lc:` namespace.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// One field within a custom namespace schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomField {
    /// Local name (e.g. `Stock`, not `film:Stock`).
    pub name: String,
    /// Human-readable label; defaults to `name` when absent in the schema.
    pub label: String,
}

/// A registered custom XMP namespace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomNamespace {
    /// Namespace URI, e.g. `https://example.com/film/1.0/`.
    pub uri: String,
    /// Preferred prefix, e.g. `film`.
    pub prefix: String,
    /// Optional human-readable label, e.g. `Film Metadata`.
    pub label: String,
    /// Known fields; empty means any `prefix:Name` key is accepted.
    pub fields: Vec<CustomField>,
}

impl CustomNamespace {
    /// `true` when `lc:` (built-in, always available — not user-imported).
    pub fn is_builtin_lc(prefix: &str) -> bool {
        prefix == "lc"
    }

    /// Validate a local name: ASCII letters, digits, underscore, non-empty.
    pub fn valid_local_name(name: &str) -> bool {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
}

// ---------- XML schema parser

const MAX_SCHEMA_BYTES: usize = 64 * 1024;

/// Parse a namespace schema XML file.
///
/// Minimal accepted format:
/// ```xml
/// <namespace uri="https://example.com/film/1.0/" prefix="film" label="Film Metadata">
///   <field name="Stock" label="Film Stock"/>
///   <field name="Roll"/>
/// </namespace>
/// ```
pub fn parse_namespace_xml(xml: &str) -> std::result::Result<CustomNamespace, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    if xml.len() > MAX_SCHEMA_BYTES {
        return Err(format!("schema file too large ({} bytes, max {})", xml.len(), MAX_SCHEMA_BYTES));
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut uri = String::new();
    let mut prefix = String::new();
    let mut label = String::new();
    let mut fields: Vec<CustomField> = Vec::new();
    let mut in_namespace = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local_bytes = e.local_name().as_ref().to_vec();
                let name = std::str::from_utf8(&local_bytes).unwrap_or("").to_string();
                if name.eq_ignore_ascii_case("namespace") {
                    in_namespace = true;
                    for attr in e.attributes().flatten() {
                        let local = attr.key.local_name().as_ref().to_vec();
                        let key = std::str::from_utf8(&local).unwrap_or("").to_string();
                        let val = attr.unescape_value().map(|v| v.into_owned()).unwrap_or_default();
                        match key.as_str() {
                            "uri" => uri = val,
                            "prefix" => prefix = val,
                            "label" => label = val,
                            _ => {}
                        }
                    }
                } else if name.eq_ignore_ascii_case("field") && in_namespace {
                    let mut fname = String::new();
                    let mut flabel = String::new();
                    for attr in e.attributes().flatten() {
                        let local = attr.key.local_name().as_ref().to_vec();
                        let key = std::str::from_utf8(&local).unwrap_or("").to_string();
                        let val = attr.unescape_value().map(|v| v.into_owned()).unwrap_or_default();
                        match key.as_str() {
                            "name" => fname = val,
                            "label" => flabel = val,
                            _ => {}
                        }
                    }
                    if fname.is_empty() {
                        return Err("field is missing a `name` attribute".into());
                    }
                    if !CustomNamespace::valid_local_name(&fname) {
                        return Err(format!("invalid field name `{fname}`: use ASCII letters, digits, underscore"));
                    }
                    if flabel.is_empty() {
                        flabel = fname.clone();
                    }
                    fields.push(CustomField { name: fname, label: flabel });
                }
            }
            Ok(Event::End(_)) => {}
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
    }

    if uri.is_empty() {
        return Err("missing `uri` attribute on <namespace>".into());
    }
    if prefix.is_empty() {
        return Err("missing `prefix` attribute on <namespace>".into());
    }
    if prefix == "lc" || prefix == "xmp" || prefix == "dc" || prefix == "rdf" {
        return Err(format!("prefix `{prefix}` is reserved"));
    }
    if !prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("invalid prefix `{prefix}`: use ASCII letters, digits, hyphens, underscores"));
    }
    if label.is_empty() {
        label = prefix.clone();
    }

    Ok(CustomNamespace { uri, prefix, label, fields })
}

// ---------- commands

fn list(s: &Session) -> Value {
    json!(s.custom_namespaces)
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "metadata.importNamespace";
    let path_str = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let path = std::path::Path::new(path_str);
    let xml = std::fs::read_to_string(path).map_err(|e| bad(C, format!("cannot read `{path_str}`: {e}")))?;
    let ns = parse_namespace_xml(&xml).map_err(|e| bad(C, format!("invalid schema: {e}")))?;

    // Replace existing by URI or prefix, otherwise append.
    let replaced = s.custom_namespaces.iter_mut().find(|n| n.uri == ns.uri || n.prefix == ns.prefix);
    match replaced {
        Some(slot) => *slot = ns,
        None => s.custom_namespaces.push(ns),
    }
    s.save_prefs()?;
    Ok(list(s))
}

fn delete_ns(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "metadata.deleteNamespace";
    let id = str_param(p, "uri").or_else(|| str_param(p, "prefix")).ok_or_else(|| bad(C, "missing `uri` or `prefix`"))?;
    let n = s.custom_namespaces.len();
    s.custom_namespaces.retain(|ns| ns.uri != id && ns.prefix != id);
    if s.custom_namespaces.len() == n {
        return Err(bad(C, format!("no namespace `{id}`")));
    }
    s.save_prefs()?;
    Ok(list(s))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "metadata.namespaces", "Custom XMP Namespaces", [], None, "{} → [{uri, prefix, label, fields}]", always, |s, _| Ok(list(s))),
        cmd!(
            "metadata.importNamespace",
            "Import XMP Namespace Schema",
            [],
            None,
            "{path} — import an XML schema file defining a custom XMP namespace; replaces any existing registration for the same URI or prefix",
            always,
            import
        ),
        cmd!("metadata.deleteNamespace", "Delete XMP Namespace", [], None, "{uri | prefix}", always, delete_ns),
    ]
}

// ---------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_schema() {
        let xml = r#"
            <namespace uri="https://example.com/film/1.0/" prefix="film" label="Film Metadata">
              <field name="Stock" label="Film Stock"/>
              <field name="Roll"/>
            </namespace>
        "#;
        let ns = parse_namespace_xml(xml).expect("valid schema");
        assert_eq!(ns.uri, "https://example.com/film/1.0/");
        assert_eq!(ns.prefix, "film");
        assert_eq!(ns.label, "Film Metadata");
        assert_eq!(ns.fields.len(), 2);
        assert_eq!(ns.fields[0].label, "Film Stock");
        assert_eq!(ns.fields[1].name, "Roll");
        assert_eq!(ns.fields[1].label, "Roll");
    }

    #[test]
    fn parse_schema_missing_uri() {
        let xml = r#"<namespace prefix="film"><field name="Stock"/></namespace>"#;
        assert!(parse_namespace_xml(xml).is_err());
    }

    #[test]
    fn parse_schema_reserved_prefix() {
        let xml = r#"<namespace uri="https://x.example/" prefix="lc"/>"#;
        assert!(parse_namespace_xml(xml).is_err());
    }

    #[test]
    fn parse_schema_invalid_field_name() {
        let xml = r#"<namespace uri="https://x.example/" prefix="x"><field name="bad name"/></namespace>"#;
        assert!(parse_namespace_xml(xml).is_err());
    }
}

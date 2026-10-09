//! An owned, mutable XML element tree.
//!
//! RimWorld's Def inheritance works on XML *before* deserialisation, so we keep
//! Defs as generic trees and only convert them to typed structs afterwards.

use std::collections::HashSet;

/// One XML element. Comments and processing instructions are dropped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct XmlNode {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// Trimmed text content, if the element has any non-whitespace text.
    pub text: Option<String>,
    pub children: Vec<XmlNode>,
}

impl XmlNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, key: &str, value: &str) {
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value.to_owned(),
            None => self.attrs.push((key.to_owned(), value.to_owned())),
        }
    }

    pub fn remove_attr(&mut self, key: &str) {
        self.attrs.retain(|(k, _)| k != key);
    }

    /// Attribute compared case-insensitively against "true" (RimWorld writes
    /// both `True` and `true`).
    pub fn attr_is_true(&self, key: &str) -> bool {
        self.attr(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// `IsNull="True"`: the field is explicitly null.
    pub fn is_null(&self) -> bool {
        self.attr_is_true("IsNull")
    }

    /// First child element with the given name.
    pub fn child(&self, name: &str) -> Option<&XmlNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Text of a child element, ignoring explicitly-null fields.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name)
            .filter(|c| !c.is_null())
            .and_then(|c| c.text.as_deref())
    }

    /// Follows a path of child element names, e.g. `["graphicData", "texPath"]`.
    pub fn path(&self, path: &[&str]) -> Option<&XmlNode> {
        path.iter().try_fold(self, |node, name| node.child(name))
    }

    pub fn path_text(&self, path: &[&str]) -> Option<&str> {
        self.path(path)
            .filter(|c| !c.is_null())
            .and_then(|c| c.text.as_deref())
    }

    /// True if this element is a list (all element children are `<li>`).
    pub fn is_list(&self) -> bool {
        !self.children.is_empty() && self.children.iter().all(|c| c.name == "li")
    }

    /// Texts of `<li>` children of the named child, e.g. affordance lists.
    pub fn child_list_texts(&self, name: &str) -> Vec<String> {
        self.child(name)
            .map(|list| {
                list.children
                    .iter()
                    .filter(|c| c.name == "li")
                    .filter_map(|c| c.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Which content packs (by packageId) are active. Used for `MayRequire`.
#[derive(Debug, Clone, Default)]
pub struct ActivePackages {
    ids: HashSet<String>,
}

impl ActivePackages {
    pub fn new<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            ids: ids
                .into_iter()
                .map(|s| s.as_ref().trim().to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn is_active(&self, package_id: &str) -> bool {
        self.ids.contains(&package_id.trim().to_ascii_lowercase())
    }

    /// Whether a node with these attributes exists given the active packages.
    ///
    /// `MayRequire="a,b"` needs *all* listed packages, `MayRequireAnyOf="a,b"`
    /// needs at least one.
    pub fn allows(&self, node: &roxmltree::Node<'_, '_>) -> bool {
        if let Some(req) = node.attribute("MayRequire")
            && !req.split(',').all(|id| self.is_active(id))
        {
            return false;
        }
        if let Some(any) = node.attribute("MayRequireAnyOf")
            && !any.split(',').any(|id| self.is_active(id))
        {
            return false;
        }
        true
    }
}

/// Parses an XML document and returns its root element as an owned tree,
/// dropping every element whose `MayRequire`/`MayRequireAnyOf` is not met.
pub fn parse_document(text: &str, active: &ActivePackages) -> Result<XmlNode, roxmltree::Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc = roxmltree::Document::parse(text)?;
    Ok(convert(doc.root_element(), active))
}

fn convert(node: roxmltree::Node<'_, '_>, active: &ActivePackages) -> XmlNode {
    let mut out = XmlNode::new(node.tag_name().name());
    out.attrs = node
        .attributes()
        .filter(|a| a.name() != "MayRequire" && a.name() != "MayRequireAnyOf")
        .map(|a| (a.name().to_owned(), a.value().to_owned()))
        .collect();
    let mut text = String::new();
    for child in node.children() {
        if child.is_element() {
            if active.allows(&child) {
                out.children.push(convert(child, active));
            }
        } else if child.is_text()
            && let Some(t) = child.text()
        {
            text.push_str(t);
        }
    }
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        out.text = Some(trimmed.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\" ?>
<Defs>
  <!-- synthetic-fixture -->
  <ThingDef Name=\"Base\" Abstract=\"True\">
    <a>1</a>
    <list>
      <li>x</li>
      <li MayRequire=\"Test.DLC\">dlc-only</li>
      <li MayRequireAnyOf=\"Test.DLC,Test.Core\">any</li>
    </list>
    <dlcField MayRequire=\"test.dlc\">2</dlcField>
  </ThingDef>
</Defs>";

    #[test]
    fn parses_tree_and_filters_may_require() {
        let active = ActivePackages::new(["Test.Core"]);
        let root = parse_document(DOC, &active).unwrap();
        assert_eq!(root.name, "Defs");
        let def = &root.children[0];
        assert_eq!(def.attr("Name"), Some("Base"));
        assert!(def.attr_is_true("Abstract"));
        assert_eq!(def.child_text("a"), Some("1"));
        assert_eq!(def.child_list_texts("list"), vec!["x", "any"]);
        assert!(def.child("dlcField").is_none());
    }

    #[test]
    fn may_require_is_case_insensitive() {
        let active = ActivePackages::new(["TEST.dlc", "Test.Core"]);
        let root = parse_document(DOC, &active).unwrap();
        let def = &root.children[0];
        assert_eq!(def.child_list_texts("list"), vec!["x", "dlc-only", "any"]);
        assert_eq!(def.child_text("dlcField"), Some("2"));
    }

    #[test]
    fn null_fields_have_no_text() {
        let root = parse_document(
            "<Defs><D><f IsNull=\"True\" /><g>v</g></D></Defs>",
            &ActivePackages::default(),
        )
        .unwrap();
        let d = &root.children[0];
        assert!(d.child("f").unwrap().is_null());
        assert_eq!(d.child_text("f"), None);
        assert_eq!(d.path_text(&["g"]), Some("v"));
    }

    #[test]
    fn malformed_xml_is_error() {
        assert!(parse_document("<Defs><a></Defs>", &ActivePackages::default()).is_err());
    }
}

//! XML-level Def inheritance (`Name` / `ParentName` / `Abstract` / `Inherit`).
//!
//! Behaviour (see docs/research.md §4):
//! - A child starts from its parent's fully resolved tree.
//! - `Name`, `ParentName` and `Abstract` are never inherited.
//! - Per element: `Inherit="False"` replaces; `<li>` lists append; text
//!   replaces; elements with children merge recursively by element name.
//! - Parents are looked up by `Name` across all packs, preferring the child's
//!   own pack.

use std::collections::HashMap;

use crate::xml::XmlNode;

/// Attributes that describe a Def's place in the hierarchy, not its data.
const HIERARCHY_ATTRS: [&str; 3] = ["Name", "ParentName", "Abstract"];

/// A top-level Def element before inheritance.
#[derive(Debug, Clone)]
pub struct RawDef {
    pub node: XmlNode,
    pub package_id: String,
    pub file: String,
}

impl RawDef {
    pub fn name(&self) -> Option<&str> {
        self.node.attr("Name")
    }
    pub fn parent_name(&self) -> Option<&str> {
        self.node.attr("ParentName")
    }
    pub fn is_abstract(&self) -> bool {
        self.node.attr_is_true("Abstract")
    }
}

/// Merges `child` over `current` in place.
pub fn merge_into(current: &mut XmlNode, child: &XmlNode) {
    for (k, v) in &child.attrs {
        if k != "Inherit" {
            current.set_attr(k, v);
        }
    }

    if !child.children.is_empty() {
        if child.is_list() {
            current.text = None;
            current.children.extend(child.children.iter().cloned());
            return;
        }
        current.text = None;
        for cc in &child.children {
            let replace = cc
                .attr("Inherit")
                .is_some_and(|v| v.eq_ignore_ascii_case("false"));
            let existing = current.children.iter().position(|c| c.name == cc.name);
            match (replace, existing) {
                (true, Some(i)) => current.children[i] = strip_inherit(cc),
                (false, Some(i)) => merge_into(&mut current.children[i], cc),
                (_, None) => current.children.push(strip_inherit(cc)),
            }
        }
    } else if let Some(text) = &child.text {
        current.children.clear();
        current.text = Some(text.clone());
    } else if child.is_null() {
        current.children.clear();
        current.text = None;
    }
}

fn strip_inherit(node: &XmlNode) -> XmlNode {
    let mut n = node.clone();
    n.remove_attr("Inherit");
    n
}

/// Resolves inheritance for every Def. Returns one resolved tree per input
/// (same order) plus warnings for missing parents and cycles.
pub fn resolve_all(defs: &[RawDef]) -> (Vec<XmlNode>, Vec<String>) {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, d) in defs.iter().enumerate() {
        if let Some(name) = d.name() {
            by_name.entry(name).or_default().push(i);
        }
    }

    let mut resolver = Resolver {
        defs,
        by_name,
        state: vec![State::Pending; defs.len()],
        resolved: vec![None; defs.len()],
        warnings: Vec::new(),
    };
    for i in 0..defs.len() {
        resolver.resolve(i);
    }
    let resolved = resolver
        .resolved
        .into_iter()
        .map(|n| n.expect("every def resolved"))
        .collect();
    (resolved, resolver.warnings)
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Pending,
    InProgress,
    Done,
}

struct Resolver<'a> {
    defs: &'a [RawDef],
    by_name: HashMap<&'a str, Vec<usize>>,
    state: Vec<State>,
    resolved: Vec<Option<XmlNode>>,
    warnings: Vec<String>,
}

impl Resolver<'_> {
    fn find_parent(&self, child: usize, parent_name: &str) -> Option<usize> {
        let candidates = self.by_name.get(parent_name)?;
        let pkg = &self.defs[child].package_id;
        candidates
            .iter()
            .rev()
            .find(|&&c| self.defs[c].package_id == *pkg)
            .or_else(|| candidates.last())
            .copied()
    }

    fn resolve(&mut self, i: usize) {
        match self.state[i] {
            State::Done => return,
            State::InProgress => {
                // Cycle: break it by treating this def as parentless.
                self.warnings.push(format!(
                    "inheritance cycle involving {} ({})",
                    describe(&self.defs[i]),
                    self.defs[i].file
                ));
                return;
            }
            State::Pending => {}
        }
        self.state[i] = State::InProgress;

        let def = &self.defs[i];
        let mut result = None;
        if let Some(parent_name) = def.parent_name() {
            match self.find_parent(i, parent_name) {
                Some(p) => {
                    self.resolve(p);
                    if let Some(parent) = &self.resolved[p] {
                        let mut base = parent.clone();
                        for a in HIERARCHY_ATTRS {
                            base.remove_attr(a);
                        }
                        // The element name is the child's (normally identical).
                        base.name = def.node.name.clone();
                        merge_into(&mut base, &def.node);
                        result = Some(base);
                    }
                }
                None => self.warnings.push(format!(
                    "{} ({}): parent Name=\"{parent_name}\" not found",
                    describe(def),
                    def.file
                )),
            }
        }
        self.resolved[i] = Some(result.unwrap_or_else(|| def.node.clone()));
        self.state[i] = State::Done;
    }
}

fn describe(def: &RawDef) -> String {
    let n = &def.node;
    match (n.child_text("defName"), n.attr("Name")) {
        (Some(d), _) => format!("{} {d}", n.name),
        (None, Some(name)) => format!("{} Name={name}", n.name),
        (None, None) => format!("unnamed {}", n.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{ActivePackages, parse_document};

    fn raw(xml: &str) -> Vec<RawDef> {
        let root = parse_document(xml, &ActivePackages::default()).unwrap();
        root.children
            .into_iter()
            .map(|node| RawDef {
                node,
                package_id: "test".into(),
                file: "test.xml".into(),
            })
            .collect()
    }

    fn resolve(xml: &str) -> (Vec<XmlNode>, Vec<String>) {
        resolve_all(&raw(xml))
    }

    #[test]
    fn child_inherits_and_overrides_fields() {
        let (r, w) = resolve(
            r#"<Defs>
              <ThingDef Name="Base" Abstract="True">
                <a>1</a><b>2</b>
                <graphicData><texPath>Rock</texPath><color>(1,1,1)</color></graphicData>
              </ThingDef>
              <ThingDef ParentName="Base">
                <defName>Child</defName><b>3</b>
                <graphicData><color>(105,95,97)</color></graphicData>
              </ThingDef>
            </Defs>"#,
        );
        assert!(w.is_empty(), "{w:?}");
        let c = &r[1];
        assert_eq!(c.child_text("a"), Some("1"));
        assert_eq!(c.child_text("b"), Some("3"));
        assert_eq!(c.path_text(&["graphicData", "texPath"]), Some("Rock"));
        assert_eq!(c.path_text(&["graphicData", "color"]), Some("(105,95,97)"));
        // Hierarchy attributes are not inherited.
        assert!(!c.attr_is_true("Abstract"));
        assert_eq!(c.attr("Name"), None);
    }

    #[test]
    fn lists_append_unless_inherit_false() {
        let (r, _) = resolve(
            r#"<Defs>
              <TerrainDef Name="Base" Abstract="True">
                <affordances><li>Walkable</li></affordances>
                <tags><li>A</li></tags>
              </TerrainDef>
              <TerrainDef ParentName="Base">
                <defName>T</defName>
                <affordances><li>Heavy</li></affordances>
                <tags Inherit="False"><li>B</li></tags>
              </TerrainDef>
            </Defs>"#,
        );
        assert_eq!(
            r[1].child_list_texts("affordances"),
            vec!["Walkable", "Heavy"]
        );
        assert_eq!(r[1].child_list_texts("tags"), vec!["B"]);
        assert_eq!(r[1].child("tags").unwrap().attr("Inherit"), None);
    }

    #[test]
    fn is_null_with_inherit_false_clears_field() {
        let (r, _) = resolve(
            r#"<Defs>
              <TerrainDef Name="Base" Abstract="True"><designationCategory>Floors</designationCategory></TerrainDef>
              <TerrainDef ParentName="Base"><defName>T</defName><designationCategory Inherit="False" IsNull="True" /></TerrainDef>
            </Defs>"#,
        );
        assert_eq!(r[1].child_text("designationCategory"), None);
    }

    #[test]
    fn multi_level_chain_and_concrete_parent() {
        let (r, w) = resolve(
            r#"<Defs>
              <ThingDef Name="Human" ParentName="BasePawn"><defName>Human</defName><speed>4.6</speed></ThingDef>
              <ThingDef Name="BasePawn" Abstract="True"><category>Pawn</category><speed>1</speed></ThingDef>
              <ThingDef ParentName="Human"><defName>Clone</defName></ThingDef>
            </Defs>"#,
        );
        assert!(w.is_empty(), "{w:?}");
        // Parent declared later in the file still resolves.
        assert_eq!(r[0].child_text("category"), Some("Pawn"));
        assert_eq!(r[0].child_text("speed"), Some("4.6"));
        assert_eq!(r[2].child_text("category"), Some("Pawn"));
        assert_eq!(r[2].child_text("speed"), Some("4.6"));
        assert_eq!(r[2].child_text("defName"), Some("Clone"));
    }

    #[test]
    fn missing_parent_warns_and_keeps_own_fields() {
        let (r, w) =
            resolve(r#"<Defs><ThingDef ParentName="Nope"><defName>X</defName></ThingDef></Defs>"#);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("Nope"));
        assert_eq!(r[0].child_text("defName"), Some("X"));
    }

    #[test]
    fn cycles_do_not_hang() {
        let (r, w) = resolve(
            r#"<Defs>
              <ThingDef Name="A" ParentName="B"><a>1</a></ThingDef>
              <ThingDef Name="B" ParentName="A"><b>1</b></ThingDef>
            </Defs>"#,
        );
        assert_eq!(r.len(), 2);
        assert!(w.iter().any(|w| w.contains("cycle")));
    }

    #[test]
    fn prefers_parent_from_same_package() {
        let mut defs = raw(r#"<Defs>
              <ThingDef Name="P" Abstract="True"><v>core</v></ThingDef>
              <ThingDef Name="P" Abstract="True"><v>mod</v></ThingDef>
              <ThingDef ParentName="P"><defName>C</defName></ThingDef>
            </Defs>"#);
        defs[1].package_id = "other".into();
        let (r, _) = resolve_all(&defs);
        assert_eq!(r[2].child_text("v"), Some("core"));
    }
}

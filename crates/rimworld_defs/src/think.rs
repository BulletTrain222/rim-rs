//! `ThinkTreeDef`: the XML behaviour trees that decide what pawns do.
//!
//! Each node is `<li Class="ThinkNode_*|JobGiver_*">` with optional
//! `<subNodes>` and node-specific parameters (e.g. `<treeDef>`,
//! `<tagToGive>`, `<invert>`, `<ticksBetweenWandersRange>`). The tree root is
//! `<thinkRoot Class="...">`. We keep nodes generic: the class name, the
//! children, and the raw parameter XML; the simulation decides which classes
//! it understands.

use crate::database::Def;
use crate::typed::HasDefName;
use crate::xml::XmlNode;

#[derive(Debug, Clone)]
pub struct ThinkNodeSpec {
    /// C# class name, e.g. `ThinkNode_Priority`, `JobGiver_WanderColony`.
    pub class: String,
    pub sub_nodes: Vec<ThinkNodeSpec>,
    /// The node's XML without `subNodes`, for reading parameters.
    pub params: XmlNode,
    /// `ThinkNode.UniqueSaveKey`: keys per-pawn think data (e.g. a chance
    /// node's last try).
    // COMPATIBILITY TODO: currently approximate — the game derives keys from
    // class-name hashes with collisions rerolled across every think tree in
    // load order; here: HashCombine(tree defName hash, preorder index).
    pub save_key: i32,
}

impl ThinkNodeSpec {
    fn from_xml(node: &XmlNode) -> Self {
        let sub_nodes = node
            .child("subNodes")
            .map(|s| {
                s.children
                    .iter()
                    .filter(|c| c.name == "li")
                    .map(ThinkNodeSpec::from_xml)
                    .collect()
            })
            .unwrap_or_default();
        let mut params = node.clone();
        params.children.retain(|c| c.name != "subNodes");
        Self {
            class: node.attr("Class").unwrap_or("").to_owned(),
            sub_nodes,
            params,
            save_key: 0,
        }
    }

    fn assign_keys(&mut self, tree_hash: i32, next: &mut i32) {
        self.save_key = hash_combine(tree_hash, *next);
        *next += 1;
        for c in &mut self.sub_nodes {
            c.assign_keys(tree_hash, next);
        }
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.child_text(name)
    }

    /// Total nodes in this subtree (including self).
    pub fn count(&self) -> usize {
        1 + self
            .sub_nodes
            .iter()
            .map(ThinkNodeSpec::count)
            .sum::<usize>()
    }

    /// Visits every node depth-first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a ThinkNodeSpec)) {
        f(self);
        for c in &self.sub_nodes {
            c.walk(f);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThinkTreeDef {
    pub def_name: String,
    pub root: Option<ThinkNodeSpec>,
    /// Trees with an insert tag are injected into `ThinkNode_SubtreesByTag`
    /// nodes with the same `insertTag`, highest `insertPriority` first.
    pub insert_tag: Option<String>,
    pub insert_priority: f32,
}

impl HasDefName for ThinkTreeDef {
    fn def_name(&self) -> &str {
        &self.def_name
    }
}

impl ThinkTreeDef {
    pub(crate) fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut root = def.node.child("thinkRoot").map(ThinkNodeSpec::from_xml);
        if let Some(r) = root.as_mut() {
            let mut next = 0;
            r.assign_keys(stable_string_hash(&def.def_name), &mut next);
        }
        if root.is_none() {
            warnings.push(format!("ThinkTreeDef {} has no thinkRoot", def.def_name));
        }
        let insert_priority = match def.node.child_text("insertPriority") {
            None => 0.0,
            Some(t) => t.parse().unwrap_or_else(|_| {
                warnings.push(format!(
                    "ThinkTreeDef {}: bad insertPriority {t:?}",
                    def.def_name
                ));
                0.0
            }),
        };
        Self {
            def_name: def.def_name.clone(),
            root,
            insert_tag: def.node.child_text("insertTag").map(str::to_owned),
            insert_priority,
        }
    }

    /// `ThinkNode_Subtree` references (`treeDef`) anywhere in this tree.
    pub fn subtree_refs(&self) -> Vec<&str> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.walk(&mut |n| {
                if n.class == "ThinkNode_Subtree"
                    && let Some(t) = n.param("treeDef")
                {
                    out.push(t);
                }
            });
        }
        out
    }
}

/// `GenText.StableStringHash`.
fn stable_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(23i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32))
}

/// `Gen.HashCombineInt(seed, value)`.
fn hash_combine(seed: i32, value: i32) -> i32 {
    seed ^ value
        .wrapping_add(0x9E37_79B9u32 as i32)
        .wrapping_add(seed.wrapping_shl(6))
        .wrapping_add(seed >> 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_documents;
    use crate::xml::ActivePackages;

    #[test]
    fn parses_tree_structure_and_params() {
        let xml = r#"<Defs>
          <ThinkTreeDef>
            <defName>Test</defName>
            <thinkRoot Class="ThinkNode_Priority">
              <subNodes>
                <li Class="ThinkNode_Subtree"><treeDef>Other</treeDef></li>
                <li Class="ThinkNode_Tagger">
                  <tagToGive>Idle</tagToGive>
                  <subNodes>
                    <li Class="JobGiver_WanderAnywhere">
                      <ticksBetweenWandersRange>120~240</ticksBetweenWandersRange>
                    </li>
                  </subNodes>
                </li>
              </subNodes>
            </thinkRoot>
          </ThinkTreeDef>
          <ThinkTreeDef><defName>Hook</defName><insertTag>T</insertTag>
            <insertPriority>100</insertPriority><thinkRoot Class="JobGiver_Idle" /></ThinkTreeDef>
        </Defs>"#;
        let (db, report) = load_documents("core", &[("t.xml", xml)], &ActivePackages::default());
        assert!(report.warnings.is_empty());
        let mut w = Vec::new();
        let t = ThinkTreeDef::from_def(db.get("ThinkTreeDef", "Test").unwrap(), &mut w);
        let root = t.root.as_ref().unwrap();
        assert_eq!(root.class, "ThinkNode_Priority");
        assert_eq!(root.count(), 4);
        let tagger = &root.sub_nodes[1];
        assert_eq!(tagger.param("tagToGive"), Some("Idle"));
        assert!(tagger.params.child("subNodes").is_none());
        assert_eq!(
            tagger.sub_nodes[0].param("ticksBetweenWandersRange"),
            Some("120~240")
        );
        assert_eq!(t.subtree_refs(), vec!["Other"]);

        let hook = ThinkTreeDef::from_def(db.get("ThinkTreeDef", "Hook").unwrap(), &mut w);
        assert_eq!(hook.insert_tag.as_deref(), Some("T"));
        assert_eq!(hook.insert_priority, 100.0);
        assert!(w.is_empty());
    }
}

//! Generic database of resolved Defs, keyed by Def type and `defName`.

use std::collections::{BTreeMap, HashMap};

use crate::xml::XmlNode;

/// A concrete (non-abstract) Def after inheritance.
#[derive(Debug, Clone)]
pub struct Def {
    pub def_type: String,
    pub def_name: String,
    pub node: XmlNode,
    pub package_id: String,
    pub file: String,
}

/// All Defs of one type, in load order, with a `defName` index.
#[derive(Debug, Default, Clone)]
pub struct DefTable {
    defs: Vec<Def>,
    index: HashMap<String, usize>,
}

impl DefTable {
    pub fn get(&self, def_name: &str) -> Option<&Def> {
        self.index.get(def_name).map(|&i| &self.defs[i])
    }
    pub fn iter(&self) -> impl Iterator<Item = &Def> {
        self.defs.iter()
    }
    pub fn len(&self) -> usize {
        self.defs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Every loaded Def, grouped by type (`ThingDef`, `TerrainDef`, ...).
#[derive(Debug, Default, Clone)]
pub struct DefDatabase {
    tables: BTreeMap<String, DefTable>,
}

impl DefDatabase {
    /// Inserts a Def. A later Def with the same type and `defName` replaces the
    /// earlier one; the replaced Def is returned so the caller can warn.
    pub fn insert(&mut self, def: Def) -> Option<Def> {
        let table = self.tables.entry(def.def_type.clone()).or_default();
        match table.index.get(&def.def_name) {
            Some(&i) => Some(std::mem::replace(&mut table.defs[i], def)),
            None => {
                table.index.insert(def.def_name.clone(), table.defs.len());
                table.defs.push(def);
                None
            }
        }
    }

    pub fn get(&self, def_type: &str, def_name: &str) -> Option<&Def> {
        self.tables.get(def_type)?.get(def_name)
    }

    pub fn table(&self, def_type: &str) -> Option<&DefTable> {
        self.tables.get(def_type)
    }

    pub fn count(&self, def_type: &str) -> usize {
        self.tables.get(def_type).map_or(0, DefTable::len)
    }

    /// `(def type, count)` for every type, sorted by type name.
    pub fn counts(&self) -> Vec<(&str, usize)> {
        self.tables
            .iter()
            .map(|(k, t)| (k.as_str(), t.len()))
            .collect()
    }

    pub fn total(&self) -> usize {
        self.tables.values().map(DefTable::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(t: &str, n: &str, label: &str) -> Def {
        let mut node = XmlNode::new(t);
        let mut l = XmlNode::new("label");
        l.text = Some(label.into());
        node.children.push(l);
        Def {
            def_type: t.into(),
            def_name: n.into(),
            node,
            package_id: "p".into(),
            file: "f".into(),
        }
    }

    #[test]
    fn lookup_by_type_and_name() {
        let mut db = DefDatabase::default();
        db.insert(def("ThingDef", "Human", "human"));
        db.insert(def("BodyDef", "Human", "human body"));
        db.insert(def("ThingDef", "Granite", "granite"));
        assert_eq!(db.count("ThingDef"), 2);
        assert_eq!(db.total(), 3);
        // defName is unique per type, not globally.
        assert_eq!(
            db.get("BodyDef", "Human").unwrap().node.child_text("label"),
            Some("human body")
        );
        assert!(db.get("ThingDef", "Nope").is_none());
        assert!(db.get("NopeDef", "Human").is_none());
        assert_eq!(db.counts(), vec![("BodyDef", 1), ("ThingDef", 2)]);
    }

    #[test]
    fn duplicate_replaces_and_returns_old() {
        let mut db = DefDatabase::default();
        assert!(db.insert(def("ThingDef", "A", "one")).is_none());
        let old = db.insert(def("ThingDef", "A", "two")).unwrap();
        assert_eq!(old.node.child_text("label"), Some("one"));
        assert_eq!(db.count("ThingDef"), 1);
        assert_eq!(
            db.get("ThingDef", "A").unwrap().node.child_text("label"),
            Some("two")
        );
    }
}

//! Labels as the game words them (`Thing.Label`, `GenLabel.ThingLabel`,
//! zone names), from Def labels and the install's keyed strings.

use rimworld_defs::{DefId, ThingDef};
use rimworld_sim::Sim;
use rimworld_sim::map::{Buildable, ConstructStage};
use rimworld_sim::sim::{ThingRef, ZoneLabel, ZoneRef};

use super::Lang;

/// `CapitalizeFirst`.
pub fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `LabelAsStuff`: the stuff's adjective, else its label.
pub fn stuff_adjective(sim: &Sim, stuff: DefId<ThingDef>) -> String {
    let d = &sim.defs.things[stuff];
    d.stuff_props
        .as_ref()
        .and_then(|p| p.stuff_adjective.clone())
        .unwrap_or_else(|| d.label.clone())
}

/// `GenLabel.ThingLabel(def, stuff)`: "granite wall".
pub fn made_of(
    sim: &Sim,
    lang: &Lang,
    def: DefId<ThingDef>,
    stuff: Option<DefId<ThingDef>>,
) -> String {
    let label = &sim.defs.things[def].label;
    match stuff {
        Some(s) => lang.tr_args("ThingMadeOfStuffLabel", &[&stuff_adjective(sim, s), label]),
        None => label.clone(),
    }
}

/// `Thing.LabelNoCount` and the stack count (`Label` adds " xN").
pub fn thing_label(sim: &Sim, lang: &Lang, t: ThingRef) -> String {
    match t {
        ThingRef::Pawn(p) => pawn_label(sim, p),
        ThingRef::Rock(c) => sim.map.buildings[c]
            .map(|b| sim.defs.things[b].label.clone())
            .unwrap_or_default(),
        ThingRef::Item(id) => {
            if let Some(it) = sim.map.item(id) {
                let base = match sim.corpse_pawn(id) {
                    Some(p) if sim.pawn(p).is_some_and(|p| p.is_colonist) => {
                        lang.tr_args("CorpseLabel", &[&pawn_label(sim, p)])
                    }
                    _ => sim.defs.things[it.def].label.clone(),
                };
                return if it.stack_count > 1 {
                    format!("{base} x{}", it.stack_count)
                } else {
                    base
                };
            }
            if let Some(s) = sim.map.structure(id) {
                return made_of(sim, lang, s.def, s.stuff);
            }
            if let Some(k) = sim.map.constructible(id) {
                let base = match k.building {
                    Buildable::Thing(b) => made_of(sim, lang, b, k.stuff),
                    Buildable::Floor(f) => sim.defs.terrain[f].label.clone(),
                };
                let extra = match k.stage {
                    ConstructStage::Blueprint => lang.tr("BlueprintLabelExtra"),
                    ConstructStage::Frame => lang.tr("FrameLabelExtra"),
                };
                return base + &extra;
            }
            if let Some(p) = sim.map.plant(id) {
                return sim.defs.things[p.def].label.clone();
            }
            String::new()
        }
    }
}

/// `LabelShort`: a colonist's name, an animal's kind.
pub fn pawn_label(sim: &Sim, p: rimworld_sim::PawnId) -> String {
    let Some(pawn) = sim.pawn(p) else {
        return String::new();
    };
    if sim.is_animal(p) {
        sim.defs.pawn_kinds[pawn.kind].label.clone()
    } else {
        pawn.name.clone()
    }
}

/// A zone's name: "Stockpile zone 1".
pub fn zone_label(sim: &Sim, lang: &Lang, z: ZoneRef) -> String {
    let (label, n) = sim.zone_label(z);
    let base = match label {
        ZoneLabel::Stockpile => lang.tr("Stockpile"),
        ZoneLabel::DumpingStockpile => lang.tr("DumpingStockpile"),
        ZoneLabel::Growing => lang.tr("GrowingZone"),
    };
    if n == 0 { base } else { format!("{base} {n}") }
}

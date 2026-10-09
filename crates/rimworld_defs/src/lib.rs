//! Loading RimWorld XML Defs from the user's installation.
//!
//! Layers, bottom-up:
//! - [`xml`]: owned XML trees, `MayRequire` filtering
//! - [`inherit`]: `Name`/`ParentName`/`Abstract`/`Inherit` resolution
//! - [`database`]: generic Defs keyed by type and `defName`
//! - [`typed`]: typed views (`TerrainDef`, `ThingDef`, `PawnKindDef`) and
//!   reference validation
//!
//! This crate has no engine dependency and never writes game data anywhere.

pub mod database;
pub mod inherit;
pub mod keyed;
pub mod loader;
pub mod think;
pub mod typed;
pub mod values;
pub mod xml;

pub mod health;
pub use database::{Def, DefDatabase, DefTable as RawDefTable};
pub use loader::{LoadError, LoadReport, PackSource, load_documents, load_packs};
pub use think::{ThinkNodeSpec, ThinkTreeDef};
pub use typed::{
    BiomeDef, BuildingProperties, DefId, DefTable, DietCategory, ExpectationDef, FilthProperties,
    FoodPreferability, GameDefs, GlowerProperties, GraphicData, HasDefName, IngestibleProperties,
    IngredientCountDef, JobDef, JoyGiverDef, JoyKindDef, ManeuverDef, MentalBreakDef,
    MentalBreakIntensity, MentalStateDef, NeedDef, Passability, PawnKindDef, PlantProperties,
    ProjectileProperties, RaceProperties, RecipeDef, RefuelableProperties, ResearchProjectDef,
    RottableProperties, SkillNeed, SpecialThingFilterDef, StatDef, StuffProperties, TerrainDef,
    ThingDef, ThingFilterSpec, ThoughtDef, ThoughtStage, VerbProperties, WorkGiverDef, WorkTypeDef,
    filth_flags, food_type, tech_level,
};
pub use values::Rgba;

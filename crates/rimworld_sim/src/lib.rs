//! Engine-independent simulation.
//!
//! No rendering or engine types live here, so the simulation can be tested
//! headless and reused by any front-end (desktop today, mobile later).

pub mod bills;
pub mod camera;
pub mod cell_finder;
pub mod clean;
pub mod climate;
pub mod combat;
pub mod construct;
pub mod cook;
pub mod deconstruct;
pub mod farm;
pub mod flick;
pub mod floorwork;
pub mod food;
pub mod geom;
pub mod grid;
pub mod hash;
pub mod haul;
pub mod health;
pub mod hunt;
pub mod job;
pub mod light;
pub mod map;
pub mod mapgen;
pub mod mine;
pub mod mood;
pub mod native_mapgen;
pub mod needs;
pub mod netsort;
pub mod noise;
mod noise_vectors;
pub mod path;
pub mod pawn;
pub mod plant;
pub mod rand;
pub mod ranged;
pub mod recreation;
pub mod refuel;
pub mod region;
pub mod repair;
pub mod rescue;
pub mod research;
pub mod reservation;
pub mod rest;
pub mod roof;
pub mod scenario;
pub mod sha256;
pub mod sim;
pub mod stats;
pub mod storage;
pub mod tend;
pub mod think;
pub mod work;

pub use grid::{Cell, Grid, GridSize};
pub use job::{IngestStage, Job, JobKind, Rot4, WanderParams};
pub use map::{Item, ItemId, Map};
pub use mapgen::{MapGenError, TestMapPalette, generate_test_map};
pub use needs::{Need, NeedKind, Needs};
pub use path::{MoveCosts, Path, PathError, PathGrid, find_path};
pub use pawn::{Carried, Pawn, PawnId, Step};
pub use sim::{Command, CommandError, LoadError, Sim, SpawnError, TICKS_PER_SECOND};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod work_tests;

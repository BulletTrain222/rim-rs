//! What the Work tab reads and sets (`Pawn_WorkSettings`,
//! `Pawn_SkillTracker.AverageOfRelevantSkillsFor`,
//! `PlaySettings.useWorkPriorities`; docs/research.md §67).

use rimworld_defs::{DefId, WorkTypeDef};

use super::Sim;
use crate::pawn::PawnId;
use crate::stats::Passion;

impl Sim {
    /// `Pawn_WorkSettings.EverWork`.
    pub fn ever_works(&self, pawn: PawnId) -> bool {
        self.pawn(pawn).is_some_and(|p| p.work.is_some())
    }

    /// `Pawn_WorkSettings.GetPriority`: with manual priorities off a
    /// humanlike's enabled work reads 3. `None` for a pawn that never
    /// works.
    pub fn work_priority(&self, pawn: PawnId, t: DefId<WorkTypeDef>) -> Option<i32> {
        let p = self.pawn(pawn)?;
        let humanlike = !self.is_animal(pawn);
        p.work
            .as_ref()
            .map(|w| w.effective(t, self.use_work_priorities, humanlike))
    }

    /// `Pawn_WorkSettings.SetPriority` by work type.
    // COMPATIBILITY TODO: currently approximate — turning a type off does
    // not end the pawn's current job from it (`Notify_WorkTypeDisabled`):
    // jobs don't record their work giver.
    pub fn set_work_type_priority(
        &mut self,
        pawn: PawnId,
        t: DefId<WorkTypeDef>,
        priority: i32,
    ) -> bool {
        let disabled = self.work_type_disabled(pawn, t);
        match self
            .index_of(pawn)
            .and_then(|i| self.pawns[i].work.as_mut())
        {
            Some(w) => w.set(t, priority, disabled),
            None => false,
        }
    }

    /// `Pawn.WorkTypeIsDisabled`.
    // COMPATIBILITY TODO: currently approximate — backstories, traits and
    // ages don't disable work types here, so none is disabled.
    pub fn work_type_disabled(&self, _pawn: PawnId, _t: DefId<WorkTypeDef>) -> bool {
        false
    }

    /// `AverageOfRelevantSkillsFor`: the mean level of the type's relevant
    /// skills, 3 without any.
    pub fn average_relevant_skill(&self, pawn: PawnId, t: DefId<WorkTypeDef>) -> f32 {
        let skills = &self.defs.work_types[t].relevant_skills;
        let Some(p) = self.pawn(pawn) else {
            return 0.0;
        };
        if skills.is_empty() {
            return 3.0;
        }
        skills.iter().map(|s| p.skills.level(s) as f32).sum::<f32>() / skills.len() as f32
    }

    /// `MaxPassionOfRelevantSkillsFor`.
    pub fn max_relevant_passion(&self, pawn: PawnId, t: DefId<WorkTypeDef>) -> Passion {
        let Some(p) = self.pawn(pawn) else {
            return Passion::None;
        };
        self.defs.work_types[t]
            .relevant_skills
            .iter()
            .map(|s| p.skills.passion(s))
            .max()
            .unwrap_or(Passion::None)
    }

    /// `PawnColumnWorker_WorkPriority.IsIncapableOfWholeWorkType`: no work
    /// giver of the type has every capacity it requires (a type without
    /// givers counts as incapable).
    pub fn incapable_of_work_type(&self, pawn: PawnId, t: DefId<WorkTypeDef>) -> bool {
        let Some(h) = self.health_view(pawn) else {
            return true;
        };
        !self.defs.work_types[t].givers_by_priority.iter().any(|&g| {
            self.defs.work_givers[g]
                .required_capacities
                .iter()
                .all(|c| h.capable_of(c))
        })
    }

    /// `PlaySettings.useWorkPriorities` (the Work tab's "Manual
    /// priorities"); the stored numbers are kept either way.
    pub fn set_use_work_priorities(&mut self, on: bool) {
        self.use_work_priorities = on;
    }
}

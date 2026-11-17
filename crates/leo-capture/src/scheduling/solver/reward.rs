//! The common objective, independent of an optimizer's prices or surrogate score.
use crate::workload::{Job, RewardMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RewardModel {
    /// Adjacent priority levels differ by this factor. One ignores priority.
    pub priority_base: f64,
}
impl Default for RewardModel {
    fn default() -> Self {
        Self { priority_base: 2.0 }
    }
}
impl RewardModel {
    pub fn validate(&self) -> Result<(), String> {
        if !self.priority_base.is_finite() || self.priority_base < 1.0 {
            return Err("Reward priority_base must be finite and at least one".into());
        }
        Ok(())
    }

    /// Area is the geometric AOI area used by the polygon verifier, in km².
    pub fn full_reward(&self, job: &Job, area_km2: f64) -> Result<f64, String> {
        self.validate()?;
        let reward = area_km2 * self.priority_base.powi(i32::from(job.acquisition.priority));
        if !area_km2.is_finite() || area_km2 <= 0.0 || !reward.is_finite() || reward <= 0.0 {
            return Err("Non-finite or non-positive area-weighted job reward".into());
        }
        Ok(reward)
    }

    pub fn earned(
        &self,
        job: &Job,
        area_km2: f64,
        unique_area_km2: f64,
        complete: bool,
    ) -> Result<f64, String> {
        if !unique_area_km2.is_finite() || unique_area_km2 < 0.0 {
            return Err("Invalid unique covered area".into());
        }
        let full = self.full_reward(job, area_km2)?;
        Ok(match job.acquisition.effective_reward_mode() {
            RewardMode::UniqueArea => full * (unique_area_km2 / area_km2).clamp(0.0, 1.0),
            RewardMode::CompleteObservation => {
                if complete {
                    full
                } else {
                    0.0
                }
            }
        })
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Evaluation {
    pub true_reward: f64,
    /// Reward if every due request were completed; not a feasible-plan bound.
    pub available_reward: f64,
    pub reward_percent: Option<f64>,
}
impl Evaluation {
    pub(super) fn add(
        &mut self,
        model: &RewardModel,
        job: &Job,
        area: f64,
        unique: f64,
        complete: bool,
    ) -> Result<(), String> {
        self.available_reward += model.full_reward(job, area)?;
        self.true_reward += model.earned(job, area, unique, complete)?;
        if !self.available_reward.is_finite() || !self.true_reward.is_finite() {
            return Err("Total reward exceeds finite numeric range".into());
        }
        self.reward_percent =
            (self.available_reward > 0.0).then(|| 100.0 * self.true_reward / self.available_reward);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn area_and_adjacent_priorities_scale_linearly_and_geometrically() {
        let mut job = super::super::tests::tiny_model().jobs.remove(0);
        job.acquisition.priority = 3;
        let model = RewardModel::default();
        assert_eq!(model.full_reward(&job, 10.0).unwrap(), 80.0);
        assert_eq!(model.full_reward(&job, 20.0).unwrap(), 160.0);
        job.acquisition.priority = 4;
        assert_eq!(model.full_reward(&job, 10.0).unwrap(), 160.0);
        job.acquisition.reward_mode = Some(RewardMode::UniqueArea);
        assert_eq!(model.earned(&job, 10.0, 2.5, false).unwrap(), 40.0);
        job.acquisition.reward_mode = Some(RewardMode::CompleteObservation);
        assert_eq!(model.earned(&job, 10.0, 2.5, false).unwrap(), 0.0);
        assert_eq!(model.earned(&job, 10.0, 10.0, true).unwrap(), 160.0);
        assert_eq!(
            RewardModel { priority_base: 1.0 }
                .full_reward(&job, 10.0)
                .unwrap(),
            10.0
        );
        assert!(RewardModel { priority_base: 0.5 }.validate().is_err());
        job.acquisition.priority = u16::MAX;
        assert!(model.full_reward(&job, 10.0).is_err());
    }
}

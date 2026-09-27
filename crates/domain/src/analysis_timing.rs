use crate::{AnalysisRun, AnalysisRunState, AnalysisStage, AnalysisUnitState};
use serde::{Deserialize, Serialize};

/// 累计执行时间；活动区间用于前端实时显示，暂停与应用关闭不计入。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTiming {
    pub elapsed_ms: u64,
    pub active_since_unix_ms: Option<i64>,
    pub incomplete: bool,
}

impl ExecutionTiming {
    pub fn started(now: i64) -> Self {
        Self {
            active_since_unix_ms: Some(now),
            ..Self::default()
        }
    }

    pub fn update(&mut self, now: i64, active: bool) {
        if let Some(start) = self.active_since_unix_ms {
            self.elapsed_ms = self
                .elapsed_ms
                .saturating_add(now.saturating_sub(start).max(0) as u64);
        }
        self.active_since_unix_ms = active.then_some(now);
    }

    /// 崩溃后只保留已保存的计时，不能把离线时间误算为执行时间。
    pub fn interrupt(&mut self) {
        self.incomplete |= self.active_since_unix_ms.is_some();
        self.active_since_unix_ms = None;
    }

    pub fn add(&mut self, other: &Self) {
        self.elapsed_ms = self.elapsed_ms.saturating_add(other.elapsed_ms);
        self.incomplete |= other.incomplete;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageTiming {
    pub stage: AnalysisStage,
    pub timing: ExecutionTiming,
}

impl AnalysisRun {
    pub fn update_timing(&mut self, now: i64) {
        let active = !self.interrupted
            && matches!(
                self.state,
                AnalysisRunState::Queued | AnalysisRunState::Running | AnalysisRunState::Cancelling
            );
        if let Some(timing) = &mut self.timing {
            timing.update(now, active);
        }
        let stage = self
            .units
            .iter()
            .find(|unit| {
                matches!(
                    unit.state,
                    AnalysisUnitState::Pending | AnalysisUnitState::Running
                )
            })
            .map(|unit| unit.stage)
            .or_else(|| (!self.relations_planned).then_some(AnalysisStage::Relation))
            .or_else(|| (!self.names_planned).then_some(AnalysisStage::Naming))
            .map(|stage| {
                if stage == AnalysisStage::EvidenceSelection {
                    AnalysisStage::Relation
                } else {
                    stage
                }
            });
        if active && self.timing.is_some() {
            if let Some(stage) = stage {
                if !self.stage_timings.iter().any(|item| item.stage == stage) {
                    let elapsed_ms = if self.stage_timings.is_empty() {
                        self.timing.as_ref().map_or(0, |timing| timing.elapsed_ms)
                    } else {
                        0
                    };
                    self.stage_timings.push(StageTiming {
                        stage,
                        timing: ExecutionTiming {
                            elapsed_ms,
                            ..ExecutionTiming::default()
                        },
                    });
                }
            }
        }
        for item in &mut self.stage_timings {
            item.timing.update(now, active && Some(item.stage) == stage);
        }
        for unit in &mut self.units {
            let executing = active
                && matches!(
                    unit.state,
                    AnalysisUnitState::Pending | AnalysisUnitState::Running
                );
            if let Some(timing) = &mut unit.timing {
                timing.update(now, executing && timing.active_since_unix_ms.is_some());
            }
            if let Some(timing) = &mut unit.request_timing {
                timing.update(now, executing && timing.active_since_unix_ms.is_some());
            }
        }
    }

    pub fn interrupt_timing(&mut self) {
        if let Some(timing) = &mut self.timing {
            timing.interrupt();
        }
        for item in &mut self.stage_timings {
            item.timing.interrupt();
        }
        for unit in &mut self.units {
            if let Some(timing) = &mut unit.timing {
                timing.interrupt();
            }
            if let Some(timing) = &mut unit.request_timing {
                timing.interrupt();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_resume_and_retries_only_accumulate_active_intervals() {
        let mut timing = ExecutionTiming::default();
        timing.update(100, true);
        timing.update(600, false);
        timing.update(10_000, true);
        timing.update(10_300, false);
        assert_eq!(timing.elapsed_ms, 800);
        assert_eq!(timing.active_since_unix_ms, None);
    }

    #[test]
    fn crash_keeps_only_recorded_time_and_marks_it_incomplete() {
        let mut timing = ExecutionTiming::default();
        timing.update(100, true);
        timing.update(600, true);
        timing.interrupt();
        timing.update(10_000, true);
        timing.update(10_300, false);
        assert_eq!(timing.elapsed_ms, 800);
        assert!(timing.incomplete);
    }
}

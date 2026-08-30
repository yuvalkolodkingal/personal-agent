//! Admission control for local models sharing a bounded GPU-memory budget.

use std::collections::BTreeMap;
use thiserror::Error;

/// The maximum model-declared GPU memory admitted on the 6 GB reference GPU.
///
/// The remaining 1.5 GB is deliberately left to CUDA/runtime overhead, desktop
/// rendering, and transient inference allocations.
pub const DEFAULT_VRAM_CEILING_MIB: u32 = 4_500;

/// A local model known to the shared resource arbiter.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LocalModel {
    /// Qwen3-TTS 0.6B on CUDA.
    Qwen3Tts,
    /// faster-whisper large-v3-turbo with int8 weights on CUDA.
    FasterWhisperLargeV3TurboInt8,
    /// Optional GPU visual-grounding model.
    VisionGrounding,
    /// CPU Moonshine recognition fallback.
    Moonshine,
    /// CPU Kokoro speech-synthesis fallback.
    Kokoro,
    /// CPU OCR fallback.
    Ocrs,
}

impl LocalModel {
    /// Stable model identifier used by the worker protocol.
    #[must_use]
    pub const fn worker_id(self) -> &'static str {
        match self {
            Self::Qwen3Tts => "qwen3-tts",
            Self::FasterWhisperLargeV3TurboInt8 => "faster-whisper-large-v3-turbo-int8",
            Self::VisionGrounding => "vision-grounding",
            Self::Moonshine => "moonshine",
            Self::Kokoro => "kokoro",
            Self::Ocrs => "ocrs",
        }
    }

    #[must_use]
    const fn declaration(self) -> ModelDeclaration {
        match self {
            Self::Qwen3Tts => ModelDeclaration::gpu(1_400, ModelPriority::ActiveTts),
            Self::FasterWhisperLargeV3TurboInt8 => {
                ModelDeclaration::gpu(1_500, ModelPriority::ActiveStt)
            }
            Self::VisionGrounding => ModelDeclaration::gpu(1_200, ModelPriority::Vision),
            Self::Moonshine | Self::Kokoro | Self::Ocrs => ModelDeclaration::cpu(),
        }
    }

    /// Declared GPU-memory cost in MiB. CPU fallbacks always report zero.
    #[must_use]
    pub const fn vram_mib(self) -> u32 {
        self.declaration().vram_mib
    }

    /// Whether this model uses the shared GPU-memory budget.
    #[must_use]
    pub const fn uses_gpu(self) -> bool {
        self.declaration().uses_gpu
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ModelPriority {
    Vision,
    ActiveStt,
    ActiveTts,
    CpuFallback,
}

#[derive(Clone, Copy, Debug)]
struct ModelDeclaration {
    vram_mib: u32,
    priority: ModelPriority,
    uses_gpu: bool,
}

impl ModelDeclaration {
    const fn gpu(vram_mib: u32, priority: ModelPriority) -> Self {
        Self {
            vram_mib,
            priority,
            uses_gpu: true,
        }
    }

    const fn cpu() -> Self {
        Self {
            vram_mib: 0,
            priority: ModelPriority::CpuFallback,
            uses_gpu: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ModelState {
    loaded: bool,
    active_leases: u32,
}

/// A deterministic admission plan. Unloads must complete before this plan is
/// committed and the requested model is loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionPlan {
    requested: LocalModel,
    unload: Vec<LocalModel>,
    projected_vram_mib: u32,
    already_loaded: bool,
}

impl AdmissionPlan {
    /// Model whose load is being admitted.
    #[must_use]
    pub const fn requested(&self) -> LocalModel {
        self.requested
    }

    /// Idle models to unload, in lowest-priority-first order.
    #[must_use]
    pub fn models_to_unload(&self) -> &[LocalModel] {
        &self.unload
    }

    /// Declared GPU memory after applying the plan.
    #[must_use]
    pub const fn projected_vram_mib(&self) -> u32 {
        self.projected_vram_mib
    }

    /// Whether the requested model was already resident.
    #[must_use]
    pub const fn already_loaded(&self) -> bool {
        self.already_loaded
    }
}

/// A model cannot be admitted without evicting an active workload.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error(
    "cannot admit {requested:?}: {required_vram_mib} MiB would exceed the {ceiling_vram_mib} MiB ceiling while higher-priority or active models are resident"
)]
pub struct AdmissionDenied {
    /// Requested model.
    pub requested: LocalModel,
    /// Memory still required after considering every idle eviction candidate.
    pub required_vram_mib: u32,
    /// Configured model-memory ceiling.
    pub ceiling_vram_mib: u32,
}

/// Registry and admission controller for local CPU and GPU models.
#[derive(Clone, Debug)]
pub struct ModelArbiter {
    ceiling_vram_mib: u32,
    states: BTreeMap<LocalModel, ModelState>,
}

impl Default for ModelArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelArbiter {
    /// Build the reference-machine arbiter with a 4.5 GB model ceiling.
    #[must_use]
    pub fn new() -> Self {
        Self::with_ceiling_mib(DEFAULT_VRAM_CEILING_MIB)
    }

    /// Build an arbiter with an explicit ceiling. This is useful for hardware
    /// profiles smaller than the reference machine and deterministic tests.
    #[must_use]
    pub fn with_ceiling_mib(ceiling_vram_mib: u32) -> Self {
        let states = [
            LocalModel::Qwen3Tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
            LocalModel::VisionGrounding,
            LocalModel::Moonshine,
            LocalModel::Kokoro,
            LocalModel::Ocrs,
        ]
        .into_iter()
        .map(|model| (model, ModelState::default()))
        .collect();
        Self {
            ceiling_vram_mib,
            states,
        }
    }

    /// Configured model-memory ceiling.
    #[must_use]
    pub const fn ceiling_vram_mib(&self) -> u32 {
        self.ceiling_vram_mib
    }

    /// Currently declared resident GPU memory.
    #[must_use]
    pub fn resident_vram_mib(&self) -> u32 {
        self.states
            .iter()
            .filter(|(_, state)| state.loaded)
            .map(|(model, _)| model.vram_mib())
            .sum()
    }

    /// Whether a model is recorded as resident.
    #[must_use]
    pub fn is_loaded(&self, model: LocalModel) -> bool {
        self.state(model).loaded
    }

    /// Whether a resident model currently has an active user.
    #[must_use]
    pub fn is_active(&self, model: LocalModel) -> bool {
        self.state(model).active_leases > 0
    }

    /// Compute evictions for a model load without changing registry state.
    /// CPU fallbacks are always admitted and never cause GPU eviction.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionDenied`] when idle evictions cannot make enough room.
    pub fn plan_admission(&self, requested: LocalModel) -> Result<AdmissionPlan, AdmissionDenied> {
        let current_vram = self.resident_vram_mib();
        if self.is_loaded(requested) {
            return Ok(AdmissionPlan {
                requested,
                unload: Vec::new(),
                projected_vram_mib: current_vram,
                already_loaded: true,
            });
        }
        if !requested.uses_gpu() {
            return Ok(AdmissionPlan {
                requested,
                unload: Vec::new(),
                projected_vram_mib: current_vram,
                already_loaded: false,
            });
        }

        let mut projected = current_vram.saturating_add(requested.vram_mib());
        let mut candidates = self
            .states
            .iter()
            .filter(|(model, state)| {
                **model != requested && model.uses_gpu() && state.loaded && state.active_leases == 0
            })
            .map(|(model, _)| (*model, model.declaration().priority))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(model, priority)| (*priority, *model));

        let mut unload = Vec::new();
        for (candidate, _) in candidates {
            if projected <= self.ceiling_vram_mib {
                break;
            }
            projected = projected.saturating_sub(candidate.vram_mib());
            unload.push(candidate);
        }
        if projected > self.ceiling_vram_mib {
            return Err(AdmissionDenied {
                requested,
                required_vram_mib: projected,
                ceiling_vram_mib: self.ceiling_vram_mib,
            });
        }
        Ok(AdmissionPlan {
            requested,
            unload,
            projected_vram_mib: projected,
            already_loaded: false,
        })
    }

    /// Commit a plan after every requested worker unload has succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionDenied`] if a planned eviction became active or the
    /// registry otherwise changed such that the request no longer fits.
    pub fn commit_admission(&mut self, plan: &AdmissionPlan) -> Result<(), AdmissionDenied> {
        for model in &plan.unload {
            let state = self.state(*model);
            if !state.loaded || state.active_leases > 0 {
                return Err(self.denied(plan.requested));
            }
        }

        let mut projected = self.resident_vram_mib();
        if !self.is_loaded(plan.requested) {
            projected = projected.saturating_add(plan.requested.vram_mib());
        }
        for model in &plan.unload {
            projected = projected.saturating_sub(model.vram_mib());
        }
        if projected > self.ceiling_vram_mib {
            return Err(self.denied(plan.requested));
        }
        for model in &plan.unload {
            self.state_mut(*model).loaded = false;
        }
        self.state_mut(plan.requested).loaded = true;
        Ok(())
    }

    /// Record one active use of a resident model.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionDenied`] when the model has not been admitted.
    pub fn activate(&mut self, model: LocalModel) -> Result<(), AdmissionDenied> {
        if !self.is_loaded(model) {
            return Err(self.denied(model));
        }
        let state = self.state_mut(model);
        state.active_leases = state.active_leases.saturating_add(1);
        Ok(())
    }

    /// Release one active use. Extra releases are harmless.
    pub fn release(&mut self, model: LocalModel) {
        let state = self.state_mut(model);
        state.active_leases = state.active_leases.saturating_sub(1);
    }

    /// Reconcile an explicit worker unload with registry state.
    pub fn mark_unloaded(&mut self, model: LocalModel) {
        let state = self.state_mut(model);
        state.loaded = false;
        state.active_leases = 0;
    }

    /// Reconcile registry state after the shared neural worker exits.
    ///
    /// Process termination releases the worker's CUDA context and invalidates
    /// every GPU-model lease. CPU fallbacks are logical entries and remain
    /// unaffected.
    pub fn reset_worker_gpu_models(&mut self) {
        for model in [
            LocalModel::Qwen3Tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
            LocalModel::VisionGrounding,
        ] {
            self.mark_unloaded(model);
        }
    }

    fn state(&self, model: LocalModel) -> ModelState {
        self.states.get(&model).copied().unwrap_or_default()
    }

    fn state_mut(&mut self, model: LocalModel) -> &mut ModelState {
        self.states.entry(model).or_default()
    }

    fn denied(&self, requested: LocalModel) -> AdmissionDenied {
        AdmissionDenied {
            requested,
            required_vram_mib: self
                .resident_vram_mib()
                .saturating_add(requested.vram_mib()),
            ceiling_vram_mib: self.ceiling_vram_mib,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit(arbiter: &mut ModelArbiter, model: LocalModel) -> AdmissionPlan {
        let plan = arbiter.plan_admission(model).expect("admission plan");
        arbiter.commit_admission(&plan).expect("commit admission");
        plan
    }

    #[test]
    fn declared_gpu_models_fit_reference_budget_and_cpu_is_free() {
        let mut arbiter = ModelArbiter::new();
        for model in [
            LocalModel::Qwen3Tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
            LocalModel::VisionGrounding,
        ] {
            assert!(admit(&mut arbiter, model).models_to_unload().is_empty());
        }
        assert_eq!(arbiter.resident_vram_mib(), 4_100);

        let cpu = admit(&mut arbiter, LocalModel::Moonshine);
        assert!(cpu.models_to_unload().is_empty());
        assert_eq!(cpu.projected_vram_mib(), 4_100);
        assert_eq!(arbiter.resident_vram_mib(), 4_100);
    }

    #[test]
    fn admission_evicts_idle_models_in_vision_stt_tts_order() {
        let mut arbiter = ModelArbiter::with_ceiling_mib(2_900);
        admit(&mut arbiter, LocalModel::VisionGrounding);
        admit(&mut arbiter, LocalModel::FasterWhisperLargeV3TurboInt8);

        let tts = arbiter
            .plan_admission(LocalModel::Qwen3Tts)
            .expect("vision can be evicted");
        assert_eq!(tts.models_to_unload(), &[LocalModel::VisionGrounding]);
        arbiter.commit_admission(&tts).expect("commit tts");

        let mut stt_before_tts = ModelArbiter::with_ceiling_mib(2_900);
        admit(&mut stt_before_tts, LocalModel::Qwen3Tts);
        admit(
            &mut stt_before_tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
        );
        let vision = stt_before_tts
            .plan_admission(LocalModel::VisionGrounding)
            .expect("idle STT can be evicted before idle TTS");
        assert_eq!(
            vision.models_to_unload(),
            &[LocalModel::FasterWhisperLargeV3TurboInt8]
        );

        let mut tts_is_last = ModelArbiter::with_ceiling_mib(1_500);
        admit(&mut tts_is_last, LocalModel::Qwen3Tts);
        let stt = tts_is_last
            .plan_admission(LocalModel::FasterWhisperLargeV3TurboInt8)
            .expect("idle TTS remains evictable as a last resort");
        assert_eq!(stt.models_to_unload(), &[LocalModel::Qwen3Tts]);
    }

    #[test]
    fn active_models_are_never_evicted() {
        let mut arbiter = ModelArbiter::with_ceiling_mib(2_600);
        admit(&mut arbiter, LocalModel::Qwen3Tts);
        admit(&mut arbiter, LocalModel::VisionGrounding);
        arbiter
            .activate(LocalModel::VisionGrounding)
            .expect("vision active");

        let denied = arbiter
            .plan_admission(LocalModel::FasterWhisperLargeV3TurboInt8)
            .expect_err("active vision plus TTS cannot be evicted enough");
        assert_eq!(denied.requested, LocalModel::FasterWhisperLargeV3TurboInt8);
        assert!(arbiter.is_loaded(LocalModel::VisionGrounding));
        assert!(arbiter.is_active(LocalModel::VisionGrounding));
    }

    #[test]
    fn stale_plan_cannot_evict_a_model_that_became_active() {
        let mut arbiter = ModelArbiter::with_ceiling_mib(2_900);
        admit(&mut arbiter, LocalModel::VisionGrounding);
        admit(&mut arbiter, LocalModel::FasterWhisperLargeV3TurboInt8);
        let plan = arbiter.plan_admission(LocalModel::Qwen3Tts).expect("plan");
        arbiter
            .activate(LocalModel::VisionGrounding)
            .expect("activate");
        assert!(arbiter.commit_admission(&plan).is_err());
        assert!(arbiter.is_loaded(LocalModel::VisionGrounding));
        assert!(!arbiter.is_loaded(LocalModel::Qwen3Tts));
    }

    #[test]
    fn worker_exit_clears_gpu_residency_and_active_leases() {
        let mut arbiter = ModelArbiter::new();
        for model in [
            LocalModel::Qwen3Tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
            LocalModel::VisionGrounding,
        ] {
            admit(&mut arbiter, model);
            arbiter.activate(model).expect("activate model");
        }

        arbiter.reset_worker_gpu_models();

        for model in [
            LocalModel::Qwen3Tts,
            LocalModel::FasterWhisperLargeV3TurboInt8,
            LocalModel::VisionGrounding,
        ] {
            assert!(!arbiter.is_loaded(model));
            assert!(!arbiter.is_active(model));
        }
        assert_eq!(arbiter.resident_vram_mib(), 0);
    }
}

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::parser::file_context::SemanticScopeKind;

pub const NUM_POLICIES: usize = 16;
pub const NUM_FILE_KINDS: usize = 3;
pub const NUM_BLOCK_KINDS: usize = 6;
const FILE_EMA_LEN: usize = NUM_POLICIES * NUM_FILE_KINDS; // 48
const BLOCK_EMA_LEN: usize = NUM_POLICIES * NUM_BLOCK_KINDS; // 96
const MIN_OBSERVATIONS: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FileContextKind {
    Header = 0,
    Implementation = 1,
    Paired = 2,
}

impl FileContextKind {
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "h" | "hh" | "hpp" | "hxx" | "ipp" | "inl" => Self::Header,
            "c" | "cc" | "cpp" | "cxx" => Self::Implementation,
            _ => Self::Header,
        }
    }

}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum BlockContextKind {
    Namespace = 0,
    Type = 1,
    Function = 2,
    Preprocessor = 3,
    Global = 4,
    Template = 5,
}

impl BlockContextKind {
    pub fn from_scope_kind(kind: SemanticScopeKind) -> Self {
        match kind {
            SemanticScopeKind::Namespace => Self::Namespace,
            SemanticScopeKind::Type => Self::Type,
            SemanticScopeKind::Function => Self::Function,
            SemanticScopeKind::Preprocessor => Self::Preprocessor,
            SemanticScopeKind::Template => Self::Template,
            SemanticScopeKind::Attribute => Self::Global,
        }
    }
}

pub fn policy_index(policy_name: &str) -> Option<u8> {
    match policy_name {
        "dash_comment_normalizer" => Some(0),
        "section_title_normalizer" => Some(1),
        "compact_declarations" => Some(2),
        "class_layout" => Some(3),
        "lua_macro_spacing" => Some(4),
        "namespace_end_comments" => Some(5),
        "pragma_once_spacing" => Some(6),
        "include_guards" => Some(7),
        "include_order" => Some(8),
        "logical_keyword_operators" => Some(9),
        "function_void_params" => Some(10),
        "operator_overload_spacing" => Some(11),
        "clang_format" => Some(12),
        "naming_conventions" => Some(13),
        "snake_case" => Some(14),
        "numeric_literal_suffix" => Some(15),
        _ => None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyContextTracker {
    file_ema: Vec<f32>,
    file_cnt: Vec<u32>,
    block_ema: Vec<f32>,
    block_cnt: Vec<u32>,
}

impl Default for PolicyContextTracker {
    fn default() -> Self {
        Self {
            file_ema: vec![0.5; FILE_EMA_LEN],
            file_cnt: vec![0; FILE_EMA_LEN],
            block_ema: vec![0.5; BLOCK_EMA_LEN],
            block_cnt: vec![0; BLOCK_EMA_LEN],
        }
    }
}

#[cfg(test)]
pub struct PolicyOutcomeRecord {
    pub policy_index: u8,
    pub block_kind: BlockContextKind,
    pub success: bool,
}

impl PolicyContextTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = bincode::serde::encode_to_vec(self, bincode::config::standard())
            .map_err(|e| anyhow::anyhow!("bincode encode: {}", e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_path(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let (tracker, _): (Self, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
        Some(tracker)
    }

    #[cfg(test)]
    fn file_idx(policy_idx: u8, file_kind: FileContextKind) -> usize {
        (policy_idx as usize) * NUM_FILE_KINDS + (file_kind as usize)
    }

    #[cfg(test)]
    fn block_idx(policy_idx: u8, block_kind: BlockContextKind) -> usize {
        (policy_idx as usize) * NUM_BLOCK_KINDS + (block_kind as usize)
    }

    #[cfg(test)]
    pub fn observe_file(&mut self, policy_idx: u8, file_kind: FileContextKind, success: bool) {
        const EMA_ALPHA: f32 = 0.20;
        let idx = Self::file_idx(policy_idx, file_kind);
        if idx >= FILE_EMA_LEN {
            return;
        }
        let outcome = if success { 1.0f32 } else { 0.0f32 };
        self.file_ema[idx] = EMA_ALPHA * outcome + (1.0 - EMA_ALPHA) * self.file_ema[idx];
        self.file_cnt[idx] = self.file_cnt[idx].saturating_add(1);
    }

    #[cfg(test)]
    pub fn observe_block(
        &mut self,
        policy_idx: u8,
        block_kind: BlockContextKind,
        success: bool,
    ) {
        const EMA_ALPHA: f32 = 0.20;
        let idx = Self::block_idx(policy_idx, block_kind);
        if idx >= BLOCK_EMA_LEN {
            return;
        }
        let outcome = if success { 1.0f32 } else { 0.0f32 };
        self.block_ema[idx] = EMA_ALPHA * outcome + (1.0 - EMA_ALPHA) * self.block_ema[idx];
        self.block_cnt[idx] = self.block_cnt[idx].saturating_add(1);
    }

    #[cfg(test)]
    pub fn batch_observe_outcomes(
        &mut self,
        file_kind: FileContextKind,
        outcomes: &[PolicyOutcomeRecord],
    ) {
        for record in outcomes {
            self.observe_file(record.policy_index, file_kind, record.success);
            self.observe_block(record.policy_index, record.block_kind, record.success);
        }
    }

    #[cfg(test)]
    fn context_modifier(&self, policy_idx: u8, file_kind: FileContextKind, block_kind: BlockContextKind) -> f64 {
        let file_mod = self.file_modifier(policy_idx, file_kind);
        let block_mod = self.block_modifier(policy_idx, block_kind);
        (file_mod * block_mod) as f64
    }

    #[cfg(test)]
    fn file_modifier(&self, policy_idx: u8, file_kind: FileContextKind) -> f32 {
        let idx = Self::file_idx(policy_idx, file_kind);
        if idx >= FILE_EMA_LEN || self.file_cnt[idx] < MIN_OBSERVATIONS {
            return 1.0;
        }
        interp_modifier(self.file_ema[idx])
    }

    #[cfg(test)]
    fn block_modifier(&self, policy_idx: u8, block_kind: BlockContextKind) -> f32 {
        let idx = Self::block_idx(policy_idx, block_kind);
        if idx >= BLOCK_EMA_LEN || self.block_cnt[idx] < MIN_OBSERVATIONS {
            return 1.0;
        }
        interp_modifier(self.block_ema[idx])
    }

    /// Batch-compute all 16 policy file modifiers for a given file_kind.
    /// Returns [f32; 24] (padded to 24 for NEON alignment; indices 16..23 = 1.0).
    pub fn batch_file_modifiers(&self, file_kind: FileContextKind) -> [f32; 24] {
        let mut result = [1.0f32; 24];
        #[cfg(target_arch = "aarch64")]
        {
            unsafe { self.batch_file_modifiers_neon(file_kind, &mut result) };
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            self.batch_file_modifiers_scalar(file_kind, &mut result);
        }
        result
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn batch_file_modifiers_scalar(&self, file_kind: FileContextKind, out: &mut [f32; 24]) {
        let fk = file_kind as usize;
        for p in 0..NUM_POLICIES {
            let idx = p * NUM_FILE_KINDS + fk;
            if self.file_cnt[idx] >= MIN_OBSERVATIONS {
                out[p] = interp_modifier(self.file_ema[idx]);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    unsafe fn batch_file_modifiers_neon(&self, file_kind: FileContextKind, out: &mut [f32; 24]) {
        use core::arch::aarch64::*;

        let fk = file_kind as usize;
        let min_obs = vdupq_n_u32(MIN_OBSERVATIONS);
        let one = vdupq_n_f32(1.0);

        // Process 4 policies at a time (6 iterations = 24 slots, covering all 22 + 2 padding)
        let mut p = 0usize;
        while p + 4 <= 24 {
            // Gather 4 EMA values and 4 counts for policies [p..p+4]
            let mut ema_vals = [0.5f32; 4];
            let mut cnt_vals = [0u32; 4];
            for i in 0..4 {
                let pi = p + i;
                if pi < NUM_POLICIES {
                    let idx = pi * NUM_FILE_KINDS + fk;
                    ema_vals[i] = self.file_ema[idx];
                    cnt_vals[i] = self.file_cnt[idx];
                }
            }

            let ema_v = vld1q_f32(ema_vals.as_ptr());
            let cnt_v = vld1q_u32(cnt_vals.as_ptr());

            // Compute modifiers via interp: piecewise linear [0,0.5] -> [0.5,1.0], [0.5,1.0] -> [1.0,1.3]
            let mod_v = interp_modifier_neon(ema_v);

            // Mask: only apply if count >= MIN_OBSERVATIONS
            let mask = vcgeq_u32(cnt_v, min_obs);
            let masked = vbslq_f32(mask, mod_v, one);

            vst1q_f32(out.as_mut_ptr().add(p), masked);
            p += 4;
        }
    }

    /// Batch-compute all 16 policy block modifiers for a given block_kind.
    /// Returns [f32; 24] (padded to 24 for NEON alignment; indices 16..23 = 1.0).
    pub fn batch_block_modifiers(&self, block_kind: BlockContextKind) -> [f32; 24] {
        let mut result = [1.0f32; 24];
        #[cfg(target_arch = "aarch64")]
        {
            unsafe { self.batch_block_modifiers_neon(block_kind, &mut result) };
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            self.batch_block_modifiers_scalar(block_kind, &mut result);
        }
        result
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn batch_block_modifiers_scalar(&self, block_kind: BlockContextKind, out: &mut [f32; 24]) {
        let bk = block_kind as usize;
        for p in 0..NUM_POLICIES {
            let idx = p * NUM_BLOCK_KINDS + bk;
            if self.block_cnt[idx] >= MIN_OBSERVATIONS {
                out[p] = interp_modifier(self.block_ema[idx]);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    unsafe fn batch_block_modifiers_neon(&self, block_kind: BlockContextKind, out: &mut [f32; 24]) {
        use core::arch::aarch64::*;

        let bk = block_kind as usize;
        let min_obs = vdupq_n_u32(MIN_OBSERVATIONS);
        let one = vdupq_n_f32(1.0);

        let mut p = 0usize;
        while p + 4 <= 24 {
            let mut ema_vals = [0.5f32; 4];
            let mut cnt_vals = [0u32; 4];
            for i in 0..4 {
                let pi = p + i;
                if pi < NUM_POLICIES {
                    let idx = pi * NUM_BLOCK_KINDS + bk;
                    ema_vals[i] = self.block_ema[idx];
                    cnt_vals[i] = self.block_cnt[idx];
                }
            }

            let ema_v = vld1q_f32(ema_vals.as_ptr());
            let cnt_v = vld1q_u32(cnt_vals.as_ptr());

            let mod_v = interp_modifier_neon(ema_v);

            let mask = vcgeq_u32(cnt_v, min_obs);
            let masked = vbslq_f32(mask, mod_v, one);

            vst1q_f32(out.as_mut_ptr().add(p), masked);
            p += 4;
        }
    }
}

/// Piecewise linear interpolation: ema -> modifier
/// [0.0] -> 0.5, [0.5] -> 1.0, [1.0] -> 1.3
#[inline]
#[cfg(any(test, not(target_arch = "aarch64")))]
fn interp_modifier(ema: f32) -> f32 {
    let x = ema.clamp(0.0, 1.0);
    if x <= 0.5 {
        0.5 + (1.0 - 0.5) * (x * 2.0)
    } else {
        1.0 + (1.3 - 1.0) * ((x - 0.5) * 2.0)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn interp_modifier_neon(ema: core::arch::aarch64::float32x4_t) -> core::arch::aarch64::float32x4_t {
    use core::arch::aarch64::*;

    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);
    let half = vdupq_n_f32(0.5);
    // Clamp to [0, 1]
    let x = vminq_f32(vmaxq_f32(ema, zero), one);

    // Low branch: 0.5 + 0.5 * (x * 2.0) = 0.5 + x
    let low = vaddq_f32(half, x);

    // High branch: 1.0 + 0.3 * ((x - 0.5) * 2.0) = 1.0 + 0.6 * (x - 0.5)
    let high = vmlaq_f32(one, vdupq_n_f32(0.6), vsubq_f32(x, half));

    // Select: x <= 0.5 ? low : high
    let mask = vcleq_f32(x, half);
    vbslq_f32(vreinterpretq_u32_f32(vreinterpretq_f32_u32(mask)), low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_modifier_with_no_observations() {
        let tracker = PolicyContextTracker::new();
        let m = tracker.context_modifier(0, FileContextKind::Header, BlockContextKind::Global);
        assert!((m - 1.0).abs() < 1e-6, "expected neutral modifier, got {}", m);
    }

    #[test]
    fn modifier_increases_after_successes() {
        let mut tracker = PolicyContextTracker::new();
        for _ in 0..10 {
            tracker.observe_file(0, FileContextKind::Header, true);
            tracker.observe_block(0, BlockContextKind::Namespace, true);
        }
        let m = tracker.context_modifier(0, FileContextKind::Header, BlockContextKind::Namespace);
        assert!(m > 1.0, "expected boost after successes, got {}", m);
    }

    #[test]
    fn modifier_decreases_after_failures() {
        let mut tracker = PolicyContextTracker::new();
        for _ in 0..10 {
            tracker.observe_file(0, FileContextKind::Implementation, false);
            tracker.observe_block(0, BlockContextKind::Function, false);
        }
        let m = tracker.context_modifier(0, FileContextKind::Implementation, BlockContextKind::Function);
        assert!(m < 1.0, "expected penalty after failures, got {}", m);
    }

    #[test]
    fn batch_file_modifiers_neutral_initially() {
        let tracker = PolicyContextTracker::new();
        let mods = tracker.batch_file_modifiers(FileContextKind::Header);
        for i in 0..NUM_POLICIES {
            assert!((mods[i] - 1.0).abs() < 1e-6, "policy {} not neutral: {}", i, mods[i]);
        }
    }

    #[test]
    fn batch_file_modifiers_reflect_learning() {
        let mut tracker = PolicyContextTracker::new();
        // Policy 5 succeeds a lot in headers
        for _ in 0..10 {
            tracker.observe_file(5, FileContextKind::Header, true);
        }
        // Policy 10 fails a lot in headers
        for _ in 0..10 {
            tracker.observe_file(10, FileContextKind::Header, false);
        }
        let mods = tracker.batch_file_modifiers(FileContextKind::Header);
        assert!(mods[5] > 1.0, "policy 5 should be boosted: {}", mods[5]);
        assert!(mods[10] < 1.0, "policy 10 should be penalized: {}", mods[10]);
        // Other policies neutral
        assert!((mods[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn interp_modifier_boundaries() {
        assert!((interp_modifier(0.0) - 0.5).abs() < 1e-6);
        assert!((interp_modifier(0.5) - 1.0).abs() < 1e-6);
        assert!((interp_modifier(1.0) - 1.3).abs() < 1e-6);
        assert!((interp_modifier(0.25) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn persistence_round_trip() {
        let mut tracker = PolicyContextTracker::new();
        for _ in 0..5 {
            tracker.observe_file(3, FileContextKind::Paired, true);
            tracker.observe_block(3, BlockContextKind::Type, true);
        }
        let dir = std::env::temp_dir().join("mj_fmt_test_ctx_tracker");
        let path = dir.join("tracker.bin");
        tracker.save_to_path(&path).unwrap();
        let loaded = PolicyContextTracker::load_from_path(&path).unwrap();
        assert!((loaded.file_ema[3 * NUM_FILE_KINDS + 2] - tracker.file_ema[3 * NUM_FILE_KINDS + 2]).abs() < 1e-6);
        assert_eq!(loaded.file_cnt[3 * NUM_FILE_KINDS + 2], tracker.file_cnt[3 * NUM_FILE_KINDS + 2]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn policy_index_covers_all_known() {
        assert_eq!(policy_index("dash_comment_normalizer"), Some(0));
        assert_eq!(policy_index("numeric_literal_suffix"), Some(15));
        assert_eq!(policy_index("unknown_policy"), None);
        assert_eq!(policy_index("trim_trailing_whitespace"), None);
        assert_eq!(policy_index("align_assignments"), None);
    }

    #[test]
    fn batch_observe_outcomes_updates_both() {
        let mut tracker = PolicyContextTracker::new();
        let outcomes = vec![
            PolicyOutcomeRecord { policy_index: 0, block_kind: BlockContextKind::Global, success: true },
            PolicyOutcomeRecord { policy_index: 1, block_kind: BlockContextKind::Namespace, success: false },
        ];
        for _ in 0..5 {
            tracker.batch_observe_outcomes(FileContextKind::Header, &outcomes);
        }
        let m0 = tracker.context_modifier(0, FileContextKind::Header, BlockContextKind::Global);
        let m1 = tracker.context_modifier(1, FileContextKind::Header, BlockContextKind::Namespace);
        assert!(m0 > 1.0, "policy 0 should be boosted: {}", m0);
        assert!(m1 < 1.0, "policy 1 should be penalized: {}", m1);
    }

    #[test]
    fn batch_block_modifiers_neutral_initially() {
        let tracker = PolicyContextTracker::new();
        let mods = tracker.batch_block_modifiers(BlockContextKind::Function);
        for p in 0..NUM_POLICIES {
            assert!((mods[p] - 1.0).abs() < 1e-6, "policy {p} should be neutral");
        }
    }

    #[test]
    fn batch_block_modifiers_reflect_learning() {
        let mut tracker = PolicyContextTracker::new();
        for _ in 0..5 {
            tracker.observe_block(2, BlockContextKind::Function, true);
            tracker.observe_block(3, BlockContextKind::Function, false);
        }
        let mods = tracker.batch_block_modifiers(BlockContextKind::Function);
        assert!(mods[2] > 1.0, "policy 2 should be boosted: {}", mods[2]);
        assert!(mods[3] < 1.0, "policy 3 should be penalized: {}", mods[3]);
        assert!((mods[0] - 1.0).abs() < 1e-6, "policy 0 should be neutral");
    }
}

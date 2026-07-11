use tracing::{info, warn};

use crate::devices::channels::{ChannelLabel, ChannelLayout, ChannelPosition};
use crate::media::pipeline::ChannelProducers;

pub const SURROUND_ATTENUATION: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixOptions {
    pub lfe_to_stereo: bool,
    pub surround_attenuation: f64,
}

impl Default for MixOptions {
    fn default() -> Self {
        Self {
            lfe_to_stereo: false,
            surround_attenuation: SURROUND_ATTENUATION,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixMatrix {
    /// `out_channels` rows, `in_channels` columns
    rows: Vec<Vec<f64>>,
    in_channels: usize,
    out_channels: usize,
}

impl MixMatrix {
    pub fn in_channels(&self) -> usize {
        self.in_channels
    }

    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[Vec<f64>] {
        &self.rows
    }

    pub fn is_identity(&self) -> bool {
        if self.in_channels != self.out_channels {
            return false;
        }
        for (i, row) in self.rows.iter().enumerate() {
            for (j, &w) in row.iter().enumerate() {
                let expected = if i == j { 1.0 } else { 0.0 };
                if (w - expected).abs() > f64::EPSILON {
                    return false;
                }
            }
        }
        true
    }

    /// Build a mix matrix from source and destination layouts.
    ///
    /// Falls back to a generic count-based matrix when either side lacks positional
    /// information.
    pub fn build(src: &ChannelLayout, dst: &ChannelLayout, opts: MixOptions) -> MixMatrix {
        let mut matrix = match (src, dst) {
            (ChannelLayout::Positioned(src_pos), ChannelLayout::Positioned(dst_pos)) => {
                build_positioned(*src_pos, *dst_pos, opts)
            }
            _ => build_count_based(src.count(), dst.count(), opts),
        };
        matrix.normalize();
        matrix
    }

    /// Scale the matrix down when any output row's weights sum past 1.0, so a full-scale input
    /// can never clip. The scale is uniform across all rows to preserve channel balance.
    fn normalize(&mut self) {
        let max_row_sum = self
            .rows
            .iter()
            .map(|row| row.iter().map(|w| w.abs()).sum::<f64>())
            .fold(0.0, f64::max);
        if max_row_sum > 1.0 {
            for row in &mut self.rows {
                for w in row {
                    *w /= max_row_sum;
                }
            }
        }
    }

    /// Mix a single frame of planar samples.
    ///
    /// `input` and `output` must have lengths `in_channels` and `out_channels`
    /// respectively. `output` is overwritten (not accumulated).
    pub fn mix_frame(&self, input: &[f64], output: &mut [f64]) {
        debug_assert_eq!(input.len(), self.in_channels);
        debug_assert_eq!(output.len(), self.out_channels);

        for (i, row) in self.rows.iter().enumerate() {
            let mut acc = 0.0f64;
            for (j, &w) in row.iter().enumerate() {
                if w != 0.0 {
                    acc += w * input[j];
                }
            }
            // normalization keeps row sums <= 1.0, so this is a pure safety net
            output[i] = acc.clamp(-1.0, 1.0);
        }
    }
}

/// Look up the canonical index of a single-bit position within a positioned layout's
/// channel order (ascending bit order, matching [`ChannelPosition::positions`]).
fn position_index(haystack: ChannelPosition, needle: ChannelPosition) -> Option<usize> {
    if needle.bits().count_ones() != 1 || !haystack.contains(needle) {
        return None;
    }
    // Number of set bits strictly below the needle's bit gives the index.
    let below = haystack.bits() & (needle.bits() - 1);
    Some(below.count_ones() as usize)
}

fn build_positioned(src: ChannelPosition, dst: ChannelPosition, opts: MixOptions) -> MixMatrix {
    // Some decoders report mono as FRONT_LEFT; normalize so it plays in both channels.
    let src = if src.bits().count_ones() == 1 {
        ChannelPosition::FRONT_CENTER
    } else {
        src
    };

    let in_channels = src.bits().count_ones() as usize;
    let out_channels = dst.bits().count_ones() as usize;

    let mut rows = vec![vec![0.0f64; in_channels]; out_channels];

    let a = opts.surround_attenuation;

    let mut route = |out_pos: ChannelPosition, in_pos: ChannelPosition, weight: f64| {
        if weight == 0.0 {
            return;
        }
        let (Some(oi), Some(ii)) = (position_index(dst, out_pos), position_index(src, in_pos))
        else {
            return;
        };
        rows[oi][ii] += weight;
    };

    let has_l = dst.contains(ChannelPosition::FRONT_LEFT);
    let has_r = dst.contains(ChannelPosition::FRONT_RIGHT);
    let has_c = dst.contains(ChannelPosition::FRONT_CENTER);
    let has_rl = dst.contains(ChannelPosition::REAR_LEFT);
    let has_rr = dst.contains(ChannelPosition::REAR_RIGHT);
    let has_rc = dst.contains(ChannelPosition::REAR_CENTER);

    let src_only_mono = src == ChannelPosition::FRONT_CENTER;

    for in_pos in src.positions() {
        if dst.contains(in_pos) {
            route(in_pos, in_pos, 1.0);
        }
    }

    // Non-mono center folds into L/R when the destination has no center.
    if src.contains(ChannelPosition::FRONT_CENTER) && !has_c && !src_only_mono {
        if has_l {
            route(
                ChannelPosition::FRONT_LEFT,
                ChannelPosition::FRONT_CENTER,
                a,
            );
        }
        if has_r {
            route(
                ChannelPosition::FRONT_RIGHT,
                ChannelPosition::FRONT_CENTER,
                a,
            );
        }
    }

    // Stereo to mono averages the front pair.
    if !has_l && !has_r && has_c {
        if src.contains(ChannelPosition::FRONT_LEFT) {
            route(
                ChannelPosition::FRONT_CENTER,
                ChannelPosition::FRONT_LEFT,
                0.5,
            );
        }
        if src.contains(ChannelPosition::FRONT_RIGHT) {
            route(
                ChannelPosition::FRONT_CENTER,
                ChannelPosition::FRONT_RIGHT,
                0.5,
            );
        }
    }

    if src.contains(ChannelPosition::SIDE_LEFT) && !dst.contains(ChannelPosition::SIDE_LEFT) {
        if has_rl {
            route(ChannelPosition::REAR_LEFT, ChannelPosition::SIDE_LEFT, a);
        } else if has_l {
            route(ChannelPosition::FRONT_LEFT, ChannelPosition::SIDE_LEFT, a);
        }
    }
    if src.contains(ChannelPosition::SIDE_RIGHT) && !dst.contains(ChannelPosition::SIDE_RIGHT) {
        if has_rr {
            route(ChannelPosition::REAR_RIGHT, ChannelPosition::SIDE_RIGHT, a);
        } else if has_r {
            route(ChannelPosition::FRONT_RIGHT, ChannelPosition::SIDE_RIGHT, a);
        }
    }
    if src.contains(ChannelPosition::REAR_LEFT) && !has_rl && has_l {
        route(ChannelPosition::FRONT_LEFT, ChannelPosition::REAR_LEFT, a);
    }
    if src.contains(ChannelPosition::REAR_RIGHT) && !has_rr && has_r {
        route(ChannelPosition::FRONT_RIGHT, ChannelPosition::REAR_RIGHT, a);
    }
    if src.contains(ChannelPosition::REAR_CENTER) && !has_rc {
        if has_rl && has_rr {
            route(ChannelPosition::REAR_LEFT, ChannelPosition::REAR_CENTER, a);
            route(ChannelPosition::REAR_RIGHT, ChannelPosition::REAR_CENTER, a);
        } else if has_rl {
            route(ChannelPosition::REAR_LEFT, ChannelPosition::REAR_CENTER, a);
        } else if has_rr {
            route(ChannelPosition::REAR_RIGHT, ChannelPosition::REAR_CENTER, a);
        } else if has_l && has_r {
            route(ChannelPosition::FRONT_LEFT, ChannelPosition::REAR_CENTER, a);
            route(
                ChannelPosition::FRONT_RIGHT,
                ChannelPosition::REAR_CENTER,
                a,
            );
        } else if has_c {
            route(
                ChannelPosition::FRONT_CENTER,
                ChannelPosition::REAR_CENTER,
                a,
            );
        }
    }

    if opts.lfe_to_stereo
        && src.contains(ChannelPosition::LFE1)
        && !dst.contains(ChannelPosition::LFE1)
    {
        if has_l {
            route(ChannelPosition::FRONT_LEFT, ChannelPosition::LFE1, a);
        }
        if has_r {
            route(ChannelPosition::FRONT_RIGHT, ChannelPosition::LFE1, a);
        }
    }

    // Mono duplicates into L/R when there is no center output.
    if src_only_mono && !has_c {
        if has_l {
            route(
                ChannelPosition::FRONT_LEFT,
                ChannelPosition::FRONT_CENTER,
                1.0,
            );
        }
        if has_r {
            route(
                ChannelPosition::FRONT_RIGHT,
                ChannelPosition::FRONT_CENTER,
                1.0,
            );
        }
    }

    MixMatrix {
        rows,
        in_channels,
        out_channels,
    }
}

/// Count-only fallback for layouts without usable position metadata.
fn build_count_based(in_count: usize, out_count: usize, _opts: MixOptions) -> MixMatrix {
    let mut rows = vec![vec![0.0f64; in_count]; out_count];

    if in_count == 0 || out_count == 0 {
        return MixMatrix {
            rows,
            in_channels: in_count,
            out_channels: out_count,
        };
    }

    if in_count == out_count {
        for (i, row) in rows.iter_mut().enumerate() {
            row[i] = 1.0;
        }
    } else if in_count == 1 && out_count == 2 {
        rows[0][0] = 1.0;
        rows[1][0] = 1.0;
    } else if in_count == 2 && out_count == 1 {
        rows[0][0] = 0.5;
        rows[0][1] = 0.5;
    } else if in_count < out_count {
        for (i, row) in rows.iter_mut().enumerate().take(in_count) {
            row[i] = 1.0;
        }
    } else {
        warn!(
            "No positional layout available for {} -> {} channel mix; using coarse count-based fallback",
            in_count, out_count
        );
        #[allow(clippy::needless_range_loop)]
        for j in 0..in_count {
            let target = j.min(out_count - 1);
            rows[target][j] += 1.0 / out_count.max(1) as f64;
        }
    }

    MixMatrix {
        rows,
        in_channels: in_count,
        out_channels: out_count,
    }
}

pub struct ChannelMixer {
    matrix: MixMatrix,
    in_channels: usize,
    output_planes: Vec<Vec<f64>>,
    in_frame: Vec<f64>,
    out_frame: Vec<f64>,
    // needed for testing only
    #[allow(dead_code)]
    out_channels: usize,
}

impl ChannelMixer {
    pub fn new(source: ChannelLayout, dest: ChannelLayout, opts: MixOptions) -> Self {
        let matrix = MixMatrix::build(&source, &dest, opts);
        let in_channels = matrix.in_channels();
        let out_channels = matrix.out_channels();

        info!(
            "ChannelMixer: {} -> {} channels (identity={})",
            in_channels,
            out_channels,
            matrix.is_identity()
        );

        Self {
            matrix,
            in_channels,
            out_channels,
            output_planes: (0..out_channels).map(|_| Vec::new()).collect(),
            in_frame: vec![0.0; in_channels],
            out_frame: vec![0.0; out_channels],
        }
    }

    pub fn needs_mixing(&self) -> bool {
        !self.matrix.is_identity()
    }

    #[cfg(test)]
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    pub fn reset(&mut self) {
        for plane in &mut self.output_planes {
            plane.clear();
        }
    }

    /// Pre-size the output planes so steady-state `process` calls never allocate. Call once at
    /// pipeline setup with the largest frame count a cycle can produce.
    pub fn ensure_output_capacity(&mut self, frames: usize) {
        for plane in &mut self.output_planes {
            if plane.capacity() < frames {
                plane.reserve(frames - plane.len());
            }
        }
    }

    /// Mix planar per-channel input planes and write the mixed planes to `output`.
    ///
    /// `input` is expected to carry `in_channels` planes of equal length (the number of
    /// frames). Missing planes or samples are treated as silence. Returns the number of
    /// frames written.
    pub fn process(&mut self, input: &[Vec<f64>], output: &mut ChannelProducers<f64>) -> usize {
        let frames = (0..self.in_channels)
            .filter_map(|ch| input.get(ch).map(Vec::len))
            .min()
            .unwrap_or(0);
        if frames == 0 {
            return 0;
        }

        for plane in &mut self.output_planes {
            plane.clear();
            plane.reserve(frames);
        }

        for i in 0..frames {
            for ch in 0..self.in_channels {
                self.in_frame[ch] = input.get(ch).map(|p| p[i]).unwrap_or(0.0);
            }
            self.matrix.mix_frame(&self.in_frame, &mut self.out_frame);
            for (ch, &s) in self.out_frame.iter().enumerate() {
                self.output_planes[ch].push(s);
            }
        }

        let slices: smallvec::SmallVec<[&[f64]; 8]> =
            self.output_planes.iter().map(|p| p.as_slice()).collect();
        // the mixer always emits `out_channels` planes to match the producers, so a mismatch is a
        // bug rather than a stream change — just log it.
        if let Err(mismatch) = output.write_slices(&slices) {
            tracing::warn!("mixer output channel mismatch: {mismatch:?}; dropping frames");
        }

        frames
    }
}

/// Classify labels into the most specific layout without losing channel count.
pub fn layout_from_labels(labels: Vec<ChannelLabel>) -> ChannelLayout {
    if labels.is_empty() {
        return ChannelLayout::None;
    }

    let mut pos = ChannelPosition::empty();
    let mut all_positioned = true;
    for label in &labels {
        match label {
            ChannelLabel::Positioned(p) => {
                if p.bits().count_ones() == 1 && !pos.contains(*p) {
                    pos |= *p;
                } else {
                    all_positioned = false;
                    break;
                }
            }
            ChannelLabel::Discrete(_) => {
                all_positioned = false;
                break;
            }
        }
    }
    if all_positioned {
        ChannelLayout::Positioned(pos)
    } else {
        ChannelLayout::Custom(labels.into_boxed_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matrix_when_layouts_match() {
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&stereo, &stereo, MixOptions::default());
        assert!(m.is_identity());
        assert_eq!(m.in_channels(), 2);
        assert_eq!(m.out_channels(), 2);
    }

    #[test]
    fn mono_to_stereo_duplicates() {
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&mono, &stereo, MixOptions::default());
        let rows = m.rows();
        // L = FC, R = FC
        assert!((rows[0][0] - 1.0).abs() < 1e-9);
        assert!((rows[1][0] - 1.0).abs() < 1e-9);
        assert_eq!(m.in_channels(), 1);
        assert_eq!(m.out_channels(), 2);

        let out = mix_frame(&m, &[0.5]);
        assert!((out[0] - 0.5).abs() < 1e-9);
        assert!((out[1] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mono_reported_as_front_left_still_duplicates_to_stereo() {
        // Some decoders report a mono source as FRONT_LEFT (the first WAVE position bit
        // from Position::from_count(1)) rather than FRONT_CENTER. The mixer must
        // normalize this so mono plays in both ears, not just the left.
        let mono_as_fl = ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT);
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&mono_as_fl, &stereo, MixOptions::default());
        let rows = m.rows();
        assert_eq!(m.in_channels(), 1);
        assert_eq!(m.out_channels(), 2);
        // Both L and R should get the full signal.
        assert!(
            (rows[0][0] - 1.0).abs() < 1e-9,
            "L coefficient was {}",
            rows[0][0]
        );
        assert!(
            (rows[1][0] - 1.0).abs() < 1e-9,
            "R coefficient was {}",
            rows[1][0]
        );

        let out = mix_frame(&m, &[0.7]);
        assert!((out[0] - 0.7).abs() < 1e-9);
        assert!((out[1] - 0.7).abs() < 1e-9);
    }

    #[test]
    fn stereo_to_mono_averages() {
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let m = MixMatrix::build(&stereo, &mono, MixOptions::default());
        let rows = m.rows();
        // FC = 0.5*FL + 0.5*FR (front pair folds into center at full, then divided)
        assert!((rows[0][0] - 0.5).abs() < 1e-9);
        assert!((rows[0][1] - 0.5).abs() < 1e-9);

        let out = mix_frame(&m, &[0.8, -0.4]);
        assert!((out[0] - 0.2).abs() < 1e-9);
    }

    #[test]
    fn five_point_one_to_stereo_atsc() {
        let src = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT,
        );
        let dst =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&src, &dst, MixOptions::default());
        let rows = m.rows();

        // Input order (ascending bit): FL, FR, FC, LFE, RL, RR
        // L = FL + a*FC + a*RL            (LFE dropped by default)
        // R = FR + a*FC + a*RR
        // then the whole matrix is scaled by n = 1/(1 + 2a) so the rows sum to 1.0
        let a = SURROUND_ATTENUATION;
        let n = 1.0 / (1.0 + 2.0 * a);
        assert!((rows[0][0] - n).abs() < 1e-9); // FL
        assert!((rows[0][1] - 0.0).abs() < 1e-9); // FR
        assert!((rows[0][2] - a * n).abs() < 1e-9); // FC
        assert!((rows[0][3] - 0.0).abs() < 1e-9); // LFE
        assert!((rows[0][4] - a * n).abs() < 1e-9); // RL
        assert!((rows[0][5] - 0.0).abs() < 1e-9); // RR

        assert!((rows[1][0] - 0.0).abs() < 1e-9);
        assert!((rows[1][1] - n).abs() < 1e-9);
        assert!((rows[1][2] - a * n).abs() < 1e-9);
        assert!((rows[1][3] - 0.0).abs() < 1e-9);
        assert!((rows[1][4] - 0.0).abs() < 1e-9);
        assert!((rows[1][5] - a * n).abs() < 1e-9);
    }

    #[test]
    fn five_point_one_to_stereo_with_lfe() {
        let src = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT,
        );
        let dst =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let opts = MixOptions {
            lfe_to_stereo: true,
            ..MixOptions::default()
        };
        let m = MixMatrix::build(&src, &dst, opts);
        let rows = m.rows();
        let a = SURROUND_ATTENUATION;
        // LFE is index 3 in input order; it keeps the attenuation ratio relative to FL after
        // normalization.
        assert!((rows[0][3] / rows[0][0] - a).abs() < 1e-9);
        assert!((rows[1][3] / rows[1][1] - a).abs() < 1e-9);
    }

    #[test]
    fn seven_point_one_to_five_point_one_folds_sides() {
        // 7.1: FL FR FC LFE RL RR SL SR
        let src = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT
                | ChannelPosition::SIDE_LEFT
                | ChannelPosition::SIDE_RIGHT,
        );
        // 5.1: FL FR FC LFE RL RR
        let dst = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT,
        );
        let m = MixMatrix::build(&src, &dst, MixOptions::default());
        let rows = m.rows();
        let a = SURROUND_ATTENUATION;
        // Output order: FL FR FC LFE RL RR
        // Input order: FL FR FC LFE RL RR SL SR
        // The RL/RR rows sum to 1 + a, so the matrix is normalized by n = 1/(1 + a).
        let n = 1.0 / (1.0 + a);
        // SL (index 6) folds into RL (output index 4) at a.
        assert!((rows[4][4] - n).abs() < 1e-9); // RL identity
        assert!((rows[4][6] - a * n).abs() < 1e-9); // SL -> RL
        // SR (index 7) folds into RR (output index 5) at a.
        assert!((rows[5][5] - n).abs() < 1e-9);
        assert!((rows[5][7] - a * n).abs() < 1e-9);
    }

    #[test]
    fn mono_to_seven_point_one_routes_to_center() {
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let seven1 = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT
                | ChannelPosition::SIDE_LEFT
                | ChannelPosition::SIDE_RIGHT,
        );
        let m = MixMatrix::build(&mono, &seven1, MixOptions::default());
        let rows = m.rows();
        // Input: FC. Output order: FL FR FC LFE RL RR SL SR
        assert!((rows[2][0] - 1.0).abs() < 1e-9); // FC gets the mono signal
        // Everything else silent.
        for i in [0, 1, 3, 4, 5, 6, 7] {
            assert!((rows[i][0] - 0.0).abs() < 1e-9);
        }
    }

    #[test]
    fn stereo_to_seven_point_one_passes_through_front_pair() {
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let seven1 = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT
                | ChannelPosition::SIDE_LEFT
                | ChannelPosition::SIDE_RIGHT,
        );
        let m = MixMatrix::build(&stereo, &seven1, MixOptions::default());
        let rows = m.rows();
        // Output order: FL FR FC LFE RL RR SL SR. Input: FL FR.
        assert!((rows[0][0] - 1.0).abs() < 1e-9);
        assert!((rows[1][1] - 1.0).abs() < 1e-9);
        for i in [2, 3, 4, 5, 6, 7] {
            assert!((rows[i][0] - 0.0).abs() < 1e-9);
            assert!((rows[i][1] - 0.0).abs() < 1e-9);
        }
    }

    #[test]
    fn count_based_mono_stereo_fallback() {
        let m = build_count_based(1, 2, MixOptions::default());
        assert!((m.rows()[0][0] - 1.0).abs() < 1e-9);
        assert!((m.rows()[1][0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn count_based_stereo_mono_fallback() {
        let m = build_count_based(2, 1, MixOptions::default());
        assert!((m.rows()[0][0] - 0.5).abs() < 1e-9);
        assert!((m.rows()[0][1] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn count_based_identity() {
        let m = build_count_based(6, 6, MixOptions::default());
        assert!(m.is_identity());
    }

    #[test]
    fn count_based_zero_channels_does_not_underflow() {
        let no_output = build_count_based(2, 0, MixOptions::default());
        assert_eq!(no_output.in_channels(), 2);
        assert_eq!(no_output.out_channels(), 0);

        let no_input = build_count_based(0, 2, MixOptions::default());
        assert_eq!(no_input.in_channels(), 0);
        assert_eq!(no_input.out_channels(), 2);
    }

    #[test]
    fn normalization_prevents_clipping() {
        // 5.1 -> Stereo: L = FL + a*FC + a*RL sums to 1 + 2a ≈ 2.414, so the matrix is scaled
        // down to keep a full-scale input at exactly 1.0 instead of hard-clipping.
        let src = ChannelLayout::Positioned(
            ChannelPosition::FRONT_LEFT
                | ChannelPosition::FRONT_RIGHT
                | ChannelPosition::FRONT_CENTER
                | ChannelPosition::LFE1
                | ChannelPosition::REAR_LEFT
                | ChannelPosition::REAR_RIGHT,
        );
        let dst =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&src, &dst, MixOptions::default());
        // Input order: FL, FR, FC, LFE, RL, RR
        let out = mix_frame(&m, &[1.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
        assert!(
            out[0] <= 1.0 && (out[0] - 1.0).abs() < 1e-9,
            "L was {}",
            out[0]
        );
        assert!(
            out[1] <= 1.0 && (out[1] - 1.0).abs() < 1e-9,
            "R was {}",
            out[1]
        );

        // quieter content is attenuated by the same factor, not distorted: the front pair keeps
        // its balance against the folded channels
        let a = SURROUND_ATTENUATION;
        let n = 1.0 / (1.0 + 2.0 * a);
        let quiet = mix_frame(&m, &[0.5, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!((quiet[0] - 0.5 * n).abs() < 1e-9, "L was {}", quiet[0]);
    }

    #[test]
    fn normalization_skips_matrices_that_cannot_clip() {
        // mono -> stereo duplicates at full gain; every row already sums to 1.0 so the matrix
        // must be left untouched
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let m = MixMatrix::build(&mono, &stereo, MixOptions::default());
        assert!((m.rows()[0][0] - 1.0).abs() < 1e-9);
        assert!((m.rows()[1][0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn custom_layout_falls_back_to_count() {
        let src = ChannelLayout::Custom(Box::new([
            ChannelLabel::Discrete(0),
            ChannelLabel::Discrete(1),
        ]));
        let dst = ChannelLayout::from_count_canonical(1);
        let m = MixMatrix::build(&src, &dst, MixOptions::default());

        assert!((m.rows()[0][0] - 0.5).abs() < 1e-9);
        assert!((m.rows()[0][1] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn layout_from_labels_keeps_duplicate_positions_custom() {
        let layout = layout_from_labels(vec![
            ChannelLabel::Positioned(ChannelPosition::FRONT_LEFT),
            ChannelLabel::Positioned(ChannelPosition::FRONT_LEFT),
        ]);

        assert!(matches!(layout, ChannelLayout::Custom(_)));
        assert_eq!(layout.count(), 2);
    }

    #[test]
    fn position_index_works() {
        let mask = ChannelPosition::FRONT_LEFT
            | ChannelPosition::FRONT_RIGHT
            | ChannelPosition::FRONT_CENTER;
        assert_eq!(position_index(mask, ChannelPosition::FRONT_LEFT), Some(0));
        assert_eq!(position_index(mask, ChannelPosition::FRONT_RIGHT), Some(1));
        assert_eq!(position_index(mask, ChannelPosition::FRONT_CENTER), Some(2));
        assert_eq!(position_index(mask, ChannelPosition::LFE1), None);
    }

    fn mix_frame(m: &MixMatrix, input: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0; m.out_channels()];
        m.mix_frame(input, &mut out);
        out
    }

    fn run_process(mixer: &mut ChannelMixer, input: Vec<Vec<f64>>) -> Vec<Vec<f64>> {
        use crate::media::pipeline::ChannelBuffers;

        let frames = input.first().map(|v| v.len()).unwrap_or(0);
        let (mut producers, mut consumers) =
            ChannelBuffers::<f64>::new(mixer.out_channels(), frames.max(1) * 2).split();

        let written = mixer.process(&input, &mut producers);
        assert_eq!(written, frames);

        let read = consumers.try_read_to_staging(frames);
        assert_eq!(read, frames);
        consumers.staging().iter().map(|s| s.to_vec()).collect()
    }

    #[test]
    fn process_mono_to_stereo_duplicates_plane() {
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let mut mixer = ChannelMixer::new(mono, stereo, MixOptions::default());
        assert!(mixer.needs_mixing());

        let out = run_process(&mut mixer, vec![vec![0.1, -0.2, 0.3]]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vec![0.1, -0.2, 0.3]);
        assert_eq!(out[1], vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn process_stereo_to_mono_averages() {
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let mut mixer = ChannelMixer::new(stereo, mono, MixOptions::default());

        let out = run_process(&mut mixer, vec![vec![0.8, 0.0], vec![-0.4, 1.0]]);
        assert_eq!(out.len(), 1);
        assert!((out[0][0] - 0.2).abs() < 1e-9);
        assert!((out[0][1] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn process_empty_input_writes_nothing() {
        let stereo =
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT);
        let mono = ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER);
        let mut mixer = ChannelMixer::new(stereo, mono, MixOptions::default());

        use crate::media::pipeline::ChannelBuffers;
        let (mut producers, _consumers) = ChannelBuffers::<f64>::new(1, 8).split();
        assert_eq!(mixer.process(&[vec![], vec![]], &mut producers), 0);
    }
}

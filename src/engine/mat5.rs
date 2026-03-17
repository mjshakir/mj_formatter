use serde::{Deserialize, Serialize};

pub const N: usize = 5;
pub type Mat5 = [[f64; N]; N];

// ── Vector helpers ──────────────────────────────────────────────────────────

#[inline(always)]
pub fn vec5_sub(a: &[f64; N], b: &[f64; N]) -> [f64; N] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3], a[4] - b[4]]
}

#[inline(always)]
pub fn vec5_add_clamp(a: &[f64; N], b: &[f64; N]) -> [f64; N] {
    [
        (a[0] + b[0]).clamp(0.0, 1.0),
        (a[1] + b[1]).clamp(0.0, 1.0),
        (a[2] + b[2]).clamp(0.0, 1.0),
        (a[3] + b[3]).clamp(0.0, 1.0),
        (a[4] + b[4]).clamp(0.0, 1.0),
    ]
}

// ── NEON-accelerated dot product ────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub fn dot5(a: &[f64; N], b: &[f64; N]) -> f64 {
    unsafe {
        use core::arch::aarch64::*;
        let a01 = vld1q_f64(a.as_ptr());
        let b01 = vld1q_f64(b.as_ptr());
        let a23 = vld1q_f64(a.as_ptr().add(2));
        let b23 = vld1q_f64(b.as_ptr().add(2));
        let prod01 = vmulq_f64(a01, b01);
        let prod23 = vmulq_f64(a23, b23);
        let sum4 = vaddq_f64(prod01, prod23);
        vgetq_lane_f64(sum4, 0) + vgetq_lane_f64(sum4, 1) + a[4] * b[4]
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[inline(always)]
pub fn dot5(a: &[f64; N], b: &[f64; N]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3] + a[4] * b[4]
}

// ── Matrix constructors ────────────────────────────────────────────────────

#[inline]
pub fn mat5_zeros() -> Mat5 {
    [[0.0; N]; N]
}

#[inline]
pub fn mat5_identity() -> Mat5 {
    let mut m = [[0.0; N]; N];
    for i in 0..N {
        m[i][i] = 1.0;
    }
    m
}

#[inline]
pub fn mat5_diagonal(diag: &[f64; N]) -> Mat5 {
    let mut m = [[0.0; N]; N];
    for i in 0..N {
        m[i][i] = diag[i];
    }
    m
}

// ── Element-wise arithmetic ────────────────────────────────────────────────

pub fn mat5_add(a: &Mat5, b: &Mat5) -> Mat5 {
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = a[i][j] + b[i][j];
        }
    }
    r
}

pub fn mat5_sub(a: &Mat5, b: &Mat5) -> Mat5 {
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = a[i][j] - b[i][j];
        }
    }
    r
}

pub fn mat5_scale(a: &Mat5, s: f64) -> Mat5 {
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = a[i][j] * s;
        }
    }
    r
}

// ── Matrix multiplication ──────────────────────────────────────────────────

pub fn mat5_mul(a: &Mat5, b: &Mat5) -> Mat5 {
    let bt = mat5_transpose(b);
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = dot5(&a[i], &bt[j]);
        }
    }
    r
}

pub fn mat5_transpose(a: &Mat5) -> Mat5 {
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = a[j][i];
        }
    }
    r
}

// ── Matrix-vector product ──────────────────────────────────────────────────

#[inline]
pub fn mat5_matvec(a: &Mat5, v: &[f64; N]) -> [f64; N] {
    [
        dot5(&a[0], v),
        dot5(&a[1], v),
        dot5(&a[2], v),
        dot5(&a[3], v),
        dot5(&a[4], v),
    ]
}

// ── Outer product ──────────────────────────────────────────────────────────

pub fn mat5_outer(v: &[f64; N]) -> Mat5 {
    let mut r = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            r[i][j] = v[i] * v[j];
        }
    }
    r
}

// ── Quadratic form: v^T * M * v ────────────────────────────────────────────

#[inline]
pub fn mat5_quadratic(m: &Mat5, v: &[f64; N]) -> f64 {
    let mv = mat5_matvec(m, v);
    dot5(&mv, v)
}

// ── Diagonal extraction ────────────────────────────────────────────────────

#[inline]
pub fn mat5_diag(m: &Mat5) -> [f64; N] {
    [m[0][0], m[1][1], m[2][2], m[3][3], m[4][4]]
}

// ── Cholesky decomposition (lower-triangular L where A = L*L^T) ────────────

pub fn mat5_cholesky(a: &Mat5) -> Option<Mat5> {
    let mut l = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..=i {
            let mut sum = 0.0;
            if j == i {
                for k in 0..j {
                    sum += l[j][k] * l[j][k];
                }
                let val = a[j][j] - sum;
                if val <= 0.0 {
                    return None;
                }
                l[j][j] = val.sqrt();
            } else {
                for k in 0..j {
                    sum += l[i][k] * l[j][k];
                }
                if l[j][j].abs() < 1e-15 {
                    return None;
                }
                l[i][j] = (a[i][j] - sum) / l[j][j];
            }
        }
    }
    Some(l)
}

// ── Determinant via Cholesky: det(A) = prod(L[i][i])^2 ─────────────────────

pub fn mat5_determinant_spd(a: &Mat5) -> f64 {
    match mat5_cholesky(a) {
        Some(l) => {
            let mut prod = 1.0;
            for i in 0..N {
                prod *= l[i][i];
            }
            prod * prod
        }
        None => 0.0,
    }
}

// ── Inverse via Cholesky: solve L*L^T * X = I ──────────────────────────────

pub fn mat5_inverse_spd(a: &Mat5) -> Option<Mat5> {
    let l = mat5_cholesky(a)?;

    // Forward solve: L * Y = I → Y = L^{-1}
    let mut l_inv = [[0.0; N]; N];
    for i in 0..N {
        l_inv[i][i] = 1.0 / l[i][i];
        for j in (0..i).rev() {
            let mut sum = 0.0;
            for k in j..i {
                sum += l[i][k] * l_inv[k][j];
            }
            l_inv[i][j] = -sum / l[i][i];
        }
    }

    // A^{-1} = L^{-T} * L^{-1} = (L^{-1})^T * L^{-1}
    let l_inv_t = mat5_transpose(&l_inv);
    Some(mat5_mul(&l_inv_t, &l_inv))
}

// ── Symmetrize (average upper and lower triangles) ─────────────────────────

pub fn mat5_symmetrize(a: &Mat5) -> Mat5 {
    let mut r = *a;
    for i in 0..N {
        for j in (i + 1)..N {
            let avg = 0.5 * (r[i][j] + r[j][i]);
            r[i][j] = avg;
            r[j][i] = avg;
        }
    }
    r
}

// ── Enforce SPD: if Cholesky fails, fall back to diagonal + regularization ─

pub fn enforce_spd(m: &mut Mat5) {
    *m = mat5_symmetrize(m);
    if mat5_cholesky(m).is_some() {
        return;
    }
    // Add increasing regularization until SPD
    for reg in &[1e-6, 1e-5, 1e-4, 1e-3, 1e-2] {
        for i in 0..N {
            m[i][i] += reg;
        }
        if mat5_cholesky(m).is_some() {
            return;
        }
    }
    // Last resort: replace with diagonal
    let diag = mat5_diag(m);
    *m = mat5_diagonal(&[
        diag[0].max(1e-4),
        diag[1].max(1e-4),
        diag[2].max(1e-4),
        diag[3].max(1e-4),
        diag[4].max(1e-4),
    ]);
}

// ── Serde wrapper for Mat5 (needed because [[f64;5];5] doesn't impl Serialize) ─

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerializableMat5(pub [[f64; N]; N]);

impl From<Mat5> for SerializableMat5 {
    fn from(m: Mat5) -> Self {
        Self(m)
    }
}

impl From<SerializableMat5> for Mat5 {
    fn from(s: SerializableMat5) -> Self {
        s.0
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-10;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    fn mat_approx_eq(a: &Mat5, b: &Mat5) -> bool {
        for i in 0..N {
            for j in 0..N {
                if !approx_eq(a[i][j], b[i][j]) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn identity_properties() {
        let id = mat5_identity();
        assert!(approx_eq(mat5_determinant_spd(&id), 1.0));
        let inv = mat5_inverse_spd(&id).unwrap();
        assert!(mat_approx_eq(&id, &inv));
    }

    #[test]
    fn diagonal_determinant() {
        let d = mat5_diagonal(&[2.0, 3.0, 4.0, 5.0, 6.0]);
        assert!(approx_eq(mat5_determinant_spd(&d), 720.0));
    }

    #[test]
    fn diagonal_inverse() {
        let d = mat5_diagonal(&[2.0, 4.0, 5.0, 8.0, 10.0]);
        let inv = mat5_inverse_spd(&d).unwrap();
        let product = mat5_mul(&d, &inv);
        assert!(mat_approx_eq(&product, &mat5_identity()));
    }

    #[test]
    fn cholesky_roundtrip() {
        // Build a known SPD matrix: A = L * L^T
        let mut l = mat5_zeros();
        l[0][0] = 2.0;
        l[1][0] = 0.5;
        l[1][1] = 1.5;
        l[2][0] = 0.3;
        l[2][1] = 0.2;
        l[2][2] = 1.0;
        l[3][0] = 0.1;
        l[3][1] = 0.4;
        l[3][2] = 0.15;
        l[3][3] = 0.8;
        l[4][0] = 0.2;
        l[4][1] = 0.1;
        l[4][2] = 0.05;
        l[4][3] = 0.3;
        l[4][4] = 1.2;
        let lt = mat5_transpose(&l);
        let a = mat5_mul(&l, &lt);
        let l_computed = mat5_cholesky(&a).expect("cholesky should succeed");
        assert!(mat_approx_eq(&l, &l_computed));
    }

    #[test]
    fn inverse_times_original_is_identity() {
        // SPD matrix
        let mut a = mat5_diagonal(&[4.0, 3.0, 2.0, 5.0, 1.0]);
        a[0][1] = 0.5;
        a[1][0] = 0.5;
        a[0][2] = 0.3;
        a[2][0] = 0.3;
        a[1][2] = 0.2;
        a[2][1] = 0.2;
        a[3][4] = 0.1;
        a[4][3] = 0.1;
        let inv = mat5_inverse_spd(&a).unwrap();
        let product = mat5_mul(&a, &inv);
        for i in 0..N {
            for j in 0..N {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (product[i][j] - expected).abs() < 1e-8,
                    "product[{}][{}] = {}, expected {}",
                    i, j, product[i][j], expected
                );
            }
        }
    }

    #[test]
    fn quadratic_form_matches_manual() {
        let m = mat5_identity();
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert!(approx_eq(mat5_quadratic(&m, &v), 55.0));
    }

    #[test]
    fn outer_product_is_symmetric() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let outer = mat5_outer(&v);
        for i in 0..N {
            for j in 0..N {
                assert!(approx_eq(outer[i][j], outer[j][i]));
                assert!(approx_eq(outer[i][j], v[i] * v[j]));
            }
        }
    }

    #[test]
    fn dot5_matches_manual() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        let b = [5.0, 4.0, 3.0, 2.0, 1.0];
        assert!(approx_eq(dot5(&a, &b), 35.0));
    }

    #[test]
    fn matvec_identity() {
        let id = mat5_identity();
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let result = mat5_matvec(&id, &v);
        for i in 0..N {
            assert!(approx_eq(result[i], v[i]));
        }
    }

    #[test]
    fn enforce_spd_recovers_non_spd() {
        let mut m = mat5_diagonal(&[-0.01, 0.5, 0.3, 0.2, 0.1]);
        enforce_spd(&mut m);
        assert!(mat5_cholesky(&m).is_some(), "enforce_spd must produce SPD matrix");
    }

    #[test]
    fn enforce_spd_preserves_valid_spd() {
        let original = mat5_diagonal(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mut m = original;
        enforce_spd(&mut m);
        assert!(mat_approx_eq(&m, &original));
    }

    #[test]
    fn symmetrize_works() {
        let mut a = mat5_zeros();
        a[0][1] = 2.0;
        a[1][0] = 4.0;
        let s = mat5_symmetrize(&a);
        assert!(approx_eq(s[0][1], 3.0));
        assert!(approx_eq(s[1][0], 3.0));
    }

    #[test]
    fn mul_associativity() {
        let a = mat5_diagonal(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let b = mat5_diagonal(&[5.0, 4.0, 3.0, 2.0, 1.0]);
        let ab = mat5_mul(&a, &b);
        let expected = mat5_diagonal(&[5.0, 8.0, 9.0, 8.0, 5.0]);
        assert!(mat_approx_eq(&ab, &expected));
    }

    #[test]
    fn add_sub_inverse() {
        let a = mat5_diagonal(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let b = mat5_diagonal(&[0.5, 0.5, 0.5, 0.5, 0.5]);
        let sum = mat5_add(&a, &b);
        let diff = mat5_sub(&sum, &b);
        assert!(mat_approx_eq(&diff, &a));
    }

    #[test]
    fn cholesky_fails_on_non_spd() {
        let m = mat5_diagonal(&[-1.0, 1.0, 1.0, 1.0, 1.0]);
        assert!(mat5_cholesky(&m).is_none());
    }

    #[test]
    fn scale_works() {
        let a = mat5_diagonal(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let scaled = mat5_scale(&a, 2.0);
        let expected = mat5_diagonal(&[2.0, 4.0, 6.0, 8.0, 10.0]);
        assert!(mat_approx_eq(&scaled, &expected));
    }

    #[test]
    fn neon_dot5_matches_scalar() {
        let a = [0.123, 0.456, 0.789, 0.321, 0.654];
        let b = [0.987, 0.654, 0.321, 0.789, 0.456];
        let result = dot5(&a, &b);
        let expected = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3] + a[4] * b[4];
        assert!(
            (result - expected).abs() < 1e-14,
            "dot5={}, expected={}",
            result, expected
        );
    }

    #[test]
    fn dense_spd_inverse() {
        // Build a dense SPD matrix via A = B^T * B + epsilon * I
        let b = [
            [1.0, 0.5, 0.3, 0.2, 0.1],
            [0.5, 2.0, 0.4, 0.3, 0.2],
            [0.3, 0.4, 1.5, 0.25, 0.15],
            [0.2, 0.3, 0.25, 1.8, 0.35],
            [0.1, 0.2, 0.15, 0.35, 1.2],
        ];
        let bt = mat5_transpose(&b);
        let mut a = mat5_mul(&bt, &b);
        for i in 0..N {
            a[i][i] += 0.01;
        }
        let inv = mat5_inverse_spd(&a).expect("dense SPD inverse should succeed");
        let product = mat5_mul(&a, &inv);
        for i in 0..N {
            for j in 0..N {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (product[i][j] - expected).abs() < 1e-6,
                    "dense: product[{}][{}] = {}, expected {}",
                    i, j, product[i][j], expected
                );
            }
        }
    }

    #[test]
    fn joseph_form_simulation() {
        // Simulate one Kalman update step with Joseph form
        let p = mat5_diagonal(&[0.1, 0.1, 0.1, 0.1, 0.1]);
        let q = mat5_diagonal(&[0.001, 0.001, 0.001, 0.001, 0.001]);
        let r = mat5_diagonal(&[0.05, 0.05, 0.05, 0.05, 0.05]);

        let p_pred = mat5_add(&p, &q);
        let s = mat5_add(&p_pred, &r);
        let s_inv = mat5_inverse_spd(&s).unwrap();
        let k = mat5_mul(&p_pred, &s_inv);
        let i_k = mat5_sub(&mat5_identity(), &k);
        let p_upd = mat5_add(
            &mat5_mul(&i_k, &mat5_mul(&p_pred, &mat5_transpose(&i_k))),
            &mat5_mul(&k, &mat5_mul(&r, &mat5_transpose(&k))),
        );

        // Updated covariance should be SPD and smaller than predicted
        assert!(mat5_cholesky(&p_upd).is_some(), "Joseph form must produce SPD");
        for i in 0..N {
            assert!(
                p_upd[i][i] < p_pred[i][i],
                "Updated variance should be less than predicted"
            );
            assert!(p_upd[i][i] > 0.0, "Updated variance must be positive");
        }
    }
}

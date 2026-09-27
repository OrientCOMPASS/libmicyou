/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Numeric kernels for the MCYI VM.
//!
//! Every kernel is a mechanical transcription of the corresponding numpy
//! reference in `tools/onnx-port/onnxref/replay.py` (which is itself
//! validated bit-approximately against onnxruntime). All arithmetic is
//! f32; rounding points mirror the reference so streaming goldens stay
//! within ~1e-5.
//!
//! # Safety
//!
//! All kernels take raw pointers. Callers (the VM `exec` loop) guarantee:
//! * every pointer is valid for `numel(shape)` f32 elements (slot bounds
//!   are validated at parse);
//! * output regions never overlap input regions of the same op (compiler
//!   live-range packing + debug-build validation).

/// Element-wise binary operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

const MAX_NDIM: usize = 6;

#[inline]
fn numel(shape: &[u32]) -> usize {
    shape.iter().fold(1usize, |a, &d| a * d as usize)
}

#[inline]
fn row_major_strides(shape: &[u32]) -> [usize; MAX_NDIM] {
    let mut st = [0usize; MAX_NDIM];
    let n = shape.len();
    if n == 0 {
        return st;
    }
    st[n - 1] = 1;
    for k in (0..n - 1).rev() {
        st[k] = st[k + 1] * shape[k + 1] as usize;
    }
    st
}

/// Map own-shape strides into the output axis space (right-aligned), using
/// stride 0 for broadcast (size-1 or missing) axes.
fn broadcast_strides(ish: &[u32], osh: &[u32]) -> [usize; MAX_NDIM] {
    let own = row_major_strides(ish);
    let mut out = [0usize; MAX_NDIM];
    let n = osh.len();
    debug_assert!(ish.len() <= n);
    let pad = n - ish.len();
    for k in 0..n {
        if k < pad {
            out[k] = 0; // axis absent in input → broadcast
        } else {
            let ik = k - pad;
            out[k] = if ish[ik] == 1 { 0 } else { own[ik] };
        }
    }
    out
}

#[inline(always)]
fn apply_bin(op: BinOp, x: f32, y: f32) -> f32 {
    match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
        BinOp::Pow => {
            // fast path: the models only ever square (correctly-rounded
            // powf(x, 2) is bit-identical to a single multiply)
            if y == 2.0 {
                x * x
            } else {
                x.powf(y)
            }
        }
    }
}

/// NumPy-style broadcasting binary op. `osh` is the (statically known)
/// broadcast result shape.
///
/// # Safety
/// See module docs.
pub unsafe fn binary(
    op: BinOp,
    a: *const f32,
    ash: &[u32],
    b: *const f32,
    bsh: &[u32],
    o: *mut f32,
    osh: &[u32],
) {
    debug_assert!(osh.len() <= MAX_NDIM);
    let an = numel(ash);
    let bn = numel(bsh);
    let on = numel(osh);
    let n = osh.len();

    // fast path: identical shapes → flat zip
    if an == on && bn == on && ash == osh && bsh == osh {
        for i in 0..on {
            *o.add(i) = apply_bin(op, *a.add(i), *b.add(i));
        }
        return;
    }
    // scalar fast paths
    if bn == 1 && an == on && ash == osh {
        let bv = *b;
        for i in 0..on {
            *o.add(i) = apply_bin(op, *a.add(i), bv);
        }
        return;
    }
    if an == 1 && bn == on && bsh == osh {
        let av = *a;
        for i in 0..on {
            *o.add(i) = apply_bin(op, av, *b.add(i));
        }
        return;
    }

    if n == 0 {
        *o = apply_bin(op, *a, *b);
        return;
    }

    let astr = broadcast_strides(ash, osh);
    let bstr = broadcast_strides(bsh, osh);
    let inner = osh[n - 1] as usize;
    let outer = if inner == 0 { 0 } else { on / inner };
    let alast = astr[n - 1];
    let blast = bstr[n - 1];
    debug_assert!(inner == 0 || alast <= 1);
    debug_assert!(inner == 0 || blast <= 1);

    let mut coords = [0usize; MAX_NDIM];
    for oi in 0..outer {
        let mut aoff = 0usize;
        let mut boff = 0usize;
        for k in 0..n - 1 {
            aoff += coords[k] * astr[k];
            boff += coords[k] * bstr[k];
        }
        let dst = o.add(oi * inner);
        match (alast, blast) {
            (1, 1) => {
                let sa = a.add(aoff);
                let sb = b.add(boff);
                for j in 0..inner {
                    *dst.add(j) = apply_bin(op, *sa.add(j), *sb.add(j));
                }
            }
            (1, _) => {
                let sa = a.add(aoff);
                let bv = *b.add(boff);
                for j in 0..inner {
                    *dst.add(j) = apply_bin(op, *sa.add(j), bv);
                }
            }
            (_, 1) => {
                let av = *a.add(aoff);
                let sb = b.add(boff);
                for j in 0..inner {
                    *dst.add(j) = apply_bin(op, av, *sb.add(j));
                }
            }
            _ => {
                let av = *a.add(aoff);
                let bv = *b.add(boff);
                for j in 0..inner {
                    *dst.add(j) = apply_bin(op, av, bv);
                }
            }
        }
        // odometer over outer axes 0..n-2
        if n >= 2 {
            let mut k = n - 2;
            loop {
                coords[k] += 1;
                if coords[k] < osh[k] as usize {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
    }
}

/// SIGMOID (6) / SQRT (7) / LOG (8).
///
/// # Safety
/// See module docs.
pub unsafe fn unary(opcode: u16, x: *const f32, o: *mut f32, n: usize) {
    match opcode {
        6 => {
            for i in 0..n {
                let v = *x.add(i);
                *o.add(i) = 1.0 / (1.0 + (-v).exp());
            }
        }
        7 => {
            for i in 0..n {
                *o.add(i) = (*x.add(i)).sqrt();
            }
        }
        _ => {
            for i in 0..n {
                *o.add(i) = (*x.add(i)).ln();
            }
        }
    }
}

/// # Safety
/// See module docs.
pub unsafe fn clip(x: *const f32, o: *mut f32, n: usize, lo: Option<f32>, hi: Option<f32>) {
    for i in 0..n {
        let mut v = *x.add(i);
        if let Some(l) = lo {
            if v < l {
                v = l;
            }
        }
        if let Some(h) = hi {
            if v > h {
                v = h;
            }
        }
        *o.add(i) = v;
    }
}

/// Batched MatMul: `a [..., M, K] × b [..., K, N] → o [..., M, N]` with
/// numpy batch broadcasting (b is 2-D in both shipped models).
///
/// # Safety
/// See module docs.
pub unsafe fn matmul(
    a: *const f32,
    ash: &[u32],
    b: *const f32,
    bsh: &[u32],
    o: *mut f32,
    osh: &[u32],
) {
    debug_assert!(ash.len() >= 2 && bsh.len() >= 2 && osh.len() >= 2);
    debug_assert!(bsh.len() == 2 || bsh.len() == osh.len());
    let nb = osh.len() - 2;
    let m = osh[nb] as usize;
    let n = osh[nb + 1] as usize;
    let k = ash[ash.len() - 1] as usize;
    debug_assert_eq!(bsh[bsh.len() - 2] as usize, k);

    // Batch-axis strides must come from the FULL tensor layout (a batch
    // step skips the whole trailing [M,K] / [K,N] block), with numpy-style
    // right alignment and stride-0 broadcasting for size-1 batch dims.
    fn batch_strides(ish: &[u32], nb: usize) -> [usize; MAX_NDIM] {
        let full = row_major_strides(ish);
        let mut out = [0usize; MAX_NDIM];
        let inb = ish.len().saturating_sub(2);
        let pad = nb.saturating_sub(inb);
        for k in 0..nb {
            if k >= pad {
                let ik = k - pad;
                out[k] = if ish[ik] == 1 { 0 } else { full[ik] };
            }
        }
        out
    }
    let astr = batch_strides(ash, nb);
    let bstr = batch_strides(bsh, nb);

    let n_batches = numel(&osh[..nb]).max(1);
    let mut coords = [0usize; MAX_NDIM];
    for batch in 0..n_batches {
        let mut aoff = 0usize;
        let mut boff = 0usize;
        for d in 0..nb {
            aoff += coords[d] * astr[d];
            boff += coords[d] * bstr[d];
        }
        let abase = a.add(aoff); // [M, K]
        let bbase = b.add(boff); // [K, N]
        let obase = o.add(batch * m * n);
        for i in 0..m {
            let orow = obase.add(i * n);
            for j in 0..n {
                *orow.add(j) = 0.0;
            }
            let arow = abase.add(i * k);
            for kk in 0..k {
                let av = *arow.add(kk);
                let brow = bbase.add(kk * n);
                for j in 0..n {
                    *orow.add(j) += av * *brow.add(j);
                }
            }
        }
        if nb > 0 {
            let mut d = nb - 1;
            loop {
                coords[d] += 1;
                if coords[d] < osh[d] as usize {
                    break;
                }
                coords[d] = 0;
                if d == 0 {
                    break;
                }
                d -= 1;
            }
        }
    }
}

/// Conv2d / ConvTranspose2d attribute block (decoded by the VM from the
/// op's i64 attribute table). `pb`/`pr`/`opt_*` are carried for completeness —
/// output sizes come from the statically compiled output slot shapes.
#[allow(dead_code)]
pub struct ConvAttrs {
    pub kh: usize,
    pub kw: usize,
    pub sh: usize,
    pub sw: usize,
    pub dh: usize,
    pub dw: usize,
    pub pt: i32,
    pub pl: i32,
    pub pb: i32,
    pub pr: i32,
    pub group: usize,
    pub opt_h: usize,
    pub opt_w: usize,
}

/// NCHW Conv2d (`transpose == false`, weight `[M, C/g, kh, kw]`) or
/// ConvTranspose2d (`transpose == true`, weight `[C, M/g, kh, kw]`).
/// `scratch` is used for im2col staging (general path only).
///
/// # Safety
/// See module docs.
#[allow(clippy::too_many_arguments)]
pub unsafe fn conv(
    transpose: bool,
    x: *const f32,
    xsh: &[u32],
    w: *const f32,
    wsh: &[u32],
    bias: Option<*const f32>,
    o: *mut f32,
    osh: &[u32],
    at: &ConvAttrs,
    scratch: &mut Vec<f32>,
) {
    debug_assert_eq!(xsh.len(), 4);
    let nbatch = xsh[0] as usize;
    let c_in = xsh[1] as usize;
    let h_in = xsh[2] as usize;
    let w_in = xsh[3] as usize;
    let (kh, kw, sh, sw, dh, dw, group) = (at.kh, at.kw, at.sh, at.sw, at.dh, at.dw, at.group);
    let (pt, pl) = (at.pt as i64, at.pl as i64);
    let h_out = osh[2] as usize;
    let w_out = osh[3] as usize;
    let cpg = c_in / group;
    let hw_in = h_in * w_in;
    let hw_out = h_out * w_out;

    if !transpose {
        let m_out = wsh[0] as usize;
        let mpg = m_out / group;

        if kh == 1 && kw == 1 && sh == 1 && sw == 1 && dh == 1 && dw == 1 {
            // pointwise == GEMM per group: [mpg, cpg] × [cpg, HW]
            for nb in 0..nbatch {
                for gi in 0..group {
                    let xg = x.add(nb * c_in * hw_in + gi * cpg * hw_in);
                    let wg = w.add(gi * mpg * cpg);
                    let og = o.add(nb * m_out * hw_out + gi * mpg * hw_out);
                    for mi in 0..mpg {
                        let orow = og.add(mi * hw_out);
                        let bv = bias.map_or(0.0, |b| *b.add(gi * mpg + mi));
                        for p in 0..hw_out {
                            *orow.add(p) = bv;
                        }
                        for ci in 0..cpg {
                            let wv = *wg.add(mi * cpg + ci);
                            let xrow = xg.add(ci * hw_in);
                            for p in 0..hw_out {
                                *orow.add(p) += wv * *xrow.add(p);
                            }
                        }
                    }
                }
            }
            return;
        }

        if group == c_in && m_out == c_in {
            // depthwise: w [C, 1, kh, kw]
            for nb in 0..nbatch {
                for c in 0..c_in {
                    let xp = x.add((nb * c_in + c) * hw_in);
                    let wp = w.add(c * kh * kw);
                    let op_ = o.add((nb * c_in + c) * hw_out);
                    let bv = bias.map_or(0.0, |b| *b.add(c));
                    for oh in 0..h_out {
                        for ow in 0..w_out {
                            let mut acc = bv;
                            for i in 0..kh {
                                let ih = (oh * sh + i * dh) as i64 - pt;
                                if ih < 0 || ih >= h_in as i64 {
                                    continue;
                                }
                                for j in 0..kw {
                                    let iw = (ow * sw + j * dw) as i64 - pl;
                                    if iw < 0 || iw >= w_in as i64 {
                                        continue;
                                    }
                                    acc += *wp.add(i * kw + j)
                                        * *xp.add(ih as usize * w_in + iw as usize);
                                }
                            }
                            *op_.add(oh * w_out + ow) = acc;
                        }
                    }
                }
            }
            return;
        }

        // general grouped conv: im2col + GEMM
        let col_k = cpg * kh * kw;
        scratch.clear();
        scratch.resize(col_k * hw_out, 0.0);
        let colbase = scratch.as_mut_ptr();
        for nb in 0..nbatch {
            for gi in 0..group {
                let xg = x.add(nb * c_in * hw_in + gi * cpg * hw_in);
                for ci in 0..cpg {
                    for i in 0..kh {
                        for j in 0..kw {
                            let col_row = ci * kh * kw + i * kw + j;
                            for oh in 0..h_out {
                                let ih = (oh * sh + i * dh) as i64 - pt;
                                let row_ok = ih >= 0 && ih < h_in as i64;
                                for ow in 0..w_out {
                                    let iw = (ow * sw + j * dw) as i64 - pl;
                                    let v = if row_ok && iw >= 0 && iw < w_in as i64 {
                                        *xg.add(ci * hw_in + ih as usize * w_in + iw as usize)
                                    } else {
                                        0.0
                                    };
                                    *colbase.add(col_row * hw_out + oh * w_out + ow) = v;
                                }
                            }
                        }
                    }
                }
                let wg = w.add(gi * mpg * col_k);
                let og = o.add(nb * m_out * hw_out + gi * mpg * hw_out);
                for mi in 0..mpg {
                    let orow = og.add(mi * hw_out);
                    let bv = bias.map_or(0.0, |b| *b.add(gi * mpg + mi));
                    for p in 0..hw_out {
                        *orow.add(p) = bv;
                    }
                    for kk in 0..col_k {
                        let wv = *wg.add(mi * col_k + kk);
                        let crow = colbase.add(kk * hw_out);
                        for p in 0..hw_out {
                            *orow.add(p) += wv * *crow.add(p);
                        }
                    }
                }
            }
        }
        return;
    }

    // ── ConvTranspose: scatter-add ──
    let mpg = wsh[1] as usize;
    let m_out = mpg * group;
    debug_assert_eq!(osh[1] as usize, m_out);
    let on = numel(osh);
    for i in 0..on {
        *o.add(i) = 0.0;
    }
    for nb in 0..nbatch {
        for gi in 0..group {
            for ci in 0..cpg {
                let c = gi * cpg + ci;
                let xp = x.add(nb * c_in * hw_in + c * hw_in);
                let wp = w.add(c * mpg * kh * kw);
                for a in 0..h_in {
                    let oh_base = a as i64 * sh as i64 - pt;
                    for bidx in 0..w_in {
                        let xv = *xp.add(a * w_in + bidx);
                        let ow_base = bidx as i64 * sw as i64 - pl;
                        for i in 0..kh {
                            let oh = oh_base + i as i64 * dh as i64;
                            if oh < 0 || oh >= h_out as i64 {
                                continue;
                            }
                            for j in 0..kw {
                                let ow = ow_base + j as i64 * dw as i64;
                                if ow < 0 || ow >= w_out as i64 {
                                    continue;
                                }
                                let pos = (oh as usize) * w_out + ow as usize;
                                for mi in 0..mpg {
                                    let dst =
                                        o.add(nb * m_out * hw_out + (gi * mpg + mi) * hw_out + pos);
                                    *dst += *wp.add(mi * kh * kw + i * kw + j) * xv;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(bias) = bias {
        for nb in 0..nbatch {
            for m in 0..m_out {
                let bv = *bias.add(m);
                let p0 = nb * m_out * hw_out + m * hw_out;
                for p in 0..hw_out {
                    *o.add(p0 + p) += bv;
                }
            }
        }
    }
}

/// ONNX GRU (gate order z,r,h; optional biases; forward/backward/
/// bidirectional; `linear_before_reset` 0/1).
///
/// X `[seq, batch, in]`, W `[dir, 3h, in]`, R `[dir, 3h, h]`,
/// B `[dir, 6h]` (W-biases ‖ R-biases), H0 `[dir, batch, h]`.
/// Y `[seq, dir, batch, h]` (optional), Y_h `[dir, batch, h]` (optional).
///
/// # Safety
/// See module docs.
#[allow(clippy::too_many_arguments)]
pub unsafe fn gru(
    x: *const f32,
    xsh: &[u32],
    w: *const f32,
    r: *const f32,
    b: Option<*const f32>,
    h0: Option<*const f32>,
    y: Option<*mut f32>,
    yh: Option<*mut f32>,
    hidden: usize,
    direction: i64,
    lbr: bool,
    scratch: &mut Vec<f32>,
) {
    debug_assert_eq!(xsh.len(), 3);
    let seq = xsh[0] as usize;
    let batch = xsh[1] as usize;
    let in_dim = xsh[2] as usize;
    let h = hidden;
    let h3 = 3 * h;
    let num_dir = if direction == 2 { 2usize } else { 1usize };

    // scratch layout: rt [h*h3] | xproj [seq*batch*h3] | state [batch*h]
    //                 | rproj [batch*h3] | bz [h3]
    let rt_n = h * h3;
    let xp_n = seq * batch * h3;
    let total = rt_n + xp_n + batch * h + batch * h3 + h3;
    if scratch.len() < total {
        scratch.resize(total, 0.0);
    }
    let rt = scratch.as_mut_ptr();
    let xproj = rt.add(rt_n);
    let state = xproj.add(xp_n);
    let rproj = state.add(batch * h);
    let bz = rproj.add(batch * h3);

    let n_dirs = num_dir;
    for w_idx in 0..n_dirs {
        let reverse = direction == 1 || (direction == 2 && w_idx == 1);
        let wd = w.add(w_idx * h3 * in_dim);
        let rd = r.add(w_idx * h3 * h);

        // transpose R → rt: rt[k*h3 + j] = Rd[j*h + k]
        for j in 0..h3 {
            for kk in 0..h {
                *rt.add(kk * h3 + j) = *rd.add(j * h + kk);
            }
        }
        // xproj[t] = X[t] @ Wdᵀ : [batch, in] × [in, h3]
        for t in 0..seq {
            let xb = x.add(t * batch * in_dim);
            let pb = xproj.add(t * batch * h3);
            for i in 0..batch {
                let prow = pb.add(i * h3);
                for j in 0..h3 {
                    *prow.add(j) = 0.0;
                }
                for kk in 0..in_dim {
                    let xv = *xb.add(i * in_dim + kk);
                    for j in 0..h3 {
                        *prow.add(j) += xv * *wd.add(j * in_dim + kk);
                    }
                }
            }
        }
        // combined gate biases bz[j] = Wb[j] + Rb[j]
        let (wb_h_ptr, rb_h_ptr): (*const f32, *const f32) = match b {
            Some(bp) => (bp.add(w_idx * 6 * h + 2 * h), bp.add(w_idx * 6 * h + 5 * h)),
            None => (std::ptr::null(), std::ptr::null()),
        };
        if b.is_some() {
            let bd = b.unwrap().add(w_idx * 6 * h);
            for j in 0..h3 {
                *bz.add(j) = *bd.add(j) + *bd.add(h3 + j);
            }
        } else {
            for j in 0..h3 {
                *bz.add(j) = 0.0;
            }
        }
        // initial state
        match h0 {
            Some(hp) => {
                for i in 0..batch * h {
                    *state.add(i) = *hp.add(w_idx * batch * h + i);
                }
            }
            None => {
                for i in 0..batch * h {
                    *state.add(i) = 0.0;
                }
            }
        }

        for si in 0..seq {
            let t = if reverse { seq - 1 - si } else { si };
            let pb = xproj.add(t * batch * h3);
            // rproj = state @ Rᵀ : [batch, h] × [h, h3]
            for i in 0..batch {
                let orow = rproj.add(i * h3);
                for j in 0..h3 {
                    *orow.add(j) = 0.0;
                }
                for kk in 0..h {
                    let sv = *state.add(i * h + kk);
                    let wrow = rt.add(kk * h3);
                    for j in 0..h3 {
                        *orow.add(j) += sv * *wrow.add(j);
                    }
                }
            }
            // gates + state update (rounding order mirrors replay.py)
            for i in 0..batch {
                for j in 0..h {
                    let xz = *pb.add(i * h3 + j);
                    let rz_ = *rproj.add(i * h3 + j);
                    let xr = *pb.add(i * h3 + h + j);
                    let rr = *rproj.add(i * h3 + h + j);
                    let xh_ = *pb.add(i * h3 + 2 * h + j);
                    let rh = *rproj.add(i * h3 + 2 * h + j);
                    let bz_z = *bz.add(j);
                    let bz_r = *bz.add(h + j);
                    let wb_h = if wb_h_ptr.is_null() {
                        0.0
                    } else {
                        *wb_h_ptr.add(j)
                    };
                    let rb_h = if rb_h_ptr.is_null() {
                        0.0
                    } else {
                        *rb_h_ptr.add(j)
                    };
                    let zt = 1.0 / (1.0 + (-((xz + rz_) + bz_z)).exp());
                    let rt_ = 1.0 / (1.0 + (-((xr + rr) + bz_r)).exp());
                    let ht = if lbr {
                        ((xh_ + wb_h) + rt_ * (rh + rb_h)).tanh()
                    } else {
                        ((xh_ + rt_ * rh) + (wb_h + rb_h)).tanh()
                    };
                    let prev = *state.add(i * h + j);
                    *state.add(i * h + j) = (1.0 - zt) * ht + zt * prev;
                }
            }
            if let Some(yp) = y {
                // Y layout [seq, dir, batch, h]
                let dst = yp.add((t * num_dir + w_idx) * batch * h);
                for i in 0..batch * h {
                    *dst.add(i) = *state.add(i);
                }
            }
        }
        if let Some(yhp) = yh {
            let dst = yhp.add(w_idx * batch * h);
            for i in 0..batch * h {
                *dst.add(i) = *state.add(i);
            }
        }
    }
}

/// BatchNormalization (inference), NCHW, channels on axis 1.
///
/// # Safety
/// See module docs.
#[allow(clippy::too_many_arguments)]
pub unsafe fn batch_norm(
    x: *const f32,
    xsh: &[u32],
    scale: *const f32,
    bias: *const f32,
    mean: *const f32,
    var: *const f32,
    o: *mut f32,
    eps: f32,
) {
    let c = xsh[1] as usize;
    let spatial = numel(xsh) / c;
    for ch in 0..c {
        let inv = 1.0 / (*var.add(ch) + eps).sqrt();
        let sc = *scale.add(ch);
        let bi = *bias.add(ch);
        let mu = *mean.add(ch);
        let xs = x.add(ch * spatial);
        let os = o.add(ch * spatial);
        for p in 0..spatial {
            *os.add(p) = ((*xs.add(p) - mu) * inv) * sc + bi;
        }
    }
}

/// LayerNormalization (opset-17 semantics): normalize over dims `axis..`;
/// scale/bias have the shape of the normalized tail.
///
/// # Safety
/// See module docs.
pub unsafe fn layer_norm(
    x: *const f32,
    xsh: &[u32],
    scale: *const f32,
    bias: Option<*const f32>,
    o: *mut f32,
    axis: usize,
    eps: f32,
) {
    let block: usize = xsh[axis..].iter().fold(1usize, |a, &d| a * d as usize);
    let leading = numel(xsh) / block;
    for l in 0..leading {
        let xs = x.add(l * block);
        let os = o.add(l * block);
        let mut sum = 0.0f32;
        for i in 0..block {
            sum += *xs.add(i);
        }
        let mean = sum / block as f32;
        let mut vs = 0.0f32;
        for i in 0..block {
            let c = *xs.add(i) - mean;
            vs += c * c;
        }
        let inv = 1.0 / (vs / block as f32 + eps).sqrt();
        for i in 0..block {
            let c = *xs.add(i) - mean;
            let mut v = (c * inv) * *scale.add(i);
            if let Some(bp) = bias {
                v += *bp.add(i);
            }
            *os.add(i) = v;
        }
    }
}

/// General N-D transpose.
///
/// # Safety
/// See module docs.
pub unsafe fn transpose(x: *const f32, xsh: &[u32], perm: &[i64], o: *mut f32) {
    let n = xsh.len();
    debug_assert!(n <= MAX_NDIM && n == perm.len());
    let mut osh = [0u32; MAX_NDIM];
    for k in 0..n {
        osh[k] = xsh[perm[k] as usize];
    }
    let total = numel(&osh[..n]);
    let xstr = row_major_strides(xsh);
    let mut coords = [0usize; MAX_NDIM];
    for oi in 0..total {
        if oi > 0 {
            let mut k = n - 1;
            loop {
                coords[k] += 1;
                if coords[k] < osh[k] as usize {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
        let mut src = 0usize;
        for k in 0..n {
            src += coords[k] * xstr[perm[k] as usize];
        }
        *o.add(oi) = *x.add(src);
    }
}

/// Concat along `axis`.
///
/// # Safety
/// See module docs.
pub unsafe fn concat(ins: &[(*const f32, &[u32])], axis: usize, o: *mut f32, osh: &[u32]) {
    let inner: usize = osh[axis + 1..].iter().fold(1usize, |a, &d| a * d as usize);
    let outer: usize = osh[..axis].iter().fold(1usize, |a, &d| a * d as usize);
    let out_axis = osh[axis] as usize;
    for oi in 0..outer {
        let mut cum = 0usize;
        for &(ptr, sh) in ins {
            let rows = sh[axis] as usize;
            let src = ptr.add(oi * rows * inner);
            let dst = o.add(oi * out_axis * inner + cum * inner);
            std::ptr::copy_nonoverlapping(src, dst, rows * inner);
            cum += rows;
        }
        debug_assert_eq!(cum, out_axis);
    }
}

/// Slice with compile-time-normalized ranges.
/// i64 attrs: `[n, (axis, start, end, step) × n]`; for negative steps,
/// `end == -1` means "through index 0" (ONNX semantics) and the compiler
/// encodes an empty selection as `(start=0, end=0)`.
///
/// # Safety
/// See module docs.
pub unsafe fn slice(x: *const f32, xsh: &[u32], attrs: &[i64], o: *mut f32, osh: &[u32]) {
    let n = xsh.len();
    debug_assert!(n <= MAX_NDIM && n >= 1);
    let cnt = attrs[0] as usize;
    let mut start = [0i64; MAX_NDIM];
    let mut step = [1i64; MAX_NDIM];
    let mut counts = [1usize; MAX_NDIM];
    for d in 0..n {
        counts[d] = xsh[d] as usize;
    }
    for k in 0..cnt {
        let axis = attrs[1 + 4 * k] as usize;
        let s = attrs[2 + 4 * k];
        let e = attrs[3 + 4 * k];
        let st = attrs[4 + 4 * k];
        start[axis] = s;
        step[axis] = st;
        let c = if st > 0 {
            ((e - s) + st - 1) / st
        } else if s == 0 && e == 0 {
            0 // empty-selection sentinel from the compiler
        } else {
            // iterate s, s+st, … while idx > e (e == -1 → through index 0)
            ((s - e) + (-st) - 1) / (-st)
        };
        counts[axis] = c.max(0) as usize;
    }
    let total = numel(osh);
    debug_assert_eq!(counts[..n].iter().fold(1usize, |a, &d| a * d), total);
    if total == 0 {
        return;
    }
    let xstr = row_major_strides(xsh);

    // fast path: contiguous run along the last axis
    if n == 1 || step[n - 1] == 1 {
        let inner_run = counts[n - 1];
        let outer_n = total / inner_run.max(1);
        let mut coords = [0usize; MAX_NDIM];
        let mut dst = 0usize;
        for oi in 0..outer_n {
            if oi > 0 && n >= 2 {
                let mut k = n - 2;
                loop {
                    coords[k] += 1;
                    if coords[k] < counts[k] {
                        break;
                    }
                    coords[k] = 0;
                    if k == 0 {
                        break;
                    }
                    k -= 1;
                }
            }
            let mut src = start[n - 1] as usize * xstr[n - 1];
            for k in 0..n - 1 {
                src += (start[k] + coords[k] as i64 * step[k]) as usize * xstr[k];
            }
            std::ptr::copy_nonoverlapping(x.add(src), o.add(dst), inner_run);
            dst += inner_run;
        }
        return;
    }

    // general path: per-element odometer
    let mut coords = [0usize; MAX_NDIM];
    for oi in 0..total {
        if oi > 0 {
            let mut k = n - 1;
            loop {
                coords[k] += 1;
                if coords[k] < counts[k] {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
        let mut src = 0usize;
        for k in 0..n {
            src += (start[k] + coords[k] as i64 * step[k]) as usize * xstr[k];
        }
        *o.add(oi) = *x.add(src);
    }
}

/// Constant padding. i64 attrs: `[begin_0..begin_{n-1}, end_0..end_{n-1}]`.
///
/// # Safety
/// See module docs.
pub unsafe fn pad(x: *const f32, xsh: &[u32], pads: &[i64], cv: f32, o: *mut f32, osh: &[u32]) {
    let n = xsh.len();
    debug_assert!(n <= MAX_NDIM && pads.len() == 2 * n);
    let total = numel(osh);
    for i in 0..total {
        *o.add(i) = cv;
    }
    let in_total = numel(xsh);
    if in_total == 0 {
        return;
    }
    let ostr = row_major_strides(osh);
    let inner = xsh[n - 1] as usize; // contiguous in both src and dst
    let outer = in_total / inner;
    let mut coords = [0usize; MAX_NDIM];
    for oi in 0..outer {
        if oi > 0 && n >= 2 {
            let mut k = n - 2;
            loop {
                coords[k] += 1;
                if coords[k] < xsh[k] as usize {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
        let mut dst = pads[n - 1] as usize * ostr[n - 1];
        for k in 0..n - 1 {
            dst += (coords[k] + pads[k] as usize) * ostr[k];
        }
        std::ptr::copy_nonoverlapping(x.add(oi * inner), o.add(dst), inner);
    }
}

/// Broadcast copy (Expand): every input dim is 1 or equal to the out dim.
///
/// # Safety
/// See module docs.
pub unsafe fn expand(x: *const f32, xsh: &[u32], o: *mut f32, osh: &[u32]) {
    let n = osh.len();
    let on = numel(osh);
    if n == 0 {
        *o = *x;
        return;
    }
    if numel(xsh) == on {
        std::ptr::copy_nonoverlapping(x, o, on);
        return;
    }
    let astr = broadcast_strides(xsh, osh);
    let inner = osh[n - 1] as usize;
    let outer = if inner == 0 { 0 } else { on / inner };
    let alast = astr[n - 1];
    let mut coords = [0usize; MAX_NDIM];
    for oi in 0..outer {
        let mut aoff = 0usize;
        for k in 0..n - 1 {
            aoff += coords[k] * astr[k];
        }
        let dst = o.add(oi * inner);
        if alast == 1 {
            std::ptr::copy_nonoverlapping(x.add(aoff), dst, inner);
        } else {
            let av = *x.add(aoff);
            for j in 0..inner {
                *dst.add(j) = av;
            }
        }
        if n >= 2 {
            let mut k = n - 2;
            loop {
                coords[k] += 1;
                if coords[k] < osh[k] as usize {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
    }
}

/// Gather with constant indices (`idx` = attribute values after the axis).
///
/// # Safety
/// See module docs.
pub unsafe fn gather(x: *const f32, xsh: &[u32], axis: i64, idx: &[i64], o: *mut f32, osh: &[u32]) {
    let n = xsh.len();
    let axis = if axis < 0 {
        (axis + n as i64) as usize
    } else {
        axis as usize
    };
    let inner: usize = xsh[axis + 1..].iter().fold(1usize, |a, &d| a * d as usize);
    let outer: usize = xsh[..axis].iter().fold(1usize, |a, &d| a * d as usize);
    let dim = xsh[axis] as i64;
    debug_assert_eq!(numel(osh), outer * idx.len() * inner);
    for oi in 0..outer {
        for (t, &ix) in idx.iter().enumerate() {
            let ixd = if ix < 0 { ix + dim } else { ix } as usize;
            let src = x.add((oi * dim as usize + ixd) * inner);
            let dst = o.add((oi * idx.len() + t) * inner);
            std::ptr::copy_nonoverlapping(src, dst, inner);
        }
    }
}

/// ReduceMean / ReduceL2 over the given axes (l2 = sqrt of summed squares).
///
/// # Safety
/// See module docs.
pub unsafe fn reduce(
    l2: bool,
    x: *const f32,
    xsh: &[u32],
    axes: &[i64],
    _keepdims: bool,
    o: *mut f32,
    osh: &[u32],
) {
    let n = xsh.len();
    debug_assert!(n <= MAX_NDIM);
    let total_in = numel(xsh);
    let total_out = numel(osh);
    for i in 0..total_out {
        *o.add(i) = 0.0;
    }
    let mut reduced = [false; MAX_NDIM];
    for &a in axes {
        let a = if a < 0 {
            (a + n as i64) as usize
        } else {
            a as usize
        };
        reduced[a] = true;
    }
    // output flat index as a linear form over input coordinates:
    // non-reduced axes keep row-major order of the output shape
    let mut ostr = [0usize; MAX_NDIM];
    {
        let mut acc = 1usize;
        for k in (0..n).rev() {
            if !reduced[k] {
                ostr[k] = acc;
                acc *= xsh[k] as usize;
            }
        }
        debug_assert_eq!(acc, total_out);
    }
    let mut count = 1usize;
    for k in 0..n {
        if reduced[k] {
            count *= xsh[k] as usize;
        }
    }
    let mut coords = [0usize; MAX_NDIM];
    for xi in 0..total_in {
        if xi > 0 {
            let mut k = n - 1;
            loop {
                coords[k] += 1;
                if coords[k] < xsh[k] as usize {
                    break;
                }
                coords[k] = 0;
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
        let mut dst = 0usize;
        for k in 0..n {
            dst += coords[k] * ostr[k];
        }
        let v = *x.add(xi);
        *o.add(dst) += if l2 { v * v } else { v };
    }
    if l2 {
        for i in 0..total_out {
            *o.add(i) = (*o.add(i)).sqrt();
        }
    } else {
        let inv = 1.0f32 / count as f32;
        for i in 0..total_out {
            *o.add(i) *= inv;
        }
    }
}

/// Resize: linear mode, half_pixel transform, target sizes from `osh`.
/// Separable per-dimension passes ping-pong between the scratch buffers;
/// the final pass writes to `o`.
///
/// # Safety
/// See module docs.
pub unsafe fn resize(
    x: *const f32,
    xsh: &[u32],
    o: *mut f32,
    osh: &[u32],
    scratch: &mut Vec<f32>,
    scratch2: &mut Vec<f32>,
) {
    let n = xsh.len();
    debug_assert_eq!(n, osh.len());
    debug_assert!(n <= MAX_NDIM);
    let mut max_total = 1usize;
    let mut dims: [usize; MAX_NDIM] = [0; MAX_NDIM];
    let mut m = 0usize;
    for d in 0..n {
        let mx = xsh[d].max(osh[d]) as usize;
        max_total *= mx;
        if xsh[d] != osh[d] {
            dims[m] = d;
            m += 1;
        }
    }
    let out_total = numel(osh);
    if m == 0 {
        std::ptr::copy_nonoverlapping(x, o, out_total);
        return;
    }
    scratch.resize(max_total, 0.0);
    scratch2.resize(max_total, 0.0);
    let mut cur_shape = [0u32; MAX_NDIM];
    cur_shape[..n].copy_from_slice(xsh);
    let mut src: *const f32 = x;
    for pass in 0..m {
        let d = dims[pass];
        let last = pass + 1 == m;
        let dst: *mut f32 = if last {
            o
        } else if pass % 2 == 0 {
            scratch.as_mut_ptr()
        } else {
            scratch2.as_mut_ptr()
        };
        let in_d = cur_shape[d] as usize;
        let out_d = osh[d] as usize;
        let ratio = in_d as f32 / out_d as f32;
        let outer: usize = cur_shape[..d].iter().fold(1usize, |a, &v| a * v as usize);
        let inner: usize = cur_shape[d + 1..n]
            .iter()
            .fold(1usize, |a, &v| a * v as usize);
        for oo in 0..outer {
            for p in 0..out_d {
                let coord = (p as f32 + 0.5) * ratio - 0.5;
                let lo = coord.floor() as i64;
                let frac = coord - lo as f32;
                let lo_c = lo.clamp(0, in_d as i64 - 1) as usize;
                let hi_c = (lo + 1).clamp(0, in_d as i64 - 1) as usize;
                let slow = src.add((oo * in_d + lo_c) * inner);
                let shigh = src.add((oo * in_d + hi_c) * inner);
                let drow = dst.add((oo * out_d + p) * inner);
                for i in 0..inner {
                    let lv = *slow.add(i);
                    let hv = *shigh.add(i);
                    *drow.add(i) = lv + (hv - lv) * frac;
                }
            }
        }
        cur_shape[d] = osh[d];
        src = dst as *const f32;
    }
}

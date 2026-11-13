//! Frame layout and register allocation.

use dj_ir::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Value directly in the register.
    Reg(u32),
    /// Register holds a cell (captured binding).
    Cell(u32),
}

impl Storage {
    pub fn reg(self) -> u32 {
        match self {
            Storage::Reg(r) | Storage::Cell(r) => r,
        }
    }
}

pub struct FrameLayout {
    /// Blocks in emission order; entry first.
    pub order: Vec<BlockId>,
    pub local_store: Vec<Storage>,
    /// Register of each temp (`u32::MAX` for temps never defined in reachable code).
    pub temp_reg: Vec<u32>,
    /// First register of the outgoing-argument window.
    pub out_base: u32,
    pub register_count: u32,
}

const NONE: u32 = u32::MAX;

pub fn is_dedicated(f: &Function, l: LocalId) -> bool {
    let nparams = f.param_count as usize + f.has_rest as usize;
    let loc = &f.locals[l.index()];
    l.index() < nparams || loc.captured || loc.kind != LocalKind::Synthetic
}

impl FrameLayout {
    pub fn compute(f: &Function) -> FrameLayout {
        let order = block_order(f);
        let nlocals = f.locals.len();
        let ntemps = f.temp_count as usize;

        // --- dedicated registers -------------------------------------------
        let nparams = f.param_count as usize + f.has_rest as usize;
        let mut local_store = vec![Storage::Reg(NONE); nlocals];
        let mut next_reg = nparams as u32;
        for (i, l) in f.locals.iter().enumerate() {
            let id = LocalId(i as u32);
            if !is_dedicated(f, id) {
                continue;
            }
            let r = if i < nparams {
                i as u32
            } else {
                next_reg += 1;
                next_reg - 1
            };
            local_store[i] = if l.captured { Storage::Cell(r) } else { Storage::Reg(r) };
        }
        let dedicated_end = next_reg.max(nparams as u32);

        // --- linear positions ------------------------------------------------
        // Instruction k (terminators included) uses at slot 2k, defines at 2k+1.
        let mut block_first = vec![0usize; f.blocks.len()];
        let mut block_last = vec![0usize; f.blocks.len()];
        let mut k = 0usize;
        for &b in &order {
            block_first[b.index()] = k;
            k += f.block(b).insts.len() + 1;
            block_last[b.index()] = k - 1;
        }

        // --- peepholes: coalescing and aliasing ------------------------------
        let mut use_count = vec![0u32; ntemps];
        for &b in &order {
            let blk = f.block(b);
            for i in &blk.insts {
                for t in i.kind.uses() {
                    use_count[t.index()] += 1;
                }
            }
            for t in blk.term.kind.uses() {
                use_count[t.index()] += 1;
            }
        }
        // temp → dedicated local whose register it shares
        let mut alias: Vec<Option<LocalId>> = vec![None; ntemps];
        let reg_local = |l: LocalId| is_dedicated(f, l) && !f.locals[l.index()].captured;
        for &b in &order {
            let insts = &f.block(b).insts;
            // Coalesce `t = op; store l, t`.
            let mut write_pos: Vec<(LocalId, usize)> = Vec::new();
            for (j, inst) in insts.iter().enumerate() {
                match &inst.kind {
                    InstKind::StoreLocal { local, src } => {
                        let coalesce = j > 0
                            && reg_local(*local)
                            && use_count[src.index()] == 1
                            && insts[j - 1].kind.defs().contains(src)
                            && !matches!(insts[j - 1].kind, InstKind::CatchValue { .. });
                        if coalesce {
                            alias[src.index()] = Some(*local);
                            write_pos.push((*local, j - 1));
                        } else {
                            write_pos.push((*local, j));
                        }
                    }
                    InstKind::DeclareLocal { local } | InstKind::CopyBinding { local } => write_pos.push((*local, j)),
                    _ => {}
                }
            }
            // Alias `t = load l` to l's register when no write to l occurs
            // strictly between the load and t's last use in this block.
            for (j, inst) in insts.iter().enumerate() {
                let InstKind::LoadLocal { dst, local, .. } = &inst.kind else { continue };
                if alias[dst.index()].is_some() || !reg_local(*local) {
                    continue;
                }
                let mut last_use = None;
                let mut all_here = 0;
                for (jj, later) in insts.iter().enumerate().skip(j + 1) {
                    if later.kind.uses().contains(dst) {
                        last_use = Some(jj);
                        all_here += later.kind.uses().iter().filter(|t| *t == dst).count() as u32;
                    }
                }
                let term_uses = f.block(b).term.kind.uses().iter().filter(|t| *t == dst).count() as u32;
                if term_uses > 0 {
                    last_use = Some(insts.len());
                    all_here += term_uses;
                }
                if all_here != use_count[dst.index()] {
                    continue; // used in another block
                }
                let Some(lu) = last_use else {
                    alias[dst.index()] = Some(*local); // unused load
                    continue;
                };
                let clobbered = write_pos.iter().any(|&(l, w)| l == *local && w > j && w < lu);
                if !clobbered {
                    alias[dst.index()] = Some(*local);
                }
            }
        }

        // --- liveness over virtual registers ---------------------------------
        // vreg ids: temps 0..ntemps, synthetic locals ntemps + l.
        let nv = ntemps + nlocals;
        let words = nv.div_ceil(64).max(1);
        let vreg_local = |l: LocalId| !is_dedicated(f, l);
        let mut gen_ = vec![vec![0u64; words]; f.blocks.len()];
        let mut kill = vec![vec![0u64; words]; f.blocks.len()];
        let set = |bs: &mut [u64], v: usize| bs[v / 64] |= 1 << (v % 64);
        let get = |bs: &[u64], v: usize| bs[v / 64] & (1 << (v % 64)) != 0;
        let inst_vregs = |kind: &InstKind, uses: &mut Vec<usize>, defs: &mut Vec<usize>| {
            for t in kind.uses() {
                if alias[t.index()].is_none() {
                    uses.push(t.index());
                }
            }
            for t in kind.defs() {
                if alias[t.index()].is_none() {
                    defs.push(t.index());
                }
            }
            match kind {
                InstKind::LoadLocal { local, .. } if vreg_local(*local) => uses.push(ntemps + local.index()),
                InstKind::StoreLocal { local, .. } if vreg_local(*local) => defs.push(ntemps + local.index()),
                _ => {}
            }
        };
        for &b in &order {
            let blk = f.block(b);
            let (g, kl) = (&mut gen_[b.index()], &mut kill[b.index()]);
            let handle = |uses: &[usize], defs: &[usize], g: &mut Vec<u64>, kl: &mut Vec<u64>| {
                for &u in uses {
                    if !get(kl, u) {
                        set(g, u);
                    }
                }
                for &d in defs {
                    set(kl, d);
                }
            };
            for i in &blk.insts {
                let (mut u, mut d) = (Vec::new(), Vec::new());
                inst_vregs(&i.kind, &mut u, &mut d);
                handle(&u, &d, g, kl);
            }
            let u: Vec<usize> = blk.term.kind.uses().iter().filter(|t| alias[t.index()].is_none()).map(|t| t.index()).collect();
            handle(&u, &[], g, kl);
        }
        let mut live_in = vec![vec![0u64; words]; f.blocks.len()];
        let mut live_out = vec![vec![0u64; words]; f.blocks.len()];
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order.iter().rev() {
                let bi = b.index();
                let blk = f.block(b);
                let mut out = vec![0u64; words];
                for s in blk.term.kind.successors() {
                    for w in 0..words {
                        out[w] |= live_in[s.index()][w];
                    }
                }
                let hin = blk.handler.map(|h| live_in[h.index()].clone());
                if let Some(h) = &hin {
                    for w in 0..words {
                        out[w] |= h[w];
                    }
                }
                let mut inn = vec![0u64; words];
                for w in 0..words {
                    inn[w] = gen_[bi][w] | (out[w] & !kill[bi][w]) | hin.as_ref().map_or(0, |h| h[w]);
                }
                if out != live_out[bi] || inn != live_in[bi] {
                    live_out[bi] = out;
                    live_in[bi] = inn;
                    changed = true;
                }
            }
        }

        // --- intervals -------------------------------------------------------
        let mut start = vec![usize::MAX; nv];
        let mut end = vec![0usize; nv];
        let mut seen = vec![false; nv];
        let mut touch = |v: usize, s: usize, e: usize, start: &mut Vec<usize>, end: &mut Vec<usize>| {
            seen[v] = true;
            start[v] = start[v].min(s);
            end[v] = end[v].max(e);
        };
        for &b in &order {
            let bi = b.index();
            let blk = f.block(b);
            let bs = 2 * block_first[bi];
            let be = 2 * block_last[bi] + 1;
            for v in 0..nv {
                if get(&live_in[bi], v) {
                    let s = if b == order[0] { 0 } else { bs };
                    touch(v, s, bs, &mut start, &mut end);
                }
                if get(&live_out[bi], v) {
                    touch(v, be, be, &mut start, &mut end);
                }
            }
            for (j, i) in blk.insts.iter().enumerate() {
                let k = block_first[bi] + j;
                let (mut u, mut d) = (Vec::new(), Vec::new());
                inst_vregs(&i.kind, &mut u, &mut d);
                for v in u {
                    touch(v, 2 * k, 2 * k, &mut start, &mut end);
                }
                for v in d {
                    touch(v, 2 * k + 1, 2 * k + 1, &mut start, &mut end);
                }
            }
            let k = block_last[bi];
            for t in blk.term.kind.uses() {
                if alias[t.index()].is_none() {
                    touch(t.index(), 2 * k, 2 * k, &mut start, &mut end);
                }
            }
        }

        // --- linear scan (no spilling; lowest free register first) -----------
        let mut vreg_reg = vec![NONE; nv];
        let mut intervals: Vec<usize> = (0..nv).filter(|&v| seen[v]).collect();
        intervals.sort_by_key(|&v| (start[v], v));
        let mut active: Vec<usize> = Vec::new();
        let mut free: std::collections::BTreeSet<u32> = Default::default();
        let mut top = dedicated_end;
        for v in intervals {
            active.retain(|&a| {
                if end[a] < start[v] {
                    free.insert(vreg_reg[a]);
                    false
                } else {
                    true
                }
            });
            let r = match free.pop_first() {
                Some(r) => r,
                None => {
                    top += 1;
                    top - 1
                }
            };
            vreg_reg[v] = r;
            active.push(v);
        }

        for (i, l) in local_store.iter_mut().enumerate() {
            if vreg_local(LocalId(i as u32)) {
                let r = vreg_reg[ntemps + i];
                // Never referenced in reachable code: give it a harmless slot.
                *l = Storage::Reg(if r == NONE { 0 } else { r });
            }
        }
        let mut temp_reg = vec![NONE; ntemps];
        for t in 0..ntemps {
            temp_reg[t] = match alias[t] {
                Some(l) => local_store[l.index()].reg(),
                None => vreg_reg[t],
            };
        }

        // --- outgoing argument window ----------------------------------------
        let mut window = 0u32;
        for &b in &order {
            for i in &f.block(b).insts {
                let n = match &i.kind {
                    InstKind::Call { args, .. } | InstKind::New { args, .. } | InstKind::SuperCall { args, .. } => args.len(),
                    InstKind::CopyDataProps { excluded, .. } => excluded.len(),
                    _ => 0,
                };
                window = window.max(n as u32);
            }
        }
        FrameLayout { order, local_store, temp_reg, out_base: top, register_count: top + window }
    }
}

/// Reachable blocks (normal and exceptional edges), entry first, then index order.
fn block_order(f: &Function) -> Vec<BlockId> {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut stack = vec![f.entry];
    seen[f.entry.index()] = true;
    while let Some(b) = stack.pop() {
        for s in f.successors(b) {
            if !seen[s.index()] {
                seen[s.index()] = true;
                stack.push(s);
            }
        }
    }
    let mut order = vec![f.entry];
    order.extend((0..n).filter(|&i| seen[i] && i != f.entry.index()).map(|i| BlockId(i as u32)));
    order
}

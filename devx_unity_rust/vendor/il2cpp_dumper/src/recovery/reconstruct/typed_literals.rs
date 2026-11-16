//! Restores typed primitive literals (`float`, `double`, `bool`), drops ABI
//! `(bool & 1)` masks, and splits coalesced multi-bool stores back into
//! individual `bool` field writes.

use std::collections::HashMap;

use crate::recovery::csharp::{BinOp, CmpKind, Expr, Stmt};
use crate::recovery::resolve::ResolveMaps;
use crate::recovery::types::{ManagedType, PrimKind};

use super::copy_prop::for_each_expr_mut;
use super::il2cpp_cleanup::expr_children_mut;
use super::store_sugar::{parent_chain_tdis, parse_raw_target};

struct LiteralCtx<'a> {
    maps: &'a ResolveMaps,
    local_types: &'a HashMap<String, i32>,
    local_mty: &'a HashMap<String, ManagedType>,
    param_prims: HashMap<String, PrimKind>,
    ret_prim: Option<PrimKind>,
    declaring_tdi: i32,
}

/// Applies typed literal normalization, `(bool & 1)` cleanup, and packed
/// multi-bool store splitting over `stmts`. Returns the number of edits made.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    stmts: &mut Vec<Stmt>,
    maps: &ResolveMaps,
    local_types: &HashMap<String, i32>,
    local_mty: &HashMap<String, ManagedType>,
    param_sigs: &[(String, String)],
    ret_type: &ManagedType,
    declaring_tdi: i32,
) -> usize {
    let mut param_prims = HashMap::new();
    for (ty, name) in param_sigs {
        if let Some(pk) = prim_of_type_name(ty) {
            param_prims.insert(name.clone(), pk);
        }
    }
    let ctx = LiteralCtx {
        maps,
        local_types,
        local_mty,
        param_prims,
        ret_prim: prim_of_managed(ret_type),
        declaring_tdi,
    };
    rewrite_stmt_list(stmts, &ctx)
}

fn prim_of_managed(ty: &ManagedType) -> Option<PrimKind> {
    match ty {
        ManagedType::Primitive(p @ (PrimKind::Boolean | PrimKind::R4 | PrimKind::R8)) => Some(*p),
        _ => None,
    }
}

fn prim_of_type_name(ty: &str) -> Option<PrimKind> {
    match ty.trim() {
        "bool" | "Boolean" | "System.Boolean" => Some(PrimKind::Boolean),
        "float" | "Single" | "System.Single" => Some(PrimKind::R4),
        "double" | "Double" | "System.Double" => Some(PrimKind::R8),
        _ => None,
    }
}

fn lookup_field_type_tdi(maps: &ResolveMaps, recv_tdi: i32, field: &str) -> Option<i32> {
    parent_chain_tdis(maps, recv_tdi).into_iter().find_map(|p| {
        maps.field_type_by_name
            .get(&(p, field.to_string()))
            .copied()
    })
}

fn lookup_field_prim(maps: &ResolveMaps, recv_tdi: i32, field: &str) -> Option<PrimKind> {
    parent_chain_tdis(maps, recv_tdi).into_iter().find_map(|p| {
        maps.field_prim_by_name
            .get(&(p, field.to_string()))
            .copied()
    })
}

fn lookup_field_offset(maps: &ResolveMaps, recv_tdi: i32, field: &str) -> Option<i64> {
    for p in parent_chain_tdis(maps, recv_tdi) {
        if let Some(fields) = maps.tdi_fields.get(&p) {
            if let Some((off, _, _)) = fields.iter().find(|(_, name, _)| name == field) {
                return Some(*off);
            }
        }
    }
    // Fallback for lightweight unit-test maps where only `field_layouts` is populated.
    for p in parent_chain_tdis(maps, recv_tdi) {
        if let Some(((_, off), _)) = maps
            .field_layouts
            .iter()
            .find(|((tdi, _), (name, _))| *tdi == p && name == field)
        {
            return Some(*off);
        }
    }
    None
}

fn lookup_field_layout_and_prim<'a>(
    maps: &'a ResolveMaps,
    recv_tdi: i32,
    off: i64,
) -> Option<(&'a str, Option<PrimKind>)> {
    for p in parent_chain_tdis(maps, recv_tdi) {
        if let Some((name, _)) = maps.field_layouts.get(&(p, off)) {
            let pk = parent_chain_tdis(maps, recv_tdi)
                .into_iter()
                .find_map(|tp| maps.field_prims.get(&(tp, off)).copied())
                .or_else(|| lookup_field_prim(maps, p, name));
            return Some((name.as_str(), pk));
        }
    }
    None
}

fn resolve_dotted_recv_tdi(base: &str, ctx: &LiteralCtx) -> Option<i32> {
    if base.contains(' ') {
        return None;
    }
    let mut parts = base.split('.');
    let root = parts.next()?;
    let mut tdi = if root == "this" {
        (ctx.declaring_tdi >= 0).then_some(ctx.declaring_tdi)?
    } else {
        *ctx.local_types.get(root)?
    };
    for field in parts {
        tdi = lookup_field_type_tdi(ctx.maps, tdi, field)?;
    }
    Some(tdi)
}

fn field_recv_tdi(recv: &Expr, ctx: &LiteralCtx) -> Option<i32> {
    match recv {
        Expr::This => (ctx.declaring_tdi >= 0).then_some(ctx.declaring_tdi),
        Expr::Var(v) => ctx.local_types.get(v).copied(),
        Expr::Field(inner, name) => {
            if name == "static_fields" {
                return field_recv_tdi(inner, ctx);
            }
            let inner_tdi = field_recv_tdi(inner, ctx)?;
            lookup_field_type_tdi(ctx.maps, inner_tdi, name)
        }
        _ => None,
    }
}

fn var_prim(name: &str, ctx: &LiteralCtx) -> Option<PrimKind> {
    ctx.param_prims.get(name).copied().or_else(|| {
        ctx.local_mty
            .get(name)
            .and_then(prim_of_managed)
    })
}

fn expr_prim_type(e: &Expr, ctx: &LiteralCtx) -> Option<PrimKind> {
    match e {
        Expr::Bool(_) | Expr::Cmp(..) | Expr::Not(_) => Some(PrimKind::Boolean),
        Expr::Float(_) => Some(PrimKind::R4),
        Expr::Double(_) => Some(PrimKind::R8),
        Expr::Var(name) => var_prim(name, ctx),
        Expr::Cast(ty, _) => prim_of_type_name(ty),
        Expr::Field(recv, name) => {
            let tdi = field_recv_tdi(recv, ctx)?;
            lookup_field_prim(ctx.maps, tdi, name)
        }
        Expr::StaticField(ty, field) => {
            let tdi = ctx.maps.static_field_types.get(&(ty.clone(), field.clone()))?;
            let (ns, name) = ctx.maps.tdi_names.get(tdi)?;
            if ns == "System" {
                prim_of_type_name(name)
            } else {
                None
            }
        }
        Expr::RawCall(addr, _, ret_hint) => ctx
            .maps
            .methods_by_rva
            .get(addr)
            .and_then(|m| prim_of_managed(&m.sig.ret))
            .or_else(|| ret_hint.as_deref().and_then(prim_of_type_name)),
        _ => None,
    }
}

fn target_prim_type(target: &str, ctx: &LiteralCtx) -> Option<PrimKind> {
    if !target.contains('(') && !target.contains('*') {
        if let Some(pk) = var_prim(target, ctx) {
            return Some(pk);
        }
        if let Some((recv, field)) = target.rsplit_once('.') {
            if let Some(tdi) = resolve_dotted_recv_tdi(recv, ctx) {
                return lookup_field_prim(ctx.maps, tdi, field);
            }
        }
    }
    None
}

fn decode_prim_int(val: i64, pk: PrimKind) -> Option<Expr> {
    match pk {
        PrimKind::Boolean => match val {
            0 => Some(Expr::Bool(false)),
            1 => Some(Expr::Bool(true)),
            _ => None,
        },
        PrimKind::R4 => {
            let bits = if (0..=u32::MAX as i64).contains(&val) {
                val as u32
            } else if (i32::MIN as i64..0).contains(&val) {
                (val as i32) as u32
            } else {
                return None;
            };
            if bits == 0 {
                return Some(Expr::Float(0.0));
            }
            let f = f32::from_bits(bits);
            (f.is_normal() || bits == 0x8000_0000).then_some(Expr::Float(f))
        }
        PrimKind::R8 => {
            let bits = val as u64;
            if bits == 0 {
                return Some(Expr::Double(0.0));
            }
            let d = f64::from_bits(bits);
            (d.is_normal() || bits == 0x8000_0000_0000_0000).then_some(Expr::Double(d))
        }
        _ => None,
    }
}

fn unwrap_bool_bit_and(e: &Expr, ctx: &LiteralCtx, require_bool: bool) -> Option<Expr> {
    let Expr::Bin(BinOp::And, lhs, rhs) = e else {
        return None;
    };
    let candidate = if matches!(rhs.as_ref(), Expr::Int(1)) {
        lhs.as_ref()
    } else if matches!(lhs.as_ref(), Expr::Int(1)) {
        rhs.as_ref()
    } else {
        return None;
    };
    if require_bool || expr_prim_type(candidate, ctx) == Some(PrimKind::Boolean) {
        Some(candidate.clone())
    } else {
        None
    }
}

fn apply_expected_prim(e: &mut Expr, pk: PrimKind, ctx: &LiteralCtx) -> usize {
    let mut n = 0;
    if pk == PrimKind::Boolean {
        if let Some(unwrapped) = unwrap_bool_bit_and(e, ctx, true) {
            *e = unwrapped;
            n += 1;
        }
    }
    match e {
        Expr::Int(v) => {
            if let Some(lit) = decode_prim_int(*v, pk) {
                *e = lit;
                n += 1;
            }
        }
        Expr::Cond(_, then_e, else_e) => {
            n += apply_expected_prim(then_e, pk, ctx);
            n += apply_expected_prim(else_e, pk, ctx);
        }
        _ => {}
    }
    n
}

fn try_bool_cmp_fold(op: CmpKind, bool_expr: &Expr, int_val: i64) -> Option<Expr> {
    let is_true_form = match (op, int_val) {
        (CmpKind::Eq, 1) | (CmpKind::Ne, 0) => true,
        (CmpKind::Eq, 0) | (CmpKind::Ne, 1) => false,
        _ => return None,
    };
    Some(if is_true_form {
        bool_expr.clone()
    } else {
        Expr::Not(Box::new(bool_expr.clone()))
    })
}

fn rewrite_expr(e: &mut Expr, ctx: &LiteralCtx) -> usize {
    let mut n = 0;
    for c in expr_children_mut(e) {
        n += rewrite_expr(c, ctx);
    }
    if let Some(unwrapped) = unwrap_bool_bit_and(e, ctx, false) {
        *e = unwrapped;
        n += 1;
    }
    if let Expr::RawCall(addr, args, _) = e {
        if let Some(m) = ctx.maps.methods_by_rva.get(addr) {
            // Parameter indices exclude `this` when `!is_static` and renderer kept `this` out of `args`,
            // or include `this` if `args.len() == m.sig.params.len() + 1`. Match suffix cleanly.
            let param_offset = args.len().saturating_sub(m.sig.params.len());
            for (idx, param) in m.sig.params.iter().enumerate() {
                if let Some(pk) = prim_of_managed(param) {
                    if let Some(arg) = args.get_mut(param_offset + idx) {
                        n += apply_expected_prim(arg, pk, ctx);
                    }
                }
            }
        }
    }
    if let Expr::Cmp(op, lhs, rhs) = e {
        let lp = expr_prim_type(lhs, ctx);
        let rp = expr_prim_type(rhs, ctx);
        if lp == Some(PrimKind::Boolean) {
            if let Expr::Int(v) = rhs.as_ref() {
                if let Some(folded) = try_bool_cmp_fold(*op, lhs, *v) {
                    *e = folded;
                    return n + 1;
                }
            }
        } else if rp == Some(PrimKind::Boolean) {
            if let Expr::Int(v) = lhs.as_ref() {
                if let Some(folded) = try_bool_cmp_fold(*op, rhs, *v) {
                    *e = folded;
                    return n + 1;
                }
            }
        }
        if let Some(pk @ (PrimKind::R4 | PrimKind::R8)) = lp {
            n += apply_expected_prim(rhs, pk, ctx);
        } else if let Some(pk @ (PrimKind::R4 | PrimKind::R8)) = rp {
            n += apply_expected_prim(lhs, pk, ctx);
        }
    }
    n
}

fn try_split_packed_bool_assign(
    target: &str,
    rhs: &Expr,
    ctx: &LiteralCtx,
) -> Option<Vec<Stmt>> {
    let Expr::Int(val) = rhs else {
        return None;
    };
    if *val < 0 {
        return None;
    }
    let u = *val as u64;

    let (recv_prefix, recv_tdi, base_off, width_hint): (String, i32, i64, Option<usize>) =
        if let Some((base, off, width)) = parse_raw_target(target) {
            if width < 2 || !target.starts_with("*(") {
                return None;
            }
            let tdi = resolve_dotted_recv_tdi(base, ctx)?;
            (base.to_string(), tdi, off, Some(width as usize))
        } else if let Some((recv, first_field)) = target.rsplit_once('.') {
            if *val < 2 || recv.contains('(') || recv.contains('*') {
                return None;
            }
            let tdi = resolve_dotted_recv_tdi(recv, ctx)?;
            let off = lookup_field_offset(ctx.maps, tdi, first_field)?;
            if lookup_field_prim(ctx.maps, tdi, first_field) != Some(PrimKind::Boolean) {
                return None;
            }
            (recv.to_string(), tdi, off, None)
        } else {
            return None;
        };

    let width = width_hint.unwrap_or_else(|| {
        if u <= 0xffff {
            2
        } else if u <= 0xffff_ffff {
            4
        } else {
            8
        }
    });
    if width < 2 {
        return None;
    }

    let mut out = Vec::new();
    for k in 0..width {
        let byte_val = ((u >> (k * 8)) & 0xff) as u8;
        match lookup_field_layout_and_prim(ctx.maps, recv_tdi, base_off + k as i64) {
            Some((fname, Some(PrimKind::Boolean))) => {
                if byte_val > 1 {
                    return None;
                }
                out.push(Stmt::Assign(
                    format!("{recv_prefix}.{fname}"),
                    None,
                    Expr::Bool(byte_val == 1),
                ));
            }
            None if byte_val == 0 => {}
            _ => return None,
        }
    }
    (out.len() >= 2).then_some(out)
}

fn rewrite_stmt_list(stmts: &mut Vec<Stmt>, ctx: &LiteralCtx) -> usize {
    let mut n = 0;
    let mut rebuilt: Option<Vec<Stmt>> = None;

    for idx in 0..stmts.len() {
        let s = &mut stmts[idx];
        for k in super::children_mut(s) {
            n += rewrite_stmt_list(k, ctx);
        }
        if let Stmt::Using { decl, .. } = s {
            let mut single = vec![decl.as_ref().clone()];
            let dn = rewrite_stmt_list(&mut single, ctx);
            if dn > 0 {
                if let Some(d) = single.into_iter().next() {
                    **decl = d;
                }
                n += dn;
            }
        }
        for_each_expr_mut(s, &mut |e| {
            n += rewrite_expr(e, ctx);
        });

        match s {
            Stmt::Assign(target, decl, rhs) => {
                if let Some(split) = try_split_packed_bool_assign(target, rhs, ctx) {
                    let prefix = rebuilt.get_or_insert_with(|| stmts[..idx].to_vec());
                    prefix.extend(split);
                    n += 1;
                    continue;
                }
                if let Some(pk) = decl
                    .as_deref()
                    .and_then(prim_of_type_name)
                    .or_else(|| target_prim_type(target, ctx))
                {
                    n += apply_expected_prim(rhs, pk, ctx);
                }
            }
            Stmt::Return(Some(expr)) => {
                if let Some(pk) = ctx.ret_prim {
                    n += apply_expected_prim(expr, pk, ctx);
                }
            }
            _ => {}
        }

        if let Some(ref mut out) = rebuilt {
            out.push(s.clone());
        }
    }

    if let Some(out) = rebuilt {
        *stmts = out;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::csharp::render_stmts;

    fn make_ctx_maps() -> ResolveMaps {
        let mut m = ResolveMaps::default();
        m.field_layouts
            .insert((5, 0x3e0), ("<CanMove>k__BackingField".into(), "Ctrl".into()));
        m.field_prims.insert((5, 0x3e0), PrimKind::Boolean);
        m.field_prim_by_name
            .insert((5, "<CanMove>k__BackingField".into()), PrimKind::Boolean);

        m.field_layouts
            .insert((5, 0x3e1), ("<CanRot>k__BackingField".into(), "Ctrl".into()));
        m.field_prims.insert((5, 0x3e1), PrimKind::Boolean);
        m.field_prim_by_name
            .insert((5, "<CanRot>k__BackingField".into()), PrimKind::Boolean);

        m.field_layouts
            .insert((5, 0x3d7), ("IsVirtual".into(), "Ctrl".into()));
        m.field_prims.insert((5, 0x3d7), PrimKind::Boolean);
        m.field_prim_by_name
            .insert((5, "IsVirtual".into()), PrimKind::Boolean);

        m.field_layouts
            .insert((5, 0x3e4), ("ApplySuitAnimTime".into(), "Ctrl".into()));
        m.field_prims.insert((5, 0x3e4), PrimKind::R4);
        m.field_prim_by_name
            .insert((5, "ApplySuitAnimTime".into()), PrimKind::R4);

        m.tdi_fields.insert(
            5,
            vec![
                (0x3d7, "IsVirtual".into(), -1),
                (0x3e0, "<CanMove>k__BackingField".into(), -1),
                (0x3e1, "<CanRot>k__BackingField".into(), -1),
                (0x3e4, "ApplySuitAnimTime".into(), -1),
            ],
        );
        m
    }

    #[test]
    fn rewrites_float_bit_pattern_and_bool_masks_and_splits_packed_bools() {
        let maps = make_ctx_maps();
        let locals = HashMap::new();
        let mty = HashMap::new();
        let params = vec![("bool".into(), "value".into())];
        let mut stmts = vec![
            Stmt::Assign(
                "this.ApplySuitAnimTime".into(),
                None,
                Expr::Int(3221225472),
            ),
            Stmt::Assign(
                "this.<CanMove>k__BackingField".into(),
                None,
                Expr::Bin(
                    BinOp::And,
                    Box::new(Expr::Var("value".into())),
                    Box::new(Expr::Int(1)),
                ),
            ),
            Stmt::Assign(
                "this.<CanMove>k__BackingField".into(),
                None,
                Expr::Int(257),
            ),
            Stmt::If {
                cond: Expr::Cmp(
                    CmpKind::Eq,
                    Box::new(Expr::Field(Box::new(Expr::This), "IsVirtual".into())),
                    Box::new(Expr::Int(0)),
                ),
                then_body: vec![Stmt::Assign(
                    "this.IsVirtual".into(),
                    None,
                    Expr::Int(1),
                )],
                else_body: vec![],
            },
        ];

        let edits = apply(
            &mut stmts,
            &maps,
            &locals,
            &mty,
            &params,
            &ManagedType::Unknown,
            5,
        );
        assert!(edits >= 4, "expected all typed literal fixes: {edits}");
        let text = render_stmts(&stmts);
        assert!(text.contains("-2f"), "3221225472 should render as -2f: {text}");
        assert!(
            text.contains("this.<CanMove>k__BackingField = value;"),
            "value & 1 should simplify: {text}"
        );
        assert!(
            text.contains("this.<CanMove>k__BackingField = true;")
                && text.contains("this.<CanRot>k__BackingField = true;"),
            "257 store should split into two true stores: {text}"
        );
        assert!(
            text.contains("if (!this.IsVirtual)"),
            "IsVirtual == 0 should simplify: {text}"
        );
        assert!(
            text.contains("this.IsVirtual = true;"),
            "1 assigned to bool should render true: {text}"
        );
    }

    #[test]
    fn splits_raw_multi_bool_and_rewrites_bool_and_float_returns() {
        let maps = make_ctx_maps();
        let locals = HashMap::new();
        let mty = HashMap::new();

        let mut raw_stmts = vec![Stmt::Assign(
            "*(short*)(this + 992)".into(), // 0x3e0 = 992
            None,
            Expr::Int(256), // 0x0100 => false at 0x3e0, true at 0x3e1
        )];
        apply(
            &mut raw_stmts,
            &maps,
            &locals,
            &mty,
            &[],
            &ManagedType::Primitive(PrimKind::Boolean),
            5,
        );
        let t1 = render_stmts(&raw_stmts);
        assert!(
            t1.contains("this.<CanMove>k__BackingField = false;")
                && t1.contains("this.<CanRot>k__BackingField = true;"),
            "raw short bool store should split: {t1}"
        );

        let mut ret_stmts = vec![Stmt::Return(Some(Expr::Int(1)))];
        apply(
            &mut ret_stmts,
            &maps,
            &locals,
            &mty,
            &[],
            &ManagedType::Primitive(PrimKind::Boolean),
            5,
        );
        assert_eq!(render_stmts(&ret_stmts).trim(), "return true;");
    }

    #[test]
    fn does_not_split_mixed_bool_and_non_bool_fields_or_small_subnormal_float() {
        let maps = make_ctx_maps();
        let locals = HashMap::new();
        let mty = HashMap::new();
        let mut stmts = vec![
            // ApplySuitAnimTime sits at 0x3e4; 0x3e1 is CanRot (bool), 0x3e2-0x3e3 padding,
            // so value 0x0100_0001 across 4 bytes has non-zero at +3 or covers 0x3e4 if 8 bytes.
            Stmt::Assign(
                "this.ApplySuitAnimTime".into(),
                None,
                Expr::Int(5), // subnormal as f32 -> leave untouched
            ),
            Stmt::Assign(
                "*(short*)(this + 0x3e3)".into(),
                None,
                Expr::Int(257),
            ),
        ];
        let edits = apply(
            &mut stmts,
            &maps,
            &locals,
            &mty,
            &[],
            &ManagedType::Unknown,
            5,
        );
        assert_eq!(edits, 0);
    }
}

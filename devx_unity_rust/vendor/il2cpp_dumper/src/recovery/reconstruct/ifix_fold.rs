//! Optional folding of InjectFix (`IFix`) wrapper boilerplate.
//!
//! When an IL2CPP game ships with InjectFix instrumentation, every hotfixable
//! method opens with an injected patch guard:
//!
//! ```csharp
//! if (!WrappersManagerImpl.IsPatched(4038))
//! {
//!     // original body
//! }
//! else
//! {
//!     // patch dispatch
//! }
//! ```
//!
//! (or the non-negated `if (WrappersManagerImpl.IsPatched(4038)) { ... } else { ... }`
//! or early-exit `if (WrappersManagerImpl.IsPatched(4038)) { ...; return; }`).
//!
//! When opted in (`--fold-ifix` / `Config::fold_ifix_wrappers`), this pass
//! replaces the wrapper shell with a `// [IFix patch #<id>]` marker followed by
//! the unpatched original statements, provided the discarded patch arm defines
//! no label targeted from outside that arm.

use std::collections::{HashMap, HashSet};

use super::super::csharp::{CmpKind, Expr, Stmt};
use super::children;

/// Unwrap InjectFix `WrappersManagerImpl.IsPatched(id)` wrappers in place.
/// Returns the number of wrappers folded.
pub(crate) fn apply(stmts: &mut Vec<Stmt>) -> usize {
    let mut goto_counts: HashMap<String, usize> = HashMap::new();
    count_gotos(stmts, &mut goto_counts);
    let mut folded = 0;
    fold_list(stmts, &mut goto_counts, &mut folded);
    folded
}

fn is_wrappers_manager_receiver(e: &Expr) -> bool {
    match e {
        Expr::Var(s) => s == "WrappersManagerImpl" || s.ends_with(".WrappersManagerImpl"),
        Expr::Field(_, name) | Expr::StaticField(_, name) => name == "WrappersManagerImpl",
        _ => false,
    }
}

fn match_patch_id(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(id) => Some(*id),
        Expr::Cast(_, inner) => match_patch_id(inner),
        _ => None,
    }
}

fn match_ifix_call(e: &Expr) -> Option<i64> {
    let Expr::Call { recv, name, args } = e else {
        return None;
    };
    let is_callee = match recv.as_deref() {
        Some(r) => name == "IsPatched" && is_wrappers_manager_receiver(r),
        None => {
            name == "WrappersManagerImpl.IsPatched" || name.ends_with(".WrappersManagerImpl.IsPatched")
        }
    };
    if !is_callee || args.len() != 1 {
        return None;
    }
    match_patch_id(&args[0])
}

fn is_zero_or_false(e: &Expr) -> bool {
    matches!(e, Expr::Bool(false) | Expr::Int(0))
}

fn is_one_or_true(e: &Expr) -> bool {
    matches!(e, Expr::Bool(true) | Expr::Int(1))
}

/// Match `WrappersManagerImpl.IsPatched(id)` (possibly wrapped in `!` or `== 0`/`!= 0`).
/// Returns `(negated, patch_id)` where `negated == true` means `then_body` is the
/// unpatched original code.
fn match_ifix_cond(e: &Expr) -> Option<(bool, i64)> {
    match e {
        Expr::Not(inner) => {
            let (neg, id) = match_ifix_cond(inner)?;
            Some((!neg, id))
        }
        Expr::Cmp(CmpKind::Eq, a, b) => {
            if is_zero_or_false(b) {
                let (neg, id) = match_ifix_cond(a)?;
                Some((!neg, id))
            } else if is_zero_or_false(a) {
                let (neg, id) = match_ifix_cond(b)?;
                Some((!neg, id))
            } else if is_one_or_true(b) {
                match_ifix_cond(a)
            } else if is_one_or_true(a) {
                match_ifix_cond(b)
            } else {
                None
            }
        }
        Expr::Cmp(CmpKind::Ne, a, b) => {
            if is_zero_or_false(b) {
                match_ifix_cond(a)
            } else if is_zero_or_false(a) {
                match_ifix_cond(b)
            } else if is_one_or_true(b) {
                let (neg, id) = match_ifix_cond(a)?;
                Some((!neg, id))
            } else if is_one_or_true(a) {
                let (neg, id) = match_ifix_cond(b)?;
                Some((!neg, id))
            } else {
                None
            }
        }
        _ => match_ifix_call(e).map(|id| (false, id)),
    }
}

fn ends_with_terminator(stmts: &[Stmt]) -> bool {
    let last = stmts
        .iter()
        .rev()
        .find(|s| !matches!(s, Stmt::Comment(_) | Stmt::Label(_) | Stmt::Attribute { .. }));
    match last {
        Some(
            Stmt::Return(_)
            | Stmt::Throw(_)
            | Stmt::Goto(_)
            | Stmt::GotoCase(_)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::YieldBreak,
        ) => true,
        Some(Stmt::If {
            then_body,
            else_body,
            ..
        }) => {
            !else_body.is_empty()
                && ends_with_terminator(then_body)
                && ends_with_terminator(else_body)
        }
        _ => false,
    }
}

fn count_gotos(stmts: &[Stmt], counts: &mut HashMap<String, usize>) {
    let mut subs = Vec::new();
    for s in stmts {
        if let Stmt::Goto(lbl) = s {
            *counts.entry(lbl.clone()).or_insert(0) += 1;
        }
        subs.clear();
        children(s, &mut subs);
        for child in &subs {
            count_gotos(child, counts);
        }
    }
}

fn collect_labels(stmts: &[Stmt], labels: &mut HashSet<String>) {
    let mut subs = Vec::new();
    for s in stmts {
        if let Stmt::Label(lbl) = s {
            labels.insert(lbl.clone());
        }
        subs.clear();
        children(s, &mut subs);
        for child in &subs {
            collect_labels(child, labels);
        }
    }
}

fn safe_to_discard(discarded: &[Stmt], total_gotos: &mut HashMap<String, usize>) -> bool {
    if discarded.is_empty() {
        return true;
    }
    let mut discarded_labels = HashSet::new();
    collect_labels(discarded, &mut discarded_labels);
    let mut discarded_gotos = HashMap::new();
    count_gotos(discarded, &mut discarded_gotos);

    for lbl in &discarded_labels {
        let total = total_gotos.get(lbl).copied().unwrap_or(0);
        let inside = discarded_gotos.get(lbl).copied().unwrap_or(0);
        if total > inside {
            return false;
        }
    }

    for (lbl, inside) in discarded_gotos {
        if let Some(entry) = total_gotos.get_mut(&lbl) {
            *entry = entry.saturating_sub(inside);
        }
    }
    true
}

fn try_fold_if(stmt: &Stmt, total_gotos: &mut HashMap<String, usize>) -> Option<Vec<Stmt>> {
    let Stmt::If {
        cond,
        then_body,
        else_body,
    } = stmt
    else {
        return None;
    };
    let (negated, patch_id) = match_ifix_cond(cond)?;

    let (kept, discarded): (&[Stmt], &[Stmt]) = if negated {
        (then_body.as_slice(), else_body.as_slice())
    } else if !else_body.is_empty() {
        (else_body.as_slice(), then_body.as_slice())
    } else if ends_with_terminator(then_body) {
        (&[], then_body.as_slice())
    } else {
        return None;
    };

    if !safe_to_discard(discarded, total_gotos) {
        return None;
    }

    let mut replacement = Vec::with_capacity(1 + kept.len());
    replacement.push(Stmt::Comment(format!("[IFix patch #{patch_id}]")));
    replacement.extend(kept.iter().cloned());
    Some(replacement)
}

fn fold_list(list: &mut Vec<Stmt>, total_gotos: &mut HashMap<String, usize>, folded: &mut usize) {
    let mut i = 0;
    while i < list.len() {
        if let Some(replacement) = try_fold_if(&list[i], total_gotos) {
            list.splice(i..=i, replacement);
            *folded += 1;
            // Advance past the inserted `[IFix patch #...]` comment so any nested
            // wrapper in the spliced body is visited without re-visiting the comment.
            i += 1;
            continue;
        }
        for child in super::children_mut(&mut list[i]) {
            fold_list(child, total_gotos, folded);
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_patched_call(id: i64) -> Expr {
        Expr::Call {
            recv: Some(Box::new(Expr::Var("WrappersManagerImpl".into()))),
            name: "IsPatched".into(),
            args: vec![Expr::Int(id)],
        }
    }

    #[test]
    fn folds_negated_is_patched_keeping_then_body() {
        let mut stmts = vec![Stmt::If {
            cond: Expr::Not(Box::new(is_patched_call(4038))),
            then_body: vec![Stmt::Assign("x".into(), None, Expr::Int(1))],
            else_body: vec![Stmt::Return(None)],
        }];
        assert_eq!(apply(&mut stmts), 1);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Comment(c) if c == "[IFix patch #4038]"));
        assert!(matches!(&stmts[1], Stmt::Assign(t, None, Expr::Int(1)) if t == "x"));
    }

    #[test]
    fn folds_positive_is_patched_with_else_keeping_else_body() {
        let mut stmts = vec![Stmt::If {
            cond: Expr::Cmp(
                CmpKind::Ne,
                Box::new(is_patched_call(99)),
                Box::new(Expr::Int(0)),
            ),
            then_body: vec![Stmt::Return(Some(Expr::Int(0)))],
            else_body: vec![Stmt::Assign("y".into(), None, Expr::Int(2))],
        }];
        assert_eq!(apply(&mut stmts), 1);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Comment(c) if c == "[IFix patch #99]"));
        assert!(matches!(&stmts[1], Stmt::Assign(t, None, Expr::Int(2)) if t == "y"));
    }

    #[test]
    fn folds_early_exit_positive_is_patched_guard() {
        let mut stmts = vec![
            Stmt::If {
                cond: is_patched_call(12),
                then_body: vec![Stmt::Return(None)],
                else_body: vec![],
            },
            Stmt::Assign("z".into(), None, Expr::Int(3)),
        ];
        assert_eq!(apply(&mut stmts), 1);
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Comment(c) if c == "[IFix patch #12]"));
        assert!(matches!(&stmts[1], Stmt::Assign(t, None, Expr::Int(3)) if t == "z"));
    }

    #[test]
    fn skips_folding_if_discarded_arm_holds_externally_targeted_label() {
        let mut stmts = vec![Stmt::If {
            cond: Expr::Not(Box::new(is_patched_call(7))),
            then_body: vec![Stmt::Goto("L_shared".into())],
            else_body: vec![Stmt::Label("L_shared".into()), Stmt::Return(None)],
        }];
        assert_eq!(apply(&mut stmts), 0);
        assert!(matches!(&stmts[0], Stmt::If { .. }));
    }
}

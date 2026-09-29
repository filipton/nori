//! A short animated list (the equalizer's devices): a leaving row keeps its place while it folds away.

/// One row as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<K> {
    pub key: K,
    /// On screen, all or part of the way.
    pub shown: bool,
    /// False while it leaves.
    pub wanted: bool,
}

/// The rows to draw once the list is `keys`, from the rows drawn `before` (none the first time, when
/// every row is simply shown). A removed row stays at its old place, leaving, until no longer shown.
/// Twin of Android's `AnimatedRows` (DevicesSection.kt).
pub fn merge_rows<K: Clone + PartialEq>(before: Option<&[Row<K>]>, keys: &[K]) -> Vec<Row<K>> {
    let old = before.unwrap_or(&[]);
    let mut next: Vec<Row<K>> = keys
        .iter()
        .map(|k| match old.iter().rev().find(|r| r.key == *k) {
            Some(r) => Row { key: k.clone(), shown: r.shown, wanted: true },
            None => Row { key: k.clone(), shown: before.is_none(), wanted: true },
        })
        .collect();
    for (i, r) in old.iter().enumerate() {
        if !keys.contains(&r.key) && (r.shown || r.wanted) {
            let at = i.min(next.len());
            next.insert(at, Row { key: r.key.clone(), shown: r.shown, wanted: false });
        }
    }
    next
}

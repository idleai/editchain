//! Bounded line alignment. Unresolved large replacements remain unmeasured.

pub(super) fn split(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect()
}

pub(super) fn unchanged(before: &[String], after: &[String]) -> Option<Vec<(usize, usize)>> {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = before
        .iter()
        .skip(prefix)
        .rev()
        .zip(after.iter().skip(prefix).rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = before.len().saturating_sub(suffix);
    let new_end = after.len().saturating_sub(suffix);
    let old = before.get(prefix..old_end)?;
    let new = after.get(prefix..new_end)?;
    let width = new.len().checked_add(1)?;
    let cells = old.len().checked_add(1)?.checked_mul(width)?;
    if cells > 1_000_000 {
        return None;
    }
    let mut table = vec![0_usize; cells];
    for (i, left) in old.iter().enumerate().rev() {
        for (j, right) in new.iter().enumerate().rev() {
            let here = i.checked_mul(width)?.checked_add(j)?;
            let down = here.checked_add(width)?;
            let next = here.checked_add(1)?;
            let value = if left == right {
                table.get(down.checked_add(1)?)?.saturating_add(1)
            } else {
                (*table.get(down)?).max(*table.get(next)?)
            };
            *table.get_mut(here)? = value;
        }
    }
    let mut pairs: Vec<_> = (0..prefix).map(|i| (i, i)).collect();
    let (mut i, mut j) = (0_usize, 0_usize);
    while let (Some(left), Some(right)) = (old.get(i), new.get(j)) {
        if left == right {
            pairs.push((i.checked_add(prefix)?, j.checked_add(prefix)?));
            i = i.checked_add(1)?;
            j = j.checked_add(1)?;
        } else {
            let here = i.checked_mul(width)?.checked_add(j)?;
            if table.get(here.checked_add(width)?)? >= table.get(here.checked_add(1)?)? {
                i = i.checked_add(1)?;
            } else {
                j = j.checked_add(1)?;
            }
        }
    }
    pairs.extend((0..suffix).map(|i| (old_end.saturating_add(i), new_end.saturating_add(i))));
    Some(pairs)
}

pub(super) fn unique_block(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut matches = haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, lines)| *lines == needle);
    let (position, _) = matches.next()?;
    matches.next().is_none().then_some(position)
}

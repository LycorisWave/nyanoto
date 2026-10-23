//! Соседний альбом исполнителя: когда доиграл и для кнопок на краю альбома.

use std::collections::HashSet;

/// Следующий альбом: по списку и по кругу, пропуская сыгранные, или случайный несыгранный.
/// `None` — сыграны все.
pub fn next_album(
    titles: &[Option<String>],
    here: usize,
    played: &HashSet<Option<String>>,
    random: Option<u64>,
) -> Option<usize> {
    let unplayed: Vec<usize> = (0..titles.len())
        .filter(|&index| !played.contains(&titles[index]))
        .collect();
    if unplayed.is_empty() {
        return None;
    }
    match random {
        Some(number) => Some(unplayed[number as usize % unplayed.len()]),
        None => unplayed
            .iter()
            .copied()
            .find(|&index| index > here)
            .or_else(|| unplayed.first().copied()),
    }
}

/// Для кнопок: вперёд — следующий (по кругу), назад — откуда пришли или предыдущий.
/// `None` — альбом один.
pub fn beside(count: usize, here: usize, forward: bool, back: Option<usize>) -> Option<usize> {
    if count < 2 {
        return None;
    }
    if forward {
        return Some((here + 1) % count);
    }
    back.filter(|back| *back != here && *back < count).or(Some((here + count - 1) % count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(names: &[&str]) -> Vec<Option<String>> {
        names.iter().map(|name| Some(name.to_string())).collect()
    }

    fn played(names: &[&str]) -> HashSet<Option<String>> {
        names.iter().map(|name| Some(name.to_string())).collect()
    }

    #[test]
    fn in_order_goes_forward_then_wraps_then_stops() {
        let albums = titles(&["A", "B", "C", "D"]);
        // Начали с B: дальше C, D, потом A, потом всё.
        assert_eq!(next_album(&albums, 1, &played(&["B"]), None), Some(2));
        assert_eq!(next_album(&albums, 2, &played(&["B", "C"]), None), Some(3));
        assert_eq!(next_album(&albums, 3, &played(&["B", "C", "D"]), None), Some(0));
        assert_eq!(next_album(&albums, 0, &played(&["A", "B", "C", "D"]), None), None);
    }

    #[test]
    fn random_never_repeats_and_ends() {
        let albums = titles(&["A", "B", "C"]);
        let mut seen = played(&["A"]);
        let mut here = 0;
        for number in [7u64, 3] {
            let next = next_album(&albums, here, &seen, Some(number)).unwrap();
            assert!(seen.insert(albums[next].clone()), "альбом сыгран дважды");
            here = next;
        }
        assert_eq!(next_album(&albums, here, &seen, Some(5)), None);
    }

    /// Синглы 1, 2, 3: кнопками можно пройти вперёд и назад, по кругу.
    #[test]
    fn buttons_walk_albums_both_ways() {
        assert_eq!(beside(3, 0, true, None), Some(1));
        assert_eq!(beside(3, 1, true, None), Some(2));
        assert_eq!(beside(3, 2, true, None), Some(0));
        assert_eq!(beside(3, 2, false, None), Some(1));
        assert_eq!(beside(3, 0, false, None), Some(2));
        // Пришли из третьего (вразнобой) — назад туда же.
        assert_eq!(beside(3, 0, false, Some(2)), Some(2));
        assert_eq!(beside(3, 1, false, Some(1)), Some(0));
        assert_eq!(beside(1, 0, true, None), None);
        assert_eq!(beside(1, 0, false, Some(0)), None);
    }

    #[test]
    fn single_album_artist_just_stops() {
        assert_eq!(next_album(&titles(&["A"]), 0, &played(&["A"]), None), None);
    }
}

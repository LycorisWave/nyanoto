//! Очередь воспроизведения.

use std::path::PathBuf;

/// Элемент очереди. `id` не меняется при перестановках и не переиспользуется,
/// поэтому один и тот же файл может стоять в очереди несколько раз.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub path: PathBuf,
    /// Поставлен вручную («Играть следующим», «В очередь», «Играть сейчас»),
    /// а не пришёл с альбомом или плейлистом: счётчик очереди считает только такие.
    pub manual: bool,
}

#[derive(Default)]
pub(crate) struct Queue {
    items: Vec<QueueItem>,
    next_id: u64,
}

impl Queue {
    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn get(&self, index: usize) -> Option<&QueueItem> {
        self.items.get(index)
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    /// Новая очередь (альбом, плейлист, восстановленная после запуска).
    pub fn replace(&mut self, paths: Vec<PathBuf>) {
        self.items.clear();
        self.insert(0, paths, false);
    }

    /// Добавляет в конец вручную.
    pub fn append(&mut self, paths: Vec<PathBuf>) {
        self.insert(self.items.len(), paths, true);
    }

    /// Вставляет на место `index` вручную.
    pub fn insert_at(&mut self, index: usize, paths: Vec<PathBuf>) {
        self.insert(index, paths, true);
    }

    fn insert(&mut self, index: usize, paths: Vec<PathBuf>, manual: bool) {
        let index = index.min(self.items.len());
        let first_id = self.next_id;
        self.next_id += paths.len() as u64;
        let new = paths
            .into_iter()
            .zip(first_id..)
            .map(|(path, id)| QueueItem { id, path, manual });
        self.items.splice(index..index, new);
    }

    /// Переставляет элемент на место `index` (считая уже без него).
    /// `false`, если такого элемента нет.
    pub fn move_to(&mut self, id: u64, index: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        let item = self.items.remove(from);
        let index = index.min(self.items.len());
        self.items.insert(index, item);
        true
    }

    /// Ставит элементы в порядке `ids` (перемешивание и возврат порядка):
    /// чего нет в `ids`, остаётся в прежнем порядке после них. `id` не меняются.
    pub fn reorder(&mut self, ids: &[u64]) {
        let mut rest = std::mem::take(&mut self.items);
        let mut ordered = Vec::with_capacity(rest.len());
        for id in ids {
            if let Some(index) = rest.iter().position(|item| item.id == *id) {
                ordered.push(rest.remove(index));
            }
        }
        ordered.extend(rest);
        self.items = ordered;
    }

    /// Убирает элемент и возвращает место, где он стоял.
    pub fn remove(&mut self, id: u64) -> Option<usize> {
        let index = self.index_of(id)?;
        self.items.remove(index);
        Some(index)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn ids_survive_edits() {
        let mut queue = Queue::default();
        queue.replace(paths(&["a", "b", "c"]));
        let b = queue.get(1).unwrap().id;

        queue.insert_at(1, paths(&["x", "y"]));
        assert_eq!(queue.index_of(b), Some(3));

        let x = queue.get(1).unwrap().id;
        assert_eq!(queue.remove(x), Some(1));
        assert_eq!(queue.index_of(b), Some(2));
        assert_eq!(queue.len(), 4);
    }

    #[test]
    fn move_keeps_the_id() {
        let mut queue = Queue::default();
        queue.replace(paths(&["a", "b", "c", "d"]));
        let a = queue.get(0).unwrap().id;
        assert!(queue.move_to(a, 2));
        let order: Vec<_> = queue.items().iter().map(|item| item.path.to_str().unwrap()).collect();
        assert_eq!(order, ["b", "c", "a", "d"]);
        assert_eq!(queue.index_of(a), Some(2));
        assert!(queue.move_to(a, 99), "за концом — в конец");
        assert_eq!(queue.index_of(a), Some(3));
        assert!(!queue.move_to(1000, 0));
    }

    #[test]
    fn ids_are_not_reused() {
        let mut queue = Queue::default();
        queue.replace(paths(&["a", "b"]));
        let old: Vec<u64> = queue.items().iter().map(|item| item.id).collect();
        queue.replace(paths(&["a", "b"]));
        assert!(queue.items().iter().all(|item| !old.contains(&item.id)));
    }

    #[test]
    fn reorder_keeps_ids_and_puts_the_rest_after() {
        let mut queue = Queue::default();
        queue.replace(paths(&["a", "b", "c", "d"]));
        let ids: Vec<u64> = queue.items().iter().map(|item| item.id).collect();
        queue.reorder(&[ids[2], ids[0], 999]);
        let order: Vec<_> = queue.items().iter().map(|item| item.path.to_str().unwrap()).collect();
        assert_eq!(order, ["c", "a", "b", "d"]);
        assert_eq!(queue.index_of(ids[3]), Some(3));
    }

    /// Альбом — не «вручную»; «В очередь» и «Играть следующим» — вручную.
    #[test]
    fn manual_marks_only_user_additions() {
        let mut queue = Queue::default();
        queue.replace(paths(&["a", "b"]));
        queue.append(paths(&["c"]));
        queue.insert_at(1, paths(&["x"]));
        let manual: Vec<bool> = queue.items().iter().map(|item| item.manual).collect();
        assert_eq!(manual, [false, true, false, true]);
        queue.replace(paths(&["d"]));
        assert!(!queue.get(0).unwrap().manual);
    }
}

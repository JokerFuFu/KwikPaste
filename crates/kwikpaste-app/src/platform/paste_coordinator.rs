//! 协调本进程的粘贴写回与注入，防止重叠操作或显式复制使旧任务粘贴错误内容。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub(super) struct PasteCoordinator {
    busy: AtomicBool,
    generation: AtomicU64,
    started_ticks: AtomicI64,
}

pub(super) struct PasteLease {
    state: Arc<PasteCoordinator>,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct PasteToken {
    state: Arc<PasteCoordinator>,
    generation: u64,
}

impl PasteCoordinator {
    /// 在写回剪贴板前取得单次租约。
    pub(super) fn try_begin(self: &Arc<Self>, ticks: i64) -> Option<PasteLease> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.started_ticks.store(ticks, Ordering::SeqCst);
        Some(PasteLease {
            state: self.clone(),
            generation,
        })
    }

    /// 复制动作发生后，旧粘贴任务不能继续注入。
    pub(super) fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl PasteCoordinator {
    /// Queued controls observed before a newer operation cannot cancel or modify it.
    pub(super) fn started_after(&self, ticks: i64) -> bool {
        self.busy.load(Ordering::SeqCst) && self.started_ticks.load(Ordering::SeqCst) > ticks
    }

    pub(super) fn cancel_before(&self, ticks: i64) {
        if !self.started_after(ticks) {
            self.cancel();
        }
    }
}

impl PasteLease {
    pub(super) fn is_current(&self) -> bool {
        self.state.generation.load(Ordering::SeqCst) == self.generation
    }

    pub(super) fn token(&self) -> PasteToken {
        PasteToken {
            state: self.state.clone(),
            generation: self.generation,
        }
    }
}

impl PasteToken {
    pub(super) fn is_current(&self) -> bool {
        self.state.busy.load(Ordering::SeqCst)
            && self.state.generation.load(Ordering::SeqCst) == self.generation
    }
}

impl Drop for PasteLease {
    fn drop(&mut self) {
        self.state.busy.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_old_recapture_and_copy_hide_do_not_cancel_a_new_paste() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        state.cancel_before(150);
        assert!(!first.is_current());
        drop(first);
        let next = state.try_begin(200).unwrap();
        for old_control in [150, 175] {
            assert!(state.started_after(old_control));
            state.cancel_before(old_control);
            assert!(next.is_current());
            assert!(next.token().is_current());
        }
        assert!(!state.started_after(250));
        state.cancel_before(250);
        assert!(!next.is_current());
    }

    #[test]
    fn overlapping_paste_cannot_replace_clipboard_while_first_is_waiting() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        assert!(state.try_begin(100).is_none());
        assert!(first.is_current());
        drop(first);
        assert!(state.try_begin(100).is_some());
    }

    #[test]
    fn copy_cancels_old_task_without_releasing_its_lease() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        state.cancel();
        assert!(!first.is_current());
        assert!(state.try_begin(100).is_none());
        drop(first);
        let next = state.try_begin(100).unwrap();
        assert!(next.is_current());
    }

    #[test]
    fn cloned_tokens_cannot_release_or_outlive_the_owning_lease() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(100).unwrap();
        let token = lease.token();
        drop(token.clone());
        assert!(state.try_begin(100).is_none());
        assert!(token.is_current());
        drop(lease);
        assert!(!token.is_current());
        assert!(state.try_begin(100).is_some());
    }

    #[test]
    fn queued_injection_token_cannot_authorize_a_replacement_paste() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(100).unwrap();
        let token = lease.token();
        state.cancel();
        assert!(!token.is_current());
        drop(lease);
        let next = state.try_begin(100).unwrap();
        assert!(next.token().is_current());
        assert!(!token.is_current());
    }
}

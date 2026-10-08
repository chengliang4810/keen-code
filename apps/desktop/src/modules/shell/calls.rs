use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

const MAX_PENDING_CALLS: usize = 256;
const MAX_COMPLETED_CALLS: usize = 256;

struct PendingCall {
    cancellation: CancellationToken,
}

#[derive(Default)]
struct Calls {
    active: HashMap<String, CancellationToken>,
    pending: HashMap<String, PendingCall>,
    completed: VecDeque<String>,
    closed: bool,
}

#[derive(Default)]
pub(super) struct ShellCalls(Mutex<Calls>);

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err("invalid shell call ID".into());
    }
    Ok(())
}

impl ShellCalls {
    pub(super) fn begin(&self, id: &str) -> Result<CancellationToken, String> {
        validate_id(id)?;
        let mut calls = self.0.lock().unwrap();
        if calls.closed {
            return Err("shell session closed".into());
        }
        if calls.active.contains_key(id) || calls.completed.iter().any(|known| known == id) {
            return Err("shell call ID already used".into());
        }
        if calls.active.len() >= 32 {
            return Err("too many active shell calls".into());
        }
        let cancellation = calls
            .pending
            .remove(id)
            .map_or_else(CancellationToken::new, |pending| pending.cancellation);
        calls.active.insert(id.into(), cancellation.clone());
        Ok(cancellation)
    }

    pub(super) fn cancel(&self, id: &str) -> Result<(), String> {
        validate_id(id)?;
        let mut calls = self.0.lock().unwrap();
        if let Some(cancellation) = calls.active.get(id) {
            cancellation.cancel();
            return Ok(());
        }
        if calls.closed || calls.completed.iter().any(|known| known == id) {
            return Ok(());
        }
        if calls.pending.len() >= MAX_PENDING_CALLS && !calls.pending.contains_key(id) {
            return Err("too many pending shell cancellations".into());
        }
        let pending = calls
            .pending
            .entry(id.into())
            .or_insert_with(|| PendingCall {
                cancellation: CancellationToken::new(),
            });
        pending.cancellation.cancel();
        Ok(())
    }

    pub(super) fn finish(&self, id: &str) {
        let mut calls = self.0.lock().unwrap();
        calls.active.remove(id);
        calls.completed.push_back(id.into());
        while calls.completed.len() > MAX_COMPLETED_CALLS {
            calls.completed.pop_front();
        }
    }

    pub(super) fn close(&self) {
        let mut calls = self.0.lock().unwrap();
        calls.closed = true;
        for cancellation in calls.active.values() {
            cancellation.cancel();
        }
        calls.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_registration_prevents_execution() {
        let calls = ShellCalls::default();
        calls.cancel("call-a").unwrap();
        assert!(calls.begin("call-a").unwrap().is_cancelled());
        assert!(!calls.begin("call-b").unwrap().is_cancelled());
    }

    #[test]
    fn cancellation_targets_only_its_call_and_session() {
        let calls = ShellCalls::default();
        let a = calls.begin("call-a").unwrap();
        let b = calls.begin("call-b").unwrap();
        let other = ShellCalls::default().begin("call-a").unwrap();
        calls.cancel("call-a").unwrap();
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled());
        assert!(!other.is_cancelled());
        calls.finish("call-a");
        assert!(calls.begin("call-a").is_err());
    }

    #[test]
    fn pending_cancellations_and_completion_history_are_bounded() {
        let calls = ShellCalls::default();
        for index in 0..MAX_PENDING_CALLS {
            calls.cancel(&format!("pending-{index}")).unwrap();
        }
        assert!(calls.cancel("overflow").is_err());
        for index in 0..(MAX_COMPLETED_CALLS * 2) {
            let id = format!("completed-{index}");
            calls.begin(&id).unwrap();
            calls.finish(&id);
        }
        let data = calls.0.lock().unwrap();
        assert_eq!(data.pending.len(), MAX_PENDING_CALLS);
        assert_eq!(data.completed.len(), MAX_COMPLETED_CALLS);
    }

    #[test]
    fn close_cancels_active_calls_and_rejects_new_ones() {
        let calls = ShellCalls::default();
        let cancellation = calls.begin("call").unwrap();
        calls.close();
        assert!(cancellation.is_cancelled());
        assert!(calls.begin("next").is_err());
    }
}
